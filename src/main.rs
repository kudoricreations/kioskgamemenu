//! KioskGameMenu: a fullscreen, gamepad-controlled game launcher for Linux
//! kiosks. See README.md for the architecture and operating modes.

use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn log_line(level: &str, message: std::fmt::Arguments) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    eprintln!("[{timestamp}] {level}: {message}");
}

macro_rules! log_info { ($($arg:tt)*) => { crate::log_line("info", format_args!($($arg)*)) } }
macro_rules! log_warn { ($($arg:tt)*) => { crate::log_line("warning", format_args!($($arg)*)) } }

mod config;
mod game;
mod gamepad;
mod graphics;
mod launcher;
mod menu;
mod statistics;

use config::Config;
use game::{Discovery, Game};
use graphics::SessionEnd;
use launcher::LaunchPlan;
use menu::MenuState;
use statistics::StatsStore;

const EXIT_FAILURE: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_DISPLAY_INIT: u8 = 3;
const EXIT_DISPLAY_RESTORE: u8 = 4;
/// Same meanings as in a POSIX shell.
const EXIT_CANNOT_EXECUTE: u8 = 126;
const EXIT_NOT_FOUND: u8 = 127;

/// A game may still be shutting down its display when `wait` returns, for
/// instance through a helper process, so recovery gets several attempts.
const RESTORE_ATTEMPTS: u32 = 5;
const RESTORE_DELAY: Duration = Duration::from_secs(1);

const USAGE: &str = "\
Usage: kioskgamemenu [OPTIONS]

Options:
  --config PATH   Use this configuration file
                  (default: ./config.toml, then config.toml next to the binary)
  --check         Validate the configuration and all games, then exit
  --launch ID     Launch one game without opening the menu, as the menu would
  --help          Show this help
  --version       Show the version";

/// An unrecoverable failure: a process exit status and its reason.
struct Fatal {
    code: u8,
    message: String,
}

impl Fatal {
    fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, PartialEq)]
enum Task {
    Menu,
    Check,
    Launch(String),
    Help,
    Version,
}

#[derive(Debug, PartialEq)]
struct Cli {
    task: Task,
    config: Option<PathBuf>,
}

fn parse_cli(args: impl IntoIterator<Item = OsString>) -> Result<Cli, String> {
    let mut cli = Cli {
        task: Task::Menu,
        config: None,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .ok_or_else(|| format!("{name} requires a value"))
        };
        match arg.to_str() {
            Some("--config") => cli.config = Some(value("--config")?.into()),
            Some("--check") => cli.task = Task::Check,
            Some("--launch") => {
                let id = value("--launch")?.to_string_lossy().into_owned();
                cli.task = Task::Launch(id);
            }
            Some("--help" | "-h") => cli.task = Task::Help,
            Some("--version" | "-V") => cli.task = Task::Version,
            _ => return Err(format!("unknown argument {}", arg.to_string_lossy())),
        }
    }
    Ok(cli)
}

/// Finds the configuration file when `--config` is not given.
fn default_config_path() -> PathBuf {
    let local = PathBuf::from("config.toml");
    if local.is_file() {
        return local;
    }
    env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("config.toml")))
        .filter(|path| path.is_file())
        .unwrap_or(local)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

/// Validates a game and commits its statistics. Once this returns `Ok` the
/// launch has been counted, and a later failure to run the game does not undo
/// that. Statistics are never a reason to refuse a game: if the database is
/// unavailable the launch proceeds and goes uncounted on disk.
fn commit_launch(game: &Game, store: &StatsStore) -> Result<LaunchPlan, String> {
    let plan = launcher::prepare(game, env::var_os("PATH").as_deref())?;
    if let Err(e) = store.record_launch(&game.id, now()) {
        log_warn!("launch of {} not recorded: {e}", game.id);
    }
    Ok(plan)
}

fn warn_about_held_devices() {
    for device in launcher::held_devices() {
        log_warn!("still open after SDL shutdown, may block the game: {device}");
    }
}

/// Mode B. Reached only with SDL fully shut down; returns only if `exec` failed.
fn replace_with_game(plan: &LaunchPlan) -> Fatal {
    log_info!("replacing launcher with {}", plan.program.display());
    let _ = io::stdout().flush();
    start_failure(plan, &launcher::replace_process(plan))
}

fn start_failure(plan: &LaunchPlan, error: &io::Error) -> Fatal {
    let code = match error.kind() {
        io::ErrorKind::NotFound => EXIT_NOT_FOUND,
        _ => EXIT_CANNOT_EXECUTE,
    };
    let program = plan.program.display();
    Fatal::new(code, format!("cannot execute {program}: {error}"))
}

/// Runs a menu session, retrying when restoring the display after a game.
fn restore_session(
    config: &Config,
    menu: &mut MenuState,
    on_select: &mut dyn FnMut(&mut MenuState) -> Option<LaunchPlan>,
) -> Result<SessionEnd, Fatal> {
    let mut attempt = 1;
    loop {
        match graphics::run_session(config, menu, on_select) {
            Ok(end) => return Ok(end),
            Err(e) if attempt == RESTORE_ATTEMPTS => {
                let message = format!("could not restore the display: {e}");
                return Err(Fatal::new(EXIT_DISPLAY_RESTORE, message));
            }
            Err(e) => {
                log_warn!("display not ready (attempt {attempt}/{RESTORE_ATTEMPTS}): {e}");
                attempt += 1;
                thread::sleep(RESTORE_DELAY);
            }
        }
    }
}

fn run_menu(config: &Config, discovery: Discovery, store: &StatsStore) -> Result<u8, Fatal> {
    let stats = store.load_all().unwrap_or_else(|e| {
        log_warn!("continuing without saved statistics: {e}");
        Default::default()
    });
    let skipped = discovery.errors.len();
    let mut menu = MenuState::new(discovery.games, stats, config.menu.sort, skipped);

    let store = store.clone();
    let mut on_select = move |menu: &mut MenuState| {
        let game = menu.selected_game()?;
        let id = game.id.clone();
        match commit_launch(game, &store) {
            Ok(plan) => {
                menu.note_launch(&id, now());
                Some(plan)
            }
            Err(e) => {
                log_warn!("cannot launch {id}: {e}");
                menu.set_notice(e, true);
                None
            }
        }
    };

    let mut end = graphics::run_session(config, &mut menu, &mut on_select)
        .map_err(|e| Fatal::new(EXIT_DISPLAY_INIT, e))?;
    loop {
        let plan = match end {
            SessionEnd::Quit => return Ok(0),
            SessionEnd::Launch(plan) => plan,
        };
        // run_session has returned, so SDL has released the display.
        warn_about_held_devices();

        if !config.launcher.keep_launcher_running {
            return Err(replace_with_game(&plan));
        }

        log_info!("starting {}", plan.title);
        match launcher::run_child(&plan) {
            Ok(status) => {
                log_info!("{} finished: {status}", plan.title);
                if let Some(message) = launcher::describe_exit(status) {
                    menu.set_notice(message, true);
                }
            }
            Err(e) => {
                let message = format!("Could not start {}: {e}", plan.title);
                log_warn!("{message}");
                menu.set_notice(message, true);
            }
        }
        end = restore_session(config, &mut menu, &mut on_select)?;
    }
}

/// `--launch`: the launch path of the menu with no graphics involved. This is
/// what lets both modes be exercised without a display or an emulator.
fn run_single(config: &Config, games: &[Game], store: &StatsStore, id: &str) -> Result<u8, Fatal> {
    let game = games
        .iter()
        .find(|game| game.id == id)
        .ok_or_else(|| Fatal::new(EXIT_USAGE, format!("no game with id {id:?}")))?;
    let plan = commit_launch(game, store).map_err(|e| Fatal::new(EXIT_FAILURE, e))?;
    if !config.launcher.keep_launcher_running {
        return Err(replace_with_game(&plan));
    }
    match launcher::run_child(&plan) {
        Ok(status) => {
            log_info!("{} finished: {status}", plan.title);
            Ok(launcher::exit_code(status))
        }
        Err(e) => Err(start_failure(&plan, &e)),
    }
}

/// `--check`: reports every problem an administrator could fix.
fn run_check(discovery: &Discovery) -> u8 {
    let path_var = env::var_os("PATH");
    let mut failures = discovery.errors.len();
    for error in &discovery.errors {
        println!("INVALID  {error}");
    }
    for game in &discovery.games {
        match launcher::prepare(game, path_var.as_deref()) {
            Ok(plan) => println!("OK       {} -> {}", game.id, plan.program.display()),
            Err(e) => {
                failures += 1;
                println!("FAILED   {}: {e}", game.id);
            }
        }
    }
    println!(
        "{} game(s) found, {failures} problem(s)",
        discovery.games.len()
    );
    if failures == 0 { 0 } else { EXIT_FAILURE }
}

fn run(cli: Cli) -> Result<u8, Fatal> {
    match cli.task {
        Task::Help => {
            println!("{USAGE}");
            return Ok(0);
        }
        Task::Version => {
            println!("kioskgamemenu {}", env!("CARGO_PKG_VERSION"));
            return Ok(0);
        }
        _ => {}
    }

    let path = cli.config.unwrap_or_else(default_config_path);
    let config = load_config(&path)?;
    let discovery = game::discover(&config.games.directory);
    let store = StatsStore::new(&config.storage.database);

    match cli.task {
        Task::Check => Ok(run_check(&discovery)),
        Task::Launch(id) => run_single(&config, &discovery.games, &store, &id),
        _ => {
            for error in &discovery.errors {
                log_warn!("skipping game: {error}");
            }
            log_info!(
                "{} game(s) found, launch mode: {}",
                discovery.games.len(),
                if config.launcher.keep_launcher_running {
                    "child process"
                } else {
                    "replace process"
                }
            );
            run_menu(&config, discovery, &store)
        }
    }
}

fn load_config(path: &Path) -> Result<Config, Fatal> {
    Config::load(path).map_err(|e| Fatal::new(EXIT_USAGE, e))
}

fn main() -> ExitCode {
    let cli = match parse_cli(env::args_os().skip(1)) {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    match run(cli) {
        Ok(code) => ExitCode::from(code),
        Err(fatal) => {
            log_line("error", format_args!("{}", fatal.message));
            ExitCode::from(fatal.code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, String> {
        parse_cli(args.iter().map(OsString::from))
    }

    #[test]
    fn no_arguments_opens_the_menu_and_launches_nothing() {
        let cli = parse(&[]).unwrap();
        assert_eq!(cli.task, Task::Menu);
        assert_eq!(cli.config, None);
    }

    #[test]
    fn parses_options() {
        let cli = parse(&["--config", "/etc/k.toml", "--launch", "tekken-2"]).unwrap();
        assert_eq!(cli.task, Task::Launch("tekken-2".into()));
        assert_eq!(cli.config, Some(PathBuf::from("/etc/k.toml")));
        assert_eq!(parse(&["--check"]).unwrap().task, Task::Check);
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(parse(&["--launch"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
    }
}
