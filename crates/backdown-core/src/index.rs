//! Walk a directory tree and record every regular file as a new scan. Read-only; symlinks are
//! not followed.
//!
//! Unchanged files (same size and mtime) extend their current row and keep its hashes; a changed
//! file closes its old row and opens a new version; files that disappeared are closed at the end.
//! Rows are committed in batches, so an interrupted run keeps its progress and the next run
//! finishes the job (an unfinished scan never appears on the timeline).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use walkdir::WalkDir;

use crate::{Db, find, paths};

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
}

pub fn index(
    db: &mut Db,
    root: &Path,
    opts: &Options,
    progress: &mut dyn FnMut(&Stats),
) -> Result<Stats> {
    let root = paths::canonical(root).with_context(|| format!("can't open {}", root.display()))?;
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
    conn.execute(
        "INSERT INTO scans(root_id, started_at) VALUES (?1, ?2)",
        params![root_id, now()],
    )?;
    let scan = conn.last_insert_rowid();

    let mut find =
        conn.prepare_cached("SELECT id, size, mtime FROM files WHERE path = ?1 AND current = 1")?;
    let mut extend = conn.prepare_cached(
        "UPDATE files SET last_scan = ?2, root_id = ?3, dev = ?4, ino = ?5 WHERE id = ?1",
    )?;
    let mut close = conn.prepare_cached("UPDATE files SET current = 0 WHERE id = ?1")?;
    let mut open = conn.prepare_cached(
        "INSERT INTO files (root_id, path, dir, size, mtime, dev, ino, first_scan, last_scan, current, lname)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, 1, ?9)",
    )?;
    let mut dirs = DirWriter::new(db, root_id, scan)?;
    let mut streams = crate::streams::Recorder::new(conn, root_id)?;

    let mut st = Stats {
        root: root.clone(),
        scan,
        ..Stats::default()
    };
    // the folders from the root down to the current entry; a folder's totals are final when the walk
    // leaves it (walkdir yields each folder before its contents)
    let mut stack: Vec<OpenDir> = Vec::new();
    let mut last = Instant::now();
    conn.execute_batch("BEGIN")?;
    // opened verbatim on Windows (long paths, names ending in a dot); stored in normal form
    for entry in WalkDir::new(paths::os(&root))
        .follow_links(false)
        .same_file_system(opts.one_file_system)
    {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                note_error(&mut st, e.to_string());
                continue;
            }
        };
        while stack.len() > entry.depth() {
            dirs.close_top(&mut stack)?;
        }
        if entry.file_type().is_dir() {
            stack.push(OpenDir {
                path: paths::to_bytes(entry.path()),
                lname: find::lname(entry.path()),
                parent: (entry.depth() > 0).then(|| paths::to_bytes(parent_of(entry.path()))),
                files: 0,
                bytes: 0,
                newest: 0,
            });
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                note_error(&mut st, e.to_string());
                continue;
            }
        };
        let path = paths::to_bytes(entry.path());
        let size = meta.len() as i64;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);
        let (dev, ino) = inode(&meta);
        let existing: Option<(i64, i64, i64)> = find
            .query_row(params![path], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?;
        let dir = paths::to_bytes(parent_of(entry.path()));
        match existing {
            Some((id, s, m)) if s == size && m == mtime => {
                extend.execute(params![id, scan, root_id, dev, ino])?;
                streams.note(id, entry.path(), false)?;
            }
            Some((id, ..)) => {
                close.execute(params![id])?;
                let name = find::lname(entry.path());
                open.execute(params![
                    root_id, path, dir, size, mtime, dev, ino, scan, name
                ])?;
                streams.note(conn.last_insert_rowid(), entry.path(), true)?;
                st.changed += 1;
            }
            None => {
                let name = find::lname(entry.path());
                open.execute(params![
                    root_id, path, dir, size, mtime, dev, ino, scan, name
                ])?;
                streams.note(conn.last_insert_rowid(), entry.path(), true)?;
                st.new += 1;
            }
        }
        if let Some(top) = stack.last_mut() {
            top.files += 1;
            top.bytes += meta.len();
            top.newest = top.newest.max(mtime);
        }
        st.files += 1;
        st.bytes += meta.len();
        if st.files.is_multiple_of(BATCH) {
            conn.execute_batch("COMMIT; BEGIN")?;
        }
        if last.elapsed() >= Duration::from_millis(250) {
            progress(&st);
            last = Instant::now();
        }
    }
    while !stack.is_empty() {
        dirs.close_top(&mut stack)?;
    }
    streams.finish()?;
    // anything of this root not seen in this scan is gone
    st.removed = conn.execute(
        "UPDATE files SET current = 0 WHERE root_id = ?1 AND current = 1 AND last_scan < ?2",
        params![root_id, scan],
    )? as u64;
    conn.execute(
        "UPDATE dirs SET current = 0 WHERE root_id = ?1 AND current = 1 AND last_scan < ?2",
        params![root_id, scan],
    )?;
    conn.execute(
        "UPDATE scans SET finished_at = ?2, files = ?3, bytes = ?4 WHERE id = ?1",
        params![scan, now(), st.files as i64, st.bytes as i64],
    )?;
    conn.execute(
        "UPDATE roots SET last_scan = ?2 WHERE id = ?1",
        params![root_id, scan],
    )?;
    conn.execute_batch("COMMIT")?;
    progress(&st);
    Ok(st)
}

struct OpenDir {
    path: Vec<u8>,
    lname: String,
    parent: Option<Vec<u8>>,
    files: u64,
    bytes: u64,
    newest: i64,
}

/// Writes finished folders as versions, like files: unchanged totals extend the current row.
struct DirWriter<'c> {
    root_id: i64,
    scan: i64,
    find: rusqlite::CachedStatement<'c>,
    extend: rusqlite::CachedStatement<'c>,
    close: rusqlite::CachedStatement<'c>,
    open: rusqlite::CachedStatement<'c>,
}

impl<'c> DirWriter<'c> {
    fn new(db: &'c Db, root_id: i64, scan: i64) -> Result<Self> {
        let c = &db.conn;
        Ok(DirWriter {
            root_id,
            scan,
            find: c.prepare_cached(
                "SELECT id, files, bytes, newest FROM dirs WHERE path = ?1 AND current = 1",
            )?,
            extend: c.prepare_cached("UPDATE dirs SET last_scan = ?2, root_id = ?3 WHERE id = ?1")?,
            close: c.prepare_cached("UPDATE dirs SET current = 0 WHERE id = ?1")?,
            open: c.prepare_cached(
                "INSERT INTO dirs (root_id, path, parent, files, bytes, newest, first_scan, last_scan, current, lname)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 1, ?8)",
            )?,
        })
    }

    /// Pop the innermost open folder, record it, and add its totals to its parent.
    fn close_top(&mut self, stack: &mut Vec<OpenDir>) -> Result<()> {
        let d = stack.pop().expect("caller checks the stack is not empty");
        let (files, bytes) = (d.files as i64, d.bytes as i64);
        let existing: Option<(i64, i64, i64, i64)> = self
            .find
            .query_row(params![d.path], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .optional()?;
        match existing {
            Some((id, f, b, n)) if (f, b, n) == (files, bytes, d.newest) => {
                self.extend.execute(params![id, self.scan, self.root_id])?;
            }
            other => {
                if let Some((id, ..)) = other {
                    self.close.execute(params![id])?;
                }
                self.open.execute(params![
                    self.root_id,
                    d.path,
                    d.parent,
                    files,
                    bytes,
                    d.newest,
                    self.scan,
                    d.lname
                ])?;
            }
        }
        if let Some(parent) = stack.last_mut() {
            parent.files += d.files;
            parent.bytes += d.bytes;
            parent.newest = parent.newest.max(d.newest);
        }
        Ok(())
    }
}

fn parent_of(p: &Path) -> &Path {
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

fn now() -> i64 {
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
fn inode(meta: &std::fs::Metadata) -> (Option<i64>, Option<i64>) {
    use std::os::unix::fs::MetadataExt;
    (Some(meta.dev() as i64), Some(meta.ino() as i64))
}

// std doesn't expose a stable file ID on other platforms; hardlinks there are counted as separate files
#[cfg(not(unix))]
fn inode(_: &std::fs::Metadata) -> (Option<i64>, Option<i64>) {
    (None, None)
}
