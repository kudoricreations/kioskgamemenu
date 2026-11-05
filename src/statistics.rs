//! Persistent launch statistics in SQLite.
//!
//! Every operation opens its own short-lived connection. Nothing is therefore
//! held open while an emulator runs or across `exec`, and a database that was
//! temporarily inaccessible recovers by itself on the next operation.
//!
//! The schema version lives in `PRAGMA user_version`. To change the schema,
//! append a statement batch to [`MIGRATIONS`]; never edit an existing entry.

use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, params};

const MIGRATIONS: &[&str] = &["CREATE TABLE game_stats (
        game_id          TEXT PRIMARY KEY NOT NULL,
        launch_count     INTEGER NOT NULL DEFAULT 0,
        last_launched_at INTEGER
    );"];

const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

type StoreResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GameStats {
    pub launch_count: u32,
    /// Unix timestamp in seconds of the most recent launch.
    pub last_launched: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct StatsStore {
    path: PathBuf,
}

impl StatsStore {
    pub fn new(path: &Path) -> Self {
        Self { path: path.into() }
    }

    fn open(&self) -> StoreResult<Connection> {
        if let Some(parent) = self.path.parent() {
            // A failure surfaces as a clearer error from Connection::open.
            let _ = fs::create_dir_all(parent);
        }
        let mut connection = Connection::open(&self.path)?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        migrate(&mut connection)?;
        Ok(connection)
    }

    fn describe(&self, error: Box<dyn Error>) -> String {
        format!("statistics database {}: {error}", self.path.display())
    }

    /// Loads statistics for all games, creating the database when missing.
    pub fn load_all(&self) -> Result<HashMap<String, GameStats>, String> {
        let query = || -> StoreResult<_> {
            let connection = self.open()?;
            let mut statement = connection
                .prepare("SELECT game_id, launch_count, last_launched_at FROM game_stats")?;
            let rows = statement.query_map([], |row| {
                let stats = GameStats {
                    launch_count: row.get(1)?,
                    last_launched: row.get(2)?,
                };
                Ok((row.get(0)?, stats))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        };
        query().map_err(|e| self.describe(e))
    }

    /// Records one launch attempt. The change is durably committed when this
    /// returns, because the connection closes before the caller proceeds.
    pub fn record_launch(&self, game_id: &str, timestamp: i64) -> Result<(), String> {
        let update = || -> StoreResult<()> {
            let mut connection = self.open()?;
            let transaction = connection.transaction()?;
            transaction.execute(
                "INSERT INTO game_stats (game_id, launch_count, last_launched_at)
                 VALUES (?1, 1, ?2)
                 ON CONFLICT (game_id) DO UPDATE SET
                     launch_count = launch_count + 1,
                     last_launched_at = excluded.last_launched_at",
                params![game_id, timestamp],
            )?;
            Ok(transaction.commit()?)
        };
        update().map_err(|e| self.describe(e))
    }
}

fn migrate(connection: &mut Connection) -> StoreResult<()> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let latest = MIGRATIONS.len() as i64;
    if version > latest {
        return Err(format!(
            "schema version {version} is newer than this program supports ({latest})"
        )
        .into());
    }
    for (index, migration) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let transaction = connection.transaction()?;
        transaction.execute_batch(migration)?;
        transaction.pragma_update(None, "user_version", index as i64 + 1)?;
        transaction.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, StatsStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = StatsStore::new(&dir.path().join("nested/data/stats.sqlite"));
        (dir, store)
    }

    #[test]
    fn creates_the_database_and_parent_directories() {
        let (dir, store) = store();
        assert!(store.load_all().unwrap().is_empty());
        assert!(dir.path().join("nested/data/stats.sqlite").exists());
    }

    #[test]
    fn counts_launches_and_keeps_the_latest_timestamp() {
        let (_dir, store) = store();
        store.record_launch("a", 100).unwrap();
        store.record_launch("a", 200).unwrap();
        store.record_launch("b", 150).unwrap();

        let stats = store.load_all().unwrap();
        assert_eq!(
            stats["a"],
            GameStats {
                launch_count: 2,
                last_launched: Some(200)
            }
        );
        assert_eq!(stats["b"].launch_count, 1);
    }

    #[test]
    fn statistics_survive_reopening() {
        let (dir, store) = store();
        store.record_launch("a", 100).unwrap();
        drop(store);
        let reopened = StatsStore::new(&dir.path().join("nested/data/stats.sqlite"));
        assert_eq!(reopened.load_all().unwrap()["a"].launch_count, 1);
    }

    #[test]
    fn records_the_schema_version() {
        let (dir, store) = store();
        store.load_all().unwrap();
        let connection = Connection::open(dir.path().join("nested/data/stats.sqlite")).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
    }

    #[test]
    fn refuses_a_database_from_a_newer_version() {
        let (dir, store) = store();
        store.load_all().unwrap();
        let connection = Connection::open(dir.path().join("nested/data/stats.sqlite")).unwrap();
        connection.pragma_update(None, "user_version", 999).unwrap();
        drop(connection);
        assert!(store.load_all().unwrap_err().contains("newer"));
    }

    #[test]
    fn inaccessible_database_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        // A directory cannot be opened as a database file.
        let store = StatsStore::new(dir.path());
        assert!(store.load_all().is_err());
        assert!(store.record_launch("a", 1).is_err());
    }

    #[test]
    fn a_locked_database_recovers_on_the_next_operation() {
        let (dir, store) = store();
        store.record_launch("a", 1).unwrap();

        let path = dir.path().join("nested/data/stats.sqlite");
        let blocker = Connection::open(&path).unwrap();
        blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let impatient = Connection::open(&path).unwrap();
        impatient.busy_timeout(Duration::ZERO).unwrap();
        assert!(impatient.execute_batch("BEGIN IMMEDIATE").is_err());
        drop(blocker);

        store.record_launch("a", 2).unwrap();
        assert_eq!(store.load_all().unwrap()["a"].launch_count, 2);
    }
}
