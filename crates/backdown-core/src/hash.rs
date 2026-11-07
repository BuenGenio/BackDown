//! Tiered hashing: only read what could possibly be a duplicate.
//!
//! 1. size: a file whose size no other file shares is unique; it is never read.
//! 2. sample: SHA-256 of the size plus 64 KiB from the head, middle and tail. Small files are hashed
//!    whole here, so their sample *is* their full hash.
//! 3. full SHA-256, only for files whose size and sample still collide with another file.
//!
//! The files other machines hold (remote/, as last synced) count as other files here, so a file
//! whose only possible copy is on a NAS is hashed too.
//!
//! A file whose full hash is already known (imported from the legacy index, see `legacy`) is never
//! read again: it isn't sampled, and a file of its size is fully hashed instead of being compared by
//! sample.
//!
//! Hardlinks (same device + inode) are one file: they don't make a size "collide" and are read once.
//! Readers are grouped per device and run one at a time on each by default, so a spinning disk is
//! never made to seek between several files at once; `jobs_per_device` raises that for SSDs.
//! Results are committed in batches, so an interrupted run keeps what it finished.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::Result;
use rusqlite::params;
use sha2::{Digest, Sha256};

use crate::db::{Lock, Tx};
use crate::{Db, paths};

const CHUNK: u64 = 64 * 1024;
/// Files up to this size are sampled whole: their sample is their full SHA-256.
pub(crate) const SMALL: u64 = 3 * CHUNK;
const READ_BUF: usize = 1 << 20;

pub struct Options {
    pub jobs_per_device: usize,
    /// Try files again that failed to read last time (a flaky enclosure, a drive since reseated).
    pub retry_errors: bool,
}

#[derive(Debug, Default, Clone)]
pub struct Progress {
    pub stage: &'static str,
    pub files: u64,
    pub total_files: u64,
    pub bytes: u64,
    pub total_bytes: u64,
    pub errors: u64,
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub sampled: u64,
    pub hashed: u64,
    pub bytes_read: u64,
    pub errors: u64,
    /// on drives that aren't connected: left for when they are (never marked unreadable)
    pub absent: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Sample,
    Full,
}

struct Job {
    ids: Vec<i64>, // every path that is this same inode
    path: PathBuf,
    size: u64,
    dev: Option<i64>,
}

// a hardlink group counts once: key on (dev, ino) where known, else on the row itself. The
// modification time is part of it: a row that's still current because its drive hasn't been
// rescanned may hold an inode number the filesystem has since given to another file.
const IDENTITY: &str =
    "CASE WHEN ino IS NULL THEN 'r' || id ELSE dev || ':' || ino || ':' || mtime END";

pub fn hash(db: &mut Db, opts: &Options, progress: &mut dyn FnMut(&Progress)) -> Result<Stats> {
    let mut stats = Stats::default();
    if opts.retry_errors {
        db.conn.execute(
            "UPDATE files SET error = NULL WHERE current = 1 AND error IS NOT NULL",
            [],
        )?;
    }
    reuse_by_inode(db)?;
    let away = absent_roots(db)?;
    let sample_jobs = collect(
        db,
        &away,
        &mut stats,
        &format!(
            "SELECT id, path, size, dev, ino, mtime FROM files
             WHERE current = 1 AND sample IS NULL AND sha256 IS NULL AND error IS NULL AND size > 0
               AND (size IN (SELECT size FROM files WHERE current = 1 AND size > 0 GROUP BY size HAVING COUNT(DISTINCT {IDENTITY}) > 1)
                    -- a file of this size on another machine (remote/): it could be a copy of this one
                    OR size IN (SELECT size FROM remote_files WHERE size > 0))"
        ),
    )?;
    run(db, sample_jobs, Stage::Sample, opts, &mut stats, progress)?;
    reuse_by_inode(db)?;
    let full_jobs = collect(
        db,
        &away,
        &mut stats,
        "SELECT id, path, size, dev, ino, mtime FROM files f
         WHERE current = 1 AND sha256 IS NULL AND error IS NULL AND sample IS NOT NULL
           AND (EXISTS (SELECT 1 FROM files g WHERE g.current = 1 AND g.size = f.size AND g.sample = f.sample AND g.id <> f.id
                        AND NOT (f.ino IS NOT NULL AND g.dev = f.dev AND g.ino = f.ino AND g.mtime = f.mtime))
                -- a same-size file known only by its full hash: no sample to compare with
                OR EXISTS (SELECT 1 FROM files g WHERE g.current = 1 AND g.size = f.size AND g.sample IS NULL AND g.sha256 IS NOT NULL
                           AND NOT (f.ino IS NOT NULL AND g.dev = f.dev AND g.ino = f.ino AND g.mtime = f.mtime))
                -- the same sample on another machine, or a same-size file there known only by its full hash
                OR EXISTS (SELECT 1 FROM remote_files g WHERE g.size = f.size AND g.error = 0
                           AND (g.sample = f.sample OR (g.sample IS NULL AND g.sha256 IS NOT NULL))))",
    )?;
    run(db, full_jobs, Stage::Full, opts, &mut stats, progress)?;
    Ok(stats)
}

/// Copy hashes already computed for one path of an inode to its other paths: hardlinks, which
/// always have the same size and modification time. An inode number alone isn't enough: an old
/// version's may since have gone to an unrelated file, and so may a current row's, when that file
/// was deleted from a drive that hasn't been rescanned since (its row is current until then).
fn reuse_by_inode(db: &Db) -> Result<()> {
    db.conn.execute(
        "UPDATE files SET
            sample = COALESCE(sample, (SELECT g.sample FROM files g WHERE g.current = 1 AND g.dev = files.dev AND g.ino = files.ino
                                         AND g.size = files.size AND g.mtime = files.mtime AND g.sample IS NOT NULL)),
            sha256 = COALESCE(sha256, (SELECT g.sha256 FROM files g WHERE g.current = 1 AND g.dev = files.dev AND g.ino = files.ino
                                         AND g.size = files.size AND g.mtime = files.mtime AND g.sha256 IS NOT NULL))
         WHERE current = 1 AND ino IS NOT NULL AND (sample IS NULL OR sha256 IS NULL)
           AND EXISTS (SELECT 1 FROM files g WHERE g.current = 1 AND g.dev = files.dev AND g.ino = files.ino
                         AND g.size = files.size AND g.mtime = files.mtime AND g.id <> files.id)",
        [],
    )?;
    Ok(())
}

/// Roots whose drive isn't connected: the folder is missing, or empty although it held files
/// (an unplugged drive's leftover mount point). Their files wait; reading them would only fail.
fn absent_roots(db: &Db) -> Result<Vec<Vec<u8>>> {
    let mut stmt = db.conn.prepare(
        "SELECT r.path, COALESCE((SELECT files FROM scans WHERE id = r.last_scan), 0) FROM roots r",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)))?;
    let mut away = Vec::new();
    for row in rows {
        let (path, files) = row?;
        let p = paths::from_bytes(&path);
        let here = p.is_dir()
            && (files == 0 || std::fs::read_dir(&p).is_ok_and(|mut it| it.next().is_some()));
        if !here {
            away.push(paths::dir_prefix(&p));
        }
    }
    Ok(away)
}

fn collect(db: &Db, away: &[Vec<u8>], stats: &mut Stats, sql: &str) -> Result<Vec<Job>> {
    let mut stmt = db.conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Vec<u8>>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, i64>(5)?,
        ))
    })?;
    let mut jobs: Vec<Job> = Vec::new();
    // one job per file: the paths of one inode, with the same size and time (see reuse_by_inode)
    let mut by_inode: HashMap<(i64, i64, i64, i64), usize> = HashMap::new();
    for row in rows {
        let (id, path, size, dev, ino, mtime) = row?;
        if away.iter().any(|a| path.starts_with(a)) {
            stats.absent += 1;
            continue;
        }
        if let (Some(d), Some(i)) = (dev, ino) {
            if let Some(&j) = by_inode.get(&(d, i, size, mtime)) {
                jobs[j].ids.push(id);
                continue;
            }
            by_inode.insert((d, i, size, mtime), jobs.len());
        }
        jobs.push(Job {
            ids: vec![id],
            path: paths::from_bytes(&path),
            size: size as u64,
            dev,
        });
    }
    Ok(jobs)
}

type Outcome = Result<(Vec<u8>, Option<Vec<u8>>), String>; // (sample or full hash, full hash when a sample read the whole file)

fn run(
    db: &Db,
    jobs: Vec<Job>,
    stage: Stage,
    opts: &Options,
    stats: &mut Stats,
    progress: &mut dyn FnMut(&Progress),
) -> Result<()> {
    if jobs.is_empty() {
        return Ok(());
    }
    let read_size = |size: u64| {
        if stage == Stage::Full || size <= SMALL {
            size
        } else {
            SMALL
        }
    };
    let mut p = Progress {
        stage: if stage == Stage::Sample {
            "sample"
        } else {
            "full hash"
        },
        total_files: jobs.len() as u64,
        total_bytes: jobs.iter().map(|j| read_size(j.size)).sum(),
        ..Progress::default()
    };
    let mut queues: HashMap<Option<i64>, VecDeque<Job>> = HashMap::new();
    for job in jobs {
        queues.entry(job.dev).or_default().push_back(job);
    }

    let (tx, rx) = mpsc::channel::<(Vec<i64>, u64, Outcome)>();
    std::thread::scope(|scope| -> Result<()> {
        for queue in queues.into_values() {
            let queue = Arc::new(Mutex::new(queue));
            for _ in 0..opts.jobs_per_device.max(1) {
                let (queue, tx) = (Arc::clone(&queue), tx.clone());
                scope.spawn(move || {
                    let mut buf = vec![0u8; READ_BUF]; // one per reader: most files are small, so allocating per file dominates
                    while let Some(job) = queue.lock().unwrap().pop_front() {
                        let out = match stage {
                            Stage::Sample => sample(&job.path, job.size, &mut buf),
                            Stage::Full => full(&job.path, job.size, &mut buf).map(|h| (h, None)),
                        };
                        if tx.send((job.ids, job.size, out)).is_err() {
                            return;
                        }
                    }
                });
            }
        }
        drop(tx);

        let conn = &db.conn;
        let mut set_sample = conn.prepare_cached(
            "UPDATE files SET sample = ?1, sha256 = COALESCE(?2, sha256) WHERE id = ?3",
        )?;
        let mut set_full = conn.prepare_cached("UPDATE files SET sha256 = ?1 WHERE id = ?2")?;
        let mut set_error = conn.prepare_cached("UPDATE files SET error = ?1 WHERE id = ?2")?;
        let mut last = Instant::now();
        let mut tx = Tx::begin(conn, Lock::Deferred)?;
        for (ids, size, out) in rx {
            match &out {
                Ok((digest, whole)) => {
                    for id in &ids {
                        match stage {
                            Stage::Sample => set_sample.execute(params![digest, whole, id])?,
                            Stage::Full => set_full.execute(params![digest, id])?,
                        };
                    }
                    let counted = if stage == Stage::Sample {
                        &mut stats.sampled
                    } else {
                        &mut stats.hashed
                    };
                    *counted += ids.len() as u64;
                }
                Err(e) => {
                    for id in &ids {
                        set_error.execute(params![e, id])?;
                    }
                    stats.errors += ids.len() as u64;
                    p.errors += ids.len() as u64;
                }
            }
            p.files += 1;
            p.bytes += read_size(size);
            stats.bytes_read += read_size(size);
            if last.elapsed() >= Duration::from_millis(250) {
                tx.checkpoint()?;
                progress(&p);
                last = Instant::now();
            }
        }
        tx.commit()?;
        progress(&p);
        Ok(())
    })
}

/// One file's sample, and its full SHA-256 when the sample read all of it (a small file), exactly
/// as `hash` computes them: for another machine asking about a file it has (remote/). Fails if the
/// file's size isn't `size` any more.
pub fn sample_of(path: &Path, size: u64) -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
    let mut buf = vec![0u8; READ_BUF];
    sample(path, size, &mut buf)
}

/// One indexed file's full SHA-256, read now, exactly as `hash` computes it: for a command that
/// needs one file's content (`tag exif`) when `hash` has left it alone, as it does every file whose
/// size no other file shares. `size` and `mtime` are the index's; if the file doesn't match them
/// any more, this fails rather than give a hash for content the index never scanned.
pub fn full_of(path: &Path, size: u64, mtime: i64) -> Result<Vec<u8>, String> {
    let m = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if m.len() != size || crate::index::mtime_of(&m) != mtime {
        return Err(changed());
    }
    let mut buf = vec![0u8; READ_BUF];
    full(path, size, &mut buf)
}

fn sample(path: &Path, size: u64, buf: &mut [u8]) -> Outcome {
    if size <= SMALL {
        let h = full(path, size, buf)?;
        return Ok((h.clone(), Some(h)));
    }
    let io = |e: std::io::Error| e.to_string();
    let mut f = paths::open_read(path).map_err(io)?;
    if f.metadata().map_err(io)?.len() != size {
        return Err(changed());
    }
    let mut h = Sha256::new();
    h.update(size.to_le_bytes());
    let chunk = &mut buf[..CHUNK as usize];
    for offset in [0, size / 2 - CHUNK / 2, size - CHUNK] {
        f.seek(SeekFrom::Start(offset)).map_err(io)?;
        f.read_exact(chunk).map_err(io)?;
        h.update(&*chunk);
    }
    Ok((h.finalize().to_vec(), None))
}

fn full(path: &Path, size: u64, buf: &mut [u8]) -> Result<Vec<u8>, String> {
    let io = |e: std::io::Error| e.to_string();
    let mut f = paths::open_read(path).map_err(io)?;
    let mut h = Sha256::new();
    let mut read = 0u64;
    loop {
        let n = match f.read(buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        };
        h.update(&buf[..n]);
        read += n as u64;
    }
    if read != size {
        return Err(changed());
    }
    Ok(h.finalize().to_vec())
}

fn changed() -> String {
    "changed since it was indexed; run `backdown index` again".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_of_hashes_one_file_and_refuses_a_changed_one() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("IMG_0001.jpg");
        std::fs::write(&p, b"a photo nobody else has").unwrap();
        let m = std::fs::metadata(&p).unwrap();
        let (size, mtime) = (m.len(), crate::index::mtime_of(&m));

        let h = full_of(&p, size, mtime).unwrap();
        assert_eq!(h, Sha256::digest(b"a photo nobody else has").to_vec());

        // not what the index scanned: another size, or another modification time
        assert_eq!(full_of(&p, size + 1, mtime), Err(changed()));
        assert_eq!(full_of(&p, size, mtime - 60), Err(changed()));
        assert!(full_of(&tmp.path().join("gone.jpg"), size, mtime).is_err());
    }
}
