//! Copying a drive into the pool (docs/SWARM.md §4.5): a pool plan carried out, on the machine
//! the drive is on.
//!
//! Each thing given a pool folder is read from the drive as it is now (walked like `apply` walks
//! an item: its excluded paths left out, links not followed) and each file is sent to that folder:
//! a pool folder on this machine is written in-process, one on another machine through a `Sink`
//! (`backdown swarm-stdio` there, over ssh). Either way it's written by `receive::put`, which reads
//! it back there and answers with the hash it read; that has to equal the hash of what was read
//! here, or the file failed.
//!
//! What's in the pool by then is still left where it is: a file whose content a pool folder has
//! (it may have arrived since the plan was made), or that this run has sent already (the same
//! content twice on the drive), isn't sent. A file that isn't as its last scan saw it (changed,
//! new since, gone) isn't sent either: the plan and the pool's "already there" are worked out
//! from the scan, and a drive is only provably safe to free when its index is true to it.
//!
//! Every file gets a row in `pool_log` (where it went, its hash, what became of it), so a stopped
//! or interrupted run carries on where it was, and the log alone says what happened. A file sent
//! and matched there has its SHA-256 known here from then on (it was read whole to send it), so
//! freeing the drive afterwards needn't read it again.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use super::plan::{Area, AreaRef, Pool, PoolPlan, Seen};
use super::receive::{self, Origin, Received};
use crate::{Db, fmt, paths};

pub const SCHEMA: &str = "
-- copying into the pool (pool/send.rs): one row per file of a pool plan, sent or not, and why
CREATE TABLE IF NOT EXISTS pool_log (
    id      INTEGER PRIMARY KEY,
    plan_id INTEGER NOT NULL,
    thing   INTEGER NOT NULL,
    src     BLOB NOT NULL,
    size    INTEGER NOT NULL,
    mtime   INTEGER NOT NULL,
    sha256  BLOB,
    machine TEXT,                     -- where it went (NULL: nowhere)
    area    INTEGER,
    dest    TEXT,                     -- its place in that pool folder, as written
    state   TEXT NOT NULL,            -- copied | renamed | same | in-pool | twice | changed | failed
    error   TEXT,
    at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS pool_log_src ON pool_log(plan_id, src);
";

/// Where files go: one pool folder's machine. `put` sends one file; its outer error means the
/// conversation itself failed (nothing more can be sent there), its inner one that this file was
/// refused (it's logged, and the rest goes on).
pub trait Sink {
    fn put(
        &mut self,
        area: &AreaRef,
        rel: &str,
        size: u64,
        mtime: i64,
        src: &mut dyn Read,
        origin: &Origin,
    ) -> Result<std::result::Result<Received, String>>;
    /// Everything for pool folder `area` has been sent: its machine indexes it (now or soon).
    /// What it said.
    fn done(&mut self, area: &AreaRef) -> Result<String>;
}

/// Opens a conversation with another machine's end (`backdown swarm-stdio`).
pub trait Connect {
    fn connect(&mut self, machine_id: i64) -> Result<Box<dyn Sink>>;
}

/// This machine's own pool folders, written in-process. `done` doesn't index anything itself: it
/// notes the folder, and the caller indexes it (`Here::to_index`).
pub struct Here<'a> {
    pub db: &'a Db,
    pub to_index: Vec<PathBuf>,
}

impl Sink for Here<'_> {
    fn put(
        &mut self,
        area: &AreaRef,
        rel: &str,
        size: u64,
        mtime: i64,
        src: &mut dyn Read,
        origin: &Origin,
    ) -> Result<std::result::Result<Received, String>> {
        let a = super::get(self.db, area.area)?;
        Ok(receive::put(self.db, &a, rel, size, mtime, src, origin).map_err(|e| format!("{e:#}")))
    }

    fn done(&mut self, area: &AreaRef) -> Result<String> {
        let p = PathBuf::from(&area.path);
        if !self.to_index.contains(&p) {
            self.to_index.push(p);
        }
        Ok("indexed next".into())
    }
}

/// A file's sample and SHA-256 as its last scan has them.
type Scanned = (Option<Vec<u8>>, Option<Vec<u8>>);

/// Reads a source file and hashes what it reads.
struct Hashing<R> {
    inner: R,
    sum: Sha256,
    read: u64,
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.sum.update(&buf[..n]);
        self.read += n as u64;
        Ok(n)
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Stats {
    /// every file of the things sent, and their bytes
    pub files: u64,
    pub bytes: u64,
    /// written into the pool and read back there as sent
    pub copied: u64,
    pub copied_bytes: u64,
    /// of those, under a numbered name (something else was at its place)
    pub renamed: u64,
    /// that very content was at its place already
    pub same: u64,
    /// in the pool already by content, or sent once already by this run (twice on the drive)
    pub in_pool: u64,
    pub twice: u64,
    /// carried on from an earlier run
    pub resumed: u64,
    /// not as last scanned (changed, new since, gone): not sent
    pub changed: u64,
    pub failed: u64,
    pub first_problems: Vec<String>,
    /// what each machine said when its pool folders had everything
    pub done: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Progress {
    pub thing: String,
    pub files: u64,
    pub bytes: u64,
    pub total_bytes: u64,
}

fn problem(st: &mut Stats, s: String) {
    if st.first_problems.len() < 20 {
        st.first_problems.push(s);
    }
}

/// The place in its pool folder of `file`, a file of a thing (`item`, walked from `root`): its
/// place there, then the file's own path below the thing's source, with `/` between names.
fn place_of(item: &crate::plan::Item, root: &Path, file: &Path) -> Option<String> {
    if item.is_file {
        return Some(item.dest.clone());
    }
    let rel = file.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(format!("{}/{}", item.dest, parts.join("/")))
}

/// Carry out pool plan `id` (made on this machine, called `machine`): copy each thing given a
/// pool folder into it (`areas`: every pool folder, as `plan::areas` has them), this machine's
/// through `here`, the others' through `connect`. See the module's notes.
#[allow(clippy::too_many_arguments)]
pub fn send(
    db: &Db,
    id: i64,
    machine: &str,
    areas: &[Area],
    here: &mut Here,
    connect: &mut dyn Connect,
    stop: Option<&AtomicBool>,
    progress: &mut dyn FnMut(&Progress),
) -> Result<Stats> {
    let mut pp: PoolPlan = super::plan::load(db, id)?;
    if pp.machine != machine {
        bail!(
            "pool plan {id} is of a drive on {}: carry it out there",
            pp.machine
        );
    }
    let root = PathBuf::from(&pp.root);
    if !root.is_dir() || std::fs::read_dir(&root).is_ok_and(|mut d| d.next().is_none()) {
        bail!("{} isn't there: connect the drive first", pp.root);
    }
    pp.state = "sending".into();
    super::plan::save(db, &mut pp)?;
    let pool = Pool::new(db, areas)?;
    let mut sinks: HashMap<i64, Box<dyn Sink>> = HashMap::new();
    let mut st = Stats::default();
    let mut planned: HashSet<(Vec<u8>, i64)> = HashSet::new();
    let mut used: Vec<AreaRef> = Vec::new();
    let total_bytes = pp.copy_bytes;
    let mut last_tick = Instant::now();
    let stopped = || stop.is_some_and(|s| s.load(Ordering::Relaxed));
    let log = |thing: usize,
               src: &[u8],
               size: u64,
               mtime: i64,
               sha: Option<&[u8]>,
               to: Option<(&AreaRef, &str)>,
               state: &str,
               error: Option<&str>|
     -> Result<()> {
        db.conn.execute(
            "INSERT INTO pool_log(plan_id, thing, src, size, mtime, sha256, machine, area, dest, state, error, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                id,
                thing as i64,
                src,
                size as i64,
                mtime,
                sha,
                to.map(|(a, _)| a.machine.clone()),
                to.map(|(a, _)| a.area),
                to.map(|(_, d)| d.to_string()),
                state,
                error,
                crate::clock::now()
            ],
        )?;
        Ok(())
    };
    'things: for (ti, t) in pp.things.clone().iter().enumerate() {
        let Some(to) = t.to.clone().filter(|_| !t.skip) else {
            continue;
        };
        let src_root = paths::os(Path::new(&t.item.source)).into_owned();
        if !src_root.exists() {
            problem(&mut st, format!("{} isn't there any more", t.item.source));
            continue;
        }
        let excluded: Vec<PathBuf> = t
            .item
            .exclude
            .iter()
            .map(|x| paths::os(Path::new(x)).into_owned())
            .collect();
        let mut walker = WalkDir::new(&src_root).follow_links(false);
        if t.item.only_files {
            walker = walker.max_depth(1);
        }
        for entry in walker
            .into_iter()
            .filter_entry(|e| !excluded.iter().any(|x| e.path() == x))
        {
            if stopped() {
                break 'things;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    st.failed += 1;
                    problem(&mut st, format!("can't read: {e}"));
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let src = entry.path();
            let meta = entry.metadata()?;
            let (size, mt) = (meta.len(), crate::apply::mtime(&meta));
            let src_b = paths::to_bytes(src);
            st.files += 1;
            st.bytes += size;
            if last_tick.elapsed().as_millis() >= 250 {
                last_tick = Instant::now();
                progress(&Progress {
                    thing: t.item.what.clone(),
                    files: st.files,
                    bytes: st.copied_bytes,
                    total_bytes,
                });
            }
            // carried on: done by an earlier run, and unchanged since
            let done: Option<Option<Vec<u8>>> = db
                .conn
                .prepare_cached(
                    "SELECT sha256 FROM pool_log WHERE plan_id = ?1 AND src = ?2 AND size = ?3 AND mtime = ?4
                       AND state IN ('copied', 'renamed', 'same', 'in-pool', 'twice') ORDER BY id DESC LIMIT 1",
                )?
                .query_row(params![id, src_b, size as i64, mt], |r| r.get(0))
                .optional()?;
            if let Some(sha) = done {
                st.resumed += 1;
                if let Some(h) = sha {
                    planned.insert((h, size as i64));
                }
                continue;
            }
            // as its last scan saw it?
            let row: Option<Scanned> = db
                .conn
                .prepare_cached(
                    "SELECT sample, sha256 FROM files WHERE path = ?1 AND current = 1 AND size = ?2 AND mtime = ?3",
                )?
                .query_row(params![src_b, size as i64, mt], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?;
            let Some((sample, sha)) = row else {
                st.changed += 1;
                problem(
                    &mut st,
                    format!(
                        "{}: not as last scanned (rescan the drive, then plan again)",
                        src.display()
                    ),
                );
                log(ti, &src_b, size, mt, None, None, "changed", None)?;
                continue;
            };
            // in the pool already, or sent once by this run
            if let Some(h) = &sha {
                if planned.contains(&(h.clone(), size as i64)) {
                    st.twice += 1;
                    log(ti, &src_b, size, mt, Some(h), None, "twice", None)?;
                    continue;
                }
                if let Seen::In(at) = pool.see(size as i64, sample.as_deref(), Some(h))? {
                    st.in_pool += 1;
                    log(ti, &src_b, size, mt, Some(h), None, "in-pool", Some(&at))?;
                    planned.insert((h.clone(), size as i64));
                    continue;
                }
            }
            let Some(rel) = place_of(&t.item, &src_root, src) else {
                continue;
            };
            let sink: &mut dyn Sink = match to.machine_id {
                None => &mut *here,
                Some(m) => {
                    if let std::collections::hash_map::Entry::Vacant(v) = sinks.entry(m) {
                        v.insert(connect.connect(m).with_context(|| {
                            format!("reaching {} to copy into {}", to.machine, to.path)
                        })?);
                    }
                    sinks.get_mut(&m).unwrap().as_mut()
                }
            };
            let file = match paths::open_read(src) {
                Ok(f) => f,
                Err(e) => {
                    st.failed += 1;
                    problem(&mut st, format!("can't read {}: {e}", src.display()));
                    log(
                        ti,
                        &src_b,
                        size,
                        mt,
                        None,
                        None,
                        "failed",
                        Some(&e.to_string()),
                    )?;
                    continue;
                }
            };
            let mut reader = Hashing {
                inner: file,
                sum: Sha256::new(),
                read: 0,
            };
            let shown = src.display().to_string();
            let origin = Origin {
                machine,
                path: &shown,
                plan: id,
            };
            let answer = sink.put(&to, &rel, size, mt, &mut reader, &origin)?;
            let ours: [u8; 32] = reader.sum.clone().finalize().into();
            let r = match answer {
                Ok(r) => r,
                Err(e) => {
                    st.failed += 1;
                    problem(&mut st, format!("{}: {e}", src.display()));
                    log(
                        ti,
                        &src_b,
                        size,
                        mt,
                        sha.as_deref(),
                        Some((&to, &rel)),
                        "failed",
                        Some(&e),
                    )?;
                    continue;
                }
            };
            let theirs = fmt::unhex(&r.sha256).unwrap_or_default();
            let changed_meanwhile = sha.as_ref().is_some_and(|h| h[..] != ours[..]);
            if reader.read != size || theirs[..] != ours[..] || changed_meanwhile {
                st.failed += 1;
                let why = if changed_meanwhile {
                    "it changed since it was scanned".to_string()
                } else {
                    format!(
                        "the copy in {} doesn't match what was read here",
                        to.label()
                    )
                };
                problem(&mut st, format!("{}: {why}", src.display()));
                log(
                    ti,
                    &src_b,
                    size,
                    mt,
                    Some(&ours),
                    Some((&to, &r.path)),
                    "failed",
                    Some(&why),
                )?;
                continue;
            }
            match r.outcome {
                "same" => st.same += 1,
                o => {
                    st.copied += 1;
                    st.copied_bytes += size;
                    if o == "renamed" {
                        st.renamed += 1;
                    }
                }
            }
            log(
                ti,
                &src_b,
                size,
                mt,
                Some(&ours),
                Some((&to, &r.path)),
                r.outcome,
                None,
            )?;
            planned.insert((ours.to_vec(), size as i64));
            // every byte of it was just read, and its copy matched: its hash is known here now too
            // (so the drive can be proved safe to free without reading it again)
            if sha.is_none() {
                db.conn.execute(
                    "UPDATE files SET sha256 = ?2 WHERE path = ?1 AND current = 1 AND size = ?3 AND mtime = ?4 AND sha256 IS NULL",
                    params![src_b, ours.to_vec(), size as i64, mt],
                )?;
            }
            if !used.contains(&to) {
                used.push(to.clone());
            }
        }
    }
    // every pool folder that got something: its machine indexes it
    for a in &used {
        let said = match a.machine_id {
            None => here.done(a)?,
            Some(m) => match sinks.get_mut(&m) {
                Some(s) => s.done(a).unwrap_or_else(|e| format!("{e:#}")),
                None => continue,
            },
        };
        st.done.push(format!("{}: {said}", a.label()));
    }
    let mut pp = super::plan::load(db, id)?;
    pp.state = if stopped() || st.failed + st.changed > 0 {
        "partial".into()
    } else {
        "done".into()
    };
    super::plan::save(db, &mut pp)?;
    progress(&Progress {
        thing: String::new(),
        files: st.files,
        bytes: st.copied_bytes,
        total_bytes,
    });
    Ok(st)
}

/// What a pool plan's run did so far, from its log: (state, files, bytes) by state.
pub fn summary(db: &Db, id: i64) -> Result<Vec<(String, u64, u64)>> {
    let mut stmt = db.conn.prepare(
        "SELECT state, COUNT(*), COALESCE(SUM(size), 0) FROM pool_log WHERE plan_id = ?1 GROUP BY state ORDER BY state",
    )?;
    let rows = stmt.query_map([id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)? as u64,
            r.get::<_, i64>(2)? as u64,
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
