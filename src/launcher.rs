//! Turning a validated game into a running process.
//!
//! Games are started directly through `execve`, never through a shell, so each
//! `args` entry reaches the program as exactly one argument.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Command, ExitStatus};

use crate::game::Game;

/// Everything needed to start a game, fully resolved and validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    pub title: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub working_directory: PathBuf,
}

/// Resolves and validates a game without starting anything.
pub fn prepare(game: &Game, path_var: Option<&OsStr>) -> Result<LaunchPlan, String> {
    let program = game.resolve_executable(path_var)?;
    let working_directory = game.resolved_working_directory();
    if !working_directory.is_dir() {
        return Err(format!(
            "Working directory not found: {}",
            working_directory.display()
        ));
    }
    let args: Vec<String> = game.args.iter().map(|arg| game.expand(arg)).collect();
    if game.check_paths {
        // Emulators given a missing ROM tend to open their own interface,
        // which ordinary users must never see, so this has to fail here.
        if let Some(missing) = args
            .iter()
            .find(|arg| arg.starts_with('/') && !std::path::Path::new(arg).exists())
        {
            return Err(format!("File not found: {missing}"));
        }
    }
    Ok(LaunchPlan {
        title: game.title.clone(),
        program,
        args,
        env: game
            .env
            .iter()
            .map(|(name, value)| (name.clone(), game.expand(value)))
            .collect(),
        working_directory,
    })
}

impl LaunchPlan {
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .envs(self.env.iter().map(|(name, value)| (name, value)))
            .current_dir(&self.working_directory);
        command
    }
}

/// Mode A: runs the game as a child process and waits for it to exit.
pub fn run_child(plan: &LaunchPlan) -> io::Result<ExitStatus> {
    plan.command().status()
}

/// Mode B: replaces this process with the game. Returns only on failure.
///
/// A successful `exec` runs no destructors, so everything that must be
/// released, SDL above all, has to be gone before this is called.
pub fn replace_process(plan: &LaunchPlan) -> io::Error {
    plan.command().exec()
}

/// Describes an abnormal exit for the menu, or `None` for a clean exit.
pub fn describe_exit(status: ExitStatus) -> Option<String> {
    if status.success() {
        None
    } else if let Some(signal) = status.signal() {
        Some(format!("Game stopped by signal {signal}"))
    } else {
        status
            .code()
            .map(|code| format!("Game exited with status {code}"))
    }
}

/// Shell-style exit code for a finished child process.
pub fn exit_code(status: ExitStatus) -> u8 {
    match (status.code(), status.signal()) {
        (Some(code), _) => code as u8,
        (None, Some(signal)) => 128u8.saturating_add(signal as u8),
        (None, None) => 1,
    }
}

fn is_display_or_input_device(target: &str) -> bool {
    target.starts_with("/dev/dri/") || target.starts_with("/dev/input/")
}

/// Lists display and input devices this process still has open. A DRM file
/// descriptor left open here would keep DRM master away from the game.
pub fn held_devices() -> Vec<String> {
    let Ok(entries) = fs::read_dir("/proc/self/fd") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| fs::read_link(entry.path()).ok())
        .map(|target| target.to_string_lossy().into_owned())
        .filter(|target| is_display_or_input_device(target))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn game(fields: &str) -> Game {
        let text = format!("id = \"g\"\ntitle = \"G\"\nplatform = \"p\"\n{fields}");
        Game::parse(&text, Path::new("/")).unwrap()
    }

    fn args_of(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn arguments_keep_their_boundaries() {
        let plan = prepare(
            &game(
                r#"
                executable = "/usr/bin/sleep"
                args = ["-L", "a core.so", "Super Mario World.sfc", "", "'quoted' $HOME; rm -rf"]
                "#,
            ),
            None,
        )
        .unwrap();
        let command = plan.command();
        assert_eq!(command.get_program(), "/usr/bin/sleep");
        assert_eq!(
            args_of(&command),
            [
                "-L",
                "a core.so",
                "Super Mario World.sfc",
                "",
                "'quoted' $HOME; rm -rf"
            ]
        );
    }

    #[test]
    fn working_directory_and_environment_are_applied() {
        let plan = prepare(
            &game(
                r#"
                executable = "/usr/bin/sleep"
                working_directory = "/tmp"
                [env]
                KIOSK_MODE = "1"
                ROM_HOME = "{game_dir}roms"
                "#,
            ),
            None,
        )
        .unwrap();
        let command = plan.command();
        assert_eq!(command.get_current_dir(), Some(Path::new("/tmp")));
        let envs: Vec<_> = command
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.map(OsStr::to_owned)))
            .collect();
        assert_eq!(envs.len(), 2);
        assert_eq!(envs[0].0, "KIOSK_MODE");
        assert_eq!(envs[1].1.as_deref(), Some(OsStr::new("/roms")));
    }

    #[test]
    fn a_missing_rom_is_rejected_before_launch() {
        let error = prepare(
            &game(
                r#"
                executable = "/usr/bin/sleep"
                args = ["-f", "/games/no such rom.sfc"]
                "#,
            ),
            None,
        )
        .unwrap_err();
        assert!(error.contains("/games/no such rom.sfc"), "{error}");
    }

    #[test]
    fn expanded_arguments_are_path_checked() {
        let dir = tempfile::tempdir().unwrap();
        let text = "id = \"g\"\ntitle = \"G\"\nplatform = \"p\"\n\
                    executable = \"/usr/bin/sleep\"\nargs = [\"{game_dir}/rom.bin\"]\n";
        let game = Game::parse(text, dir.path()).unwrap();
        assert!(prepare(&game, None).is_err());
        fs::write(dir.path().join("rom.bin"), "").unwrap();
        let plan = prepare(&game, None).unwrap();
        assert_eq!(plan.args[0], dir.path().join("rom.bin").to_str().unwrap());
    }

    #[test]
    fn path_checking_can_be_disabled_and_ignores_other_arguments() {
        let unchecked = game(
            r#"
            executable = "/usr/bin/sleep"
            args = ["/this/will/be/created"]
            check_paths = false
            "#,
        );
        assert!(prepare(&unchecked, None).is_ok());

        let flags_only = game(
            r#"
            executable = "/usr/bin/sleep"
            args = ["-f", "--opt=/not/checked", "relative/path"]
            "#,
        );
        assert!(prepare(&flags_only, None).is_ok());
    }

    #[test]
    fn missing_executable_and_working_directory_are_reported() {
        let error = prepare(&game("executable = \"/no/such/emulator\"\n"), None).unwrap_err();
        assert!(error.contains("/no/such/emulator"), "{error}");
        let error = prepare(
            &game("executable = \"/usr/bin/sleep\"\nworking_directory = \"/no/such/dir\"\n"),
            None,
        )
        .unwrap_err();
        assert!(error.contains("/no/such/dir"), "{error}");
    }

    #[test]
    fn child_mode_reports_exit_status() {
        let ok = game("executable = \"/usr/bin/sleep\"\nargs = [\"0\"]\n");
        let status = run_child(&prepare(&ok, None).unwrap()).unwrap();
        assert!(status.success());
        assert_eq!(describe_exit(status), None);
        assert_eq!(exit_code(status), 0);

        let failing = game("executable = \"/usr/bin/false\"\n");
        let status = run_child(&prepare(&failing, None).unwrap()).unwrap();
        assert_eq!(describe_exit(status).unwrap(), "Game exited with status 1");
        assert_eq!(exit_code(status), 1);
    }

    #[test]
    fn signals_are_described_with_shell_exit_codes() {
        let killed = ExitStatus::from_raw(11);
        assert_eq!(describe_exit(killed).unwrap(), "Game stopped by signal 11");
        assert_eq!(exit_code(killed), 139);
    }

    #[test]
    fn recognises_display_and_input_devices() {
        assert!(is_display_or_input_device("/dev/dri/card0"));
        assert!(is_display_or_input_device("/dev/input/event3"));
        assert!(!is_display_or_input_device("/dev/null"));
        assert!(!is_display_or_input_device("/dev/tty1"));
    }
}
