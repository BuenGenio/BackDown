//! The index database.
//!
//! Every `index` run of a root is a *scan*. A `files` row is one version of one path: the span of
//! scans (`first_scan`..=`last_scan`) during which it existed with the same size and mtime. Changed
//! or deleted files keep their old rows (with `current = 0`), so the index can show a drive as it
//! was at any earlier scan, and old versions keep their hashes. Hashing and reports work on
//! `current = 1` rows.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS roots (
    id        INTEGER PRIMARY KEY,
    path      BLOB NOT NULL UNIQUE,
    last_scan INTEGER                 -- latest finished scan
);
CREATE TABLE IF NOT EXISTS scans (
    id          INTEGER PRIMARY KEY,  -- increases across all roots, so scan ids order in time
    root_id     INTEGER NOT NULL REFERENCES roots(id),
    started_at  INTEGER NOT NULL,     -- unix seconds
    finished_at INTEGER,              -- NULL while running or if interrupted
    files       INTEGER,
    bytes       INTEGER
);
CREATE TABLE IF NOT EXISTS files (
    id         INTEGER PRIMARY KEY,
    root_id    INTEGER NOT NULL REFERENCES roots(id),
    path       BLOB NOT NULL,
    size       INTEGER NOT NULL,
    mtime      INTEGER NOT NULL,
    dev        INTEGER,               -- device + inode identify hardlinks (NULL where the OS doesn't expose them)
    ino        INTEGER,
    sample     BLOB,                  -- SHA-256 of size + head/middle/tail; equals sha256 for small files
    sha256     BLOB,
    error      TEXT,                  -- last read error; such files are never counted as verified
    first_scan INTEGER NOT NULL,
    last_scan  INTEGER NOT NULL,
    current    INTEGER NOT NULL DEFAULT 1
);
CREATE UNIQUE INDEX IF NOT EXISTS files_current_path ON files(path) WHERE current = 1;
CREATE INDEX IF NOT EXISTS files_path_history ON files(path, first_scan);
CREATE INDEX IF NOT EXISTS files_root ON files(root_id, current, last_scan);
CREATE INDEX IF NOT EXISTS files_size_sample ON files(size, sample) WHERE current = 1;
CREATE INDEX IF NOT EXISTS files_inode ON files(dev, ino) WHERE current = 1;
CREATE INDEX IF NOT EXISTS files_sha256 ON files(sha256);
PRAGMA user_version = 1;
";

pub struct Db {
    pub conn: Connection,
}

#[derive(Debug, Serialize)]
pub struct Status {
    pub roots: Vec<String>,
    pub scans: u64,
    pub files: u64,
    pub bytes: u64,
    pub sampled: u64,
    pub hashed: u64,
    pub unreadable: u64,
    pub versions: u64,
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        let conn =
            Connection::open(path).with_context(|| format!("opening index {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;",
        )?;
        conn.execute_batch(SCHEMA)?;
        Ok(Db { conn })
    }

    pub fn status(&self) -> Result<Status> {
        let roots = self
            .conn
            .prepare("SELECT path FROM roots ORDER BY path")?
            .query_map([], |r| r.get::<_, Vec<u8>>(0))?
            .map(|p| p.map(|b| crate::paths::from_bytes(&b).display().to_string()))
            .collect::<rusqlite::Result<_>>()?;
        let n = |sql: &str| -> Result<u64> {
            Ok(self.conn.query_row(sql, [], |r| r.get::<_, i64>(0))? as u64)
        };
        Ok(Status {
            roots,
            scans: n("SELECT COUNT(*) FROM scans WHERE finished_at IS NOT NULL")?,
            files: n("SELECT COUNT(*) FROM files WHERE current = 1")?,
            bytes: n("SELECT COALESCE(SUM(size), 0) FROM files WHERE current = 1")?,
            sampled: n("SELECT COUNT(sample) FROM files WHERE current = 1")?,
            hashed: n("SELECT COUNT(sha256) FROM files WHERE current = 1")?,
            unreadable: n("SELECT COUNT(error) FROM files WHERE current = 1")?,
            versions: n("SELECT COUNT(*) FROM files WHERE current = 0")?,
        })
    }
}
