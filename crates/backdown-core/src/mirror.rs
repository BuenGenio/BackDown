//! Mirrors: standing rules that keep a folder (usually part of the archive) copied on another
//! drive, and check now and then that the copy still reads back right (APP-PLAN §3 "Mirror").
//!
//! A sync copies what's new or changed, each file verified by reading it back (as `apply` does).
//! It never deletes: a file that changed in the source has its previous copy on the mirror moved
//! into `.backdown-versions/` rather than overwritten, so a corrupted or encrypted source can't
//! silently destroy the good copy; files only on the mirror are reported, not removed.
//! A verify re-reads a sample of the mirror (or all of it) and compares hashes with the source,
//! which is how a dying mirror drive gets noticed before it's needed. Each copy, and each old copy
//! set aside, is an event in the chain of custody (custody.rs).
//!
//! Names that the mirror's drive takes for one ("Photo.jpg" and "photo.jpg" on a case-insensitive
//! drive) are kept apart the way `apply` keeps them (`names::Renamer`: the second gets a numbered
//! name, the same one every time), and streams and extended attributes go along (`extras`).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::apply::{Outcome, copy_verified, free_space, hash_file, mtime};
use crate::custody::{self, Action, Event};
use crate::names::{self, Renamer};
use crate::{Db, extras, fmt, paths};

pub const VERSIONS: &str = ".backdown-versions";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mirror {
    pub id: i64,
    pub source: String,
    pub target: String,
    pub created_at: i64,
    pub verify_every_days: i64,
    pub last_sync: Option<i64>,
    pub last_sync_summary: Option<String>,
    pub last_verify: Option<i64>,
    pub last_verify_summary: Option<String>,
    /// the last verify found every sampled file intact
    pub last_verify_ok: Option<bool>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SyncStats {
    pub files: u64,
    pub bytes: u64,
    pub copied: u64,
    pub copied_bytes: u64,
    pub updated: u64,
    pub unchanged: u64,
    pub failed: u64,
    /// on the mirror but not in the source (kept)
    pub extra: u64,
    pub skipped_links: u64,
    pub problems: Vec<String>,
    /// copied under another name: its own collides with another's on the mirror's drive
    #[serde(default)]
    pub renamed: u64,
    /// copied, but without some of its streams or extended attributes
    #[serde(default)]
    pub extras_not_kept: u64,
    /// renames and what copies didn't keep (the first ones)
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct VerifyStats {
    pub checked: u64,
    pub checked_bytes: u64,
    pub ok: u64,
    pub missing: u64,
    pub differs: u64,
    pub unreadable: u64,
    pub problems: Vec<String>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Mirror> {
    Ok(Mirror {
        id: r.get(0)?,
        source: r.get(1)?,
        target: r.get(2)?,
        created_at: r.get(3)?,
        verify_every_days: r.get(4)?,
        last_sync: r.get(5)?,
        last_sync_summary: r.get(6)?,
        last_verify: r.get(7)?,
        last_verify_summary: r.get(8)?,
        last_verify_ok: r.get(9)?,
    })
}

const COLS: &str = "id, source, target, created_at, verify_every_days, last_sync, last_sync_summary, last_verify, last_verify_summary, last_verify_ok";

pub fn add(db: &Db, source: &Path, target: &Path, verify_every_days: i64) -> Result<i64> {
    let source =
        paths::canonical(source).with_context(|| format!("can't open {}", source.display()))?;
    if !source.is_dir() {
        bail!("{} isn't a folder", source.display());
    }
    if !target.is_absolute() {
        bail!("the mirror folder must be a full path");
    }
    let target = if target.exists() {
        paths::canonical(target)?
    } else {
        target.to_path_buf()
    };
    if target.starts_with(&source) || source.starts_with(&target) {
        bail!("a mirror can't be inside what it mirrors, or the other way round");
    }
    db.conn.execute(
        "INSERT INTO mirrors(source, target, created_at, verify_every_days) VALUES (?1, ?2, ?3, ?4)",
        params![source.display().to_string(), target.display().to_string(), now(), verify_every_days.max(1)],
    )?;
    Ok(db.conn.last_insert_rowid())
}

pub fn list(db: &Db) -> Result<Vec<Mirror>> {
    let mut stmt = db
        .conn
        .prepare(&format!("SELECT {COLS} FROM mirrors ORDER BY id"))?;
    let rows = stmt.query_map([], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(db: &Db, id: i64) -> Result<Mirror> {
    db.conn
        .query_row(
            &format!("SELECT {COLS} FROM mirrors WHERE id = ?1"),
            params![id],
            row,
        )
        .optional()?
        .with_context(|| format!("there's no mirror {id}"))
}

/// Stop keeping a mirror (the copies on the mirror drive stay).
pub fn remove(db: &Db, id: i64) -> Result<()> {
    get(db, id)?;
    db.conn
        .execute("DELETE FROM mirrors WHERE id = ?1", params![id])?;
    Ok(())
}

/// Mirrors whose verify is due and whose drives are both connected.
pub fn due(db: &Db) -> Result<Vec<i64>> {
    let t = now();
    Ok(list(db)?
        .into_iter()
        .filter(|m| {
            m.last_verify
                .is_none_or(|v| t - v >= m.verify_every_days * 86_400)
        })
        .filter(|m| Path::new(&m.source).is_dir() && Path::new(&m.target).is_dir())
        .map(|m| m.id)
        .collect())
}

fn present(m: &Mirror) -> Result<(PathBuf, PathBuf)> {
    let (s, t) = (PathBuf::from(&m.source), PathBuf::from(&m.target));
    if !s.is_dir() {
        bail!("{} isn't there: connect its drive", s.display());
    }
    Ok((s, t))
}

/// Copy what's new or changed; keep old versions; report what's only on the mirror.
pub fn sync(db: &Db, id: i64, progress: &mut dyn FnMut(u64, u64)) -> Result<SyncStats> {
    let m = get(db, id)?;
    let (src, dst) = present(&m)?;
    fs::create_dir_all(paths::os(&dst))
        .with_context(|| format!("can't create {}", dst.display()))?;
    let folding = names::probe_or_assume(&dst);
    // opened verbatim on Windows (long paths, names ending in a dot)
    let (src, dst) = (paths::os(&src).into_owned(), paths::os(&dst).into_owned());
    let mut renamer = Renamer::new(&src, folding);
    let mut st = SyncStats::default();
    // what's been copied to, as the mirror's drive tells names apart
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let stamp = now();
    for entry in WalkDir::new(&src).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                st.failed += 1;
                push(&mut st.problems, format!("can't read: {e}"));
                continue;
            }
        };
        if entry.file_type().is_symlink() {
            st.skipped_links += 1;
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let src_rel = entry.path().strip_prefix(&src).unwrap_or(entry.path());
        let (rel, renamed) = renamer.map(src_rel);
        let dest = dst.join(&rel);
        seen.insert(folding.path_key(&rel));
        let meta = entry.metadata()?;
        st.files += 1;
        st.bytes += meta.len();
        if st.files.is_multiple_of(200) {
            progress(st.files, st.bytes);
        }
        if let Ok(d) = fs::metadata(&dest)
            && d.len() == meta.len()
            && mtime(&d) == mtime(&meta)
        {
            st.unchanged += 1;
            continue;
        }
        // changed in the source: the mirror's previous copy moves aside, it's never overwritten
        if dest.exists() {
            let old = dst.join(VERSIONS).join(format!("{stamp}")).join(&rel);
            if let Some(p) = old.parent() {
                fs::create_dir_all(p)?;
            }
            let old_size = fs::metadata(&dest).map_or(0, |m| m.len());
            if let Err(e) = fs::rename(&dest, &old) {
                st.failed += 1;
                push(
                    &mut st.problems,
                    format!("can't set aside the old copy of {}: {e}", rel.display()),
                );
                continue;
            }
            st.updated += 1;
            // the chain of custody: the old copy moved aside (renamed, not read)
            let ev = Event::new(
                Action::Move,
                ("mirror", id),
                &paths::to_bytes(&dest),
                None,
                old_size,
            );
            custody::append(&db.conn, &ev.to(&paths::to_bytes(&old), None))?;
        }
        if !renamed.is_empty() {
            st.renamed += 1;
            push(
                &mut st.notes,
                format!(
                    "{}: renamed {} on the mirror's drive",
                    paths::show(src_rel),
                    names::describe(&renamed)
                ),
            );
        }
        match copy_verified(entry.path(), &dest, &meta) {
            Ok(Outcome::Copied(h)) => {
                st.copied += 1;
                st.copied_bytes += meta.len();
                let (src_b, dest_b) = (paths::to_bytes(entry.path()), paths::to_bytes(&dest));
                let ev = Event::new(
                    Action::Copy,
                    ("mirror", id),
                    &src_b,
                    Some(&h[..]),
                    meta.len(),
                );
                custody::append(&db.conn, &ev.to(&dest_b, Some(&h[..])))?;
                let carried = extras::carry(entry.path(), &dest, &meta);
                if carried.lost_any() {
                    st.extras_not_kept += 1;
                }
                if let Some(n) = carried.note() {
                    push(&mut st.notes, format!("{}: {n}", paths::show(src_rel)));
                }
            }
            Ok(Outcome::AlreadyThere(_)) => {
                st.copied += 1;
                st.copied_bytes += meta.len();
            }
            Ok(Outcome::Conflict(msg)) => {
                st.failed += 1;
                push(&mut st.problems, msg);
            }
            Err(e) => {
                st.failed += 1;
                push(&mut st.problems, format!("{e:#}"));
            }
        }
    }
    // only on the mirror (deleted from the source since, or put there by hand): kept, and counted
    for entry in WalkDir::new(&dst)
        .into_iter()
        .filter_entry(|e| e.file_name() != VERSIONS)
        .filter_map(Result::ok)
    {
        if entry.file_type().is_file()
            && let Ok(rel) = entry.path().strip_prefix(&dst)
            && !seen.contains(&folding.path_key(rel))
            && !rel
                .file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".backdown-partial"))
        {
            st.extra += 1;
        }
    }
    let summary = format!(
        "{} files: {} copied ({}), {} updated (old copies kept), {} unchanged{}{}",
        fmt::count(st.files),
        fmt::count(st.copied),
        fmt::bytes(st.copied_bytes),
        fmt::count(st.updated),
        fmt::count(st.unchanged),
        if st.extra > 0 {
            format!(", {} only on the mirror", fmt::count(st.extra))
        } else {
            String::new()
        },
        if st.failed > 0 {
            format!(", {} failed", fmt::count(st.failed))
        } else {
            String::new()
        }
    );
    db.conn.execute(
        "UPDATE mirrors SET last_sync = ?2, last_sync_summary = ?3 WHERE id = ?1",
        params![id, now(), summary],
    )?;
    Ok(st)
}

fn push(v: &mut Vec<String>, s: String) {
    if v.len() < 20 {
        v.push(s);
    }
}

/// Is this file part of today's sample? A different share of the files each day, so over time
/// everything gets re-read.
fn sampled(rel: &Path, percent: u8, day: i64) -> bool {
    if percent >= 100 {
        return true;
    }
    let h = Sha256::digest(paths::to_bytes(rel));
    let n = u16::from_le_bytes([h[0], h[1]]) as i64;
    (n + day * 37).rem_euclid(100) < percent as i64
}

/// Re-read `percent` of the mirror's files (100: all) and compare each with its source.
pub fn verify(
    db: &Db,
    id: i64,
    percent: u8,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<VerifyStats> {
    let m = get(db, id)?;
    let (src, dst) = present(&m)?;
    if !dst.is_dir() {
        bail!(
            "the mirror {} isn't there: connect its drive",
            dst.display()
        );
    }
    // the same names `sync` gave: it depends on what the mirror's drive tells apart
    let mut renamer = Renamer::new(&src, names::probe_or_assume(&dst));
    let mut st = VerifyStats::default();
    let day = now() / 86_400;
    let mut known = db.conn.prepare_cached(
        "SELECT sha256 FROM files WHERE path = ?1 AND current = 1 AND size = ?2 AND mtime = ?3",
    )?;
    for entry in WalkDir::new(&src)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(&src)
            .unwrap_or(entry.path())
            .to_path_buf();
        if !sampled(&rel, percent, day) {
            continue;
        }
        let meta = entry.metadata()?;
        let dest = dst.join(renamer.map(&rel).0);
        st.checked += 1;
        st.checked_bytes += meta.len();
        if st.checked.is_multiple_of(100) {
            progress(st.checked, st.checked_bytes);
        }
        let Ok(dm) = fs::metadata(&dest) else {
            st.missing += 1;
            push(
                &mut st.problems,
                format!("missing on the mirror: {}", rel.display()),
            );
            continue;
        };
        if dm.len() != meta.len() {
            st.differs += 1;
            push(
                &mut st.problems,
                format!("different size on the mirror: {}", rel.display()),
            );
            continue;
        }
        // the source's hash from the index when it's current, otherwise read it
        let expected: Option<Vec<u8>> = known
            .query_row(
                params![
                    paths::to_bytes(entry.path()),
                    meta.len() as i64,
                    mtime(&meta)
                ],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let expected = match expected {
            Some(h) => h,
            None => match hash_file(entry.path()) {
                Ok(h) => h.to_vec(),
                Err(e) => {
                    st.unreadable += 1;
                    push(
                        &mut st.problems,
                        format!("can't read the source {}: {e}", rel.display()),
                    );
                    continue;
                }
            },
        };
        match hash_file(&dest) {
            Ok(h) if h.as_slice() == expected.as_slice() => st.ok += 1,
            Ok(_) => {
                st.differs += 1;
                push(
                    &mut st.problems,
                    format!("different content on the mirror: {}", rel.display()),
                );
            }
            Err(e) => {
                st.unreadable += 1;
                push(
                    &mut st.problems,
                    format!("can't read the mirror's {}: {e}", rel.display()),
                );
            }
        }
    }
    let ok = st.missing == 0 && st.differs == 0 && st.unreadable == 0;
    let summary = format!(
        "re-read {} files ({}, {}%): {}",
        fmt::count(st.checked),
        fmt::bytes(st.checked_bytes),
        percent.min(100),
        if ok {
            "all identical".to_string()
        } else {
            format!(
                "{} missing, {} different, {} unreadable",
                st.missing, st.differs, st.unreadable
            )
        }
    );
    db.conn.execute(
        "UPDATE mirrors SET last_verify = ?2, last_verify_summary = ?3, last_verify_ok = ?4 WHERE id = ?1",
        params![id, now(), summary, ok],
    )?;
    Ok(st)
}

/// Room left on the mirror's drive (for a warning before a sync).
pub fn room(m: &Mirror) -> Option<u64> {
    free_space(Path::new(&m.target))
}
