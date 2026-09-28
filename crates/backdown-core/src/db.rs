//! The index database.
//!
//! Every `index` run of a root is a *scan*. A `files` row is one version of one path: the span of
//! scans (`first_scan`..=`last_scan`) during which it existed with the same size and mtime. Changed
//! or deleted files keep their old rows (with `current = 0`), so the index can show a drive as it
//! was at any earlier scan, and old versions keep their hashes. Hashing and reports work on
//! `current = 1` rows. `dirs` is versioned the same way and holds recursive totals, so any folder
//! can be listed, with sizes, as it was at any scan.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

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
    dir        BLOB,                  -- parent folder, for listing
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
CREATE TABLE IF NOT EXISTS dirs (
    id         INTEGER PRIMARY KEY,
    root_id    INTEGER NOT NULL REFERENCES roots(id),
    path       BLOB NOT NULL,
    parent     BLOB,                  -- NULL for a root
    files      INTEGER NOT NULL,      -- recursive totals
    bytes      INTEGER NOT NULL,
    newest     INTEGER NOT NULL,      -- newest file mtime below
    first_scan INTEGER NOT NULL,
    last_scan  INTEGER NOT NULL,
    current    INTEGER NOT NULL DEFAULT 1
);
CREATE UNIQUE INDEX IF NOT EXISTS dirs_current_path ON dirs(path) WHERE current = 1;
CREATE INDEX IF NOT EXISTS dirs_parent ON dirs(parent, first_scan);
CREATE INDEX IF NOT EXISTS dirs_root ON dirs(root_id, current, last_scan);
CREATE UNIQUE INDEX IF NOT EXISTS files_current_path ON files(path) WHERE current = 1;
CREATE INDEX IF NOT EXISTS files_dir ON files(dir, first_scan);
CREATE INDEX IF NOT EXISTS files_path_history ON files(path, first_scan);
CREATE INDEX IF NOT EXISTS files_root ON files(root_id, current, last_scan);
CREATE INDEX IF NOT EXISTS files_size_sample ON files(size, sample) WHERE current = 1;
CREATE INDEX IF NOT EXISTS files_inode ON files(dev, ino) WHERE current = 1;
CREATE INDEX IF NOT EXISTS files_sha256 ON files(sha256);
-- what's recognised on each root (recognise.rs), as of one scan; added in 0.3 without a schema
-- bump, since older versions just ignore it
CREATE TABLE IF NOT EXISTS entities (
    root_id     INTEGER PRIMARY KEY REFERENCES roots(id),
    scan        INTEGER NOT NULL,
    computed_at INTEGER NOT NULL,
    data        TEXT NOT NULL         -- JSON: recognise::RootEntities
);
-- filing plans (plan.rs) and the journal of applying them (apply.rs): one row per file copied,
-- found already there, skipped as a duplicate, or in conflict; the proof of what happened
CREATE TABLE IF NOT EXISTS plans (
    id         INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL,
    target     TEXT NOT NULL,
    data       TEXT NOT NULL,         -- JSON: plan::Plan
    state      TEXT NOT NULL          -- new | running | done | partial
);
CREATE TABLE IF NOT EXISTS journal (
    id      INTEGER PRIMARY KEY,
    plan_id INTEGER NOT NULL REFERENCES plans(id),
    item    INTEGER NOT NULL,
    src     BLOB NOT NULL,
    dest    BLOB,
    size    INTEGER NOT NULL,
    mtime   INTEGER NOT NULL,
    sha256  BLOB,
    state   TEXT NOT NULL,            -- copied | already-there | duplicate | conflict | failed
    error   TEXT,
    at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS journal_src ON journal(plan_id, src);
-- standing mirrors (mirror.rs): keep a folder copied on another drive, verified now and then
CREATE TABLE IF NOT EXISTS mirrors (
    id                  INTEGER PRIMARY KEY,
    source              TEXT NOT NULL,
    target              TEXT NOT NULL,
    created_at          INTEGER NOT NULL,
    verify_every_days   INTEGER NOT NULL DEFAULT 30,
    last_sync           INTEGER,
    last_sync_summary   TEXT,
    last_verify         INTEGER,
    last_verify_summary TEXT,
    last_verify_ok      INTEGER
);
-- wipes (wipe.rs) and every file they deleted, with its proof: the copy that was re-read (or the
-- one this wipe moved it to) and its hash
CREATE TABLE IF NOT EXISTS wipes (
    id         INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL,
    paths      TEXT NOT NULL,         -- JSON list of what was selected
    move_to    TEXT,
    state      TEXT NOT NULL,         -- new | running | done | partial
    summary    TEXT
);
CREATE TABLE IF NOT EXISTS deletions (
    id        INTEGER PRIMARY KEY,
    wipe_id   INTEGER NOT NULL REFERENCES wipes(id),
    path      BLOB NOT NULL,
    size      INTEGER NOT NULL,
    sha256    BLOB NOT NULL,
    proof     BLOB NOT NULL,          -- the copy kept (empty for an empty file)
    proof_how TEXT NOT NULL,          -- re-read | moved | index | empty
    at        INTEGER NOT NULL
);
";
const VERSION: i64 = 2;

pub struct Db {
    pub conn: Connection,
}

#[derive(Debug, Serialize, Deserialize)]
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

/// v1 (0.2.0-alpha.1) had no folder data: add `files.dir` from each path. Folder totals (`dirs`)
/// appear with the next `index` of each root.
fn migrate_v1(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN; ALTER TABLE files ADD COLUMN dir BLOB;")?;
    {
        let mut read = conn.prepare("SELECT id, path FROM files")?;
        let mut write = conn.prepare("UPDATE files SET dir = ?2 WHERE id = ?1")?;
        let rows = read.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
        for row in rows {
            let (id, path) = row?;
            let path = crate::paths::from_bytes(&path);
            write.execute(rusqlite::params![
                id,
                path.parent().map(crate::paths::to_bytes)
            ])?;
        }
    }
    conn.execute_batch("COMMIT")?;
    Ok(())
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        let conn =
            Connection::open(path).with_context(|| format!("opening index {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;",
        )?;
        // a second writer (a CLI command saving results while the daemon works) waits its turn
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > VERSION {
            anyhow::bail!(
                "{} was written by a newer BackDown (schema {version}); update backdown",
                path.display()
            );
        }
        if version == 1 {
            migrate_v1(&conn)?;
        }
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(&format!("PRAGMA user_version = {VERSION}"))?;
        Ok(Db { conn })
    }

    /// A read-only connection, for readers alongside the one writer (daemon, UI, reports).
    /// Never creates or migrates anything.
    pub fn open_read_only(path: &Path) -> Result<Db> {
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("opening index {} read-only", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
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
