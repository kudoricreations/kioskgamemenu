//! Application configuration (`config.toml`).
//!
//! Relative paths are resolved against the directory containing the
//! configuration file, never against the current working directory, so the
//! launcher behaves the same however it is started.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::menu::SortMode;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub launcher: LauncherConfig,
    pub display: DisplayConfig,
    pub storage: StorageConfig,
    pub games: GamesConfig,
    pub menu: MenuConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LauncherConfig {
    /// `true`: run the game as a child process and return to the menu.
    /// `false`: replace this process with the game (`exec`).
    #[serde(rename = "keepLauncherRunning")]
    pub keep_launcher_running: bool,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            keep_launcher_running: true,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DisplayConfig {
    /// SDL video driver name such as `kmsdrm`, or `auto` to let SDL choose.
    pub backend: String,
    pub fullscreen: bool,
    /// TrueType font for all text. System fonts are tried if it cannot be read.
    pub font: PathBuf,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            backend: "kmsdrm".into(),
            fullscreen: true,
            font: "assets/fonts/Lato-Regular.ttf".into(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    pub database: PathBuf,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            database: "data/stats.sqlite".into(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GamesConfig {
    pub directory: PathBuf,
    pub platforms_directory: PathBuf,
}

impl Default for GamesConfig {
    fn default() -> Self {
        Self {
            directory: "games".into(),
            platforms_directory: "platforms".into(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MenuConfig {
    pub sort: SortMode,
}

impl Config {
    /// Reads, validates and resolves a configuration file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path)
            .map_err(|e| format!("cannot read configuration {}: {e}", path.display()))?;
        let base = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let base = fs::canonicalize(base).map_err(|e| {
            format!(
                "cannot resolve configuration directory {}: {e}",
                base.display()
            )
        })?;
        Self::parse(&text, &base).map_err(|e| format!("invalid {}: {e}", path.display()))
    }

    /// Parses configuration text, resolving relative paths against `base`.
    pub fn parse(text: &str, base: &Path) -> Result<Self, String> {
        let mut config: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        config.validate()?;
        config.resolve_paths(base);
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        let backend = &self.display.backend;
        if backend.is_empty() || backend.contains(char::is_whitespace) {
            return Err(format!(
                "display.backend must be an SDL video driver name or \"auto\", got {backend:?}"
            ));
        }
        let paths = [
            ("display.font", &self.display.font),
            ("storage.database", &self.storage.database),
            ("games.directory", &self.games.directory),
            ("games.platforms_directory", &self.games.platforms_directory),
        ];
        for (key, path) in paths {
            if path.as_os_str().is_empty() {
                return Err(format!("{key} must not be empty"));
            }
        }
        Ok(())
    }

    fn resolve_paths(&mut self, base: &Path) {
        // `join` keeps absolute paths unchanged.
        for path in [
            &mut self.display.font,
            &mut self.storage.database,
            &mut self.games.directory,
            &mut self.games.platforms_directory,
        ] {
            *path = base.join(&*path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "/opt/kiosk";

    fn parse(text: &str) -> Result<Config, String> {
        Config::parse(text, Path::new(BASE))
    }

    #[test]
    fn parses_the_documented_example() {
        let config = parse(
            r#"
            [launcher]
            keepLauncherRunning = false
            [display]
            backend = "x11"
            fullscreen = false
            [storage]
            database = "state/stats.sqlite"
            [games]
            directory = "library"
            platforms_directory = "icons"
            [menu]
            sort = "recently_played"
            "#,
        )
        .unwrap();
        assert!(!config.launcher.keep_launcher_running);
        assert_eq!(config.display.backend, "x11");
        assert!(!config.display.fullscreen);
        assert_eq!(
            config.storage.database,
            Path::new("/opt/kiosk/state/stats.sqlite")
        );
        assert_eq!(config.games.directory, Path::new("/opt/kiosk/library"));
        assert_eq!(config.menu.sort, SortMode::RecentlyPlayed);
    }

    #[test]
    fn repository_config_is_valid() {
        let text = include_str!("../config.toml");
        let config = parse(text).unwrap();
        assert!(config.launcher.keep_launcher_running);
        assert_eq!(config.display.backend, "kmsdrm");
    }

    #[test]
    fn empty_file_uses_defaults() {
        let config = parse("").unwrap();
        assert!(config.launcher.keep_launcher_running);
        assert_eq!(config.display.backend, "kmsdrm");
        assert!(config.display.fullscreen);
        assert_eq!(config.menu.sort, SortMode::NameAsc);
        assert_eq!(
            config.storage.database,
            Path::new("/opt/kiosk/data/stats.sqlite")
        );
    }

    #[test]
    fn absolute_paths_are_kept() {
        let config = parse("[games]\ndirectory = \"/srv/games\"\n").unwrap();
        assert_eq!(config.games.directory, Path::new("/srv/games"));
    }

    #[test]
    fn exact_key_spelling_is_required() {
        let error = parse("[launcher]\nkeep_launcher_running = true\n").unwrap_err();
        assert!(error.contains("keep_launcher_running"), "{error}");
    }

    #[test]
    fn rejects_unknown_sort_mode() {
        let error = parse("[menu]\nsort = \"random\"\n").unwrap_err();
        assert!(error.contains("random"), "{error}");
    }

    #[test]
    fn rejects_blank_backend_and_paths() {
        assert!(parse("[display]\nbackend = \"\"\n").is_err());
        assert!(parse("[games]\ndirectory = \"\"\n").is_err());
    }

    #[test]
    fn load_resolves_against_the_file_location_and_reports_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        let config = Config::load(&path).unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        assert_eq!(config.games.directory, canonical.join("games"));

        let error = Config::load(&dir.path().join("absent.toml")).unwrap_err();
        assert!(error.contains("absent.toml"), "{error}");
    }
}
