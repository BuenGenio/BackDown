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

mod tx;
pub use tx::{Lock, Tx};

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
    current    INTEGER NOT NULL DEFAULT 1,
    lname      TEXT                   -- its own name folded for search (find.rs); added after 0.2.0-alpha.4
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
    current    INTEGER NOT NULL DEFAULT 1,
    lname      TEXT
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
    at      INTEGER NOT NULL,
    note    TEXT                      -- renamed, or what a copy didn't keep; added in 1.0 (add_column)
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
-- rescues (rescue/): copying a failing drive to an image, finding files by content, finding lost
-- partitions. What was asked, where it's going, and the latest outcome as JSON
CREATE TABLE IF NOT EXISTS rescues (
    id         INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL,
    mode       TEXT NOT NULL,         -- image | carve | parts
    source     TEXT NOT NULL,         -- a device node or an image file
    dest       TEXT,                  -- the image file (image) or folder (carve)
    options    TEXT NOT NULL,         -- JSON
    state      TEXT NOT NULL,         -- new | running | done | stopped | failed
    summary    TEXT,
    result     TEXT,                  -- JSON: the image's totals, what was found
    updated_at INTEGER NOT NULL
);
-- drive health (health/): readings of each physical drive's SMART data, kept when they say
-- something new or once a day, under the drive's serial (or WWN) so its history follows it
-- between ports and machines; and the notifications the daemon sent about drives, mirrors and
-- jobs (read or not). Added without a schema bump: older versions just ignore them
CREATE TABLE IF NOT EXISTS health (
    id      INTEGER PRIMARY KEY,
    drive   TEXT NOT NULL,            -- sn:<serial> | wwn:<wwn>
    at      INTEGER NOT NULL,
    verdict TEXT NOT NULL,            -- ok | warning | failing | unknown
    data    TEXT NOT NULL             -- JSON: health::Health
);
CREATE INDEX IF NOT EXISTS health_drive ON health(drive, at);
CREATE TABLE IF NOT EXISTS notifications (
    id     INTEGER PRIMARY KEY,
    at     INTEGER NOT NULL,
    kind   TEXT NOT NULL,             -- health | drift | job
    key    TEXT NOT NULL,             -- what it's about, so the same thing isn't said twice
    level  TEXT NOT NULL,             -- info | warning | failing
    title  TEXT NOT NULL,
    body   TEXT NOT NULL,
    drive  TEXT,                      -- a drive's health id, to link to it
    shown  INTEGER NOT NULL,          -- sent to the desktop (not turned off or held back)
    read   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS notifications_key ON notifications(key, at);
-- other machines (remote/): reached over ssh, read-only. What each one's index holds is cached here
-- (its drives, and its current files with their hashes, pulled by scan), so copies there can count;
-- `seen_at` and `hashed_at` say when a file was last confirmed there. Added without a schema bump
CREATE TABLE IF NOT EXISTS remotes (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    url        TEXT NOT NULL,             -- ssh://[user@]host[:port]
    db         TEXT,                      -- the index there, when not its default
    command    TEXT,                      -- where backdown is there, when not on its PATH
    created_at INTEGER NOT NULL,
    last_try   INTEGER,
    last_sync  INTEGER,                   -- the last sync that finished
    last_error TEXT,
    info       TEXT                       -- JSON: what it said about itself (version, host, clock)
);
CREATE TABLE IF NOT EXISTS remote_roots (
    remote_id   INTEGER NOT NULL REFERENCES remotes(id) ON DELETE CASCADE,
    root_id     INTEGER NOT NULL,         -- the drive's id there
    path        BLOB NOT NULL,            -- its path there, in that machine's raw bytes
    display     TEXT NOT NULL,
    last_scan   INTEGER,                  -- its latest finished scan there
    scanned_at  INTEGER,                  -- when that finished (this machine's clock)
    files       INTEGER NOT NULL DEFAULT 0,
    bytes       INTEGER NOT NULL DEFAULT 0,
    synced_scan INTEGER,                  -- the scan the cached files are complete up to
    seen_at     INTEGER,                  -- when that scan finished (this machine's clock)
    cached      INTEGER,                  -- its files cached here, counted at the end of each sync
    cached_hashed INTEGER,                -- how many of those have a hash from there
    PRIMARY KEY (remote_id, root_id)
);
CREATE TABLE IF NOT EXISTS remote_files (
    remote_id INTEGER NOT NULL,
    rid       INTEGER NOT NULL,           -- the file's row id there
    root_id   INTEGER NOT NULL,
    path      BLOB NOT NULL,
    size      INTEGER NOT NULL,
    mtime     INTEGER NOT NULL,
    sample    BLOB,
    sha256    BLOB,
    error     INTEGER NOT NULL DEFAULT 0, -- that machine couldn't read it
    hashed_at INTEGER,                    -- when that machine last re-read it for us (this machine's clock)
    PRIMARY KEY (remote_id, rid)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS remote_files_sha ON remote_files(sha256);
CREATE INDEX IF NOT EXISTS remote_files_size ON remote_files(size);
CREATE INDEX IF NOT EXISTS remote_files_path ON remote_files(remote_id, path);
CREATE TABLE IF NOT EXISTS remote_settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- drive health on other machines (remote/): each machine's drives as its own index last had them
-- (latest reading, verdict, latest changes), replaced at each sync; and how asking it went, with
-- its own latest drive-health notifications. Added without a schema bump
CREATE TABLE IF NOT EXISTS remote_health (
    remote_id  INTEGER NOT NULL,
    drive      TEXT NOT NULL,             -- its health id: sn:<serial> | wwn:<wwn>
    model      TEXT,
    serial     TEXT,
    verdict    TEXT NOT NULL,             -- ok | warning | failing | unknown
    summary    TEXT NOT NULL,             -- JSON: the reading, findings, latest changes, trend
    read_at    INTEGER,                   -- when it was read there (this machine's clock)
    fetched_at INTEGER NOT NULL,
    PRIMARY KEY (remote_id, drive)
);
CREATE TABLE IF NOT EXISTS remote_health_state (
    remote_id INTEGER PRIMARY KEY,
    asked_at  INTEGER NOT NULL,           -- the last sync that asked
    ok_at     INTEGER,                    -- the last one it answered (NULL: its drives not seen yet)
    state     TEXT NOT NULL,              -- ok | unsupported (a BackDown there from before) | error
    error     TEXT,
    notes     TEXT                        -- JSON: its own latest drive-health notifications
);
-- NTFS alternate data streams (streams.rs): per file version, only for files that have some; and
-- the roots whose every file has been looked at once. Added in 1.0 without a schema bump
CREATE TABLE IF NOT EXISTS streams (
    file_id INTEGER PRIMARY KEY,      -- files.id
    count   INTEGER NOT NULL,
    bytes   INTEGER NOT NULL,
    names   TEXT NOT NULL             -- JSON: [[name, bytes], ...]
);
CREATE TABLE IF NOT EXISTS streams_roots (
    root_id INTEGER PRIMARY KEY
);
-- who and what the recognised things are about (subjects.rs): people and databases, per root, as
-- of one scan, like `entities`. Added in 1.1 without a schema bump
CREATE TABLE IF NOT EXISTS subjects (
    root_id     INTEGER PRIMARY KEY REFERENCES roots(id),
    scan        INTEGER NOT NULL,
    computed_at INTEGER NOT NULL,
    data        TEXT NOT NULL         -- JSON: subjects::RootSubjects
);
-- the swarm (swarm.rs, docs/SWARM.md). Each other machine's storage as it last said (its drives
-- and file systems with their size and free space, its indexed drives), replaced at each sync and
-- each live look; and the physical drive each indexed drive here was on when it was last scanned,
-- so one that's unplugged or moved can still be named. Added without a schema bump
CREATE TABLE IF NOT EXISTS remote_volumes (
    remote_id INTEGER PRIMARY KEY,
    state     TEXT NOT NULL,              -- ok | unsupported (a BackDown there from before) | error
    data      TEXT,                       -- JSON: its storage report (swarm::storage), as it sent it
    error     TEXT,
    asked_at  INTEGER NOT NULL,
    ok_at     INTEGER                     -- when it last answered (this machine's clock)
);
CREATE TABLE IF NOT EXISTS root_drives (
    root_id INTEGER PRIMARY KEY,          -- roots.id
    at      INTEGER NOT NULL,
    data    TEXT NOT NULL                 -- JSON: the disk (model, serial, size) and file system (label, UUID)
);
-- drives being removed from the index (forget.rs): not scanned again, and finished if a removal
-- was interrupted. Added without a schema bump
CREATE TABLE IF NOT EXISTS roots_removing (
    root_id INTEGER PRIMARY KEY,          -- roots.id
    at      INTEGER NOT NULL
);
";
const VERSION: i64 = 2;

pub struct Db {
    pub conn: Connection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Add a column an older index's table lacks (older versions just leave it empty).
fn add_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let has: bool = conn.query_row(
        &format!("SELECT COUNT(*) > 0 FROM pragma_table_info('{table}') WHERE name = ?1"),
        [column],
        |r| r.get(0),
    )?;
    if !has {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
    }
    Ok(())
}

/// The duplicates report's covering index (report::dupes): current, hashed files by sha256 with
/// their size and inode, so grouping them and collapsing hardlinks is answered from the index
/// alone, without a table lookup per row. Partial, so it holds only rows the report can use.
/// Not in SCHEMA: on a big index it takes minutes to build (about 3 on 16.9M files), longer than
/// a service may take to start, so the daemon builds it as a job of its own once it's running
/// (and `backdown dupes` before its first report). Until then the report reads the table instead.
const DUPES_INDEX: &str = "CREATE INDEX IF NOT EXISTS files_dupes ON files(sha256, size, dev, ino)
    WHERE current = 1 AND sha256 IS NOT NULL";

/// Indexes that are slow to build on a big index, so they're made by a job once the service runs
/// (`Db::build_later_indexes`) rather than when the index opens; until each exists, what it speeds
/// up still works, reading the table instead. (name, what it's for, SQL)
const LATER_INDEXES: &[(&str, &str, &str)] = &[
    (
        "dirs_path_history",
        "opening folders",
        // every version of a folder by its path: a folder's totals at a scan and how many
        // versions each subfolder has (browse.rs). Without it each is a read of all folders.
        "CREATE INDEX IF NOT EXISTS dirs_path_history ON dirs(path, first_scan)",
    ),
    ("files_dupes", "the duplicates report", DUPES_INDEX),
    // what's still to ask another machine to read for us (remote::wanted): only the rows without
    // a hash from there, largest first, so a sync with millions of files cached from there
    // doesn't sort them all for every few dozen it asks about
    (
        "remote_files_unhashed",
        "asking other machines for hashes",
        "CREATE INDEX IF NOT EXISTS remote_files_unhashed ON remote_files(remote_id, size)
            WHERE sha256 IS NULL AND error = 0",
    ),
];

impl Db {
    pub(crate) fn has_index(&self, name: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1)",
            [name],
            |r| r.get(0),
        )?)
    }

    /// The slow-to-build indexes that aren't there yet (see `LATER_INDEXES`).
    pub fn missing_later_indexes(&self) -> Result<Vec<&'static str>> {
        let mut v = Vec::new();
        for (name, _, _) in LATER_INDEXES {
            if !self.has_index(name)? {
                v.push(*name);
            }
        }
        Ok(v)
    }

    /// Build the slow-to-build indexes that are missing, saying which one is under way; what each
    /// one speeds up, for those built now.
    pub fn build_later_indexes(&self, doing: &mut dyn FnMut(&str)) -> Result<Vec<&'static str>> {
        let mut built = Vec::new();
        for (name, what, sql) in LATER_INDEXES {
            if !self.has_index(name)? {
                doing(what);
                self.conn.execute_batch(sql)?;
                built.push(*what);
            }
        }
        Ok(built)
    }

    /// Whether the duplicates report's covering index is there (see `DUPES_INDEX`).
    pub fn has_dupes_index(&self) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = 'files_dupes')",
            [],
            |r| r.get(0),
        )?)
    }

    /// Build the duplicates report's covering index if it isn't there; whether it was built now.
    pub fn ensure_dupes_index(&self) -> Result<bool> {
        if self.has_dupes_index()? {
            return Ok(false);
        }
        self.conn.execute_batch(DUPES_INDEX)?;
        Ok(true)
    }

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
        // tidying the archive (dedup.rs) and merging mailboxes (mailmerge.rs): their runs and
        // journals, added without a schema bump (older versions ignore them)
        conn.execute_batch(crate::dedup::SCHEMA)?;
        conn.execute_batch(crate::mailmerge::SCHEMA)?;
        // the chain of custody (custody.rs): every copy, move, deletion, link and merge, chained
        // and sealed; added the same way
        conn.execute_batch(crate::custody::SCHEMA)?;
        // signed manifests of drives (manifest.rs), the same way
        conn.execute_batch(crate::manifest::SCHEMA)?;
        // content search (fts.rs): documents and their FTS5 index, the same way
        conn.execute_batch(crate::fts::SCHEMA)?;
        // the swarm's pool folders on this machine (pool.rs), the same way
        conn.execute_batch(crate::pool::SCHEMA)?;
        conn.execute_batch(crate::pool::plan::SCHEMA)?;
        conn.execute_batch(crate::pool::receive::SCHEMA)?;
        conn.execute_batch(crate::pool::send::SCHEMA)?;
        add_column(&conn, "journal", "note", "TEXT")?;
        // counted once per sync instead of on every look at the machines (remote::overview):
        // a big machine's cache is millions of rows
        add_column(&conn, "remote_roots", "cached", "INTEGER")?;
        add_column(&conn, "remote_roots", "cached_hashed", "INTEGER")?;
        // names for search: added in place, without a schema bump, since older versions just
        // leave them empty on rows they write (and the next open here fills those in)
        crate::find::add_names(&conn)?;
        crate::index::add_columns(&conn)?;
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
        let scans: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM scans WHERE finished_at IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        // every count in one read of the files table: it can be bigger than the memory there is
        // to cache it in (23 GB for 17M files), so each separate count would read it from disk
        let c: [i64; 6] = self.conn.query_row(
            "SELECT COALESCE(SUM(current = 1), 0),
                    COALESCE(SUM(CASE WHEN current = 1 THEN size END), 0),
                    COUNT(CASE WHEN current = 1 THEN sample END),
                    COUNT(CASE WHEN current = 1 THEN sha256 END),
                    COUNT(CASE WHEN current = 1 THEN error END),
                    COALESCE(SUM(current = 0), 0)
             FROM files",
            [],
            |r| {
                Ok([
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ])
            },
        )?;
        let [files, bytes, sampled, hashed, unreadable, versions] = c.map(|n| n as u64);
        Ok(Status {
            roots,
            scans: scans as u64,
            files,
            bytes,
            sampled,
            hashed,
            unreadable,
            versions,
        })
    }
}
