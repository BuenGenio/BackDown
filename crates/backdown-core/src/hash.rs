//! Tiered hashing: only read what could possibly be a duplicate.
//!
//! 1. size: a file whose size no other file shares is unique; it is never read.
//! 2. sample: SHA-256 of the size plus 64 KiB from the head, middle and tail. Small files are hashed
//!    whole here, so their sample *is* their full hash.
//! 3. full SHA-256, only for files whose size and sample still collide with another file.
//!
//! Hardlinks (same device + inode) are one file: they don't make a size "collide" and are read once.
//! Readers are grouped per device and run one at a time on each by default, so a spinning disk is
//! never made to seek between several files at once; `jobs_per_device` raises that for SSDs.
//! Results are committed in batches, so an interrupted run keeps what it finished.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::Result;
use rusqlite::params;
use sha2::{Digest, Sha256};

use crate::{Db, paths};

const CHUNK: u64 = 64 * 1024;
const SMALL: u64 = 3 * CHUNK;
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

// a hardlink group counts once: key on (dev, ino) where known, else on the row itself
const IDENTITY: &str = "CASE WHEN ino IS NULL THEN 'r' || id ELSE dev || ':' || ino END";

pub fn hash(db: &mut Db, opts: &Options, progress: &mut dyn FnMut(&Progress)) -> Result<Stats> {
    let mut stats = Stats::default();
    if opts.retry_errors {
        db.conn.execute(
            "UPDATE files SET error = NULL WHERE current = 1 AND error IS NOT NULL",
            [],
        )?;
    }
    reuse_by_inode(db)?;
    let sample_jobs = collect(
        db,
        &format!(
            "SELECT id, path, size, dev, ino FROM files
             WHERE current = 1 AND sample IS NULL AND error IS NULL AND size > 0
               AND size IN (SELECT size FROM files WHERE current = 1 AND size > 0 GROUP BY size HAVING COUNT(DISTINCT {IDENTITY}) > 1)"
        ),
    )?;
    run(db, sample_jobs, Stage::Sample, opts, &mut stats, progress)?;
    reuse_by_inode(db)?;
    let full_jobs = collect(
        db,
        "SELECT id, path, size, dev, ino FROM files f
         WHERE current = 1 AND sha256 IS NULL AND error IS NULL AND sample IS NOT NULL
           AND EXISTS (SELECT 1 FROM files g WHERE g.current = 1 AND g.size = f.size AND g.sample = f.sample AND g.id <> f.id
                       AND NOT (f.ino IS NOT NULL AND g.dev = f.dev AND g.ino = f.ino))",
    )?;
    run(db, full_jobs, Stage::Full, opts, &mut stats, progress)?;
    Ok(stats)
}

/// Copy hashes already computed for one path of an inode to its other paths. Current rows only:
/// an old version's inode number may since have been reused by an unrelated file.
fn reuse_by_inode(db: &Db) -> Result<()> {
    db.conn.execute(
        "UPDATE files SET
            sample = COALESCE(sample, (SELECT g.sample FROM files g WHERE g.current = 1 AND g.dev = files.dev AND g.ino = files.ino AND g.sample IS NOT NULL)),
            sha256 = COALESCE(sha256, (SELECT g.sha256 FROM files g WHERE g.current = 1 AND g.dev = files.dev AND g.ino = files.ino AND g.sha256 IS NOT NULL))
         WHERE current = 1 AND ino IS NOT NULL AND (sample IS NULL OR sha256 IS NULL)
           AND EXISTS (SELECT 1 FROM files g WHERE g.current = 1 AND g.dev = files.dev AND g.ino = files.ino AND g.id <> files.id)",
        [],
    )?;
    Ok(())
}

fn collect(db: &Db, sql: &str) -> Result<Vec<Job>> {
    let mut stmt = db.conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Vec<u8>>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, Option<i64>>(4)?,
        ))
    })?;
    let mut jobs: Vec<Job> = Vec::new();
    let mut by_inode: HashMap<(i64, i64), usize> = HashMap::new();
    for row in rows {
        let (id, path, size, dev, ino) = row?;
        if let (Some(d), Some(i)) = (dev, ino) {
            if let Some(&j) = by_inode.get(&(d, i)) {
                jobs[j].ids.push(id);
                continue;
            }
            by_inode.insert((d, i), jobs.len());
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
                    while let Some(job) = queue.lock().unwrap().pop_front() {
                        let out = match stage {
                            Stage::Sample => sample(&job.path, job.size),
                            Stage::Full => full(&job.path, job.size).map(|h| (h, None)),
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
        conn.execute_batch("BEGIN")?;
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
                conn.execute_batch("COMMIT; BEGIN")?;
                progress(&p);
                last = Instant::now();
            }
        }
        conn.execute_batch("COMMIT")?;
        progress(&p);
        Ok(())
    })
}

fn sample(path: &Path, size: u64) -> Outcome {
    if size <= SMALL {
        let h = full(path, size)?;
        return Ok((h.clone(), Some(h)));
    }
    let io = |e: std::io::Error| e.to_string();
    let mut f = File::open(path).map_err(io)?;
    if f.metadata().map_err(io)?.len() != size {
        return Err(changed());
    }
    let mut h = Sha256::new();
    h.update(size.to_le_bytes());
    let mut buf = vec![0u8; CHUNK as usize];
    for offset in [0, size / 2 - CHUNK / 2, size - CHUNK] {
        f.seek(SeekFrom::Start(offset)).map_err(io)?;
        f.read_exact(&mut buf).map_err(io)?;
        h.update(&buf);
    }
    Ok((h.finalize().to_vec(), None))
}

fn full(path: &Path, size: u64) -> Result<Vec<u8>, String> {
    let io = |e: std::io::Error| e.to_string();
    let mut f = File::open(path).map_err(io)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; READ_BUF];
    let mut read = 0u64;
    loop {
        let n = match f.read(&mut buf) {
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
