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
//!
//! A root is known by its path only, so a scan of whatever is at that path would record every
//! file of the drive that isn't there as deleted. A scan is refused, recording nothing
//! (`Refused`), where that is far likelier than a drive really emptied: the root's folder is on
//! another drive than the one its last scan was on (the folder a drive is mounted on, or one made
//! there while the drive is mounted elsewhere), it's empty, or holds no files at all, though its
//! last scan found some, or it can't be listed. A folder below it that can't be read (a drive that
//! stopped answering, no permission) keeps what the index had of it, rather than having it
//! recorded as gone.

use std::cell::OnceCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::clock::now;
use crate::{Db, find, paths, swarm};

mod changes;
pub mod fsevents;
mod journal;
pub mod table;
pub mod usn;

pub use changes::ChangeSet;

const BATCH: u64 = 20_000;
const MAX_ERRORS_KEPT: usize = 20;

/// The rows `Writer::keep_unread` keeps (?1 the root, ?2 the scan, ?3..?4 a range of paths): found
/// by path alone. The unary + keeps SQLite off the root's index, which would have it go through
/// every row the walk hasn't reached yet, for each folder that couldn't be read.
const KEPT: &str = "current = 1 AND path >= ?3 AND path < ?4 AND +root_id = ?1 AND +last_scan < ?2";

// ---------- stopping a scan part way

thread_local! {
    /// set by `stop_when` for the scans this thread makes; checked at every file
    static STOP: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>> =
        const { std::cell::RefCell::new(None) };
}

/// A scan was asked to stop: what it read so far is committed (like an interrupted scan, the
/// next one carries on from there) and it stays unfinished, so it never shows on the timeline.
#[derive(Debug)]
pub struct Stopped;

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the scan was stopped")
    }
}

impl std::error::Error for Stopped {}

/// Until the guard is dropped, scans made on this thread stop at their next file once `flag` is
/// set, with the error `Stopped`.
pub fn stop_when(flag: &std::sync::Arc<std::sync::atomic::AtomicBool>) -> StopGuard {
    STOP.with(|s| *s.borrow_mut() = Some(std::sync::Arc::clone(flag)));
    StopGuard(())
}

pub struct StopGuard(());

impl Drop for StopGuard {
    fn drop(&mut self) {
        STOP.with(|s| *s.borrow_mut() = None);
    }
}

fn stop_asked() -> bool {
    STOP.with(|s| {
        s.borrow()
            .as_ref()
            .is_some_and(|f| f.load(std::sync::atomic::Ordering::Relaxed))
    })
}

pub struct Options {
    /// Stay on the filesystem the root is on (don't descend into other mounts).
    pub one_file_system: bool,
    /// Record the root even if it is now empty, or holds no files at all, although its last scan
    /// found files. Without this, such a root is refused: it is far more likely an unplugged
    /// drive's leftover mount point than a drive that really was wiped, and recording it would
    /// mark every file deleted.
    pub allow_empty: bool,
    /// Record the root even if its folder is now on another drive than the one its last scan was
    /// on (a new or reformatted drive mounted in its place). Without this, that's refused: it's
    /// far more likely the folder a drive is mounted on, or one made there while the drive is
    /// mounted elsewhere, and recording it would mark every file deleted.
    pub allow_other_drive: bool,
}

/// Why a scan was refused (see `Refused`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// the root's folder is on another drive than the one its last scan was on
    OtherDrive,
    /// the root is empty, or holds no files at all, though its last scan found files
    Empty,
    /// the root's own folder can't be listed
    Unreadable,
    /// the drive went away while it was being read (unplugged, unmounted, a USB reset): what the
    /// scan hadn't reached isn't gone, only unread, so nothing is marked gone
    Vanished,
}

/// A scan refused because what it would record is very likely wrong: it would mark files deleted
/// that are only somewhere else (an unplugged drive's mount point, a folder made where a drive is
/// mounted, a drive that stopped answering). Nothing was recorded; the message says what's wrong
/// and what to do.
#[derive(Debug)]
pub struct Refused {
    pub why: Refusal,
    pub message: String,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refused {}

fn refused(why: Refusal, message: String) -> anyhow::Error {
    Refused { why, message }.into()
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
    /// Files in folders that couldn't be read, kept as the previous scan had them (counted in
    /// `files` and `bytes`) rather than recorded as gone.
    pub kept: u64,
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
    pub files: Option<i64>,
}

impl How {
    /// For people: "rescanned from the change log in 2 s: 412 changes", "read the file table:
    /// 1.20 M files in 9 s", "full walk in 3 min: the change log was reset".
    pub fn phrase(&self) -> String {
        describe(
            &self.method,
            self.note.as_deref().unwrap_or(""),
            self.changes.unwrap_or(0) as u64,
            self.files.unwrap_or(0) as u64,
            self.took_ms.map(|t| t as u64),
        )
    }
}

impl Stats {
    /// How this scan was made, for people (see `How::phrase`).
    pub fn how(&self) -> String {
        describe(
            self.method,
            &self.note,
            self.changes,
            self.files,
            Some(self.took_ms),
        )
    }
}

fn describe(method: &str, note: &str, changes: u64, files: u64, took_ms: Option<u64>) -> String {
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
            "read the file table: {} files{took}",
            crate::fmt::count(files)
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
        "SELECT id, COALESCE(method, 'walk'), method_note, changes, took_ms, files FROM scans WHERE id = ?1",
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
                    files: r.get(5)?,
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
    let Prepared { root, root_id, had } = prepare(db, root, opts)?;
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
            Ok(t) => {
                match from_table(db, &root, root_id, had, opts, t, &saved, started, progress) {
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
        if method == Method::Table {
            bail!(
                "can't read the file table of {}: {}",
                root.display(),
                notes.join("; ")
            );
        }
    }
    let note = walk_why.unwrap_or_else(|| notes.join("; "));
    let mut st = walk_scan(db, &root, root_id, had, opts, &note, &saved, progress)?;
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
    let Prepared { root, root_id, .. } = prepare(db, root, opts)?;
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

/// A root ready to scan: canonical, checked and on record.
struct Prepared {
    root: PathBuf,
    root_id: i64,
    /// the files its last scan found
    had: i64,
}

/// The root, canonical, checked and on record; refused (`Refused`) where scanning it now would
/// record its files as deleted though they very likely aren't.
fn prepare(db: &mut Db, root: &Path, opts: &Options) -> Result<Prepared> {
    let root = paths::canonical(root).with_context(|| format!("can't open {}", root.display()))?;
    if !paths::os(&root).is_dir() {
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
    if crate::forget::is_marked(db, root_id)? {
        bail!(
            "{} is being removed from the index; it can be added again once that's done",
            root.display()
        );
    }
    let had: i64 = conn.query_row(
        "SELECT COALESCE((SELECT files FROM scans WHERE id = r.last_scan), 0) FROM roots r WHERE r.id = ?1",
        params![root_id],
        |r| r.get(0),
    )?;
    // a folder that can't be listed (a drive that stopped answering, no permission) says nothing
    // about what's on it
    let empty = match std::fs::read_dir(paths::os(&root)) {
        Ok(mut it) => it.next().is_none(),
        Err(e) => {
            return Err(refused(
                Refusal::Unreadable,
                format!(
                    "can't list {}: {e}. Is the drive still connected? Nothing was recorded",
                    root.display()
                ),
            ));
        }
    };
    if !opts.allow_empty && had > 0 && empty {
        return Err(refused(
            Refusal::Empty,
            format!(
                "{} is empty, but held {} files at its last scan. Is the drive unplugged? \
                 Nothing was recorded; if it really was emptied, index again with --allow-empty",
                root.display(),
                crate::fmt::count(had as u64)
            ),
        ));
    }
    if !opts.allow_other_drive
        && let Some(why) = wrong_drive(db, root_id, &root, &OnceCell::new())?
    {
        return Err(refused(Refusal::OtherDrive, why));
    }
    Ok(Prepared { root, root_id, had })
}

/// Why the indexed drive `root_id`, at `root`, mustn't be scanned now: its folder is on another
/// drive than the one its last scan was on, so it's very likely the folder a drive is mounted on,
/// or one made there, with the drive itself mounted elsewhere or not at all. None when it's the
/// same drive, when none was recorded (`swarm::remember_drive`), or when the drive it's on now
/// can't be told (a network share, a probe that failed). `here` is probed only if a drive was
/// recorded, and once for however many drives are asked about with it.
pub fn wrong_drive(
    db: &Db,
    root_id: i64,
    root: &Path,
    here: &OnceCell<Option<swarm::Here>>,
) -> Result<Option<String>> {
    let Some(was) = swarm::recorded_drive(db, root_id)? else {
        return Ok(None);
    };
    let Some(here) = here.get_or_init(swarm::Here::probe) else {
        return Ok(None);
    };
    let now = here.drive(root);
    if swarm::judge(&was, &now) != swarm::OnDrive::Other {
        return Ok(None);
    }
    let at = now["volume"]["mount"]
        .as_str()
        .map(|m| format!(" (mounted at {m})"))
        .unwrap_or_default();
    Ok(Some(format!(
        "{root} isn't on the drive it was last scanned on: that was {was}, and {root} is now on \
         {now}{at}. It's very likely the folder the drive is mounted on, or one made in its place, \
         with the drive mounted elsewhere or not at all. Nothing was recorded. Mount the drive at \
         {root} and index again; if this really is its drive now (a new or reformatted one), index \
         again with --allow-other-drive, or `backdown forget {root}` and index it afresh",
        root = root.display(),
        was = swarm::drive_name(&was),
        now = swarm::drive_name(&now),
    )))
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
    let now_cursor = match now_cursor {
        Ok(Some(c)) => c,
        Ok(None) if cfg!(target_os = "linux") => {
            return Err("Linux keeps no change log for drives".into());
        }
        Ok(None) => return Err("this drive keeps no change log BackDown can read".into()),
        Err(why) => return Err(why.clone()),
    };
    let prev = prev.ok_or("the drive's first scan")?;
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
        .ok_or("the previous scan didn't note where the change log was")?;
    let changes = journal::changes_since(db, root, root_id, from, now_cursor)?.within(root);
    if let Some(why) = &changes.full {
        return Err(why.clone());
    }
    Ok(changes)
}

/// A walk of the whole root (`had`: the files its last scan found).
#[allow(clippy::too_many_arguments)]
fn walk_scan(
    db: &mut Db,
    root: &Path,
    root_id: i64,
    had: i64,
    opts: &Options,
    note: &str,
    saved: &Saved,
    progress: &mut dyn FnMut(&Stats),
) -> Result<Stats> {
    let mut w = Writer::begin(&db.conn, root, root_id)?;
    let items = walk_items(root, opts.one_file_system);
    write_items(&mut w, items, true, None, progress)?;
    w.close_unseen(had, opts.allow_empty)?;
    w.finish("walk", note, 0, saved, progress)
}

/// A scan from the file table: its entries written as a walk would write them.
#[allow(clippy::too_many_arguments)]
fn from_table(
    db: &mut Db,
    root: &Path,
    root_id: i64,
    had: i64,
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
    w.close_unseen(had, opts.allow_empty)?;
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
    /// a batch was committed: rows of this scan are in the index, finished or not
    committed: bool,
    /// notes the alternate streams of files as they're recorded (Windows: streams.rs)
    streams: crate::streams::Recorder<'c>,
    /// every file of the root was looked at (a walk or the file table, not the change log): only
    /// then does the streams recorder count the drive as looked at whole
    whole: bool,
    /// the device the root was on when the scan began: still the same before anything is closed
    dev: Option<u64>,
}

impl Drop for Writer<'_> {
    /// A scan that fails part way keeps what earlier batches committed (the next scan carries
    /// on from there) and drops the batch it was in, so the connection is left usable. One that
    /// fails (or is refused) before any batch was committed leaves no trace at all.
    fn drop(&mut self) {
        if self.open {
            let _ = self.conn.execute_batch("ROLLBACK");
            if !self.committed {
                let _ = self.conn.execute(
                    "DELETE FROM scans WHERE id = ?1 AND finished_at IS NULL",
                    params![self.scan],
                );
            }
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
            committed: false,
            streams: crate::streams::Recorder::new(conn, root_id)?,
            whole: false,
            dev: device_of(root),
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
                self.streams.note(id, path, false)?;
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
        let id = self.conn.last_insert_rowid();
        self.streams.note(id, path, true)?;
        Ok(id)
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

    /// Commits a batch now and then, and reports progress. Asked to stop (`stop_when`), it
    /// commits what it has and returns `Stopped`.
    pub fn tick(&mut self, progress: &mut dyn FnMut(&Stats)) -> Result<()> {
        if self.pending >= BATCH {
            self.conn.execute_batch("COMMIT")?;
            // a scan runs for as long as the drive takes to walk, so its own batch commits are
            // the only chance to reclaim the WAL along the way (crate::db::checkpoint)
            crate::db::checkpoint(self.conn);
            self.conn.execute_batch("BEGIN")?;
            self.pending = 0;
            self.committed = true;
        }
        if stop_asked() {
            self.conn.execute_batch("COMMIT")?;
            self.open = false;
            self.pending = 0;
            crate::db::checkpoint(self.conn);
            return Err(Stopped.into());
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

    /// What the index has at or below `p` that this scan hasn't seen, kept as it was rather than
    /// recorded as gone, because `p` (a folder, or a file) couldn't be read. Their ids go in
    /// `seen` (a walk of one folder, which closes by id what it didn't see). Returns the totals of
    /// the files kept, for the folder above.
    fn keep_unread(&mut self, p: &Path, mut seen: Option<&mut Seen>) -> Result<Totals> {
        // `p` itself (from it to it followed by a NUL there's only it), and what's below it
        let own = paths::to_bytes(p);
        let mut past = own.clone();
        past.push(0);
        let mut t = Totals::default();
        for (lo, hi) in [(own, past), changes::below(p)] {
            let args = params![self.root_id, self.scan, lo, hi];
            let (files, bytes, newest): (i64, i64, i64) = self
                .conn
                .prepare_cached(&format!(
                    "SELECT COUNT(*), COALESCE(SUM(size), 0), COALESCE(MAX(mtime), 0) FROM files WHERE {KEPT}"
                ))?
                .query_row(args, |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            t.add(Totals {
                files: files as u64,
                bytes: bytes as u64,
                newest,
            });
            if let Some(s) = seen.as_deref_mut() {
                for (table, into) in [("files", &mut s.files), ("dirs", &mut s.dirs)] {
                    let mut stmt = self
                        .conn
                        .prepare_cached(&format!("SELECT id FROM {table} WHERE {KEPT}"))?;
                    for id in stmt.query_map(args, |r| r.get::<_, i64>(0))? {
                        into.insert(id?);
                    }
                }
            }
            for table in ["files", "dirs"] {
                self.pending += self
                    .conn
                    .prepare_cached(&format!("UPDATE {table} SET last_scan = ?2 WHERE {KEPT}"))?
                    .execute(args)? as u64;
            }
        }
        self.st.kept += t.files;
        self.st.files += t.files;
        self.st.bytes += t.bytes;
        Ok(t)
    }

    /// After a walk of the whole root: anything of this root it didn't see is gone. Unless it
    /// found no files at all where the last scan found `had`, and that isn't allowed: then the
    /// scan is refused (a drive's mount point with a few empty folders in it, or folders made in
    /// its place, is far likelier than a drive emptied by hand).
    fn close_unseen(&mut self, had: i64, allow_empty: bool) -> Result<()> {
        if self.st.files == 0 && had > 0 && !allow_empty {
            return Err(refused(
                Refusal::Empty,
                format!(
                    "{} holds no files at all, but held {} files at its last scan. Is the drive \
                     mounted? What's there may be folders made where it's mounted. Nothing was \
                     recorded; if it really was emptied, index again with --allow-empty",
                    self.st.root.display(),
                    crate::fmt::count(had as u64)
                ),
            ));
        }
        // the drive went away part way (unplugged, unmounted, a USB reset): the walk saw only
        // NotFound from then on, so what it didn't reach was never read, not deleted. Nothing is
        // closed: the batches committed so far stay (the next scan carries on from them), and the
        // drive keeps its index until it's back and read whole
        if !self.st.root.is_dir() || device_of(&self.st.root) != self.dev {
            return Err(refused(
                Refusal::Vanished,
                format!(
                    "{} went away while it was being read (unplugged or unmounted?): nothing was \
                     marked gone, and it's scanned again when it's back",
                    self.st.root.display()
                ),
            ));
        }
        self.whole = true;
        // not one statement inside the scan's transaction: with 1.6 M files gone, that wrote a
        // WAL bigger than the index (a transaction can't be checkpointed while it runs). A batch
        // at a time instead, each committed with the WAL reclaimed, like `tick`'s batches: what's
        // closed is what the walk really didn't see, and a scan that fails part way is carried on
        // from by the next, as after any batch
        self.conn.execute_batch("COMMIT")?;
        self.open = false;
        self.committed = true;
        self.st.removed += close_rows(self.conn, "files", self.root_id, self.scan)?;
        close_rows(self.conn, "dirs", self.root_id, self.scan)?;
        self.conn.execute_batch("BEGIN")?;
        self.open = true;
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
        if self.whole {
            self.streams.finish()?;
        }
        self.conn.execute_batch("COMMIT")?;
        self.open = false;
        progress(&self.st);
        Ok(std::mem::take(&mut self.st))
    }
}

/// Closes the rows of `table` under a root that scan `scan` didn't see, `BATCH` at a time: each
/// batch its own transaction (none may be open), the WAL reclaimed after it. How many it closed.
fn close_rows(conn: &Connection, table: &str, root_id: i64, scan: i64) -> Result<u64> {
    let sql = format!(
        "UPDATE {table} SET current = 0 WHERE id IN (SELECT id FROM {table}
           WHERE root_id = ?1 AND current = 1 AND last_scan < ?2 LIMIT {BATCH})"
    );
    let mut total = 0;
    loop {
        let n = conn.execute(&sql, params![root_id, scan])? as u64;
        if n == 0 {
            return Ok(total);
        }
        total += n;
        crate::db::checkpoint(conn);
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
    /// A folder that couldn't be listed (whole, or past some point), or a file that couldn't be
    /// looked at: an I/O error on a drive that stopped answering, no permission. What the index
    /// has at or below it that the scan hasn't seen is kept, not recorded as gone; the root
    /// itself (depth 0) failing fails the scan.
    Unreadable {
        path: PathBuf,
        depth: usize,
        msg: String,
    },
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
            Item::Unreadable { path, depth, msg } => {
                if depth == 0 && top_is_root {
                    return Err(refused(
                        Refusal::Unreadable,
                        format!(
                            "can't list {}: {msg}. Is the drive still connected? Nothing was recorded",
                            w.st.root.display()
                        ),
                    ));
                }
                w.note_error(msg);
                // what's kept counts in the folder it's in, or in itself if it's the folder open
                while stack.len() > depth + 1
                    || (stack.len() == depth + 1 && stack.last().is_some_and(|d| d.path != path))
                {
                    close_top(w, &mut stack, seen.as_deref_mut())?;
                }
                let kept = w.keep_unread(&path, seen.as_deref_mut())?;
                if let Some(top) = stack.last_mut() {
                    top.totals.add(kept);
                }
                w.tick(progress)?;
            }
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

/// Walks `top` (not following symlinks, staying on its filesystem if `one_fs`). Opened verbatim on
/// Windows (long paths, names ending in a dot); the paths it yields are in normal form, as stored.
pub(crate) fn walk_items(top: &Path, one_fs: bool) -> impl Iterator<Item = Item> + use<> {
    // the folder last met at each depth: the one being listed, when a listing breaks off part way
    // (that error doesn't say where)
    let mut open: Vec<PathBuf> = Vec::new();
    WalkDir::new(paths::os(top))
        .follow_links(false)
        .same_file_system(one_fs)
        .into_iter()
        .filter_map(move |entry| {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => return Some(unreadable(e, &open)),
            };
            let depth = entry.depth();
            if entry.file_type().is_dir() {
                let (dev, ino) = dir_identity(entry.path());
                let path = paths::normal(entry.path()).into_owned();
                open.truncate(depth);
                open.push(path.clone());
                return Some(Item::Dir {
                    path,
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
                        path: paths::normal(entry.path()).into_owned(),
                        depth,
                        size: meta.len(),
                        mtime: mtime_of(&meta),
                        dev,
                        ino,
                        links: links_of(&meta),
                    })
                }
                Err(e) => Some(unreadable(e, &open)),
            }
        })
}

/// A walk's error as an item: what couldn't be read (`open`: the folder at each depth, for a
/// listing that broke off, whose error names no path), or a plain error for what was deleted
/// while the walk went by (that is gone).
fn unreadable(e: walkdir::Error, open: &[PathBuf]) -> Item {
    let msg = e.to_string();
    if e.io_error()
        .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
    {
        return Item::Error(msg);
    }
    let at = match e.path() {
        Some(p) => Some((paths::normal(p).into_owned(), e.depth())),
        // a listing at this depth broke off: its folder is the one above
        None => e
            .depth()
            .checked_sub(1)
            .and_then(|d| Some((open.get(d)?.clone(), d))),
    };
    match at {
        Some((path, depth)) => Item::Unreadable { path, depth, msg },
        None => Item::Error(msg),
    }
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
    // whatever the Unicode form of their names (a Mac drive's folder typed on another system)
    let mine = crate::names::nfc_path(&paths::dir_prefix(root)).into_owned();
    for other in others {
        let other = other?;
        let theirs = paths::dir_prefix(&paths::from_bytes(&other));
        let theirs = crate::names::nfc_path(&theirs);
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

/// The device the root is on (on Windows, its volume's serial; elsewhere, None). It changes when
/// something is mounted there, and can when a drive is plugged in again.
pub fn device_of(path: &Path) -> Option<u64> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn how_scans_were_made_for_people() {
        assert_eq!(took(300), "under a second");
        assert_eq!(took(2_400), "2 s");
        assert_eq!(took(180_000), "3 min");
        assert_eq!(took(184_000), "3 min 4 s");
        assert_eq!(took(3_720_000), "1 h 2 min");
        let st = |method, note: &str, changes, files| Stats {
            method,
            note: note.into(),
            changes,
            files,
            took_ms: 2_000,
            ..Stats::default()
        };
        assert_eq!(
            st("journal", "", 412, 9).how(),
            "rescanned from the change log in 2 s: 412 changes"
        );
        assert_eq!(
            st("table", "", 5, 1_200_000).how(),
            "read the file table: 1.20 M files in 2 s"
        );
        assert_eq!(
            st("walk", "the change log was reset", 0, 9).how(),
            "full walk in 2 s: the change log was reset"
        );
        assert!(
            "walk journal table"
                .split(' ')
                .all(|m| m.parse::<Method>().is_ok())
        );
        assert!("everything".parse::<Method>().is_err());
    }

    /// What's kept below a folder that couldn't be read is found by path, not by going through
    /// the root's rows (millions, on a big drive) once for each such folder.
    #[test]
    fn what_is_kept_is_found_by_path() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("idx.db")).unwrap();
        for table in ["files", "dirs"] {
            let plan: Vec<String> = db
                .conn
                .prepare(&format!(
                    "EXPLAIN QUERY PLAN SELECT id FROM {table} WHERE {KEPT}"
                ))
                .unwrap()
                .query_map(params![1, 2, b"/a".to_vec(), b"/b".to_vec()], |r| {
                    r.get::<_, String>(3)
                })
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            let plan = plan.join("; ");
            assert!(
                plan.contains("INDEX") && plan.contains("(path>? AND path<?)"),
                "{table}: {plan}"
            );
            assert!(!plan.contains("_root"), "{table}: {plan}");
        }
    }

    /// The drive is unplugged part way through a walk: the walk sees NotFound from then on, so
    /// what it didn't reach was never read. Nothing is marked gone, what was read stays, and the
    /// next scan once the drive is back finds everything where it was.
    #[test]
    fn a_drive_unplugged_during_a_walk_keeps_its_index() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        for f in ["a", "b", "c/d", "c/e"] {
            std::fs::create_dir_all(root.join(f).parent().unwrap()).unwrap();
            std::fs::write(root.join(f), f.as_bytes()).unwrap();
        }
        let root = paths::canonical(&root).unwrap();
        let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
        let opts = Options {
            one_file_system: true,
            allow_empty: false,
            allow_other_drive: false,
        };
        assert_eq!(index(&mut db, &root, &opts, &mut |_| {}).unwrap().files, 4);
        let root_id = crate::forget::root_id(&db, &root).unwrap().unwrap();
        let current = |db: &Db| -> i64 {
            db.conn
                .query_row("SELECT COUNT(*) FROM files WHERE current = 1", [], |r| {
                    r.get(0)
                })
                .unwrap()
        };

        // the walk reads `a`, then the drive goes (its folder disappears, as /Volumes/X does)
        let items = vec![
            Item::Dir {
                path: root.clone(),
                depth: 0,
                dev: None,
                ino: None,
            },
            Item::File {
                path: root.join("a"),
                depth: 1,
                size: 1,
                mtime: mtime_of(&std::fs::metadata(root.join("a")).unwrap()),
                dev: None,
                ino: None,
                links: 1,
            },
            Item::Error("IO error for operation on b: No such file or directory".into()),
        ];
        let mut w = Writer::begin(&db.conn, &root, root_id).unwrap();
        write_items(&mut w, items.into_iter(), true, None, &mut |_| {}).unwrap();
        let away = tmp.path().join("unplugged");
        std::fs::rename(&root, &away).unwrap();
        let err = w.close_unseen(4, false).unwrap_err();
        assert_eq!(
            err.downcast_ref::<Refused>().map(|r| r.why),
            Some(Refusal::Vanished),
            "{err:#}"
        );
        drop(w);
        assert_eq!(current(&db), 4, "nothing marked gone");

        // plugged back in: scanned whole, nothing new or gone
        std::fs::rename(&away, &root).unwrap();
        let st = index(&mut db, &root, &opts, &mut |_| {}).unwrap();
        assert_eq!((st.files, st.new, st.removed), (4, 0, 0));
        assert_eq!(current(&db), 4);
    }

    /// A folder whose listing breaks off part way (an I/O error on a drive that stopped
    /// answering), and a file that can't be looked at: what the walk didn't get to is kept, the
    /// folders' totals still count it, and only what really went is closed.
    #[test]
    fn what_a_broken_listing_didnt_get_to_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        for f in ["d/x1", "d/x2", "d/sub/x3", "y", "z"] {
            std::fs::create_dir_all(root.join(f).parent().unwrap()).unwrap();
            std::fs::write(root.join(f), f.as_bytes()).unwrap();
        }
        let root = paths::canonical(&root).unwrap();
        let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
        let opts = Options {
            one_file_system: true,
            allow_empty: false,
            allow_other_drive: false,
        };
        let first = index(&mut db, &root, &opts, &mut |_| {}).unwrap();
        assert_eq!(first.files, 5);
        let root_id = crate::forget::root_id(&db, &root).unwrap().unwrap();

        // the second walk: d's listing breaks off after x1, y can't be looked at, z is gone
        let file = |p: &str, depth| Item::File {
            path: root.join(p),
            depth,
            size: p.len() as u64,
            mtime: mtime_of(&std::fs::metadata(root.join(p)).unwrap()),
            dev: None,
            ino: None,
            links: 1,
        };
        let dir = |p: &str, depth| Item::Dir {
            path: if p.is_empty() {
                root.clone()
            } else {
                root.join(p)
            },
            depth,
            dev: None,
            ino: None,
        };
        let items = vec![
            dir("", 0),
            dir("d", 1),
            file("d/x1", 2),
            Item::Unreadable {
                path: root.join("d"),
                depth: 1,
                msg: "d: Input/output error".into(),
            },
            Item::Unreadable {
                path: root.join("y"),
                depth: 1,
                msg: "y: Input/output error".into(),
            },
        ];
        let mut w = Writer::begin(&db.conn, &root, root_id).unwrap();
        let mut seen = Seen::default();
        write_items(
            &mut w,
            items.into_iter(),
            true,
            Some(&mut seen),
            &mut |_| {},
        )
        .unwrap();
        w.close_unseen(first.files as i64, false).unwrap();
        let st = w
            .finish("walk", "", 0, &Saved::default(), &mut |_| {})
            .unwrap();
        assert_eq!((st.files, st.kept, st.removed, st.errors), (4, 3, 1, 2));
        assert_eq!(
            seen.files.len(),
            4,
            "kept files count as seen, in a walk of one folder"
        );
        let current: Vec<String> = db
            .conn
            .prepare("SELECT path FROM files WHERE current = 1 AND last_scan = ?1 ORDER BY path")
            .unwrap()
            .query_map([st.scan], |r| r.get::<_, Vec<u8>>(0))
            .unwrap()
            .map(|p| {
                let p = paths::from_bytes(&p.unwrap());
                p.strip_prefix(&root).unwrap().display().to_string()
            })
            .collect();
        assert_eq!(current, ["d/sub/x3", "d/x1", "d/x2", "y"]);
        let totals = |p: &Path| -> (i64, i64) {
            db.conn
                .query_row(
                    "SELECT files, last_scan FROM dirs WHERE path = ?1 AND current = 1",
                    [paths::to_bytes(p)],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap()
        };
        assert_eq!(totals(&root.join("d")), (3, st.scan));
        assert_eq!(totals(&root.join("d/sub")), (1, st.scan));
        assert_eq!(totals(&root), (4, st.scan));

        // the root itself breaking off fails the scan, and leaves no trace
        let scans = |db: &Db| -> i64 {
            db.conn
                .query_row("SELECT COUNT(*) FROM scans", [], |r| r.get(0))
                .unwrap()
        };
        let before = scans(&db);
        let mut w = Writer::begin(&db.conn, &root, root_id).unwrap();
        let items = vec![
            dir("", 0),
            Item::Unreadable {
                path: root.clone(),
                depth: 0,
                msg: "Input/output error".into(),
            },
        ];
        let err = write_items(&mut w, items.into_iter(), true, None, &mut |_| {}).unwrap_err();
        drop(w);
        assert_eq!(
            err.downcast_ref::<Refused>().map(|r| r.why),
            Some(Refusal::Unreadable)
        );
        assert_eq!(scans(&db), before);
        assert!(db.conn.is_autocommit());
    }

    /// Closing what a scan didn't see on a root that lost most of its files (a drive emptied, a
    /// big folder moved away) once ran as one statement over the whole root, inside the scan's
    /// transaction: closing 1.6 M files that way wrote a WAL bigger than the index itself and
    /// filled the disk (8 Nov 2026). It goes a batch at a time now, the WAL reclaimed between.
    #[test]
    fn closing_a_root_that_lost_its_files_keeps_the_wal_small() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("idx.db");
        let db = Db::open(&path).unwrap();
        // a real folder: a scan's root has to be there still when what it didn't see is closed
        let root = &tmp.path().join("drive");
        std::fs::create_dir(root).unwrap();
        db.conn
            .execute(
                "INSERT INTO roots(path) VALUES (?1)",
                [paths::to_bytes(root)],
            )
            .unwrap();
        let root_id = db.conn.last_insert_rowid();
        db.conn
            .execute(
                "INSERT INTO scans(root_id, started_at, finished_at, files) VALUES (?1, 1, 2, 200000)",
                [root_id],
            )
            .unwrap();
        let first = db.conn.last_insert_rowid();
        // 200,000 files as a finished scan leaves them: hashed, with their inodes, so every
        // partial index on current rows has an entry to lose
        db.conn.execute_batch("BEGIN").unwrap();
        {
            let mut put = db
                .conn
                .prepare(
                    "INSERT INTO files (root_id, path, dir, size, mtime, dev, ino, sample, sha256, first_scan, last_scan, current, lname)
                     VALUES (?1, ?2, ?3, ?4, 1, 1, ?5, ?6, ?7, ?8, ?8, 1, ?9)",
                )
                .unwrap();
            for i in 0..200_000i64 {
                let dir = format!("/drive/d{}", i % 1000);
                let name = format!("f{i}");
                let mut sha = [0u8; 32];
                sha[..8].copy_from_slice(&i.to_be_bytes());
                put.execute(params![
                    root_id,
                    format!("{dir}/{name}").as_bytes(),
                    dir.as_bytes(),
                    i,
                    i,
                    &sha[..16],
                    &sha[..],
                    first,
                    name
                ])
                .unwrap();
            }
        }
        db.conn.execute_batch("COMMIT").unwrap();
        crate::db::checkpoint(&db.conn);
        let wal = || {
            std::fs::metadata(path.with_extension("db-wal"))
                .map(|m| m.len())
                .unwrap_or(0)
        };
        assert_eq!(wal(), 0, "starts with an empty WAL");

        // a scan that finds nothing there
        let mut w = Writer::begin(&db.conn, root, root_id).unwrap();
        w.close_unseen(200_000, true).unwrap();
        let st = w
            .finish("walk", "", 0, &Saved::default(), &mut |_| {})
            .unwrap();
        assert_eq!(st.removed, 200_000);
        let open: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM files WHERE current = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(open, 0);
        assert!(db.conn.is_autocommit());
        let after = wal();
        assert!(
            after < 2 << 20,
            "closing 200,000 files left a {after}-byte WAL: it should go a batch at a time, \
             reclaimed between batches"
        );
    }
}
