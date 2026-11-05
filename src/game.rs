//! Game definitions: one directory per game, each holding a `game.toml`.
//!
//! Path rules, relative to the directory containing the `game.toml`:
//!
//! * `cover`: absolute, or relative to the game directory.
//! * `working_directory`: absolute, or relative to the game directory. When
//!   omitted, the game directory itself is used.
//! * `executable`: absolute; or containing a `/`, then relative to the game
//!   directory; or a bare name, then looked up on `PATH`.
//! * `args` and `env` values are passed literally, because only the game
//!   knows which of them are paths. `{game_dir}` expands to the absolute game
//!   directory, which is how a ROM stored next to its `game.toml` is referenced.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

const GAME_FILE: &str = "game.toml";
const DEFAULT_COVER: &str = "cover.png";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GameFile {
    id: String,
    title: String,
    platform: String,
    cover: Option<String>,
    executable: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    working_directory: Option<String>,
    #[serde(default = "enabled")]
    check_paths: bool,
}

fn enabled() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq)]
pub struct Game {
    pub id: String,
    pub title: String,
    pub platform: String,
    /// Directory containing `game.toml`.
    pub dir: PathBuf,
    pub cover: PathBuf,
    pub executable: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub working_directory: Option<String>,
    /// Require arguments that are absolute paths to exist before launching.
    pub check_paths: bool,
}

impl Game {
    /// Parses and validates a definition. `dir` should be absolute.
    pub fn parse(text: &str, dir: &Path) -> Result<Self, String> {
        let file: GameFile = toml::from_str(text).map_err(|e| e.to_string())?;
        validate(&file)?;
        Ok(Self {
            cover: dir.join(file.cover.as_deref().unwrap_or(DEFAULT_COVER)),
            id: file.id,
            title: file.title.trim().to_string(),
            platform: file.platform,
            dir: dir.to_path_buf(),
            executable: file.executable,
            args: file.args,
            env: file.env,
            working_directory: file.working_directory,
            check_paths: file.check_paths,
        })
    }

    /// Expands `{game_dir}` in an argument or environment value.
    pub fn expand(&self, value: &str) -> String {
        value.replace("{game_dir}", &self.dir.to_string_lossy())
    }

    /// Resolves the working directory described in the module documentation.
    pub fn resolved_working_directory(&self) -> PathBuf {
        match &self.working_directory {
            Some(directory) => self.dir.join(directory),
            None => self.dir.clone(),
        }
    }

    /// Resolves and checks the executable described in the module documentation.
    pub fn resolve_executable(&self, path_var: Option<&OsStr>) -> Result<PathBuf, String> {
        if !self.executable.contains('/') {
            return std::env::split_paths(path_var.unwrap_or_default())
                .map(|directory| directory.join(&self.executable))
                .find(|candidate| is_executable_file(candidate))
                .ok_or_else(|| format!("Executable not found on PATH: {}", self.executable));
        }
        let path = self.dir.join(&self.executable);
        if !path.exists() {
            return Err(format!("Executable not found: {}", path.display()));
        }
        if !is_executable_file(&path) {
            return Err(format!("Not an executable file: {}", path.display()));
        }
        Ok(path)
    }
}

fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn validate(file: &GameFile) -> Result<(), String> {
    if !is_safe_name(&file.id) {
        return Err(format!(
            "id must use only letters, digits, '-', '_' or '.', got {:?}",
            file.id
        ));
    }
    if file.title.trim().is_empty() {
        return Err("title must not be empty".into());
    }
    // The platform selects a file name, so it must not be able to leave
    // the platforms directory.
    if !is_safe_name(&file.platform) {
        return Err(format!(
            "platform must use only letters, digits, '-', '_' or '.', got {:?}",
            file.platform
        ));
    }
    if file.executable.trim().is_empty() {
        return Err("executable must not be empty".into());
    }
    let has_nul = |value: &String| value.contains('\0');
    if has_nul(&file.executable)
        || file.args.iter().any(has_nul)
        || file.env.values().any(has_nul)
        || file.working_directory.as_ref().is_some_and(has_nul)
    {
        return Err("executable, args, env and working_directory must not contain NUL".into());
    }
    for name in file.env.keys() {
        if name.is_empty() || name.contains(['=', '\0']) {
            return Err(format!("invalid environment variable name {name:?}"));
        }
    }
    Ok(())
}

/// The outcome of scanning the games directory.
#[derive(Debug, Default)]
pub struct Discovery {
    pub games: Vec<Game>,
    /// One message per skipped definition.
    pub errors: Vec<String>,
}

/// Scans `directory` for `<game>/game.toml`. A definition that cannot be used
/// is reported in `errors` and never prevents the others from loading.
pub fn discover(directory: &Path) -> Discovery {
    let mut discovery = Discovery::default();
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(e) => {
            discovery
                .errors
                .push(format!("cannot read {}: {e}", directory.display()));
            return discovery;
        }
    };

    let mut directories: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter(|path| !path.file_name().is_some_and(is_hidden))
        .collect();
    // read_dir order is arbitrary; sorting keeps duplicate handling stable.
    directories.sort();

    let mut ids = HashSet::new();
    for dir in directories {
        let file = dir.join(GAME_FILE);
        if !file.exists() {
            discovery
                .errors
                .push(format!("{}: no {GAME_FILE}", dir.display()));
            continue;
        }
        let parsed = fs::read_to_string(&file)
            .map_err(|e| e.to_string())
            .and_then(|text| Game::parse(&text, &dir));
        match parsed {
            Ok(game) if ids.contains(&game.id) => discovery.errors.push(format!(
                "{}: duplicate game id {:?}",
                file.display(),
                game.id
            )),
            Ok(game) => {
                ids.insert(game.id.clone());
                discovery.games.push(game);
            }
            Err(e) => discovery.errors.push(format!("{}: {e}", file.display())),
        }
    }
    discovery
}

fn is_hidden(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

#[cfg(test)]
pub fn test_game(id: &str, title: &str) -> Game {
    let text = format!(
        "id = {id:?}\ntitle = {title:?}\nplatform = \"snes\"\nexecutable = \"/usr/bin/sleep\"\n"
    );
    Game::parse(&text, Path::new("/games").join(id).as_path()).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    const MINIMAL: &str = r#"
        id = "game"
        title = "A Game"
        platform = "snes"
        executable = "/usr/bin/retroarch"
    "#;

    fn parse(text: &str) -> Result<Game, String> {
        Game::parse(text, Path::new("/library/game"))
    }

    fn with_field(field: &str) -> Game {
        parse(&format!("{field}\n{MINIMAL}")).unwrap()
    }

    fn write_game(root: &Path, dir: &str, contents: &str) {
        let directory = root.join(dir);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(GAME_FILE), contents).unwrap();
    }

    #[test]
    fn parses_a_complete_definition() {
        let game = parse(
            r#"
            id = "super-mario-world"
            title = "Super Mario World"
            platform = "snes"
            cover = "art/front.png"
            executable = "/usr/bin/retroarch"
            args = ["-f", "-L", "/usr/lib/libretro/snes9x_libretro.so", "/games/snes/Super Mario World.sfc"]
            working_directory = "work"
            check_paths = false

            [env]
            SDL_AUDIODRIVER = "alsa"
            "#,
        )
        .unwrap();
        assert_eq!(game.id, "super-mario-world");
        assert_eq!(game.args[3], "/games/snes/Super Mario World.sfc");
        assert_eq!(game.cover, Path::new("/library/game/art/front.png"));
        assert_eq!(game.env["SDL_AUDIODRIVER"], "alsa");
        assert!(!game.check_paths);
    }

    #[test]
    fn optional_fields_have_defaults() {
        let game = parse(MINIMAL).unwrap();
        assert!(game.args.is_empty());
        assert!(game.env.is_empty());
        assert!(game.check_paths);
        assert_eq!(game.cover, Path::new("/library/game/cover.png"));
    }

    #[test]
    fn rejects_missing_and_unknown_fields() {
        assert!(parse("id = \"x\"").is_err());
        let error = parse(&format!("arguments = []\n{MINIMAL}")).unwrap_err();
        assert!(error.contains("arguments"), "{error}");
    }

    #[test]
    fn rejects_unusable_values() {
        let cases = [
            MINIMAL.replace("\"game\"", "\"\""),
            MINIMAL.replace("\"game\"", "\"a/b\""),
            MINIMAL.replace("\"game\"", "\"has space\""),
            MINIMAL.replace("\"A Game\"", "\"   \""),
            MINIMAL.replace("\"snes\"", "\"../../etc/passwd\""),
            MINIMAL.replace("\"snes\"", "\"..\""),
            MINIMAL.replace("\"/usr/bin/retroarch\"", "\"\""),
            format!("args = [\"a\\u0000b\"]\n{MINIMAL}"),
            format!("{MINIMAL}\n[env]\n\"A=B\" = \"x\"\n"),
        ];
        for case in cases {
            assert!(parse(&case).is_err(), "accepted: {case}");
        }
    }

    #[test]
    fn working_directory_resolution() {
        assert_eq!(
            parse(MINIMAL).unwrap().resolved_working_directory(),
            Path::new("/library/game")
        );
        assert_eq!(
            with_field("working_directory = \"save\"").resolved_working_directory(),
            Path::new("/library/game/save")
        );
        assert_eq!(
            with_field("working_directory = \"/var/games\"").resolved_working_directory(),
            Path::new("/var/games")
        );
    }

    #[test]
    fn absolute_cover_is_kept() {
        assert_eq!(
            with_field("cover = \"/art/a.png\"").cover,
            Path::new("/art/a.png")
        );
    }

    #[test]
    fn game_dir_placeholder_is_expanded() {
        let game = parse(MINIMAL).unwrap();
        assert_eq!(
            game.expand("--rom={game_dir}/a b.bin"),
            "--rom=/library/game/a b.bin"
        );
        assert_eq!(game.expand("plain"), "plain");
    }

    #[test]
    fn executable_resolution_covers_all_three_forms() {
        let root = tempfile::tempdir().unwrap();
        let tool = root.path().join("bin/tool");
        fs::create_dir_all(tool.parent().unwrap()).unwrap();
        fs::write(&tool, "").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        let plain = root.path().join("plain");
        fs::write(&plain, "").unwrap();

        let game_with = |executable: &str| {
            let text = MINIMAL.replace("/usr/bin/retroarch", executable);
            Game::parse(&text, root.path()).unwrap()
        };
        let path: OsString = root.path().join("bin").into();

        // Absolute, relative to the game directory, and found on PATH.
        let absolute = game_with(tool.to_str().unwrap());
        assert_eq!(absolute.resolve_executable(None).unwrap(), tool);
        assert_eq!(
            game_with("bin/tool").resolve_executable(None).unwrap(),
            tool
        );
        assert_eq!(
            game_with("tool").resolve_executable(Some(&path)).unwrap(),
            tool
        );

        // Failures name the problem.
        let missing = game_with("/no/such/emulator").resolve_executable(None);
        assert!(missing.unwrap_err().contains("not found"));
        let not_executable = game_with("./plain").resolve_executable(None);
        assert!(not_executable.unwrap_err().contains("Not an executable"));
        let not_on_path = game_with("absent").resolve_executable(Some(&path));
        assert!(not_on_path.unwrap_err().contains("PATH"));
    }

    #[test]
    fn discovery_loads_valid_games_and_reports_the_rest() {
        let root = tempfile::tempdir().unwrap();
        let valid = |id: &str| MINIMAL.replace("\"game\"", &format!("{id:?}"));
        write_game(root.path(), "b-game", &valid("b"));
        write_game(root.path(), "a-game", &valid("a"));
        write_game(root.path(), "broken", "this is not toml");
        write_game(root.path(), "invalid", "id = \"x\"");
        write_game(root.path(), "z-duplicate", &valid("a"));
        write_game(root.path(), ".hidden", "ignored");
        fs::create_dir(root.path().join("no-definition")).unwrap();
        fs::write(root.path().join("stray-file.toml"), "ignored").unwrap();

        let found = discover(root.path());
        let ids: Vec<_> = found.games.iter().map(|game| game.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(found.errors.len(), 4, "{:?}", found.errors);
        for expected in ["broken", "invalid", "no-definition", "duplicate"] {
            assert!(
                found.errors.iter().any(|error| error.contains(expected)),
                "{expected} not in {:?}",
                found.errors
            );
        }
    }

    #[test]
    fn discovery_uses_the_game_directory_for_relative_paths() {
        let root = tempfile::tempdir().unwrap();
        write_game(root.path(), "one", MINIMAL);
        let game = &discover(root.path()).games[0];
        assert_eq!(game.dir, root.path().join("one"));
        assert_eq!(game.cover, root.path().join("one/cover.png"));
    }

    #[test]
    fn missing_games_directory_is_an_empty_library_with_an_error() {
        let root = tempfile::tempdir().unwrap();
        let found = discover(&root.path().join("absent"));
        assert!(found.games.is_empty());
        assert_eq!(found.errors.len(), 1);
    }

    #[test]
    fn repository_games_are_valid() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("games");
        let found = discover(&root);
        assert!(found.errors.is_empty(), "{:?}", found.errors);
        assert_eq!(found.games.len(), 2);
    }
}
