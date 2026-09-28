//! Applying a plan: journaled, verified copies into the archive (APP-PLAN §8 "Safety model").
//!
//! Every file is copied to a temporary name next to its destination while its SHA-256 is computed
//! from the bytes read, flushed to disk, given the source's modification time, renamed into place,
//! then read back and hashed again: only a match counts as verified. Each file gets a journal row
//! (source, destination, size, mtime, hash, outcome), so an interrupted run picks up where it
//! stopped and the journal alone says what happened.
//!
//! It never writes to a source, never overwrites an existing file that differs (that's recorded as
//! a conflict and left for a person), and never deletes anything: once the archive is indexed,
//! `check` proves which source drives are safe to wipe.

use std::collections::HashSet;
use std::fs::{self, File, FileTimes};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::plan::{self, Item};
use crate::{Db, fmt, paths};

const BUF: usize = 1 << 20;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub files: u64,
    pub bytes: u64,
    /// copied and verified in this run
    pub copied: u64,
    pub copied_bytes: u64,
    /// verified by an earlier run (resumed)
    pub resumed: u64,
    /// the same file was already at the destination
    pub already_there: u64,
    /// content already in the archive (copy-unique items)
    pub duplicates: u64,
    /// a different file is at the destination: left alone
    pub conflicts: u64,
    pub failed: u64,
    pub skipped_links: u64,
    /// dry run: files that would be copied, and a destination that exists
    pub would_copy: u64,
    pub would_copy_bytes: u64,
    pub first_problems: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Progress {
    pub item: usize,
    pub items: usize,
    pub what: String,
    pub files: u64,
    pub bytes: u64,
    pub total_bytes: u64,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

pub(crate) fn mtime(m: &fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

pub(crate) fn hash_file(path: &Path) -> Result<[u8; 32]> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; BUF];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().into())
}

pub(crate) enum Outcome {
    Copied([u8; 32]),
    AlreadyThere([u8; 32]),
    Conflict(String),
}

/// Copy one file with its hash, flush, keep its mtime, rename into place, and verify by reading back.
pub(crate) fn copy_verified(src: &Path, dest: &Path, meta: &fs::Metadata) -> Result<Outcome> {
    if dest.exists() {
        let (a, b) = (
            hash_file(src).context("reading the source")?,
            hash_file(dest).context("reading the existing copy")?,
        );
        return Ok(if a == b {
            Outcome::AlreadyThere(a)
        } else {
            Outcome::Conflict(format!(
                "{} exists and differs; left as it is",
                dest.display()
            ))
        });
    }
    let parent = dest.parent().context("destination without a folder")?;
    fs::create_dir_all(parent).with_context(|| format!("can't create {}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.backdown-partial",
        dest.file_name().unwrap_or_default().to_string_lossy()
    ));
    let result = (|| -> Result<[u8; 32]> {
        let mut input = File::open(src).with_context(|| format!("can't read {}", src.display()))?;
        let mut out =
            File::create(&tmp).with_context(|| format!("can't write {}", tmp.display()))?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; BUF];
        loop {
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            out.write_all(&buf[..n])?;
        }
        out.sync_all()?;
        if let Ok(t) = meta.modified() {
            out.set_times(FileTimes::new().set_modified(t))?;
        }
        drop(out);
        let sum: [u8; 32] = h.finalize().into();
        if dest.exists() {
            bail!("{} appeared while copying", dest.display());
        }
        fs::rename(&tmp, dest)?;
        // read back what the disk now holds
        let back = hash_file(dest).context("reading the copy back")?;
        if back != sum {
            let _ = fs::remove_file(dest); // our own bad copy, never a user's file
            bail!(
                "the copy of {} read back differently: removed; try again (failing disk or cable?)",
                src.display()
            );
        }
        Ok(sum)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map(Outcome::Copied)
}

/// Content already in the archive (for copy-unique items): hashes of indexed files under the
/// archive, plus whatever this plan has copied so far.
fn archive_hashes(db: &Db, target: &Path, plan_id: i64) -> Result<HashSet<Vec<u8>>> {
    let prefix = paths::dir_prefix(target);
    let mut hi = prefix.clone();
    if let Some(l) = hi.last_mut() {
        *l += 1;
    }
    let mut set = HashSet::new();
    let mut a = db.conn.prepare("SELECT sha256 FROM files WHERE current = 1 AND sha256 IS NOT NULL AND path >= ?1 AND path < ?2")?;
    for h in a.query_map(params![prefix, hi], |r| r.get::<_, Vec<u8>>(0))? {
        set.insert(h?);
    }
    let mut b = db.conn.prepare("SELECT sha256 FROM journal WHERE plan_id = ?1 AND sha256 IS NOT NULL AND state IN ('copied', 'already-there')")?;
    for h in b.query_map(params![plan_id], |r| r.get::<_, Vec<u8>>(0))? {
        set.insert(h?);
    }
    Ok(set)
}

pub(crate) fn free_space(path: &Path) -> Option<u64> {
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    fs4::available_space(p).ok()
}

/// Run plan `plan_id` (or, with `dry_run`, only count what it would do).
pub fn apply(
    db: &mut Db,
    plan_id: i64,
    dry_run: bool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<Stats> {
    let (plan, _) = plan::load(db, plan_id)?;
    let target = PathBuf::from(&plan.target);
    let todo: Vec<(usize, &Item)> = plan
        .items
        .iter()
        .enumerate()
        .filter(|(_, i)| i.action != "skip")
        .collect();
    let total_bytes: u64 = todo.iter().map(|(_, i)| i.bytes).sum();
    for (_, it) in &todo {
        if !Path::new(&it.source).exists() {
            bail!(
                "{} isn't there: connect the drive {} first",
                it.source,
                it.drive
            );
        }
    }
    if let Some(free) = free_space(&target) {
        let resumed: i64 = db.conn.query_row(
            "SELECT COALESCE(SUM(size), 0) FROM journal WHERE plan_id = ?1 AND state IN ('copied', 'already-there')",
            params![plan_id],
            |r| r.get(0),
        )?;
        let need = total_bytes.saturating_sub(resumed as u64);
        if need > free {
            bail!(
                "the archive's drive has {} free, and this plan needs {} more: free some space or file fewer things",
                fmt::bytes(free),
                fmt::bytes(need)
            );
        }
    }
    if !dry_run {
        db.conn.execute(
            "UPDATE plans SET state = 'running' WHERE id = ?1",
            params![plan_id],
        )?;
    }
    let mut st = Stats::default();
    let mut unique_seen = if todo.iter().any(|(_, i)| i.action == "copy-unique") {
        Some(archive_hashes(db, &target, plan_id)?)
    } else {
        None
    };
    let mut last_tick = Instant::now();
    for (n, (idx, it)) in todo.iter().enumerate() {
        let src_root = PathBuf::from(&it.source);
        let dest_root = target.join(&it.dest);
        let excluded: Vec<PathBuf> = it.exclude.iter().map(PathBuf::from).collect();
        let mut walker = WalkDir::new(&src_root).follow_links(false);
        if it.only_files {
            walker = walker.max_depth(1);
        }
        let tx = db.conn.unchecked_transaction()?;
        let mut batch = 0;
        for entry in walker
            .into_iter()
            .filter_entry(|e| !excluded.iter().any(|x| e.path() == x))
        {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    st.failed += 1;
                    if st.first_problems.len() < 20 {
                        st.first_problems.push(format!("can't read: {e}"));
                    }
                    continue;
                }
            };
            let ft = entry.file_type();
            if ft.is_symlink() {
                st.skipped_links += 1;
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let src = entry.path();
            let dest = if it.is_file {
                dest_root.clone()
            } else {
                dest_root.join(src.strip_prefix(&src_root).unwrap_or(src))
            };
            let meta = entry.metadata()?;
            let (size, mt) = (meta.len(), mtime(&meta));
            st.files += 1;
            st.bytes += size;
            if last_tick.elapsed().as_millis() >= 250 {
                last_tick = Instant::now();
                progress(&Progress {
                    item: n + 1,
                    items: todo.len(),
                    what: it.what.clone(),
                    files: st.files,
                    bytes: st.bytes,
                    total_bytes,
                });
            }
            let src_b = paths::to_bytes(src);
            // resume: verified by an earlier run, and unchanged since
            let done: Option<String> = tx
                .query_row(
                    "SELECT state FROM journal WHERE plan_id = ?1 AND src = ?2 AND size = ?3 AND mtime = ?4 AND state IN ('copied', 'already-there', 'duplicate') ORDER BY id DESC LIMIT 1",
                    params![plan_id, src_b, size as i64, mt],
                    |r| r.get(0),
                )
                .optional()?;
            if done.is_some() {
                st.resumed += 1;
                continue;
            }
            // copy-unique: content that's already in the archive stays where it is
            if let Some(seen) = unique_seen.as_mut().filter(|_| it.action == "copy-unique") {
                let sha: Option<Vec<u8>> = tx
                    .query_row(
                        "SELECT sha256 FROM files WHERE path = ?1 AND current = 1",
                        params![src_b],
                        |r| r.get(0),
                    )
                    .optional()?
                    .flatten();
                if let Some(sha) = sha
                    && seen.contains(&sha)
                {
                    st.duplicates += 1;
                    if !dry_run {
                        tx.execute(
                            "INSERT INTO journal(plan_id, item, src, dest, size, mtime, sha256, state, at) VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, 'duplicate', ?7)",
                            params![plan_id, *idx as i64, src_b, size as i64, mt, sha, now()],
                        )?;
                    }
                    continue;
                }
            }
            if dry_run {
                if dest.exists() {
                    st.conflicts += 1;
                    if st.first_problems.len() < 20 {
                        st.first_problems.push(format!(
                            "{} already exists (will be compared, never overwritten)",
                            dest.display()
                        ));
                    }
                } else {
                    st.would_copy += 1;
                    st.would_copy_bytes += size;
                }
                continue;
            }
            let (state, sha, error) = match copy_verified(src, &dest, &meta) {
                Ok(Outcome::Copied(h)) => {
                    st.copied += 1;
                    st.copied_bytes += size;
                    ("copied", Some(h.to_vec()), None)
                }
                Ok(Outcome::AlreadyThere(h)) => {
                    st.already_there += 1;
                    ("already-there", Some(h.to_vec()), None)
                }
                Ok(Outcome::Conflict(msg)) => {
                    st.conflicts += 1;
                    if st.first_problems.len() < 20 {
                        st.first_problems.push(msg.clone());
                    }
                    ("conflict", None, Some(msg))
                }
                Err(e) => {
                    st.failed += 1;
                    let msg = format!("{e:#}");
                    if st.first_problems.len() < 20 {
                        st.first_problems.push(msg.clone());
                    }
                    ("failed", None, Some(msg))
                }
            };
            if let (Some(h), Some(seen)) = (&sha, unique_seen.as_mut()) {
                seen.insert(h.clone());
            }
            tx.execute(
                "INSERT INTO journal(plan_id, item, src, dest, size, mtime, sha256, state, error, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![plan_id, *idx as i64, src_b, paths::to_bytes(&dest), size as i64, mt, sha, state, error, now()],
            )?;
            batch += 1;
            if batch >= 500 {
                // keep what's done even if the next file hangs on a dying disk
                tx.execute_batch("COMMIT; BEGIN")?;
                batch = 0;
            }
        }
        tx.commit()?;
    }
    if !dry_run {
        let state = if st.failed == 0 && st.conflicts == 0 {
            "done"
        } else {
            "partial"
        };
        db.conn.execute(
            "UPDATE plans SET state = ?2 WHERE id = ?1",
            params![plan_id, state],
        )?;
    }
    Ok(st)
}

/// The journal's summary for a plan: files by outcome.
pub fn journal_summary(db: &Db, plan_id: i64) -> Result<Vec<(String, u64, u64)>> {
    let mut stmt = db.conn.prepare("SELECT state, COUNT(*), COALESCE(SUM(size), 0) FROM journal WHERE plan_id = ?1 GROUP BY state ORDER BY state")?;
    let rows = stmt.query_map(params![plan_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)? as u64,
            r.get::<_, i64>(2)? as u64,
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Plans on record, newest first: (id, created_at, target, state).
pub fn plans(db: &Db) -> Result<Vec<(i64, i64, String, String)>> {
    let mut stmt = db
        .conn
        .prepare("SELECT id, created_at, target, state FROM plans ORDER BY id DESC")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
