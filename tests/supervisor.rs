//! Tests `scripts/kiosk-loop.sh` with a scripted stand-in for the launcher.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/kiosk-loop.sh");

struct Loop {
    dir: TempDir,
}

impl Loop {
    /// Creates a stand-in launcher from a shell script body. It runs in the
    /// application directory and `runs` gains a line per start.
    fn new(body: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("kioskgamemenu");
        fs::write(&binary, format!("#!/bin/sh\necho \"$@\" >>runs\n{body}\n")).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn command(&self, action: &str) -> Command {
        let mut command = Command::new(SCRIPT);
        command
            .arg(action)
            .env("KIOSK_APP_DIR", self.dir.path())
            .env("KIOSK_RESTART_DELAY", "0")
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .stdout(Stdio::piped())
            // The script must find everything without the caller's help.
            .current_dir("/");
        command
    }

    fn start(&self) -> Child {
        self.command("run").spawn().unwrap()
    }

    fn runs(&self) -> usize {
        fs::read_to_string(self.path("runs"))
            .unwrap_or_default()
            .lines()
            .count()
    }

    fn wait_for_runs(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.runs() < count {
            assert!(Instant::now() < deadline, "only {} runs", self.runs());
            sleep(Duration::from_millis(20));
        }
    }

    fn stop(&self) -> bool {
        self.command("stop").status().unwrap().success()
    }
}

fn wait_with_timeout(child: &mut Child) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    None
}

#[test]
fn restarts_after_any_exit_and_passes_the_config() {
    let kiosk = Loop::new("exit 0");
    let mut supervisor = kiosk.start();
    kiosk.wait_for_runs(3);
    assert!(kiosk.stop());
    assert!(wait_with_timeout(&mut supervisor).is_some());

    let runs = fs::read_to_string(kiosk.path("runs")).unwrap();
    let expected = format!("--config {}", kiosk.path("config.toml").display());
    assert_eq!(runs.lines().next(), Some(expected.as_str()));
}

#[test]
fn recovers_from_a_fatal_launcher_error() {
    let kiosk = Loop::new("[ \"$(wc -l <runs)\" -gt 1 ] && exec sleep 30\nexit 4");
    let mut supervisor = kiosk.start();
    // The second start comes after the backoff that follows a quick failure.
    kiosk.wait_for_runs(2);
    assert!(kiosk.stop());
    assert!(wait_with_timeout(&mut supervisor).is_some());

    let log = fs::read_to_string(kiosk.path("data/kiosk.log")).unwrap();
    assert!(log.contains("exited with status 4"), "{log}");
}

#[test]
fn stop_ends_a_running_game_and_cleans_up() {
    // Stands in for child mode: the launcher waits on a game process.
    let kiosk = Loop::new("sleep 30 &\ngame=$!\necho $game >game.pid\nwait $game");
    let mut supervisor = kiosk.start();
    kiosk.wait_for_runs(1);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !kiosk.path("game.pid").exists() {
        assert!(Instant::now() < deadline);
        sleep(Duration::from_millis(20));
    }

    assert!(kiosk.stop());
    assert!(wait_with_timeout(&mut supervisor).is_some());
    assert_eq!(kiosk.runs(), 1);
    assert!(!kiosk.path("data/kiosk.pid").exists());
    assert!(!kiosk.path("data/kiosk.stop").exists());

    let game: String = fs::read_to_string(kiosk.path("game.pid")).unwrap();
    assert!(
        !Path::new("/proc").join(game.trim()).exists(),
        "game survived"
    );
}

#[test]
fn refuses_to_run_twice_and_stop_reports_when_idle() {
    let kiosk = Loop::new("exec sleep 30");
    assert!(!kiosk.stop());

    let mut supervisor = kiosk.start();
    kiosk.wait_for_runs(1);
    let second = kiosk.command("run").output().unwrap();
    assert_eq!(second.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&second.stderr).contains("already running"));

    assert!(kiosk.stop());
    assert!(wait_with_timeout(&mut supervisor).is_some());
}

#[test]
fn a_missing_binary_is_an_immediate_error() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(SCRIPT)
        .env("KIOSK_APP_DIR", dir.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("not found"));
}
