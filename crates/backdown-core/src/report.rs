//! Answers from a hashed index: duplicate groups, "is this safe to wipe", and the scan timeline.
//! Run `hash::hash` first; unhashed files are never reported as duplicates or as verified.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::{Db, fmt, paths};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DupeGroup {
    pub sha256: String,
    pub size: u64,
    /// distinct copies (hardlinks of one inode count once)
    pub copies: u64,
    pub reclaimable: u64,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dupes {
    pub groups: u64,
    pub reclaimable: u64,
    pub largest: Vec<DupeGroup>,
}

/// A duplicated group while the largest are picked: (reclaimable, the lower hash first on a tie,
/// size).
type Kept = (u64, Reverse<Vec<u8>>, u64);

/// Content that exists more than once (by full SHA-256), largest reclaimable space first.
///
/// Two passes:
///   1. one scan, in sha256 order, of every current hashed file groups them, collapsing
///      hardlinks by (dev, ino): the exact group count and reclaimable total over *all* groups,
///      keeping only the `limit` largest (ties go to the lower hash), without decoding a path;
///   2. the paths of just those groups.
///
/// With the covering index `files_dupes` (db.rs `DUPES_INDEX`), pass 1 reads the index alone,
/// pinned with `INDEXED BY`: left to choose, SQLite prefers the narrower `files_sha256` and then
/// looks up the table for every row (about 200 s on a 16M-file index, against a few seconds).
/// Without it (an index the daemon hasn't prepared yet, or one read read-only, like another
/// machine's through `serve-stdio`) the same scan goes through the table: slower, never wrong.
pub fn dupes(db: &Db, min_size: u64, limit: usize) -> Result<Dupes> {
    // files sharing a sha256 have the same size, so the size filter keeps or drops a group whole
    let mut scan = db.conn.prepare(if db.has_dupes_index()? {
        "SELECT sha256, size, dev, ino FROM files INDEXED BY files_dupes
         WHERE current = 1 AND sha256 IS NOT NULL AND size >= ?1
         ORDER BY sha256"
    } else {
        "SELECT sha256, size, dev, ino FROM files
         WHERE current = 1 AND sha256 IS NOT NULL AND size >= ?1
         ORDER BY sha256"
    })?;
    let rows = scan.query_map(params![min_size as i64], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<i64>>(3)?,
        ))
    })?;

    // the `limit` largest groups so far, smallest on top
    let mut top: BinaryHeap<Reverse<Kept>> = BinaryHeap::new();
    let (mut count, mut reclaimable) = (0u64, 0u64);
    let mut close = |sha: Vec<u8>, size: u64, copies: u64| {
        // hardlinks of one inode collapse to a single copy: nothing to reclaim
        if copies > 1 {
            let r = size * (copies - 1);
            count += 1;
            reclaimable += r;
            top.push(Reverse((r, Reverse(sha), size)));
            if top.len() > limit {
                top.pop();
            }
        }
    };
    let mut seen_inodes: HashSet<(i64, i64)> = HashSet::new();
    let mut current: Option<Vec<u8>> = None;
    let (mut size, mut copies) = (0u64, 0u64);
    for row in rows {
        let (sha, sz, dev, ino) = row?;
        if current.as_deref() != Some(&sha[..]) {
            if let Some(prev) = current.take() {
                close(prev, size, copies);
            }
            current = Some(sha);
            size = sz as u64;
            copies = 0;
            seen_inodes.clear();
        }
        let new_copy = match (dev, ino) {
            (Some(d), Some(i)) => seen_inodes.insert((d, i)),
            _ => true,
        };
        if new_copy {
            copies += 1;
        }
    }
    if let Some(prev) = current.take() {
        close(prev, size, copies);
    }
    let meta: Vec<(u64, u64, Vec<u8>)> = top
        .into_iter()
        .map(|Reverse((r, Reverse(sha), size))| (r, size, sha))
        .collect();

    // Pass 2: fetch paths for just the kept groups, recomputing copies the same way.
    let mut paths_of = db.conn.prepare(
        "SELECT path, dev, ino FROM files
         WHERE current = 1 AND sha256 = ?1 ORDER BY path",
    )?;
    let mut largest: Vec<DupeGroup> = Vec::with_capacity(meta.len());
    for (_, size, sha) in &meta {
        let prows = paths_of.query_map(params![sha], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<i64>>(2)?,
            ))
        })?;
        let mut paths = Vec::new();
        let mut inodes: HashSet<(i64, i64)> = HashSet::new();
        let mut copies: u64 = 0;
        for pr in prows {
            let (path, dev, ino) = pr?;
            let new_copy = match (dev, ino) {
                (Some(d), Some(i)) => inodes.insert((d, i)),
                _ => true,
            };
            if new_copy {
                copies += 1;
            }
            paths.push(paths::from_bytes(&path).display().to_string());
        }
        largest.push(DupeGroup {
            sha256: fmt::hex(sha),
            size: *size,
            copies,
            reclaimable: size * copies.saturating_sub(1),
            paths,
        });
    }
    largest.sort_by(|a, b| {
        b.reclaimable
            .cmp(&a.reclaimable)
            .then_with(|| a.paths.cmp(&b.paths))
    });
    Ok(Dupes {
        groups: count,
        reclaimable,
        largest,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// of `verified`: files whose only copy outside `path` is on another machine (remote/): hashed
    /// there, and confirmed there within the proof window
    #[serde(default)]
    pub remote: Tally,
    /// the machines that could hold such copies, and the proof window in days
    #[serde(default)]
    pub remote_machines: Vec<String>,
    #[serde(default)]
    pub proof_days: i64,
}

/// How many of the unique files a check names (the largest); its totals always count them all.
pub const UNIQUE_LISTED: u64 = 20_000;

/// Is `dir` safe to wipe? Every non-empty file under it needs a SHA-256-identical copy somewhere
/// outside it. A hardlink doesn't count: it is the same data on the same disk.
pub fn check(db: &Db, dir: &Path) -> Result<Check> {
    let dir = paths::canonical(dir).with_context(|| format!("can't open {}", dir.display()))?;
    // as the index spells it: the drive's names can be in the other Unicode form than the path given
    let dir = crate::names::as_indexed(db, &dir)?;
    check_indexed(db, &dir)
}

/// `check` for a path exactly as the index records it, whether or not the drive is connected
/// (reports cover unplugged drives too).
pub fn check_indexed(db: &Db, dir: &Path) -> Result<Check> {
    let dir = dir.to_path_buf();
    let prefix = paths::dir_prefix(&dir);
    let n = prefix.len() as i64;
    // everything below `dir` is one range of the path index: [prefix, prefix with its last byte + 1)
    let mut hi = prefix.clone();
    if let Some(last) = hi.last_mut() {
        *last += 1; // a separator is never 0xFF
    }
    let indexed: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM files WHERE current = 1 AND path >= ?1 AND path < ?2",
        params![prefix, hi],
        |r| r.get(0),
    )?;
    if indexed == 0 {
        bail!(
            "nothing is indexed under {}; run `backdown index` on it, and on the drives that should hold its copies",
            dir.display()
        );
    }

    // a copy on another machine counts too, conservatively (remote/): by hash, confirmed there
    // within the proof window
    let cutoff = crate::remote::cutoff(db)?;
    let remote = match cutoff {
        Some(_) => crate::remote::proof_machine_sql("f", 4),
        None => "(CASE WHEN ?4 IS NULL THEN NULL END)".into(),
    };
    let mut stmt = db.conn.prepare(&format!(
        "SELECT f.path, f.size, f.error,
                f.sha256 IS NOT NULL AND EXISTS (
                    SELECT 1 FROM files g
                    WHERE g.current = 1 AND g.sha256 = f.sha256 AND g.error IS NULL
                      AND substr(g.path, 1, ?1) <> ?2
                      AND NOT (f.ino IS NOT NULL AND g.dev = f.dev AND g.ino = f.ino)),
                {remote}
         FROM files f
         WHERE f.current = 1 AND f.path >= ?2 AND f.path < ?3"
    ))?;
    let rows = stmt.query_map(params![n, prefix, hi, cutoff], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, bool>(3)?,
            r.get::<_, Option<i64>>(4)?,
        ))
    })?;
    let mut machines: Vec<i64> = Vec::new();

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
        remote: Tally::default(),
        remote_machines: Vec::new(),
        proof_days: 0,
    };
    let mut by_dir: BTreeMap<String, Tally> = BTreeMap::new();
    let mut largest: std::collections::BinaryHeap<std::cmp::Reverse<(u64, String)>> =
        std::collections::BinaryHeap::new();
    for row in rows {
        let (path, size, error, verified, elsewhere) = row?;
        let (path, size) = (paths::from_bytes(&path), size as u64);
        c.total.add(size);
        if size == 0 {
            c.empty += 1;
        } else if let Some(e) = error {
            c.unreadable.add(size);
            c.unreadable_files.push((path.display().to_string(), e));
        } else if verified {
            c.verified.add(size);
        } else if let Some(m) = elsewhere {
            c.verified.add(size);
            c.remote.add(size);
            if !machines.contains(&m) {
                machines.push(m);
            }
        } else {
            c.unique.add(size);
            // first folder below `dir`, or "." for files directly in it
            let rel = path.strip_prefix(&dir).unwrap_or(&path);
            let top = match rel.parent().and_then(|p| p.components().next()) {
                Some(first) => first.as_os_str().to_string_lossy().into_owned(),
                None => ".".to_string(),
            };
            by_dir.entry(top).or_default().add(size);
            // the largest ones by name (a drive can hold millions: the totals count them all)
            if (largest.len() as u64) < UNIQUE_LISTED
                || largest
                    .peek()
                    .is_some_and(|std::cmp::Reverse((s, _))| size > *s)
            {
                largest.push(std::cmp::Reverse((size, path.display().to_string())));
                if largest.len() as u64 > UNIQUE_LISTED {
                    largest.pop();
                }
            }
        }
    }
    c.unique_files = largest
        .into_iter()
        .map(|std::cmp::Reverse((size, p))| (p, size))
        .collect();
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
    if cutoff.is_some() {
        c.remote_machines = crate::remote::list(db)?
            .into_iter()
            .filter(|m| machines.contains(&m.id))
            .map(|m| m.label())
            .collect();
        c.proof_days = crate::remote::settings(db).proof_days;
    }
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
