//! Answers from a hashed index: duplicate groups, "is this safe to wipe", and the scan timeline.
//! Run `hash::hash` first; unhashed files are never reported as duplicates or as verified.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::{Db, fmt, paths};

#[derive(Debug, Serialize, Deserialize)]
pub struct DupeGroup {
    pub sha256: String,
    pub size: u64,
    /// distinct copies (hardlinks of one inode count once)
    pub copies: u64,
    pub reclaimable: u64,
    pub paths: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Dupes {
    pub groups: u64,
    pub reclaimable: u64,
    pub largest: Vec<DupeGroup>,
}

/// Content that exists more than once (by full SHA-256), largest reclaimable space first.
pub fn dupes(db: &Db, min_size: u64, limit: usize) -> Result<Dupes> {
    let mut stmt = db.conn.prepare(
        "SELECT sha256, size, path, dev, ino FROM files
         WHERE current = 1 AND size >= ?1 AND sha256 IN (
             SELECT sha256 FROM files WHERE current = 1 AND sha256 IS NOT NULL AND size >= ?1 GROUP BY sha256 HAVING COUNT(*) > 1)
         ORDER BY sha256, path",
    )?;
    let rows = stmt.query_map(params![min_size as i64], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Vec<u8>>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, Option<i64>>(4)?,
        ))
    })?;

    let mut groups: Vec<DupeGroup> = Vec::new();
    let mut seen_inodes: HashSet<(i64, i64)> = HashSet::new();
    let mut current: Option<Vec<u8>> = None;
    for row in rows {
        let (sha, size, path, dev, ino) = row?;
        if current.as_deref() != Some(&sha[..]) {
            seen_inodes.clear();
            groups.push(DupeGroup {
                sha256: fmt::hex(&sha),
                size: size as u64,
                copies: 0,
                reclaimable: 0,
                paths: Vec::new(),
            });
            current = Some(sha);
        }
        let g = groups.last_mut().expect("pushed above");
        let new_copy = match (dev, ino) {
            (Some(d), Some(i)) => seen_inodes.insert((d, i)),
            _ => true,
        };
        if new_copy {
            g.copies += 1;
        }
        g.paths.push(paths::from_bytes(&path).display().to_string());
    }
    groups.retain(|g| g.copies > 1); // only hardlinks of one inode: nothing to reclaim
    for g in &mut groups {
        g.reclaimable = g.size * (g.copies - 1);
    }
    groups.sort_by(|a, b| {
        b.reclaimable
            .cmp(&a.reclaimable)
            .then_with(|| a.paths.cmp(&b.paths))
    });
    let reclaimable = groups.iter().map(|g| g.reclaimable).sum();
    let count = groups.len() as u64;
    groups.truncate(limit);
    Ok(Dupes {
        groups: count,
        reclaimable,
        largest: groups,
    })
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Tally {
    pub files: u64,
    pub bytes: u64,
}

impl Tally {
    fn add(&mut self, size: u64) {
        self.files += 1;
        self.bytes += size;
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UniqueDir {
    pub dir: String,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Check {
    pub path: String,
    pub total: Tally,
    /// has a SHA-256-identical copy outside `path` (not a hardlink of itself)
    pub verified: Tally,
    /// no identical copy anywhere else in the index: copy these before wiping
    pub unique: Tally,
    /// couldn't be read, so can't be verified
    pub unreadable: Tally,
    /// zero-byte files carry no data and don't block a wipe
    pub empty: u64,
    pub unique_by_dir: Vec<UniqueDir>,
    pub unique_files: Vec<(String, u64)>,
    pub unreadable_files: Vec<(String, String)>,
    pub safe: bool,
}

/// Is `dir` safe to wipe? Every non-empty file under it needs a SHA-256-identical copy somewhere
/// outside it. A hardlink doesn't count: it is the same data on the same disk.
pub fn check(db: &Db, dir: &Path) -> Result<Check> {
    let dir = dunce::canonicalize(dir).with_context(|| format!("can't open {}", dir.display()))?;
    let prefix = paths::dir_prefix(&dir);
    let n = prefix.len() as i64;
    let indexed: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM files WHERE current = 1 AND substr(path, 1, ?1) = ?2",
        params![n, prefix],
        |r| r.get(0),
    )?;
    if indexed == 0 {
        bail!(
            "nothing is indexed under {}; run `backdown index` on it, and on the drives that should hold its copies",
            dir.display()
        );
    }

    let mut stmt = db.conn.prepare(
        "SELECT f.path, f.size, f.error,
                f.sha256 IS NOT NULL AND EXISTS (
                    SELECT 1 FROM files g
                    WHERE g.current = 1 AND g.sha256 = f.sha256 AND g.error IS NULL
                      AND substr(g.path, 1, ?1) <> ?2
                      AND NOT (f.ino IS NOT NULL AND g.dev = f.dev AND g.ino = f.ino))
         FROM files f
         WHERE f.current = 1 AND substr(f.path, 1, ?1) = ?2",
    )?;
    let rows = stmt.query_map(params![n, prefix], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, bool>(3)?,
        ))
    })?;

    let mut c = Check {
        path: dir.display().to_string(),
        total: Tally::default(),
        verified: Tally::default(),
        unique: Tally::default(),
        unreadable: Tally::default(),
        empty: 0,
        unique_by_dir: Vec::new(),
        unique_files: Vec::new(),
        unreadable_files: Vec::new(),
        safe: false,
    };
    let mut by_dir: BTreeMap<String, Tally> = BTreeMap::new();
    for row in rows {
        let (path, size, error, verified) = row?;
        let (path, size) = (paths::from_bytes(&path), size as u64);
        c.total.add(size);
        if size == 0 {
            c.empty += 1;
        } else if let Some(e) = error {
            c.unreadable.add(size);
            c.unreadable_files.push((path.display().to_string(), e));
        } else if verified {
            c.verified.add(size);
        } else {
            c.unique.add(size);
            // first folder below `dir`, or "." for files directly in it
            let rel = path.strip_prefix(&dir).unwrap_or(&path);
            let top = match rel.parent().and_then(|p| p.components().next()) {
                Some(first) => first.as_os_str().to_string_lossy().into_owned(),
                None => ".".to_string(),
            };
            by_dir.entry(top).or_default().add(size);
            c.unique_files.push((path.display().to_string(), size));
        }
    }
    c.unique_by_dir = by_dir
        .into_iter()
        .map(|(dir, t)| UniqueDir {
            dir,
            files: t.files,
            bytes: t.bytes,
        })
        .collect();
    c.unique_by_dir.sort_by_key(|d| std::cmp::Reverse(d.bytes));
    c.unique_files
        .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    c.safe = c.unique.files == 0 && c.unreadable.files == 0;
    Ok(c)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Scan {
    pub id: i64,
    pub root: String,
    pub started_at: i64,
    pub finished_at: i64,
    pub files: u64,
    pub bytes: u64,
}

/// Finished scans, oldest first: the points on the timeline.
pub fn scans(db: &Db) -> Result<Vec<Scan>> {
    let mut stmt = db.conn.prepare(
        "SELECT s.id, r.path, s.started_at, s.finished_at, s.files, s.bytes
         FROM scans s JOIN roots r ON r.id = s.root_id WHERE s.finished_at IS NOT NULL ORDER BY s.id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Scan {
            id: r.get(0)?,
            root: paths::from_bytes(&r.get::<_, Vec<u8>>(1)?)
                .display()
                .to_string(),
            started_at: r.get(2)?,
            finished_at: r.get(3)?,
            files: r.get::<_, i64>(4)? as u64,
            bytes: r.get::<_, i64>(5)? as u64,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
