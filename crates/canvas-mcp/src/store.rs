//! SQLite cache of Canvas API responses, plus on-disk cache for files and images.
//! The same database file (and schema) as before, so an existing cache carries over.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};

use crate::config;

pub struct Store {
    pub db: Mutex<Connection>,
}

pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

impl Store {
    pub fn open_default() -> crate::Result<Store> {
        Store::open(&config::data_dir().join("cache.db"))
    }

    pub fn open(path: &Path) -> crate::Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            config::private(dir, true);
        }
        std::fs::create_dir_all(config::blob_dir())?;
        let db = Connection::open(path)?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute("CREATE TABLE IF NOT EXISTS cache (key TEXT PRIMARY KEY, value TEXT NOT NULL, fetched_at REAL NOT NULL)", [])?;
        Ok(Store { db: Mutex::new(db) })
    }

    /// Cached JSON text and when it was fetched.
    pub fn get(&self, key: &str) -> Option<(String, f64)> {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT value, fetched_at FROM cache WHERE key = ?", [key], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .ok()
            .flatten()
    }

    /// Store a value; returns true if it differs from what was cached.
    pub fn put(&self, key: &str, value: &str) -> bool {
        let db = self.db.lock().unwrap();
        let old: Option<String> = db.query_row("SELECT value FROM cache WHERE key = ?", [key], |r| r.get(0)).optional().ok().flatten();
        let _ = db.execute(
            "INSERT INTO cache (key, value, fetched_at) VALUES (?, ?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, fetched_at = excluded.fetched_at",
            params![key, value, now()],
        );
        old.as_deref() != Some(value)
    }

    /// Forget every cached response (when you switch to another school's Canvas).
    pub fn clear(&self) {
        let db = self.db.lock().unwrap();
        let _ = db.execute("DELETE FROM cache", []);
    }

    pub fn prefix(&self, prefix: &str) -> Vec<(String, String)> {
        let db = self.db.lock().unwrap();
        let mut stmt = match db.prepare("SELECT key, value FROM cache WHERE key >= ? AND key < ?") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let end = format!("{prefix}\u{ffff}");
        stmt.query_map(params![prefix, end], |r| Ok((r.get(0)?, r.get(1)?)))
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    /// Run a statement on the database (for the app's own tables).
    pub fn exec(&self, sql: &str, args: &[&dyn rusqlite::ToSql]) {
        let db = self.db.lock().unwrap();
        if let Err(e) = db.execute(sql, args) {
            log::warn!("db: {e}: {sql}");
        }
    }

    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> T) -> T {
        let db = self.db.lock().unwrap();
        f(&db)
    }
}
