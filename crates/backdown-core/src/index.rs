//! Scans: every regular file of a root (a drive, or a top folder) recorded as a new scan. Read-only;
//! symlinks are not followed.
//!
//! Unchanged files (same size and mtime) extend their current row and keep its hashes; a changed
//! file closes its old row and opens a new version; files that disappeared are closed. Folders are
//! versioned the same way, with recursive totals. Rows are committed in batches, so an interrupted
//! run keeps its progress and the next run finishes the job (an unfinished scan never appears on
//! the timeline).
//!
//! A scan is made one of three ways, and whichever way it's made, its rows are the ones a walk
//! would have written (proofs need every file seen, so none of them may skip any):
//!
//! - **walk**: every folder is listed and every file looked at. The reference, and the fallback
//!   whenever there's any doubt about the others.
//! - **journal**: the drive's own change log (the USN journal of an NTFS volume on Windows, a
//!   volume's FSEvents history on macOS) says which paths changed since the previous scan began;
//!   only those are looked at again (`changes`), and everything else carries over. A change log
//!   that was reset, trimmed, belongs to another volume, dropped events or says anything odd means
//!   a walk instead.
//! - **table**: an NTFS volume's master file table read straight from the device, record after
//!   record, instead of folder by folder (`table`): for first scans and whenever the change log
//!   can't be used, where the device can be read.
//!
//! `scan` picks the way (`Method::Auto`): the change log if the previous scan noted its position
//! and it's intact, else the file table, else a walk; and a walk anyway every so often (a setting:
//! every tenth scan, or after 30 days), so nothing a change log might not know about (a drive
//! changed on another computer) lasts long. Each scan records how it was made and why.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::{Db, find, paths};

mod changes;
pub mod fsevents;
mod journal;
pub mod table;
pub mod usn;

pub use changes::ChangeSet;

const BATCH: u64 = 20_000;
const MAX_ERRORS_KEPT: usize = 20;

pub struct Options {
    /// Stay on the filesystem the root is on (don't descend into other mounts).
    pub one_file_system: bool,
    /// Record the root even if it is now empty although its last scan found files. Without this,
    /// an empty root is refused: it is far more likely an unplugged drive's leftover mount point
    /// than a drive that really was wiped, and recording it would mark every file deleted.
    pub allow_empty: bool,
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub root: PathBuf,
    pub scan: i64,
    pub files: u64,
    pub bytes: u64,
    pub new: u64,
    pub changed: u64,
    pub removed: u64,
    pub errors: u64,
    pub first_errors: Vec<String>,
    /// How the scan was made: "walk", "journal" or "table".
    pub method: &'static str,
    /// Why it was made that way, for people ("the change log was reset").
    pub note: String,
    /// journal: the paths the change log named; table: the file records read.
    pub changes: u64,
    pub took_ms: u64,
}

/// How to make a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Method {
    /// The change log if it can be used, else the file table, else a walk (and a walk every so
    /// often regardless: see `Settings`).
    #[default]
    Auto,
    Walk,
    /// The change log only: fails, saying why, when it can't be used.
    Journal,
    /// The file table only: fails, saying why, when it can't be read.
    Table,
}

impl std::str::FromStr for Method {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Method> {
        Ok(match s {
            "auto" => Method::Auto,
            "walk" => Method::Walk,
            "journal" => Method::Journal,
            "table" => Method::Table,
            _ => bail!("the method is auto, walk, journal or table (not {s:?})"),
        })
    }
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Auto => "auto",
            Method::Walk => "walk",
            Method::Journal => "journal",
            Method::Table => "table",
        }
    }
}

/// Walk the root and record it as a new scan (always a walk; `scan` chooses the fastest safe way).
pub fn index(
    db: &mut Db,
    root: &Path,
    opts: &Options,
    progress: &mut dyn FnMut(&Stats),
) -> Result<Stats> {
    scan(db, root, opts, Method::Walk, progress)
}

// ---------- settings

/// How `Method::Auto` may make scans. Kept in the index (`scan_settings`), so the CLI and the
/// daemon follow the same ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Use the drive's change log and file table when possible (else always walk).
    pub fast: bool,
    /// Walk anyway when this many scans of a drive in a row weren't walks (0: never for this reason).
    pub walk_every_scans: u32,
    /// Walk anyway when a drive's last walk is this many days old (0: never for this reason).
    pub walk_every_days: u32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            fast: true,
            walk_every_scans: 10,
            walk_every_days: 30,
        }
    }
}

/// The scan settings (the defaults where none were saved, or in an index made before them).
pub fn settings(db: &Db) -> Result<Settings> {
    let mut s = Settings::default();
    let rows: Vec<(String, String)> = match db.conn.prepare("SELECT key, value FROM scan_settings")
    {
        Ok(mut stmt) => stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?,
        Err(_) => return Ok(s), // read-only, from before the table existed
    };
    for (k, v) in rows {
        match k.as_str() {
            "fast" => s.fast = v != "0",
            "walk_every_scans" => s.walk_every_scans = v.parse().unwrap_or(s.walk_every_scans),
            "walk_every_days" => s.walk_every_days = v.parse().unwrap_or(s.walk_every_days),
            _ => {}
        }
    }
    Ok(s)
}

pub fn set_settings(db: &Db, s: &Settings) -> Result<()> {
    if s.walk_every_scans > 1000 || s.walk_every_days > 3650 {
        bail!("a walk at least every 1000 scans and every 3650 days");
    }
    let mut put = db
        .conn
        .prepare("INSERT OR REPLACE INTO scan_settings(key, value) VALUES (?1, ?2)")?;
    put.execute(params!["fast", if s.fast { "1" } else { "0" }])?;
    put.execute(params!["walk_every_scans", s.walk_every_scans.to_string()])?;
    put.execute(params!["walk_every_days", s.walk_every_days.to_string()])?;
    Ok(())
}

/// Adds what scans record about how they were made, folder identities and the settings to an
/// index made before them (no schema bump: older versions ignore them). Called by `Db::open`.
pub(crate) fn add_columns(conn: &Connection) -> Result<()> {
    let has = |table: &str, col: &str| -> Result<bool> {
        Ok(conn.query_row(
            &format!("SELECT COUNT(*) > 0 FROM pragma_table_info('{table}') WHERE name = ?1"),
            [col],
            |r| r.get(0),
        )?)
    };
    for (table, col, ty) in [
        ("scans", "method", "TEXT"), // walk | journal | table (NULL: a walk, from before)
        ("scans", "method_note", "TEXT"), // why it was made that way
        ("scans", "changes", "INTEGER"), // journal: paths named; table: records read
        ("scans", "took_ms", "INTEGER"),
        ("scans", "cursor", "TEXT"), // JSON: where the drive's change log was when it began
        ("dirs", "dev", "INTEGER"),  // the folder's own identity, as for files
        ("dirs", "ino", "INTEGER"),
    ] {
        if !has(table, col)? {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {col} {ty}"))?;
        }
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS scan_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         -- a folder's other names (HFS+ folder hard links), for rescans from a change log
         CREATE INDEX IF NOT EXISTS dirs_inode ON dirs(dev, ino) WHERE current = 1;",
    )?;
    Ok(())
}

// ---------- how a scan was made, for people

/// How one scan was made.
#[derive(Debug, Clone, Serialize)]
pub struct How {
    pub scan: i64,
    pub method: String,
    pub note: Option<String>,
    pub changes: Option<i64>,
    pub took_ms: Option<i64>,
}

impl How {
    /// For people: "rescanned from the change log in 2 s: 412 changes", "read the file table:
    /// 1.20 M records in 9 s", "full walk in 3 min: the change log was reset".
    pub fn phrase(&self) -> String {
        describe(
            &self.method,
            self.note.as_deref().unwrap_or(""),
            self.changes.unwrap_or(0) as u64,
            self.took_ms.map(|t| t as u64),
        )
    }
}

impl Stats {
    /// How this scan was made, for people (see `How::phrase`).
    pub fn how(&self) -> String {
        describe(self.method, &self.note, self.changes, Some(self.took_ms))
    }
}

fn describe(method: &str, note: &str, changes: u64, took_ms: Option<u64>) -> String {
    let took = took_ms
        .map(|ms| format!(" in {}", took(ms)))
        .unwrap_or_default();
    match method {
        "journal" => format!(
            "rescanned from the change log{took}: {} change{}",
            crate::fmt::count(changes),
            if changes == 1 { "" } else { "s" }
        ),
        "table" => format!(
            "read the file table: {} records{took}",
            crate::fmt::count(changes)
        ),
        _ if note.is_empty() => format!("full walk{took}"),
        _ => format!("full walk{took}: {note}"),
    }
}

/// A duration for people: "under a second", "2 s", "3 min 4 s", "1 h 2 min".
pub fn took(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0 => "under a second".into(),
        1..=59 => format!("{s} s"),
        60..=3599 if s.is_multiple_of(60) => format!("{} min", s / 60),
        60..=3599 => format!("{} min {} s", s / 60, s % 60),
        _ => format!("{} h {} min", s / 3600, s % 3600 / 60),
    }
}

/// How the given scans were made (scans from before this was recorded were walks).
pub fn how(db: &Db, scans: &[i64]) -> Result<Vec<How>> {
    let mut stmt = db.conn.prepare_cached(
        "SELECT id, COALESCE(method, 'walk'), method_note, changes, took_ms FROM scans WHERE id = ?1",
    )?;
    let mut out = Vec::new();
    for s in scans {
        if let Some(h) = stmt
            .query_row([s], |r| {
                Ok(How {
                    scan: r.get(0)?,
                    method: r.get(1)?,
                    note: r.get(2)?,
                    changes: r.get(3)?,
                    took_ms: r.get(4)?,
                })
            })
            .optional()?
        {
            out.push(h);
        }
    }
    Ok(out)
}

// ---------- making a scan

/// Where the drive's change log stood when a scan began, and what it was made with: the start
/// of the next scan's reading of the log.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Saved {
    /// the scan stayed on the root's filesystem
    pub one_fs: bool,
    /// the root's device number (it can change when a drive is plugged in again)
    pub dev: Option<u64>,
    pub journal: Option<journal::Cursor>,
}

/// The previous finished scan of a root.
pub(crate) struct Prev {
    pub started_at: i64,
    pub saved: Option<Saved>,
}

/// Record a new scan of `root`, made the way `method` says (see the module notes).
pub fn scan(
    db: &mut Db,
    root: &Path,
    opts: &Options,
    method: Method,
    progress: &mut dyn FnMut(&Stats),
) -> Result<Stats> {
    let t0 = Instant::now();
    let (root, root_id) = prepare(db, root, opts)?;
    let settings = settings(db)?;
    let prev = previous(&db.conn, root_id)?;
    // where the change log is now, before anything is read: whatever changes from here on, the
    // next scan will hear of
    let cursor = journal::cursor_now(&root);
    let saved = Saved {
        one_fs: opts.one_file_system,
        dev: device_of(&root),
        journal: cursor.as_ref().ok().cloned().flatten(),
    };
    let started = now();

    let mut notes: Vec<String> = Vec::new();
    let walk_why = match method {
        Method::Walk => Some("asked for a full walk".to_string()),
        Method::Auto if !settings.fast => {
            Some("the change log and file table are turned off in settings".to_string())
        }
        Method::Auto => safety_walk(&db.conn, root_id, &settings)?,
        Method::Journal | Method::Table => None,
    };
    if walk_why.is_none() && matches!(method, Method::Auto | Method::Journal) {
        match journal_changes(db, &root, root_id, opts, prev.as_ref(), &cursor) {
            Ok(changes) => {
                let prev = prev.as_ref().expect("a change log needs a previous scan");
                match from_changes(db, &root, root_id, opts, prev, &changes, &saved, progress) {
                    Ok(mut st) => {
                        st.took_ms = t0.elapsed().as_millis() as u64;
                        set_took(&db.conn, st.scan, st.took_ms)?;
                        return Ok(st);
                    }
                    Err(Fallback::Odd(why)) => notes.push(why),
                    Err(Fallback::Failed(e)) => return Err(e),
                }
            }
            Err(why) => notes.push(why),
        }
        if method == Method::Journal {
            bail!(
                "can't rescan {} from its change log: {}",
                root.display(),
                notes.join("; ")
            );
        }
    }
    if walk_why.is_none() && matches!(method, Method::Auto | Method::Table) {
        match table::read(&root, opts, saved.journal.as_ref()) {
            Ok(t) => match from_table(db, &root, root_id, opts, t, &saved, started, progress) {
                Ok(mut st) => {
                    st.took_ms = t0.elapsed().as_millis() as u64;
                    set_took(&db.conn, st.scan, st.took_ms)?;
                    return Ok(st);
                }
                Err(Fallback::Odd(why)) => notes.push(why),
                Err(Fallback::Failed(e)) => return Err(e),
            },
            Err(why) => notes.push(why),
        }
        if method == Method::Table {
            bail!(
                "can't read the file table of {}: {}",
                root.display(),
                notes.join("; ")
            );
        }
    }
    let note = walk_why.unwrap_or_else(|| notes.join("; "));
    let mut st = walk_scan(db, &root, root_id, opts, &note, &saved, progress)?;
    st.took_ms = t0.elapsed().as_millis() as u64;
    set_took(&db.conn, st.scan, st.took_ms)?;
    Ok(st)
}

/// Apply a change set (the paths something says changed since the previous scan began) as a new
/// scan: what a journal-based rescan does once it has read the log. For tests and tools; fails
/// (recording nothing) where the scan would have fallen back to a walk.
pub fn scan_changes(
    db: &mut Db,
    root: &Path,
    opts: &Options,
    changes: &ChangeSet,
    progress: &mut dyn FnMut(&Stats),
) -> Result<Stats> {
    let (root, root_id) = prepare(db, root, opts)?;
    let prev = previous(&db.conn, root_id)?.context("no earlier scan of this root")?;
    let saved = Saved {
        one_fs: opts.one_file_system,
        dev: device_of(&root),
        journal: None,
    };
    let changes = changes.clone().within(&root);
    match from_changes(db, &root, root_id, opts, &prev, &changes, &saved, progress) {
        Ok(st) => Ok(st),
        Err(Fallback::Odd(why)) => bail!("{why}"),
        Err(Fallback::Failed(e)) => Err(e),
    }
}

/// Why a scan from the change log wasn't made: the log said something that calls for a walk, or
/// something failed.
pub(crate) enum Fallback {
    Odd(String),
    Failed(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for Fallback {
    fn from(e: E) -> Fallback {
        Fallback::Failed(e.into())
    }
}

/// The root, canonical, checked and on record.
fn prepare(db: &mut Db, root: &Path, opts: &Options) -> Result<(PathBuf, i64)> {
    let root =
        dunce::canonicalize(root).with_context(|| format!("can't open {}", root.display()))?;
    if !root.is_dir() {
        bail!("{} is not a directory", root.display());
    }
    let root_bytes = paths::to_bytes(&root);
    let conn = &db.conn;
    refuse_nested(db, &root, &root_bytes)?;
    conn.execute(
        "INSERT OR IGNORE INTO roots(path) VALUES (?1)",
        params![root_bytes],
    )?;
    let root_id: i64 = conn.query_row(
        "SELECT id FROM roots WHERE path = ?1",
        params![root_bytes],
        |r| r.get(0),
    )?;
    if !opts.allow_empty {
        let had: i64 = conn.query_row(
            "SELECT COALESCE((SELECT files FROM scans WHERE id = r.last_scan), 0) FROM roots r WHERE r.id = ?1",
            params![root_id],
            |r| r.get(0),
        )?;
        let empty = std::fs::read_dir(&root)
            .with_context(|| format!("can't read {}", root.display()))?
            .next()
            .is_none();
        if had > 0 && empty {
            bail!(
                "{} is empty, but held {had} files at its last scan. Is the drive unplugged? \
                 Nothing was recorded; if it really was emptied, index again with --allow-empty",
                root.display()
            );
        }
    }
    Ok((root, root_id))
}

fn previous(conn: &Connection, root_id: i64) -> Result<Option<Prev>> {
    Ok(conn
        .query_row(
            "SELECT s.started_at, s.cursor FROM roots r JOIN scans s ON s.id = r.last_scan
             WHERE r.id = ?1 AND s.finished_at IS NOT NULL",
            params![root_id],
            |r| {
                Ok(Prev {
                    started_at: r.get(0)?,
                    saved: r
                        .get::<_, Option<String>>(1)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                })
            },
        )
        .optional()?)
}

/// Whether it's time for a walk however fast the other ways are, and why.
fn safety_walk(conn: &Connection, root_id: i64, s: &Settings) -> Result<Option<String>> {
    let (since_walk, last_walk, first): (i64, Option<i64>, Option<i64>) = conn.query_row(
        "WITH w AS (SELECT MAX(id) AS id, MAX(finished_at) AS at FROM scans
                    WHERE root_id = ?1 AND finished_at IS NOT NULL AND COALESCE(method, 'walk') = 'walk')
         SELECT (SELECT COUNT(*) FROM scans WHERE root_id = ?1 AND finished_at IS NOT NULL
                   AND id > COALESCE((SELECT id FROM w), 0)),
                (SELECT at FROM w),
                (SELECT MIN(finished_at) FROM scans WHERE root_id = ?1 AND finished_at IS NOT NULL)",
        params![root_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if s.walk_every_scans > 0 && since_walk + 1 >= i64::from(s.walk_every_scans) {
        return Ok(Some(format!(
            "a full walk every {} scans, to be sure",
            s.walk_every_scans
        )));
    }
    if s.walk_every_days > 0
        && let Some(t) = last_walk.or(first)
        && now() - t >= i64::from(s.walk_every_days) * 86_400
    {
        return Ok(Some(format!(
            "a full walk every {} days, to be sure",
            s.walk_every_days
        )));
    }
    Ok(None)
}

/// What the change log says changed since the previous scan began, or why it can't say.
fn journal_changes(
    db: &Db,
    root: &Path,
    root_id: i64,
    opts: &Options,
    prev: Option<&Prev>,
    now_cursor: &std::result::Result<Option<journal::Cursor>, String>,
) -> std::result::Result<ChangeSet, String> {
    let prev = prev.ok_or("no earlier scan to start from")?;
    if !opts.one_file_system {
        return Err("scans that go into other filesystems are always walked".into());
    }
    let saved = prev
        .saved
        .as_ref()
        .ok_or("the previous scan didn't note where the change log was")?;
    if !saved.one_fs {
        return Err("the previous scan went into other filesystems".into());
    }
    let from = saved
        .journal
        .as_ref()
        .ok_or("no change log was noted for this drive at the previous scan")?;
    let now_cursor = match now_cursor {
        Ok(Some(c)) => c,
        Ok(None) => return Err("this drive has no change log BackDown can read".into()),
        Err(why) => return Err(why.clone()),
    };
    let changes = journal::changes_since(db, root, root_id, from, now_cursor)?.within(root);
    if let Some(why) = &changes.full {
        return Err(why.clone());
    }
    Ok(changes)
}

/// A walk of the whole root.
fn walk_scan(
    db: &mut Db,
    root: &Path,
    root_id: i64,
    opts: &Options,
    note: &str,
    saved: &Saved,
    progress: &mut dyn FnMut(&Stats),
) -> Result<Stats> {
    let mut w = Writer::begin(&db.conn, root, root_id)?;
    let items = walk_items(root, opts.one_file_system);
    write_items(&mut w, items, true, None, progress)?;
    w.close_unseen()?;
    w.finish("walk", note, 0, saved, progress)
}

/// A scan from the file table: its entries written as a walk would write them.
#[allow(clippy::too_many_arguments)]
fn from_table(
    db: &mut Db,
    root: &Path,
    root_id: i64,
    opts: &Options,
    t: table::Table,
    saved: &Saved,
    started: i64,
    progress: &mut dyn FnMut(&Stats),
) -> std::result::Result<Stats, Fallback> {
    let records = t.records;
    let note = t.note.clone();
    let mut w = Writer::begin(&db.conn, root, root_id)?;
    write_items(&mut w, t.items(root), true, None, progress)?;
    w.close_unseen()?;
    // on a live volume the table is a moment behind: what changed while (and just before) it was
    // read is taken from the change log, if there is one
    if let Some(catch_up) = &t.catch_up {
        let changes = catch_up.clone().within(root);
        let ctx = changes::Ctx {
            one_fs: opts.one_file_system,
            root_dev: device_of(root),
            old_dev: device_of(root),
            since: started,
        };
        let plan = changes::resolve(w.conn, root, &changes, &ctx).map_err(|e| match e {
            Fallback::Odd(why) => Fallback::Odd(format!(
                "the file table couldn't be brought up to date: {why}"
            )),
            other => other,
        })?;
        changes::execute(&mut w, plan, &ctx, progress)?;
    }
    Ok(w.finish("table", &note, records, saved, progress)?)
}

/// A scan from a change set, on top of the previous scan.
#[allow(clippy::too_many_arguments)]
fn from_changes(
    db: &mut Db,
    root: &Path,
    root_id: i64,
    opts: &Options,
    prev: &Prev,
    changes: &ChangeSet,
    saved: &Saved,
    progress: &mut dyn FnMut(&Stats),
) -> std::result::Result<Stats, Fallback> {
    if !opts.one_file_system {
        return Err(Fallback::Odd(
            "scans that go into other filesystems are always walked".into(),
        ));
    }
    if let Some(why) = &changes.full {
        return Err(Fallback::Odd(why.clone()));
    }
    let old_dev = prev.saved.as_ref().and_then(|s| s.dev);
    let ctx = changes::Ctx {
        one_fs: true,
        root_dev: device_of(root),
        old_dev: old_dev.or_else(|| device_of(root)),
        since: prev.started_at,
    };
    // everything is worked out before anything is written: a change set that turns out odd
    // leaves no trace (the caller walks instead)
    let plan = changes::resolve(&db.conn, root, changes, &ctx)?;
    let mut w = Writer::begin(&db.conn, root, root_id)?;
    let n = changes.len() as u64;
    changes::execute(&mut w, plan, &ctx, progress)?;
    let note = format!(
        "{} change{} in the log",
        crate::fmt::count(n),
        if n == 1 { "" } else { "s" }
    );
    Ok(w.finish("journal", &note, n, saved, progress)?)
}

fn set_took(conn: &Connection, scan: i64, ms: u64) -> Result<()> {
    conn.execute(
        "UPDATE scans SET took_ms = ?2 WHERE id = ?1",
        params![scan, ms as i64],
    )?;
    Ok(())
}

// ---------- writing rows

/// A folder's recursive totals, as a walk adds them up.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Totals {
    pub files: u64,
    pub bytes: u64,
    /// the newest file mtime below (0 without files)
    pub newest: i64,
}

impl Totals {
    fn add(&mut self, o: Totals) {
        self.files += o.files;
        self.bytes += o.bytes;
        self.newest = self.newest.max(o.newest);
    }
}

/// One scan being written: its rows, its counts and its transaction.
pub(crate) struct Writer<'c> {
    pub conn: &'c Connection,
    pub root_id: i64,
    pub scan: i64,
    pub st: Stats,
    pending: u64,
    last: Instant,
    /// in a transaction (until `finish` commits it)
    open: bool,
}

impl Drop for Writer<'_> {
    /// A scan that fails part way keeps what earlier batches committed (the next scan carries
    /// on from there) and drops the batch it was in, so the connection is left usable.
    fn drop(&mut self) {
        if self.open {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

impl<'c> Writer<'c> {
    /// A new scan of the root (unfinished, so invisible until `finish`), in a transaction.
    fn begin(conn: &'c Connection, root: &Path, root_id: i64) -> Result<Writer<'c>> {
        conn.execute(
            "INSERT INTO scans(root_id, started_at) VALUES (?1, ?2)",
            params![root_id, now()],
        )?;
        let scan = conn.last_insert_rowid();
        conn.execute_batch("BEGIN")?;
        Ok(Writer {
            conn,
            root_id,
            scan,
            st: Stats {
                root: root.to_path_buf(),
                scan,
                ..Stats::default()
            },
            pending: 0,
            last: Instant::now(),
            open: true,
        })
    }

    /// Records a file seen now: extends its current row if its size and mtime are unchanged,
    /// otherwise closes that row and opens a new version. Returns the row.
    pub fn file(
        &mut self,
        path: &Path,
        size: u64,
        mtime: i64,
        dev: Option<i64>,
        ino: Option<i64>,
    ) -> Result<i64> {
        let pb = paths::to_bytes(path);
        let existing: Option<(i64, i64, i64)> = self
            .conn
            .prepare_cached("SELECT id, size, mtime FROM files WHERE path = ?1 AND current = 1")?
            .query_row(params![pb], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?;
        let size = size as i64;
        self.pending += 1;
        match existing {
            Some((id, s, m)) if s == size && m == mtime => {
                self.conn
                    .prepare_cached(
                        "UPDATE files SET last_scan = ?2, root_id = ?3, dev = ?4, ino = ?5 WHERE id = ?1",
                    )?
                    .execute(params![id, self.scan, self.root_id, dev, ino])?;
                return Ok(id);
            }
            Some((id, ..)) => {
                self.conn
                    .prepare_cached("UPDATE files SET current = 0 WHERE id = ?1")?
                    .execute(params![id])?;
                self.st.changed += 1;
            }
            None => self.st.new += 1,
        }
        let dir = paths::to_bytes(parent_of(path));
        self.conn
            .prepare_cached(
                "INSERT INTO files (root_id, path, dir, size, mtime, dev, ino, first_scan, last_scan, current, lname)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, 1, ?9)",
            )?
            .execute(params![
                self.root_id,
                pb,
                dir,
                size,
                mtime,
                dev,
                ino,
                self.scan,
                find::lname(path)
            ])?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Records a folder with its totals, as `file` does files (the root has no parent).
    pub fn dir(
        &mut self,
        path: &Path,
        root: bool,
        t: Totals,
        dev: Option<i64>,
        ino: Option<i64>,
    ) -> Result<i64> {
        let pb = paths::to_bytes(path);
        let (files, bytes) = (t.files as i64, t.bytes as i64);
        let existing: Option<(i64, i64, i64, i64)> = self
            .conn
            .prepare_cached(
                "SELECT id, files, bytes, newest FROM dirs WHERE path = ?1 AND current = 1",
            )?
            .query_row(params![pb], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .optional()?;
        self.pending += 1;
        match existing {
            Some((id, f, b, n)) if (f, b, n) == (files, bytes, t.newest) => {
                self.conn
                    .prepare_cached(
                        "UPDATE dirs SET last_scan = ?2, root_id = ?3, dev = ?4, ino = ?5 WHERE id = ?1",
                    )?
                    .execute(params![id, self.scan, self.root_id, dev, ino])?;
                Ok(id)
            }
            other => {
                if let Some((id, ..)) = other {
                    self.conn
                        .prepare_cached("UPDATE dirs SET current = 0 WHERE id = ?1")?
                        .execute(params![id])?;
                }
                let parent = (!root).then(|| paths::to_bytes(parent_of(path)));
                self.conn
                    .prepare_cached(
                        "INSERT INTO dirs (root_id, path, parent, files, bytes, newest, first_scan, last_scan, current, lname, dev, ino)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 1, ?8, ?9, ?10)",
                    )?
                    .execute(params![
                        self.root_id,
                        pb,
                        parent,
                        files,
                        bytes,
                        t.newest,
                        self.scan,
                        find::lname(path),
                        dev,
                        ino
                    ])?;
                Ok(self.conn.last_insert_rowid())
            }
        }
    }

    /// Commits a batch now and then, and reports progress.
    pub fn tick(&mut self, progress: &mut dyn FnMut(&Stats)) -> Result<()> {
        if self.pending >= BATCH {
            self.conn.execute_batch("COMMIT; BEGIN")?;
            self.pending = 0;
        }
        if self.last.elapsed() >= Duration::from_millis(250) {
            progress(&self.st);
            self.last = Instant::now();
        }
        Ok(())
    }

    pub fn note_error(&mut self, msg: String) {
        note_error(&mut self.st, msg);
    }

    /// After a walk of the whole root: anything of this root it didn't see is gone.
    fn close_unseen(&mut self) -> Result<()> {
        self.st.removed += self.conn.execute(
            "UPDATE files SET current = 0 WHERE root_id = ?1 AND current = 1 AND last_scan < ?2",
            params![self.root_id, self.scan],
        )? as u64;
        self.conn.execute(
            "UPDATE dirs SET current = 0 WHERE root_id = ?1 AND current = 1 AND last_scan < ?2",
            params![self.root_id, self.scan],
        )?;
        Ok(())
    }

    /// Marks the scan finished (it appears on the timeline) with how it was made.
    fn finish(
        mut self,
        method: &'static str,
        note: &str,
        changes: u64,
        saved: &Saved,
        progress: &mut dyn FnMut(&Stats),
    ) -> Result<Stats> {
        self.st.method = method;
        self.st.note = note.to_string();
        self.st.changes = changes;
        let cursor = serde_json::to_string(saved)?;
        self.conn.execute(
            "UPDATE scans SET finished_at = ?2, files = ?3, bytes = ?4, method = ?5, method_note = ?6,
                 changes = ?7, cursor = ?8 WHERE id = ?1",
            params![
                self.scan,
                now(),
                self.st.files as i64,
                self.st.bytes as i64,
                method,
                (!note.is_empty()).then_some(note),
                changes as i64,
                cursor
            ],
        )?;
        self.conn.execute(
            "UPDATE roots SET last_scan = ?2 WHERE id = ?1",
            params![self.root_id, self.scan],
        )?;
        self.conn.execute_batch("COMMIT")?;
        self.open = false;
        progress(&self.st);
        Ok(std::mem::take(&mut self.st))
    }
}

/// What a walk (or the file table) yields, in walk order: each folder before what's in it.
pub(crate) enum Item {
    Dir {
        path: PathBuf,
        depth: usize,
        dev: Option<i64>,
        ino: Option<i64>,
    },
    File {
        path: PathBuf,
        depth: usize,
        size: u64,
        mtime: i64,
        dev: Option<i64>,
        ino: Option<i64>,
        /// how many names (hard links) it has, where known (else 1)
        links: u64,
    },
    Error(String),
}

/// The rows a walk of one folder wrote, so what it no longer holds can be closed, and the files
/// it met that have other names (which may be elsewhere, and changed with them).
#[derive(Default)]
pub(crate) struct Seen {
    pub files: HashSet<i64>,
    pub dirs: HashSet<i64>,
    pub linked: Vec<(Option<i64>, i64, PathBuf)>,
}

struct OpenDir {
    path: PathBuf,
    root: bool,
    totals: Totals,
    dev: Option<i64>,
    ino: Option<i64>,
}

/// Writes what a walk yields; a folder's totals are final when the walk leaves it. `top_is_root`:
/// the first item is the scan's root (a walk of one folder inside it otherwise).
pub(crate) fn write_items(
    w: &mut Writer,
    items: impl Iterator<Item = Item>,
    top_is_root: bool,
    mut seen: Option<&mut Seen>,
    progress: &mut dyn FnMut(&Stats),
) -> Result<()> {
    let mut stack: Vec<OpenDir> = Vec::new();
    for item in items {
        match item {
            Item::Error(e) => w.note_error(e),
            Item::Dir {
                path,
                depth,
                dev,
                ino,
            } => {
                while stack.len() > depth {
                    close_top(w, &mut stack, seen.as_deref_mut())?;
                }
                stack.push(OpenDir {
                    path,
                    root: top_is_root && depth == 0,
                    totals: Totals::default(),
                    dev,
                    ino,
                });
            }
            Item::File {
                path,
                depth,
                size,
                mtime,
                dev,
                ino,
                links,
            } => {
                while stack.len() > depth {
                    close_top(w, &mut stack, seen.as_deref_mut())?;
                }
                let id = w.file(&path, size, mtime, dev, ino)?;
                if let Some(s) = seen.as_deref_mut() {
                    s.files.insert(id);
                    if let (true, Some(ino)) = (links > 1, ino) {
                        s.linked.push((dev, ino, path));
                    }
                }
                if let Some(top) = stack.last_mut() {
                    top.totals.add(Totals {
                        files: 1,
                        bytes: size,
                        newest: mtime,
                    });
                }
                w.st.files += 1;
                w.st.bytes += size;
                w.tick(progress)?;
            }
        }
    }
    while !stack.is_empty() {
        close_top(w, &mut stack, seen.as_deref_mut())?;
    }
    Ok(())
}

/// Pop the innermost open folder, record it, and add its totals to its parent.
fn close_top(w: &mut Writer, stack: &mut Vec<OpenDir>, seen: Option<&mut Seen>) -> Result<()> {
    let d = stack.pop().expect("caller checks the stack is not empty");
    let id = w.dir(&d.path, d.root, d.totals, d.dev, d.ino)?;
    if let Some(s) = seen {
        s.dirs.insert(id);
    }
    if let Some(parent) = stack.last_mut() {
        parent.totals.add(d.totals);
    }
    Ok(())
}

/// Walks `top` (not following symlinks, staying on its filesystem if `one_fs`).
pub(crate) fn walk_items(top: &Path, one_fs: bool) -> impl Iterator<Item = Item> + use<> {
    WalkDir::new(top)
        .follow_links(false)
        .same_file_system(one_fs)
        .into_iter()
        .filter_map(|entry| {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => return Some(Item::Error(e.to_string())),
            };
            let depth = entry.depth();
            if entry.file_type().is_dir() {
                let (dev, ino) = dir_identity(entry.path());
                return Some(Item::Dir {
                    path: entry.into_path(),
                    depth,
                    dev,
                    ino,
                });
            }
            if !entry.file_type().is_file() {
                return None;
            }
            match entry.metadata() {
                Ok(meta) => {
                    let (dev, ino) = inode(&meta);
                    Some(Item::File {
                        path: entry.into_path(),
                        depth,
                        size: meta.len(),
                        mtime: mtime_of(&meta),
                        dev,
                        ino,
                        links: links_of(&meta),
                    })
                }
                Err(e) => Some(Item::Error(e.to_string())),
            }
        })
}

pub(crate) fn mtime_of(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

pub(crate) fn parent_of(p: &Path) -> &Path {
    p.parent().unwrap_or(p)
}

/// One root inside another would give the same files two owners and two timelines.
fn refuse_nested(db: &Db, root: &Path, root_bytes: &[u8]) -> Result<()> {
    let mut stmt = db.conn.prepare("SELECT path FROM roots WHERE path <> ?1")?;
    let others = stmt.query_map(params![root_bytes], |r| r.get::<_, Vec<u8>>(0))?;
    let mine = paths::dir_prefix(root);
    for other in others {
        let other = other?;
        let theirs = paths::dir_prefix(&paths::from_bytes(&other));
        if mine.starts_with(&theirs) || theirs.starts_with(&mine) {
            bail!(
                "{} overlaps the already indexed {}; index each drive or top-level folder once, without nesting",
                root.display(),
                paths::from_bytes(&other).display()
            );
        }
    }
    Ok(())
}

pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn note_error(st: &mut Stats, msg: String) {
    st.errors += 1;
    if st.first_errors.len() < MAX_ERRORS_KEPT {
        st.first_errors.push(msg);
    }
}

#[cfg(unix)]
pub(crate) fn inode(meta: &std::fs::Metadata) -> (Option<i64>, Option<i64>) {
    use std::os::unix::fs::MetadataExt;
    (Some(meta.dev() as i64), Some(meta.ino() as i64))
}

// std doesn't expose a stable file ID on other platforms; hardlinks there are counted as separate files
#[cfg(not(unix))]
pub(crate) fn inode(_: &std::fs::Metadata) -> (Option<i64>, Option<i64>) {
    (None, None)
}

/// How many names a file has (1 where the system doesn't say cheaply).
#[cfg(unix)]
fn links_of(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.nlink()
}

#[cfg(not(unix))]
fn links_of(_: &std::fs::Metadata) -> u64 {
    1
}

/// A folder's identity (device and inode; on Windows the volume serial and file ID), which tells
/// a folder that is still the same from one that took its place.
#[cfg(unix)]
pub(crate) fn dir_identity(path: &Path) -> (Option<i64>, Option<i64>) {
    std::fs::symlink_metadata(path).map_or((None, None), |m| inode(&m))
}

#[cfg(windows)]
pub(crate) fn dir_identity(path: &Path) -> (Option<i64>, Option<i64>) {
    usn::sys::identity(path).map_or((None, None), |(serial, frn)| {
        (Some(i64::from(serial)), Some(frn as i64))
    })
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn dir_identity(_: &Path) -> (Option<i64>, Option<i64>) {
    (None, None)
}

/// The device the root is on (unix only: elsewhere, None).
pub(crate) fn device_of(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| m.dev())
    }
    #[cfg(windows)]
    {
        usn::sys::identity(path).map(|(serial, _)| u64::from(serial))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        None
    }
}
