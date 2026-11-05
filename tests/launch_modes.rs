//! Runs the real binary through both launch modes using harmless programs.
//! `--launch` exercises the menu's launch path without needing a display.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tempfile::TempDir;

const LAUNCHER: &str = env!("CARGO_BIN_EXE_kioskgamemenu");
const EXIT_NOT_FOUND: i32 = 127;

struct Kiosk {
    dir: TempDir,
}

impl Kiosk {
    fn new(keep_running: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = format!(
            "[launcher]\nkeepLauncherRunning = {keep_running}\n\
             [storage]\ndatabase = \"data/stats.sqlite\"\n"
        );
        fs::write(dir.path().join("config.toml"), config).unwrap();
        Self { dir }
    }

    fn game_dir(&self, id: &str) -> PathBuf {
        self.dir.path().join("games").join(id)
    }

    fn add_game(&self, id: &str, fields: &str) {
        let directory = self.game_dir(id);
        fs::create_dir_all(&directory).unwrap();
        let text = format!("id = \"{id}\"\ntitle = \"{id}\"\nplatform = \"test\"\n{fields}");
        fs::write(directory.join("game.toml"), text).unwrap();
    }

    fn command(&self, id: &str) -> Command {
        let mut command = Command::new(LAUNCHER);
        command
            .arg("--config")
            .arg(self.dir.path().join("config.toml"))
            .args(["--launch", id])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // No part of this may depend on the working directory.
            .current_dir("/");
        command
    }

    fn launch(&self, id: &str) -> Output {
        self.command(id).output().unwrap()
    }

    fn launch_count(&self, id: &str) -> i64 {
        let path = self.dir.path().join("data/stats.sqlite");
        if !path.exists() {
            return 0;
        }
        let connection = Connection::open(path).unwrap();
        connection
            .query_row(
                "SELECT launch_count FROM game_stats WHERE game_id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap_or(0)
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn process_name(pid: u32) -> String {
    fs::read_to_string(format!("/proc/{pid}/comm"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn wait_for_process_name(child: &Child, name: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if process_name(child.id()) == name {
            return true;
        }
        sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn both_modes_run_a_game_and_count_it_once() {
    for keep_running in [true, false] {
        let kiosk = Kiosk::new(keep_running);
        kiosk.add_game(
            "sleeper",
            "executable = \"/usr/bin/sleep\"\nargs = [\"0\"]\n",
        );
        let output = kiosk.launch("sleeper");
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(kiosk.launch_count("sleeper"), 1);
        kiosk.launch("sleeper");
        assert_eq!(kiosk.launch_count("sleeper"), 2);
    }
}

#[test]
fn exec_mode_replaces_the_process() {
    let kiosk = Kiosk::new(false);
    kiosk.add_game(
        "sleeper",
        "executable = \"/usr/bin/sleep\"\nargs = [\"30\"]\n",
    );
    let mut child = kiosk.command("sleeper").spawn().unwrap();

    // The same PID is now running `sleep`, so the launcher is gone.
    let replaced = wait_for_process_name(&child, "sleep");
    // The statistics were committed before the game took over.
    let count = kiosk.launch_count("sleeper");
    child.kill().unwrap();
    let status = child.wait().unwrap();

    assert!(replaced);
    assert_eq!(count, 1);
    assert_eq!(status.signal(), Some(9));
}

#[test]
fn child_mode_keeps_the_launcher_alive_during_the_game() {
    let kiosk = Kiosk::new(true);
    kiosk.add_game(
        "sleeper",
        "executable = \"/usr/bin/sleep\"\nargs = [\"1\"]\n",
    );
    let mut child = kiosk.command("sleeper").spawn().unwrap();

    sleep(Duration::from_millis(300));
    assert_eq!(process_name(child.id()), "kioskgamemenu");
    assert_eq!(kiosk.launch_count("sleeper"), 1, "committed before launch");

    assert!(child.wait().unwrap().success());
}

#[test]
fn exit_status_reaches_the_supervisor_in_both_modes() {
    for keep_running in [true, false] {
        let kiosk = Kiosk::new(keep_running);
        kiosk.add_game("failing", "executable = \"/usr/bin/false\"\n");
        assert_eq!(kiosk.launch("failing").status.code(), Some(1));
    }
}

#[test]
fn games_receive_arguments_environment_and_working_directory() {
    for keep_running in [true, false] {
        let kiosk = Kiosk::new(keep_running);
        kiosk.add_game(
            "report",
            r#"
            executable = "/usr/bin/env"
            args = ["sh", "-c", 'printf "[%s]" "$@" "$KIOSK_VALUE" "$PWD"', "sh", "two words", "$HOME"]
            working_directory = "work dir"
            [env]
            KIOSK_VALUE = "from toml"
            "#,
        );
        let work = kiosk.game_dir("report").join("work dir");
        fs::create_dir(&work).unwrap();

        let output = kiosk.launch("report");
        let work = fs::canonicalize(work).unwrap();
        assert_eq!(
            stdout(&output),
            format!("[two words][$HOME][from toml][{}]", work.display())
        );
    }
}

#[test]
fn the_game_directory_placeholder_handles_spaces() {
    let kiosk = Kiosk::new(false);
    kiosk.add_game(
        "with-rom",
        "executable = \"/usr/bin/cat\"\nargs = [\"{game_dir}/my rom.txt\"]\n",
    );
    fs::write(kiosk.game_dir("with-rom").join("my rom.txt"), "contents").unwrap();
    assert_eq!(stdout(&kiosk.launch("with-rom")), "contents");
}

#[test]
fn the_database_is_not_inherited_by_games() {
    for keep_running in [true, false] {
        let kiosk = Kiosk::new(keep_running);
        kiosk.add_game(
            "fds",
            "executable = \"/usr/bin/ls\"\nargs = [\"-l\", \"/proc/self/fd\"]\n",
        );
        let output = kiosk.launch("fds");
        assert!(output.status.success());
        assert!(stdout(&output).contains("/proc/"), "{}", stdout(&output));
        assert!(!stdout(&output).contains("sqlite"), "{}", stdout(&output));
    }
}

#[test]
fn failed_validation_never_starts_or_counts_the_game() {
    let kiosk = Kiosk::new(false);
    kiosk.add_game("gone", "executable = \"/no/such/emulator\"\n");
    kiosk.add_game(
        "no-rom",
        "executable = \"/usr/bin/sleep\"\nargs = [\"/no/such/rom.sfc\"]\n",
    );

    for (id, expected) in [
        ("gone", "/no/such/emulator"),
        ("no-rom", "/no/such/rom.sfc"),
    ] {
        let output = kiosk.launch(id);
        assert_eq!(output.status.code(), Some(1));
        assert!(stderr(&output).contains(expected), "{}", stderr(&output));
        assert_eq!(kiosk.launch_count(id), 0);
    }
}

/// A file that passes validation but that the kernel refuses to execute,
/// because the interpreter named on its `#!` line does not exist.
fn add_unstartable_program(kiosk: &Kiosk) {
    kiosk.add_game("unstartable", "executable = \"./program\"\n");
    let program = kiosk.game_dir("unstartable").join("program");
    fs::write(&program, "#!/no/such/interpreter\n").unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn exec_failure_exits_nonzero_and_counts_as_an_attempt() {
    for keep_running in [true, false] {
        let kiosk = Kiosk::new(keep_running);
        add_unstartable_program(&kiosk);
        let output = kiosk.launch("unstartable");
        assert_eq!(output.status.code(), Some(EXIT_NOT_FOUND));
        assert!(
            stderr(&output).contains("cannot execute"),
            "{}",
            stderr(&output)
        );
        assert_eq!(kiosk.launch_count("unstartable"), 1);
    }
}

#[test]
fn an_unavailable_database_does_not_block_the_game() {
    let kiosk = Kiosk::new(true);
    kiosk.add_game(
        "sleeper",
        "executable = \"/usr/bin/sleep\"\nargs = [\"0\"]\n",
    );
    // A directory where the database file belongs.
    fs::create_dir_all(kiosk.dir.path().join("data/stats.sqlite")).unwrap();
    let output = kiosk.launch("sleeper");
    assert!(output.status.success());
    assert!(
        stderr(&output).contains("not recorded"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn unknown_game_and_bad_config_are_usage_errors() {
    let kiosk = Kiosk::new(true);
    assert_eq!(kiosk.launch("absent").status.code(), Some(2));

    fs::write(kiosk.dir.path().join("config.toml"), "[menu]\nsort = 1\n").unwrap();
    let output = kiosk.launch("absent");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("config.toml"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn check_reports_problems_without_launching() {
    let kiosk = Kiosk::new(true);
    kiosk.add_game("good", "executable = \"/usr/bin/sleep\"\nargs = [\"30\"]\n");
    kiosk.add_game("bad", "executable = \"/no/such/emulator\"\n");
    let invalid = kiosk.dir.path().join("games/invalid");
    fs::create_dir_all(&invalid).unwrap();
    fs::write(invalid.join("game.toml"), "id = 1").unwrap();

    let output = Command::new(LAUNCHER)
        .arg("--check")
        .arg("--config")
        .arg(kiosk.dir.path().join("config.toml"))
        .output()
        .unwrap();
    let report = stdout(&output);
    assert_eq!(output.status.code(), Some(1));
    assert!(report.contains("OK       good"), "{report}");
    assert!(report.contains("FAILED   bad"), "{report}");
    assert!(report.contains("INVALID"), "{report}");
    assert_eq!(kiosk.launch_count("good"), 0);
}

#[test]
fn fake_game_configurations_are_valid() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/fake");
    for config in ["child.toml", "exec.toml"] {
        let output = Command::new(LAUNCHER)
            .arg("--check")
            .arg("--config")
            .arg(root.join(config))
            .output()
            .unwrap();
        let report = stdout(&output);
        assert!(report.contains("OK       sleep-5s"), "{report}");
        assert!(report.contains("OK       exit-status-1"), "{report}");
        assert!(report.contains("FAILED   missing-emulator"), "{report}");
        assert!(report.contains("FAILED   missing-rom"), "{report}");
    }
}
