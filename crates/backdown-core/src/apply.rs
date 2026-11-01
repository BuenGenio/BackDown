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
//!
//! Names are kept apart on the archive's drive: it's probed once for what it takes for the same
//! name (case, Unicode form, names Windows can't hold), and a file or folder that would land on
//! another's name gets a numbered one instead (`names::Renamer`); a different file already there
//! under another spelling is copied beside it. Streams and extended attributes go along where the
//! system has them (`extras`). Each rename, and whatever wasn't kept, is in the journal's `note`.
//! Each file copied is also an event in the chain of custody (custody.rs), in the same transaction.

use std::collections::HashSet;
use std::fs::{self, FileTimes};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::clock::now;
use crate::custody::{self, Action};
use crate::names::{self, Folding, Renamer};
use crate::plan::{self, Item};
use crate::{Db, extras, fastcopy, fmt, mailmerge, paths};

const BUF: usize = 1 << 20;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub files: u64,
    pub bytes: u64,
    /// copied and verified in this run
    pub copied: u64,
    pub copied_bytes: u64,
    /// of those, made as clones (sharing the source's blocks: no extra space) or by the kernel
    #[serde(default)]
    pub cloned: u64,
    #[serde(default)]
    pub kernel_copied: u64,
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
    /// mailboxes merged from several snapshots (mailmerge.rs): messages written or found there,
    /// and in a dry run the mailboxes that would be merged
    #[serde(default)]
    pub merged: u64,
    #[serde(default)]
    pub merged_bytes: u64,
    #[serde(default)]
    pub would_merge: u64,
    pub first_problems: Vec<String>,
    /// copied under another name: its own collides with another's on the archive's drive
    #[serde(default)]
    pub renamed: u64,
    /// copied, but without some of its streams or extended attributes
    #[serde(default)]
    pub extras_not_kept: u64,
    /// renames and what copies didn't keep (the first ones; the journal has them all)
    #[serde(default)]
    pub notes: Vec<String>,
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

pub(crate) fn mtime(m: &fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

pub(crate) fn hash_file(path: &Path) -> Result<[u8; 32]> {
    let mut f = paths::open_read(path)?;
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

#[derive(Debug)]
pub enum Outcome {
    Copied([u8; 32]),
    AlreadyThere([u8; 32]),
    Conflict(String),
}

/// Copy one file with its hash, flush, keep its mtime, rename into place, and verify by reading
/// back. The fastest way the filesystems allow (fastcopy.rs: a clone, the kernel's copy, or the
/// plain loop); every way is verified the same.
pub fn copy_verified(src: &Path, dest: &Path, meta: &fs::Metadata) -> Result<Outcome> {
    copy_verified_how(src, dest, meta).map(|(o, _)| o)
}

/// `copy_verified`, also saying how the copy was made (`None` when nothing was copied).
pub fn copy_verified_how(
    src: &Path,
    dest: &Path,
    meta: &fs::Metadata,
) -> Result<(Outcome, Option<fastcopy::Method>)> {
    copy_verified_with(src, dest, meta, &fastcopy::NATIVE)
}

pub(crate) fn copy_verified_with(
    src: &Path,
    dest: &Path,
    meta: &fs::Metadata,
    ops: &fastcopy::Ops,
) -> Result<(Outcome, Option<fastcopy::Method>)> {
    if dest.exists() {
        let (a, b) = (
            hash_file(src).context("reading the source")?,
            hash_file(dest).context("reading the existing copy")?,
        );
        return Ok((
            if a == b {
                Outcome::AlreadyThere(a)
            } else {
                Outcome::Conflict(format!(
                    "{} exists and differs; left as it is",
                    dest.display()
                ))
            },
            None,
        ));
    }
    let parent = dest.parent().context("destination without a folder")?;
    fs::create_dir_all(parent).with_context(|| format!("can't create {}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.backdown-partial",
        dest.file_name().unwrap_or_default().to_string_lossy()
    ));
    let result = (|| -> Result<([u8; 32], fastcopy::Method)> {
        let mut input =
            paths::open_read(src).with_context(|| format!("can't read {}", src.display()))?;
        let (out, how, sum) = fastcopy::copy_into(&mut input, &tmp, ops)
            .with_context(|| format!("can't copy {} to {}", src.display(), tmp.display()))?;
        out.sync_all()?;
        if let Ok(t) = meta.modified() {
            out.set_times(FileTimes::new().set_modified(t))?;
        }
        // the read-back below should see the disk, not the pages just written
        fastcopy::forget_cached(&out);
        drop(out);
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
        Ok((sum, how))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map(|(sum, how)| (Outcome::Copied(sum), Some(how)))
}

/// `copy_verified`, unless a different name at the destination is the same one to its drive (it
/// differs only by case, or is another name the drive takes for it): then that file is compared,
/// and a different one is copied beside it under a numbered name. Where the copy is, and a note.
pub(crate) fn copy_placed(
    src: &Path,
    dest: &Path,
    meta: &fs::Metadata,
    folding: Folding,
    listing: &mut names::Listing,
) -> (
    Result<Outcome>,
    PathBuf,
    Option<String>,
    Option<fastcopy::Method>,
) {
    // only a drive that takes several spellings for one name can show another one here
    let there = if (folding.case || folding.normalization) && fs::symlink_metadata(dest).is_ok() {
        listing.variant_there(dest)
    } else {
        None
    };
    // the copy, and which fast path it took (clone, the kernel's copy, or the plain loop)
    let how_of = |r: Result<(Outcome, Option<fastcopy::Method>)>| match r {
        Ok((o, how)) => (Ok(o), how),
        Err(e) => (Err(e), None),
    };
    let (first, how) = how_of(copy_verified_how(src, dest, meta));
    let Some(there) = there else {
        return (first, dest.to_path_buf(), None, how);
    };
    let shown = names::nfc(&there.to_string_lossy()).into_owned();
    match first {
        Ok(Outcome::Conflict(_)) => {
            let alt = names::free_name(dest);
            let note = format!(
                "copied as {}: {shown} is already there, and this drive takes the two names for one",
                names::nfc(&alt.file_name().unwrap_or_default().to_string_lossy())
            );
            let (r, how) = how_of(copy_verified_how(src, &alt, meta));
            (r, alt, Some(note), how)
        }
        Ok(Outcome::AlreadyThere(h)) => (
            Ok(Outcome::AlreadyThere(h)),
            dest.with_file_name(&there),
            Some(format!("already there as {shown}")),
            how,
        ),
        other => (other, dest.to_path_buf(), None, how),
    }
}

/// Content already in the archive (for copy-unique items): hashes of indexed files under the
/// archive, plus whatever this plan has copied so far.
fn archive_hashes(db: &Db, target: &Path, plan_id: i64) -> Result<HashSet<Vec<u8>>> {
    let prefix = paths::dir_prefix(&names::as_indexed(db, target)?);
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
        for src in std::iter::once(&it.source).chain(&it.merge) {
            if !Path::new(src).exists() {
                bail!("{src} isn't there: connect the drive {} first", it.drive);
            }
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
    // what the archive's drive takes for one name (a dry run writes nothing, not even the probe,
    // so it doesn't know and renames nothing)
    let folding = if dry_run {
        Folding::EXACT
    } else {
        fs::create_dir_all(paths::os(&target))
            .with_context(|| format!("can't create {}", target.display()))?;
        names::probe_or_assume(&target)
    };
    let mut last_tick = Instant::now();
    for (n, (idx, it)) in todo.iter().enumerate() {
        // opened verbatim on Windows (long paths, names ending in a dot); journaled in normal form
        let src_root = paths::os(Path::new(&it.source)).into_owned();
        let dest_root = paths::os(&target.join(&it.dest)).into_owned();
        if it.action == "merge" {
            let items = todo.len();
            merge_item(
                db,
                plan_id,
                *idx,
                it,
                &dest_root,
                dry_run,
                &mut st,
                &mut |f, b| {
                    progress(&Progress {
                        item: n + 1,
                        items,
                        what: format!("{} (merging)", it.what),
                        files: f,
                        bytes: b,
                        total_bytes,
                    })
                },
            )?;
            continue;
        }
        let excluded: Vec<PathBuf> = it
            .exclude
            .iter()
            .map(|x| paths::os(Path::new(x)).into_owned())
            .collect();
        let mut renamer = Renamer::new(&src_root, folding);
        let mut listing = names::Listing::default();
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
            let (dest, renamed) = if it.is_file {
                (dest_root.clone(), Vec::new())
            } else {
                let (rel, renamed) = renamer.map(src.strip_prefix(&src_root).unwrap_or(src));
                (dest_root.join(rel), renamed)
            };
            let mut notes: Vec<String> = Vec::new();
            if !renamed.is_empty() {
                st.renamed += 1;
                notes.push(format!("renamed {}", names::describe(&renamed)));
            }
            if dry_run {
                if dest.exists() {
                    st.conflicts += 1;
                    if st.first_problems.len() < 20 {
                        st.first_problems.push(format!(
                            "{} already exists (will be compared, never overwritten)",
                            paths::show(&dest)
                        ));
                    }
                } else {
                    st.would_copy += 1;
                    st.would_copy_bytes += size;
                }
                continue;
            }
            let (outcome, dest, placed, how) =
                copy_placed(src, &dest, &meta, folding, &mut listing);
            notes.extend(placed);
            let (state, sha, error) = match outcome {
                Ok(Outcome::Copied(h)) => {
                    st.copied += 1;
                    st.copied_bytes += size;
                    match how {
                        Some(fastcopy::Method::Clone) => st.cloned += 1,
                        Some(fastcopy::Method::Kernel) => st.kernel_copied += 1,
                        _ => {}
                    }
                    let carried = extras::carry(src, &dest, &meta);
                    if carried.lost_any() {
                        st.extras_not_kept += 1;
                    }
                    notes.extend(carried.note());
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
            let note = (!notes.is_empty()).then(|| notes.join("; "));
            if let Some(n) = &note
                && st.notes.len() < 20
            {
                st.notes.push(format!("{}: {n}", paths::show(src)));
            }
            let dest_b = paths::to_bytes(&dest);
            tx.execute(
                "INSERT INTO journal(plan_id, item, src, dest, size, mtime, sha256, state, error, at, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![plan_id, *idx as i64, src_b, dest_b, size as i64, mt, sha, state, error, now(), note],
            )?;
            // the chain of custody, in the same transaction: only what was copied now
            if state == "copied" {
                let ev = custody::Event::new(
                    Action::Copy,
                    ("plan", plan_id),
                    &src_b,
                    sha.as_deref(),
                    size,
                );
                custody::append(&tx, &ev.to(&dest_b, sha.as_deref()))?;
            }
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

/// A "merge" item: its snapshots merged into one mailbox at `dest` (mailmerge.rs, which journals
/// every message it writes and carries on where an earlier run stopped). The plan's journal gets
/// one row for the mailbox, state "merged" (or "failed"), with the merge's summary.
#[allow(clippy::too_many_arguments)]
fn merge_item(
    db: &Db,
    plan_id: i64,
    idx: usize,
    it: &Item,
    dest: &Path,
    dry_run: bool,
    st: &mut Stats,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    if dry_run {
        // reading every message would take as long as merging: that's what `mail merge
        // --dry-run` is for
        st.would_merge += 1;
        return Ok(());
    }
    let sources: Vec<PathBuf> = std::iter::once(&it.source)
        .chain(&it.merge)
        .map(PathBuf::from)
        .collect();
    let id = match mailmerge::for_plan_item(db, plan_id, idx as i64)? {
        Some(id) => id,
        None => mailmerge::record(db, &sources, dest, Some((plan_id, idx as i64)))?,
    };
    let stop = std::sync::atomic::AtomicBool::new(false);
    let (state, error, bytes) = match mailmerge::run(db, id, &stop, &mut |_, n, b| progress(n, b)) {
        Ok(r) => {
            st.merged += r.written + r.resumed;
            st.merged_bytes += r.written_bytes;
            st.failed += r.failed + r.unreadable;
            for p in &r.problems {
                if st.first_problems.len() < 20 {
                    st.first_problems.push(p.clone());
                }
            }
            (
                if r.failed + r.unreadable == 0 {
                    "merged"
                } else {
                    "failed"
                },
                mailmerge::summary(&r),
                r.bytes,
            )
        }
        Err(e) => {
            st.failed += 1;
            let msg = format!("merging {}: {e:#}", it.what);
            if st.first_problems.len() < 20 {
                st.first_problems.push(msg.clone());
            }
            ("failed", msg, 0)
        }
    };
    db.conn.execute(
        "DELETE FROM journal WHERE plan_id = ?1 AND item = ?2 AND state IN ('merged', 'failed') AND src = ?3",
        params![plan_id, idx as i64, paths::to_bytes(Path::new(&it.source))],
    )?;
    db.conn.execute(
        "INSERT INTO journal(plan_id, item, src, dest, size, mtime, sha256, state, error, at) VALUES (?1, ?2, ?3, ?4, ?5, 0, NULL, ?6, ?7, ?8)",
        params![
            plan_id,
            idx as i64,
            paths::to_bytes(Path::new(&it.source)),
            paths::to_bytes(dest),
            bytes as i64,
            state,
            error,
            now()
        ],
    )?;
    Ok(())
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

/// A plan's journal notes (renamed files, what copies didn't keep): source, destination, note.
pub fn journal_notes(db: &Db, plan_id: i64, limit: usize) -> Result<Vec<(String, String, String)>> {
    let mut stmt = db.conn.prepare(
        "SELECT src, dest, note FROM journal WHERE plan_id = ?1 AND note IS NOT NULL ORDER BY id LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![plan_id, limit as i64], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, Option<Vec<u8>>>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (src, dest, note) = row?;
        out.push((
            paths::show(&paths::from_bytes(&src)),
            dest.map(|d| paths::show(&paths::from_bytes(&d)))
                .unwrap_or_default(),
            note,
        ));
    }
    Ok(out)
}

/// Plans on record, newest first: (id, created_at, target, state).
pub fn plans(db: &Db) -> Result<Vec<(i64, i64, String, String)>> {
    let mut stmt = db
        .conn
        .prepare("SELECT id, created_at, target, state FROM plans ORDER BY id DESC")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fastcopy::{Method, NATIVE, Ops};
    use std::fs::File;
    use std::io;

    /// a "clone" that quietly gets one byte wrong (a filesystem bug, a bad block)
    fn bad_clone(src: &File, dest: &Path) -> io::Result<File> {
        let mut bytes = Vec::new();
        src.try_clone()?.read_to_end(&mut bytes)?;
        if let Some(b) = bytes.first_mut() {
            *b ^= 0xFF;
        }
        fs::write(dest, &bytes)?;
        File::open(dest)
    }
    fn good_clone(src: &File, dest: &Path) -> io::Result<File> {
        let mut bytes = Vec::new();
        src.try_clone()?.read_to_end(&mut bytes)?;
        fs::write(dest, &bytes)?;
        File::open(dest)
    }

    #[test]
    fn a_clone_is_verified_like_any_copy() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("photo.jpg");
        fs::write(&src, vec![7u8; 300_000]).unwrap();
        let meta = fs::metadata(&src).unwrap();
        let dest = t.path().join("archive/photo.jpg");
        let bad = Ops {
            clone: bad_clone,
            kernel: NATIVE.kernel,
        };
        let e = copy_verified_with(&src, &dest, &meta, &bad).unwrap_err();
        assert!(format!("{e:#}").contains("read back differently"), "{e:#}");
        assert!(!dest.exists(), "the bad copy is removed");
        assert_eq!(
            fs::read_dir(t.path().join("archive")).unwrap().count(),
            0,
            "and no partial file is left"
        );
        let good = Ops {
            clone: good_clone,
            kernel: NATIVE.kernel,
        };
        let (o, how) = copy_verified_with(&src, &dest, &meta, &good).unwrap();
        assert!(matches!(o, Outcome::Copied(_)));
        assert_eq!(how, Some(Method::Clone));
        assert_eq!(fs::read(&dest).unwrap(), fs::read(&src).unwrap());
        assert_eq!(
            fs::metadata(&dest).unwrap().modified().unwrap(),
            meta.modified().unwrap(),
            "the source's modification time is kept"
        );
        // copying again finds it there, and says nothing was copied
        let (o, how) = copy_verified_how(&src, &dest, &meta).unwrap();
        assert!(matches!(o, Outcome::AlreadyThere(_)) && how.is_none());
    }
}
