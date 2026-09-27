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

use crate::{Db, paths};

const BATCH: u64 = 20_000;
const MAX_ERRORS_KEPT: usize = 20;

pub struct Options {
    /// Stay on the filesystem the root is on (don't descend into other mounts).
    pub one_file_system: bool,
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
        "INSERT INTO files (root_id, path, size, mtime, dev, ino, first_scan, last_scan, current)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 1)",
    )?;

    let mut st = Stats {
        root: root.clone(),
        scan,
        ..Stats::default()
    };
    let mut last = Instant::now();
    conn.execute_batch("BEGIN")?;
    for entry in WalkDir::new(&root)
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
        match existing {
            Some((id, s, m)) if s == size && m == mtime => {
                extend.execute(params![id, scan, root_id, dev, ino])?;
            }
            Some((id, ..)) => {
                close.execute(params![id])?;
                open.execute(params![root_id, path, size, mtime, dev, ino, scan])?;
                st.changed += 1;
            }
            None => {
                open.execute(params![root_id, path, size, mtime, dev, ino, scan])?;
                st.new += 1;
            }
        }
        st.files += 1;
        st.bytes += meta.len();
        if st.files % BATCH == 0 {
            conn.execute_batch("COMMIT; BEGIN")?;
        }
        if last.elapsed() >= Duration::from_millis(250) {
            progress(&st);
            last = Instant::now();
        }
    }
    // anything of this root not seen in this scan is gone
    st.removed = conn.execute(
        "UPDATE files SET current = 0 WHERE root_id = ?1 AND current = 1 AND last_scan < ?2",
        params![root_id, scan],
    )? as u64;
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
