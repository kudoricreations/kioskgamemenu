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
    pub audio: AudioConfig,
    pub appearance: AppearanceConfig,
    /// Problems that were corrected automatically, for the caller to log.
    #[serde(skip)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LauncherConfig {
    /// `true`: run the game as a child process and return to the menu.
    /// `false`: replace this process with the game (`exec`).
    #[serde(rename = "keepLauncherRunning")]
    pub keep_launcher_running: bool,
    /// true: ESC only restarts the menu. false: ESC ends the menu and the loop.
    #[serde(rename = "kioskMode")]
    pub kiosk_mode: bool,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            keep_launcher_running: true,
            kiosk_mode: true,
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

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    pub enabled: bool,
    pub music: PathBuf,
    pub select_sound: PathBuf,
    pub launch_sound: PathBuf,
    /// From 0 to 100.
    pub music_volume: i64,
    /// From 0 to 100.
    pub effects_volume: i64,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            music: "assets/music.opus".into(),
            select_sound: "assets/select.opus".into(),
            launch_sound: "assets/launch.opus".into(),
            music_volume: 50,
            effects_volume: 75,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppearanceConfig {
    pub background: PathBuf,
    pub banner: PathBuf,
    /// Darkens the wallpaper so that text stays readable, from 0 to 100.
    pub background_dim: i64,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            background: "assets/background.jpg".into(),
            banner: "assets/banner.png".into(),
            background_dim: 40,
        }
    }
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
        config.clamp_percentages();
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

    /// Replaces out-of-range percentages with the nearest valid value. A bad
    /// volume is not worth stopping a kiosk for.
    fn clamp_percentages(&mut self) {
        let values = [
            ("audio.music_volume", &mut self.audio.music_volume),
            ("audio.effects_volume", &mut self.audio.effects_volume),
            (
                "appearance.background_dim",
                &mut self.appearance.background_dim,
            ),
        ];
        for (key, value) in values {
            let clamped = (*value).clamp(0, 100);
            if clamped != *value {
                self.warnings
                    .push(format!("{key} must be from 0 to 100, using {clamped}"));
                *value = clamped;
            }
        }
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
        // Media is optional, so an empty path is valid and means "none".
        for path in [
            &mut self.audio.music,
            &mut self.audio.select_sound,
            &mut self.audio.launch_sound,
            &mut self.appearance.background,
            &mut self.appearance.banner,
        ] {
            if !path.as_os_str().is_empty() {
                *path = base.join(&*path);
            }
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
    fn parses_audio_and_appearance() {
        let config = parse(
            r#"
            [audio]
            enabled = false
            music = "sound/theme.ogg"
            select_sound = "/shared/select.wav"
            launch_sound = "sound/launch.mp3"
            music_volume = 20
            effects_volume = 100
            [appearance]
            background = "art/wall.png"
            banner = "/shared/banner.png"
            background_dim = 0
            "#,
        )
        .unwrap();
        assert!(!config.audio.enabled);
        assert_eq!(config.audio.music, Path::new("/opt/kiosk/sound/theme.ogg"));
        assert_eq!(config.audio.select_sound, Path::new("/shared/select.wav"));
        assert_eq!(config.audio.music_volume, 20);
        assert_eq!(config.audio.effects_volume, 100);
        assert_eq!(
            config.appearance.background,
            Path::new("/opt/kiosk/art/wall.png")
        );
        assert_eq!(config.appearance.banner, Path::new("/shared/banner.png"));
        assert_eq!(config.appearance.background_dim, 0);
        assert!(config.warnings.is_empty());
    }

    #[test]
    fn media_defaults_are_enabled_and_inside_assets() {
        let config = parse("").unwrap();
        assert!(config.audio.enabled);
        assert_eq!(
            config.audio.music,
            Path::new("/opt/kiosk/assets/music.opus")
        );
        assert_eq!(config.audio.music_volume, 50);
        assert_eq!(config.audio.effects_volume, 75);
        assert_eq!(
            config.appearance.background,
            Path::new("/opt/kiosk/assets/background.jpg")
        );
    }

    #[test]
    fn out_of_range_percentages_are_clamped_with_a_warning() {
        let config = parse(
            r#"
            [audio]
            music_volume = 101
            effects_volume = -5
            [appearance]
            background_dim = 9000
            "#,
        )
        .unwrap();
        assert_eq!(config.audio.music_volume, 100);
        assert_eq!(config.audio.effects_volume, 0);
        assert_eq!(config.appearance.background_dim, 100);
        assert_eq!(config.warnings.len(), 3);
        assert!(config.warnings[0].contains("audio.music_volume"));
    }

    #[test]
    fn percentage_limits_are_valid_without_a_warning() {
        let config = parse("[audio]\nmusic_volume = 100\neffects_volume = 0\n").unwrap();
        assert_eq!(config.audio.music_volume, 100);
        assert_eq!(config.audio.effects_volume, 0);
        assert!(config.warnings.is_empty());
    }

    #[test]
    fn non_numeric_volume_is_a_clear_error() {
        let error = parse("[audio]\nmusic_volume = \"loud\"\n").unwrap_err();
        assert!(error.contains("music_volume"), "{error}");
    }

    #[test]
    fn empty_media_paths_mean_none() {
        let config =
            parse("[audio]\nmusic = \"\"\n[appearance]\nbackground = \"\"\nbanner = \"\"\n")
                .unwrap();
        assert_eq!(config.audio.music, Path::new(""));
        assert_eq!(config.appearance.background, Path::new(""));
        assert_eq!(config.appearance.banner, Path::new(""));
    }

    #[test]
    fn unknown_media_keys_are_rejected() {
        assert!(parse("[audio]\nmusic_vol = 1\n").is_err());
        assert!(parse("[appearance]\nwallpaper = \"a.png\"\n").is_err());
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
