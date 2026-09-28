//! Import the index the pre-Rust BackDown scripts left behind, so none of their disk time is
//! repeated: every file they crawled becomes a row of a finished scan, and every SHA-256 they
//! computed is attached to the file it belongs to. A later `index` of the drive then finds the
//! files unchanged and keeps those hashes, exactly as after a scan of its own.
//!
//! # The legacy format
//!
//! `bd-index.sh` crawled each drive (a "volume", named in the script) in chunks planned by
//! `bd-plan.py`, into an `index/` folder:
//!
//! - `<vol>/chunks.list`: the plan, one `R<TAB>dir` (a folder, recursively) or `F<TAB>dir` (only
//!   the files directly in it) per line. The planner always writes `F<TAB><root>` first, the
//!   drive's mount point: that is how a volume's root is known here.
//! - `<vol>/chunks/<id>.tsv`: one chunk, the output of
//!   `find DIR -xdev -type f -printf '%s\t%T@\t%p\n'`: size in bytes, mtime as seconds with a
//!   fraction (`1700000000.1234567890`; before 1970 GNU find prints e.g. `-2.5000000000`), and the
//!   path as raw bytes, unescaped. A tab in a name stays inside the last field; a newline in a name
//!   spills the rest of the path onto the next line, which is rejoined here. `<id>.done` marks a
//!   finished chunk and `<id>.path` holds its plan line; a chunk without `.done` was interrupted
//!   and is left out, as the scripts did.
//! - `<vol>.tsv`: `bd-index.sh merge`, every finished chunk concatenated. It is read when present,
//!   unless a chunk finished after it was merged; then the chunks are read instead.
//! - `hashes.db`: `bd-hash.py`'s SQLite table
//!   `h(path TEXT PRIMARY KEY, size INTEGER, mtime REAL, sha256 TEXT, hashed_at REAL)`: lowercase
//!   hex SHA-256s of whole files, keyed by the same absolute paths, with the size and mtime the file
//!   had when it was hashed (as floats of the TSV's text, or of `os.stat`).
//!
//! # What becomes what
//!
//! Each volume becomes a root, at its legacy mount point or where `maps` says the drive is now,
//! with one finished scan dated from the chunks' own timestamps (first `.path` written to last
//! `.done`). Each record becomes a current `files` row with the mtime cut to whole seconds, as
//! `index` stores it (before 1970: 0), and no device/inode (the next `index` fills them in).
//! `dirs` gets the recursive totals of every folder holding files; empty folders were never
//! recorded by `find`, so they first appear with the next `index`. A legacy hash attaches where the
//! path, size and mtime (to the second: `index`'s own test for "unchanged") still match; a file
//! small enough to be sampled whole also gets it as its `sample`, which for such files *is* the full
//! hash. Larger files keep no sample: `hash` never reads a file whose full hash is known.
//!
//! Progress is committed in batches along with a resume point (file and byte offset) in
//! `legacy_imports`, so an interrupted import carries on where it stopped when run again, and a
//! finished one is recognised and left alone.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::{Db, hash, paths};

const BATCH: u64 = 20_000;
const EXAMPLES: usize = 5;
/// Linux's PATH_MAX: a "name holding newlines" longer than this is junk, not a path.
const MAX_PATH: usize = 4096;

/// Where each volume's import stands. Not a foreign key on purpose: nothing else should be kept
/// from forgetting a root.
const STATE: &str = "
CREATE TABLE IF NOT EXISTS legacy_imports (
    root_id      INTEGER PRIMARY KEY,
    scan         INTEGER NOT NULL,
    volume       TEXT NOT NULL,
    fingerprint  TEXT NOT NULL,           -- the legacy files read (names, sizes): same data or not
    next_file    INTEGER NOT NULL DEFAULT 0,  -- resume point: which input file...
    next_offset  INTEGER NOT NULL DEFAULT 0,  -- ...and the byte where its next record starts
    files        INTEGER NOT NULL DEFAULT 0,
    bytes        INTEGER NOT NULL DEFAULT 0,
    malformed    INTEGER NOT NULL DEFAULT 0,
    truncated    INTEGER NOT NULL DEFAULT 0,
    outside_root INTEGER NOT NULL DEFAULT 0,
    duplicates   INTEGER NOT NULL DEFAULT 0,
    rejoined     INTEGER NOT NULL DEFAULT 0,
    finished     INTEGER NOT NULL DEFAULT 0
);";

pub struct Options {
    /// `bd-hash.py`'s database, whose hashes are attached to the files they belong to.
    pub hashes: Option<PathBuf>,
    /// `(legacy path, path now)` for drives mounted somewhere else now. A legacy path maps every
    /// path below it; the longest match wins.
    pub maps: Vec<(PathBuf, PathBuf)>,
    /// Import only these volumes (folder names in the index folder); empty: all of them.
    pub volumes: Vec<String>,
    /// Replace what the index holds for a root, or for roots nested with it, instead of refusing.
    pub replace: bool,
    /// Read everything, write nothing.
    pub dry_run: bool,
}

#[derive(Debug, Default, Clone)]
pub struct Progress {
    pub volume: String,
    /// "reading", "folders" or "hashes"
    pub stage: &'static str,
    pub files: u64,
    /// bytes of TSV read ("reading") or rows of hashes.db looked at ("hashes")
    pub done: u64,
    pub total: u64,
}

/// What reading a volume's TSVs found.
#[derive(Debug, Default, Clone)]
pub struct Tally {
    pub files: u64,
    pub bytes: u64,
    /// lines that are neither a `size<TAB>mtime<TAB>path` record nor the rest of a name holding a
    /// newline
    pub malformed: u64,
    /// a last line cut off before its newline
    pub truncated: u64,
    /// records of paths outside the volume's root
    pub outside_root: u64,
    /// paths listed more than once (the first one is kept); not checked in a dry run
    pub duplicates: u64,
    /// names holding a newline, rejoined from the lines they spilled onto
    pub rejoined: u64,
    /// the first few malformed lines, for the report
    pub examples: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// imported now (in a dry run: would be)
    Imported,
    /// an interrupted import, finished now (would be); `from_files` were in already
    Resumed { from_files: u64 },
    /// imported earlier from this same legacy data: left as it is
    AlreadyImported,
    /// no finished chunk and no merged TSV: nothing to import
    Empty,
}

#[derive(Debug, Clone)]
pub struct VolumeReport {
    pub name: String,
    pub legacy_root: PathBuf,
    /// the root in the index
    pub root: PathBuf,
    /// a non-empty folder is at `root` on this machine now (the drive, most likely)
    pub mounted: bool,
    /// what was read: `<vol>.tsv`, or "N finished chunks"
    pub source: String,
    /// `<vol>.tsv` exists, but chunks finished after it was merged, so the chunks were read
    pub merge_outdated: bool,
    pub chunks_planned: u64,
    /// planned chunks that never finished: their folders aren't in the import
    pub chunks_unfinished: u64,
    pub unfinished_examples: Vec<String>,
    /// finished chunks not in the current plan (a plan made again later); read, like `merge` does
    pub chunks_from_earlier_plans: u64,
    pub outcome: Outcome,
    /// roots whose index data was replaced (`replace`), or an interrupted import of other legacy
    /// data that was started over
    pub replaced: Vec<PathBuf>,
    pub scan: Option<i64>,
    /// the legacy crawl's own dates, which the scan carries
    pub started_at: i64,
    pub finished_at: i64,
    pub tally: Tally,
}

#[derive(Debug, Default, Clone)]
pub struct HashTally {
    pub rows: u64,
    /// rows for files on the volumes being imported
    pub for_volumes: u64,
    /// attached to a file that had no hash yet
    pub attached: u64,
    /// the file had this very hash already (imported before, or hashed since)
    pub already: u64,
    /// the path is indexed, but its size or mtime differ: the hash is of an older version
    pub stale: u64,
    /// no indexed file at that path
    pub unmatched: u64,
    /// the file already has a different SHA-256 (kept)
    pub conflicting: u64,
    /// no valid path, size, mtime or hex SHA-256
    pub unusable: u64,
    /// zero-byte files: BackDown never hashes those
    pub empty: u64,
}

#[derive(Debug, Default)]
pub struct Report {
    pub dry_run: bool,
    pub volumes: Vec<VolumeReport>,
    /// volumes found but not asked for
    pub not_selected: Vec<String>,
    /// `.tsv` files in the index folder that aren't a volume (no `<name>/chunks.list`)
    pub ignored: Vec<String>,
    pub hashes: Option<HashTally>,
    /// the index folder holds a `hashes.db` that wasn't asked for
    pub hashes_unused: Option<PathBuf>,
}

/// Import the legacy index in `index_dir` (see the module docs), then attach `opts.hashes`.
pub fn import(
    db: &mut Db,
    index_dir: &Path,
    opts: &Options,
    progress: &mut dyn FnMut(&Progress),
) -> Result<Report> {
    if cfg!(not(unix)) {
        bail!("the legacy index holds Linux paths; import it on Linux or macOS");
    }
    let found = discover(index_dir)?;
    let maps = opts
        .maps
        .iter()
        .map(|(from, to)| {
            if !from.is_absolute() || !to.is_absolute() {
                bail!(
                    "--map {}={}: both paths must be absolute",
                    from.display(),
                    to.display()
                );
            }
            Ok((trim(&paths::to_bytes(from)), trim(&paths::to_bytes(to))))
        })
        .collect::<Result<Vec<_>>>()?;
    // every volume's root now, asked for or not: hashes are matched through all of them
    let targets: Vec<Target> = found
        .volumes
        .iter()
        .map(|v| target(&v.legacy_root, &maps))
        .collect();

    let mut report = Report {
        dry_run: opts.dry_run,
        ignored: found.ignored.clone(),
        ..Report::default()
    };
    let mut selected = Vec::new();
    for want in &opts.volumes {
        match found.volumes.iter().position(|v| &v.name == want) {
            Some(i) if selected.contains(&i) => {}
            Some(i) => selected.push(i),
            None => bail!(
                "no volume {want:?} in {}; it has: {}",
                index_dir.display(),
                found
                    .volumes
                    .iter()
                    .map(|v| v.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
    if opts.volumes.is_empty() {
        selected = (0..found.volumes.len()).collect();
    }
    for (i, v) in found.volumes.iter().enumerate() {
        if !selected.contains(&i) {
            report.not_selected.push(v.name.clone());
        }
    }
    // two volumes landing on one root (or one inside the other) would share files
    for (n, &a) in selected.iter().enumerate() {
        for &b in &selected[n + 1..] {
            let (pa, pb) = (prefix(&targets[a].root), prefix(&targets[b].root));
            if pa.starts_with(&pb) || pb.starts_with(&pa) {
                bail!(
                    "volumes {} and {} would both be imported at {} and {}, one inside the other; check --map",
                    found.volumes[a].name,
                    found.volumes[b].name,
                    show(&targets[a].root),
                    show(&targets[b].root)
                );
            }
        }
    }

    let has_state = state_exists(&db.conn)?;
    let mut plans = Vec::new();
    let mut refused = Vec::new();
    for &i in &selected {
        let v = &found.volumes[i];
        if v.input_bytes == 0 {
            plans.push((i, Plan::Nothing));
            continue;
        }
        match plan(db, has_state, v, &targets[i].root, opts.replace)? {
            Ok(p) => plans.push((i, p)),
            Err(why) => refused.push(format!("{}: {why}", v.name)),
        }
    }
    if !refused.is_empty() {
        bail!(
            "{}\nNothing was imported. To replace what the index holds for those roots, pass --replace; \
             to import the other volumes, pick them with --volume",
            refused.join("\n")
        );
    }

    if !opts.dry_run {
        db.conn.execute_batch(STATE)?;
    }
    for (i, plan) in plans {
        let (v, t) = (&found.volumes[i], &targets[i]);
        let mut r = VolumeReport {
            name: v.name.clone(),
            legacy_root: paths::from_bytes(&v.legacy_root),
            root: paths::from_bytes(&t.root),
            mounted: t.mounted,
            source: v.source.clone(),
            merge_outdated: v.merge_outdated,
            chunks_planned: v.planned,
            chunks_unfinished: v.unfinished,
            unfinished_examples: v.unfinished_examples.clone(),
            chunks_from_earlier_plans: v.earlier_plans,
            outcome: Outcome::Imported,
            replaced: Vec::new(),
            scan: None,
            started_at: v.started_at,
            finished_at: v.finished_at,
            tally: Tally::default(),
        };
        match plan {
            Plan::Nothing => r.outcome = Outcome::Empty,
            Plan::Done { scan, files, bytes } => {
                r.outcome = Outcome::AlreadyImported;
                r.scan = Some(scan);
                r.tally.files = files;
                r.tally.bytes = bytes;
            }
            Plan::Fresh { forget } if opts.dry_run => {
                r.replaced = forget.iter().map(|(_, p)| paths::from_bytes(p)).collect();
                r.tally = count_only(v, progress)?;
            }
            Plan::Resume { tally, .. } if opts.dry_run => {
                r.outcome = Outcome::Resumed {
                    from_files: tally.files,
                };
                r.tally = tally;
            }
            plan => {
                if let Plan::Fresh { forget } = &plan {
                    r.replaced = forget.iter().map(|(_, p)| paths::from_bytes(p)).collect();
                }
                let (outcome, scan, tally) = import_volume(&db.conn, v, &t.root, plan, progress)?;
                r.outcome = outcome;
                r.scan = Some(scan);
                r.tally = tally;
            }
        }
        report.volumes.push(r);
    }

    match &opts.hashes {
        Some(file) => {
            let roots: Vec<(Vec<u8>, Vec<u8>)> = found
                .volumes
                .iter()
                .zip(&targets)
                .map(|(v, t)| (v.legacy_root.clone(), t.root.clone()))
                .collect();
            let chosen: Vec<Vec<u8>> = selected
                .iter()
                .map(|&i| found.volumes[i].legacy_root.clone())
                .collect();
            let m = Matcher {
                roots: &roots,
                chosen: &chosen,
                maps: &maps,
                dry_run: opts.dry_run,
            };
            report.hashes = Some(m.attach(db, file, progress)?);
        }
        None => report.hashes_unused = found.hashes_db,
    }
    Ok(report)
}

// ---------- the legacy index folder

struct Volume {
    name: String,
    legacy_root: Vec<u8>,
    /// the TSVs to read, in order
    inputs: Vec<PathBuf>,
    input_bytes: u64,
    source: String,
    merge_outdated: bool,
    planned: u64,
    unfinished: u64,
    unfinished_examples: Vec<String>,
    earlier_plans: u64,
    started_at: i64,
    finished_at: i64,
    fingerprint: String,
}

#[derive(Default)]
struct Found {
    volumes: Vec<Volume>,
    ignored: Vec<String>,
    hashes_db: Option<PathBuf>,
}

fn discover(dir: &Path) -> Result<Found> {
    let mut entries: Vec<(OsString, PathBuf)> = fs::read_dir(dir)
        .with_context(|| format!("can't read {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| (e.file_name(), e.path()))
        .collect();
    entries.sort();
    let mut found = Found::default();
    for (name, path) in &entries {
        if path.join("chunks.list").is_file() {
            found.volumes.push(volume(dir, name)?);
        } else if let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".tsv"))
            && !dir.join(stem).join("chunks.list").is_file()
        {
            found.ignored.push(name.to_string_lossy().into_owned());
        }
    }
    let h = dir.join("hashes.db");
    if h.is_file() {
        found.hashes_db = Some(h);
    }
    if found.volumes.is_empty() {
        bail!(
            "{} holds no legacy volumes (a <volume>/chunks.list from bd-index.sh); point at the scripts' `index` folder",
            dir.display()
        );
    }
    Ok(found)
}

fn volume(dir: &Path, name: &OsStr) -> Result<Volume> {
    let display = name.to_string_lossy().into_owned();
    let vdir = dir.join(name);
    let raw = fs::read(vdir.join("chunks.list"))
        .with_context(|| format!("can't read {}", vdir.join("chunks.list").display()))?;
    let mut plan: Vec<&[u8]> = Vec::new();
    let mut in_plan: HashSet<&[u8]> = HashSet::new();
    for line in raw.split(|&b| b == b'\n') {
        if (line.starts_with(b"R\t") || line.starts_with(b"F\t")) && in_plan.insert(line) {
            plan.push(line);
        }
    }
    let legacy_root = match plan.first() {
        Some(l) if l.starts_with(b"F\t") => trim(&l[2..]),
        _ => bail!("{display}: chunks.list doesn't start with its root (F<TAB>/mount/point)"),
    };
    if !legacy_root.starts_with(b"/") || legacy_root.len() < 2 {
        bail!(
            "{display}: the root {:?} in chunks.list isn't an absolute folder below /",
            show(&legacy_root)
        );
    }

    // finished chunks, in the order `bd-index.sh merge` concatenates them (its glob's)
    let cdir = vdir.join("chunks");
    let mut done: Vec<OsString> = match fs::read_dir(&cdir) {
        Ok(it) => it
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let n = e.file_name();
                n.to_str()?.strip_suffix(".done").map(OsString::from)
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    done.sort();
    let mut finished_lines: HashSet<Vec<u8>> = HashSet::new();
    let mut chunks = Vec::new();
    let (mut started, mut finished, mut earlier) = (i64::MAX, 0i64, 0u64);
    let file = |stem: &OsStr, ext: &str| {
        let mut n = stem.to_os_string();
        n.push(ext);
        cdir.join(n)
    };
    for stem in &done {
        let tsv = file(stem, ".tsv");
        if !tsv.is_file() {
            continue;
        }
        let line = fs::read(file(stem, ".path")).ok().map(|mut l| {
            while l.last() == Some(&b'\n') {
                l.pop();
            }
            l
        });
        match line {
            Some(l) if in_plan.contains(l.as_slice()) => {
                finished_lines.insert(l);
            }
            _ => earlier += 1,
        }
        started = started.min(
            mtime_of(&file(stem, ".path"))
                .or(mtime_of(&tsv))
                .unwrap_or(i64::MAX),
        );
        finished = finished.max(mtime_of(&file(stem, ".done")).unwrap_or(0));
        chunks.push(tsv);
    }
    let unfinished: Vec<&[u8]> = plan
        .iter()
        .copied()
        .filter(|l| !finished_lines.contains(*l))
        .collect();

    let mut merged_name = name.to_os_string();
    merged_name.push(".tsv");
    let merged = dir.join(&merged_name);
    let merged_at = merged.is_file().then(|| mtime_of(&merged)).flatten();
    let (inputs, source, merge_outdated) = match merged_at {
        Some(at) if chunks.is_empty() || finished <= at => (
            vec![merged.clone()],
            merged_name.to_string_lossy().into_owned(),
            false,
        ),
        _ if chunks.is_empty() => (Vec::new(), "nothing finished".to_string(), false),
        m => (
            chunks.clone(),
            format!("{} finished chunks", chunks.len()),
            m.is_some(),
        ),
    };
    if chunks.is_empty() {
        let at = merged_at.unwrap_or_else(now);
        (started, finished) = (at, at);
    }
    let mut fp = Sha256::new();
    fp.update(&legacy_root);
    let mut input_bytes = 0;
    for input in &inputs {
        let len = fs::metadata(input).map_or(0, |m| m.len());
        input_bytes += len;
        let rel = input.strip_prefix(dir).unwrap_or(input);
        fp.update(b"\0");
        fp.update(paths::to_bytes(rel));
        fp.update(format!("\0{len}"));
    }
    Ok(Volume {
        name: display,
        legacy_root,
        inputs,
        input_bytes,
        source,
        merge_outdated,
        planned: plan.len() as u64,
        unfinished: unfinished.len() as u64,
        unfinished_examples: unfinished
            .iter()
            .take(EXAMPLES)
            .map(|l| String::from_utf8_lossy(l).replacen('\t', " ", 1))
            .collect(),
        earlier_plans: earlier,
        started_at: started.min(finished),
        finished_at: finished,
        fingerprint: crate::fmt::hex(&fp.finalize()[..16]),
    })
}

fn mtime_of(p: &Path) -> Option<i64> {
    let t = fs::metadata(p).ok()?.modified().ok()?;
    Some(t.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

// ---------- paths

/// A path without trailing slashes ("/" becomes "", which prefixes everything).
fn trim(p: &[u8]) -> Vec<u8> {
    let mut p = p.to_vec();
    while p.last() == Some(&b'/') {
        p.pop();
    }
    p
}

fn show(p: &[u8]) -> String {
    paths::from_bytes(p).display().to_string()
}

fn prefix(root: &[u8]) -> Vec<u8> {
    paths::dir_prefix(&paths::from_bytes(root))
}

/// Is `path` strictly below `dir` (given without its trailing slash)?
fn below(path: &[u8], dir: &[u8]) -> bool {
    path.len() > dir.len() + 1 && path.starts_with(dir) && path[dir.len()] == b'/'
}

/// `path` rewritten by the longest map whose legacy side is it or one of its folders.
fn map(path: &[u8], maps: &[(Vec<u8>, Vec<u8>)]) -> Option<Vec<u8>> {
    let (from, to) = maps
        .iter()
        .filter(|(from, _)| path == from || below(path, from))
        .max_by_key(|(from, _)| from.len())?;
    let out = [to.as_slice(), &path[from.len()..]].concat();
    Some(if out.is_empty() { b"/".to_vec() } else { out })
}

/// The folder `index` records a file under: the same `Path::parent` it uses.
fn parent(path: &[u8]) -> Vec<u8> {
    let p = paths::from_bytes(path);
    paths::to_bytes(p.parent().unwrap_or(&p))
}

struct Target {
    root: Vec<u8>,
    mounted: bool,
}

/// Where a legacy root lives in the index: mapped, then canonical as `index` would record it when
/// it is a folder here (a drive mounted there, or its mount point).
fn target(legacy: &[u8], maps: &[(Vec<u8>, Vec<u8>)]) -> Target {
    let mapped = map(legacy, maps).unwrap_or_else(|| legacy.to_vec());
    match dunce::canonicalize(paths::from_bytes(&mapped)) {
        Ok(c) if c.is_dir() => Target {
            mounted: fs::read_dir(&c).is_ok_and(|mut it| it.next().is_some()),
            root: paths::to_bytes(&c),
        },
        _ => Target {
            root: mapped,
            mounted: false,
        },
    }
}

// ---------- reading a TSV

/// One file, as a legacy TSV names it.
#[derive(Debug, PartialEq)]
struct Record {
    size: i64,
    /// whole seconds, as `index` stores them
    mtime: i64,
    /// the legacy path
    path: Vec<u8>,
    rejoined: bool,
}

enum Pending {
    None,
    File(Record),
    /// a record that won't be imported; lines continuing its name are swallowed with it
    Skip {
        outside: bool,
    },
}

/// `size<TAB>mtime<TAB>path` records from one TSV, with names holding a newline rejoined. A record
/// is only complete when the next one starts, so each comes with that next line's offset: the point
/// to resume from once it is stored. Tallies of skipped lines are made where resuming won't
/// count them twice.
struct Records {
    reader: BufReader<File>,
    offset: u64,
    /// the legacy root with a trailing slash
    root: Vec<u8>,
    pending: Pending,
    line: Vec<u8>,
}

impl Records {
    fn open(path: &Path, offset: u64, root: &[u8]) -> Result<Records> {
        let mut f = File::open(path).with_context(|| format!("can't read {}", path.display()))?;
        f.seek(SeekFrom::Start(offset))?;
        Ok(Records {
            reader: BufReader::with_capacity(1 << 20, f),
            offset,
            root: [root, b"/"].concat(),
            pending: Pending::None,
            line: Vec::new(),
        })
    }

    fn next(&mut self, t: &mut Tally) -> Result<Option<(Record, u64)>> {
        loop {
            let start = self.offset;
            self.line.clear();
            let n = self.reader.read_until(b'\n', &mut self.line)?;
            if n == 0 {
                return Ok(match std::mem::replace(&mut self.pending, Pending::None) {
                    Pending::File(r) => Some((r, start)),
                    Pending::Skip { outside } => {
                        count_skip(t, outside);
                        None
                    }
                    Pending::None => None,
                });
            }
            self.offset += n as u64;
            if self.line.pop() != Some(b'\n') {
                t.truncated += 1; // bd-plan.py's salvage drops such a line too
                continue;
            }
            let line = &self.line[..];
            if let Some((size, mtime, path)) = parse(line) {
                let next = if path.len() > self.root.len() && path.starts_with(&self.root) {
                    Pending::File(Record {
                        size,
                        mtime,
                        path: path.to_vec(),
                        rejoined: false,
                    })
                } else {
                    Pending::Skip { outside: true }
                };
                match std::mem::replace(&mut self.pending, next) {
                    Pending::File(r) => return Ok(Some((r, start))),
                    Pending::Skip { outside } => count_skip(t, outside),
                    Pending::None => {}
                }
                continue;
            }
            // a name holding a newline goes on here; a line with a tab can't be that (in practice)
            let continues = !line.contains(&b'\t');
            match &mut self.pending {
                Pending::File(r) if continues && r.path.len() + 1 + line.len() <= MAX_PATH => {
                    r.path.push(b'\n');
                    r.path.extend_from_slice(line);
                    r.rejoined = true;
                }
                Pending::File(_) if continues => self.pending = Pending::Skip { outside: false },
                Pending::Skip { .. } if continues => {}
                _ => {
                    t.malformed += 1;
                    if t.examples.len() < EXAMPLES {
                        let l = String::from_utf8_lossy(line);
                        t.examples.push(l.chars().take(160).collect());
                    }
                }
            }
        }
    }
}

fn count_skip(t: &mut Tally, outside: bool) {
    if outside {
        t.outside_root += 1;
    } else {
        t.malformed += 1;
    }
}

/// A line as `find -printf '%s\t%T@\t%p\n'` writes it: (size, whole seconds, path). Seconds are
/// cut like `index` cuts them: `1700000000.9` is 1700000000, and anything before 1970 is 0.
fn parse(line: &[u8]) -> Option<(i64, i64, &[u8])> {
    let mut fields = line.splitn(3, |&b| b == b'\t');
    let (size, mtime, path) = (fields.next()?, fields.next()?, fields.next()?);
    let size = number(size)?;
    let (before_1970, mtime) = match mtime.strip_prefix(b"-") {
        Some(m) => (true, m),
        None => (false, mtime),
    };
    let (whole, fraction) = match mtime.iter().position(|&b| b == b'.') {
        Some(i) => (&mtime[..i], &mtime[i + 1..]),
        None => (mtime, &[][..]),
    };
    if !fraction.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let whole = number(whole)?;
    Some((size, if before_1970 { 0 } else { whole }, path))
}

fn number(b: &[u8]) -> Option<i64> {
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(b).ok()?.parse().ok()
}

/// A dry run: read every record, write nothing.
fn count_only(v: &Volume, progress: &mut dyn FnMut(&Progress)) -> Result<Tally> {
    let mut t = Tally::default();
    let mut p = Progress {
        volume: v.name.clone(),
        stage: "reading",
        total: v.input_bytes,
        ..Progress::default()
    };
    let mut last = Instant::now();
    for input in &v.inputs {
        let mut rd = Records::open(input, 0, &v.legacy_root)?;
        while let Some((r, end)) = rd.next(&mut t)? {
            t.files += 1;
            t.bytes += r.size as u64;
            t.rejoined += u64::from(r.rejoined);
            if last.elapsed() >= Duration::from_millis(250) {
                p.files = t.files;
                progress(&Progress {
                    done: p.done + end,
                    ..p.clone()
                });
                last = Instant::now();
            }
        }
        p.done += fs::metadata(input).map_or(0, |m| m.len());
    }
    Ok(t)
}

// ---------- the index side

enum Plan {
    /// no finished chunk, no merged TSV
    Nothing,
    /// a new root; `forget` are roots to remove first
    Fresh {
        forget: Vec<(i64, Vec<u8>)>,
    },
    Resume {
        root_id: i64,
        scan: i64,
        next_file: usize,
        next_offset: u64,
        tally: Tally,
    },
    Done {
        scan: i64,
        files: u64,
        bytes: u64,
    },
}

fn state_exists(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'legacy_imports'",
        [],
        |r| r.get(0),
    )?)
}

/// What to do with a volume, or why it is refused: its root is indexed already, by a scan or by
/// an import of other data, or it nests with another root.
fn plan(
    db: &Db,
    has_state: bool,
    v: &Volume,
    root: &[u8],
    replace: bool,
) -> Result<std::result::Result<Plan, String>> {
    let conn = &db.conn;
    let mine = prefix(root);
    let mut same = None;
    let mut conflicts: Vec<(i64, Vec<u8>, String)> = Vec::new();
    let mut stmt = conn.prepare("SELECT id, path FROM roots")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
    for row in rows {
        let (id, path) = row?;
        if path == root {
            same = Some(id);
            continue;
        }
        let theirs = prefix(&path);
        if mine.starts_with(&theirs) || theirs.starts_with(&mine) {
            let why = format!(
                "{} overlaps the already indexed {}",
                show(root),
                show(&path)
            );
            conflicts.push((id, path, why));
        }
    }
    let mut forget: Vec<(i64, Vec<u8>)> = Vec::new();
    if let Some(id) = same {
        let imported = if has_state {
            conn.query_row(
                "SELECT scan, fingerprint, finished, next_file, next_offset, files, bytes, malformed,
                        truncated, outside_root, duplicates, rejoined
                 FROM legacy_imports WHERE root_id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, bool>(2)?,
                        r.get::<_, i64>(3)? as usize,
                        r.get::<_, i64>(4)? as u64,
                        Tally {
                            files: r.get::<_, i64>(5)? as u64,
                            bytes: r.get::<_, i64>(6)? as u64,
                            malformed: r.get::<_, i64>(7)? as u64,
                            truncated: r.get::<_, i64>(8)? as u64,
                            outside_root: r.get::<_, i64>(9)? as u64,
                            duplicates: r.get::<_, i64>(10)? as u64,
                            rejoined: r.get::<_, i64>(11)? as u64,
                            examples: Vec::new(),
                        },
                    ))
                },
            )
            .optional()?
        } else {
            None
        };
        let other_scans: i64 = conn.query_row(
            "SELECT COUNT(*) FROM scans WHERE root_id = ?1 AND id <> ?2",
            params![id, imported.as_ref().map_or(-1, |i| i.0)],
            |r| r.get(0),
        )?;
        match imported {
            Some((scan, fp, true, .., tally)) if fp == v.fingerprint => {
                return Ok(Ok(Plan::Done {
                    scan,
                    files: tally.files,
                    bytes: tally.bytes,
                }));
            }
            Some((scan, fp, false, next_file, next_offset, tally))
                if fp == v.fingerprint && other_scans == 0 =>
            {
                return Ok(Ok(Plan::Resume {
                    root_id: id,
                    scan,
                    next_file,
                    next_offset,
                    tally,
                }));
            }
            // an interrupted import of other legacy data, or a root that was never scanned:
            // nothing of value to lose, so start over without asking
            Some((_, _, false, ..)) | None if other_scans == 0 => forget.push((id, root.to_vec())),
            Some((_, _, true, ..)) if other_scans == 0 => conflicts.push((
                id,
                root.to_vec(),
                format!("{} was imported earlier from other legacy data", show(root)),
            )),
            _ => conflicts.push((
                id,
                root.to_vec(),
                format!("{} is already indexed by BackDown", show(root)),
            )),
        }
    }
    if !conflicts.is_empty() && !replace {
        let why: Vec<String> = conflicts.into_iter().map(|c| c.2).collect();
        return Ok(Err(why.join("; ")));
    }
    forget.extend(conflicts.into_iter().map(|(id, path, _)| (id, path)));
    Ok(Ok(Plan::Fresh { forget }))
}

/// Remove a root and everything the index holds for it.
fn forget_root(conn: &Connection, id: i64) -> Result<()> {
    for sql in [
        "DELETE FROM legacy_imports WHERE root_id = ?1",
        "DELETE FROM files WHERE root_id = ?1",
        "DELETE FROM dirs WHERE root_id = ?1",
        "DELETE FROM scans WHERE root_id = ?1",
        "DELETE FROM roots WHERE id = ?1",
    ] {
        conn.execute(sql, params![id])?;
    }
    Ok(())
}

fn import_volume(
    conn: &Connection,
    v: &Volume,
    root: &[u8],
    plan: Plan,
    progress: &mut dyn FnMut(&Progress),
) -> Result<(Outcome, i64, Tally)> {
    let (root_id, scan, next_file, next_offset, mut t, outcome) = match plan {
        Plan::Fresh { forget } => {
            conn.execute_batch("BEGIN")?;
            for (id, _) in &forget {
                forget_root(conn, *id)?;
            }
            conn.execute("INSERT INTO roots(path) VALUES (?1)", params![root])?;
            let root_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO scans(root_id, started_at) VALUES (?1, ?2)",
                params![root_id, v.started_at],
            )?;
            let scan = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO legacy_imports(root_id, scan, volume, fingerprint) VALUES (?1, ?2, ?3, ?4)",
                params![root_id, scan, v.name, v.fingerprint],
            )?;
            conn.execute_batch("COMMIT")?;
            (root_id, scan, 0, 0, Tally::default(), Outcome::Imported)
        }
        Plan::Resume {
            root_id,
            scan,
            next_file,
            next_offset,
            tally,
        } => {
            let from_files = tally.files;
            (
                root_id,
                scan,
                next_file,
                next_offset,
                tally,
                Outcome::Resumed { from_files },
            )
        }
        Plan::Nothing | Plan::Done { .. } => unreachable!("nothing to import"),
    };

    let mut insert = conn.prepare_cached(
        "INSERT OR IGNORE INTO files (root_id, path, dir, size, mtime, first_scan, last_scan, current)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 1)",
    )?;
    let mut save = conn.prepare_cached(
        "UPDATE legacy_imports SET next_file = ?2, next_offset = ?3, files = ?4, bytes = ?5,
            malformed = ?6, truncated = ?7, outside_root = ?8, duplicates = ?9, rejoined = ?10
         WHERE root_id = ?1",
    )?;
    let mut save = |t: &Tally, file: usize, offset: u64| {
        save.execute(params![
            root_id,
            file as i64,
            offset as i64,
            t.files as i64,
            t.bytes as i64,
            t.malformed as i64,
            t.truncated as i64,
            t.outside_root as i64,
            t.duplicates as i64,
            t.rejoined as i64
        ])
    };
    let sizes: Vec<u64> = v
        .inputs
        .iter()
        .map(|p| fs::metadata(p).map_or(0, |m| m.len()))
        .collect();
    let mut p = Progress {
        volume: v.name.clone(),
        stage: "reading",
        files: t.files,
        done: 0,
        total: v.input_bytes,
    };
    let mut last = Instant::now();
    let mut in_batch = 0u64;
    conn.execute_batch("BEGIN")?;
    for (i, input) in v.inputs.iter().enumerate().skip(next_file) {
        let base: u64 = sizes[..i].iter().sum();
        let from = if i == next_file { next_offset } else { 0 };
        let mut rd = Records::open(input, from, &v.legacy_root)?;
        while let Some((rec, end)) = rd.next(&mut t)? {
            let path = [root, &rec.path[v.legacy_root.len()..]].concat();
            let dir = parent(&path);
            if insert.execute(params![root_id, path, dir, rec.size, rec.mtime, scan])? == 0 {
                t.duplicates += 1;
            } else {
                t.files += 1;
                t.bytes += rec.size as u64;
                t.rejoined += u64::from(rec.rejoined);
            }
            in_batch += 1;
            let batch_done = in_batch >= BATCH;
            if batch_done {
                save(&t, i, end)?;
                conn.execute_batch("COMMIT; BEGIN")?;
                in_batch = 0;
            }
            if batch_done || last.elapsed() >= Duration::from_millis(250) {
                p.files = t.files;
                p.done = base + end;
                progress(&p);
                last = Instant::now();
            }
        }
    }
    save(&t, v.inputs.len(), 0)?;
    conn.execute_batch("COMMIT")?;

    // folder totals and the finished scan, together: a scan never shows without its folders
    p.stage = "folders";
    p.done = p.total;
    progress(&p);
    conn.execute_batch("BEGIN")?;
    let all = write_dirs(conn, root_id, scan, root)?;
    conn.execute(
        "UPDATE scans SET finished_at = ?2, files = ?3, bytes = ?4 WHERE id = ?1",
        params![scan, v.finished_at, all.files as i64, all.bytes as i64],
    )?;
    conn.execute(
        "UPDATE roots SET last_scan = ?2 WHERE id = ?1",
        params![root_id, scan],
    )?;
    conn.execute(
        "UPDATE legacy_imports SET finished = 1 WHERE root_id = ?1",
        params![root_id],
    )?;
    conn.execute_batch("COMMIT")?;
    Ok((outcome, scan, t))
}

#[derive(Debug, Default, Clone, Copy)]
struct Totals {
    files: u64,
    bytes: u64,
    newest: i64,
}

impl Totals {
    fn add(&mut self, o: Totals) {
        self.files += o.files;
        self.bytes += o.bytes;
        self.newest = self.newest.max(o.newest);
    }
}

/// Recursive totals of every folder holding files, from the rows just imported, as `dirs` rows of
/// `scan`: the same rows `index` writes, so its next scan finds them unchanged. Returns the root's.
fn write_dirs(conn: &Connection, root_id: i64, scan: i64, root: &[u8]) -> Result<Totals> {
    let mut direct: HashMap<Vec<u8>, Totals> = HashMap::new();
    direct.insert(root.to_vec(), Totals::default());
    {
        let mut stmt =
            conn.prepare("SELECT dir, size, mtime FROM files WHERE root_id = ?1 AND current = 1")?;
        let mut rows = stmt.query(params![root_id])?;
        while let Some(r) = rows.next()? {
            let Some(dir) = r.get_ref(0)?.as_blob_or_null()? else {
                continue;
            };
            let file = Totals {
                files: 1,
                bytes: r.get::<_, i64>(1)? as u64,
                newest: r.get(2)?,
            };
            match direct.get_mut(dir) {
                Some(t) => t.add(file),
                None => {
                    direct.insert(dir.to_vec(), file);
                }
            }
        }
    }
    // every folder between a file and the root, including those holding only folders
    let keys: Vec<Vec<u8>> = direct.keys().cloned().collect();
    for mut at in keys {
        while at != root {
            let up = parent(&at);
            if up.len() >= at.len() || direct.contains_key(&up) {
                break;
            }
            direct.insert(up.clone(), Totals::default());
            at = up;
        }
    }
    // deepest first, each folder adds its totals to its parent's
    let mut dirs: Vec<(Vec<u8>, Totals)> = direct.into_iter().collect();
    dirs.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let parents: Vec<Option<usize>> = dirs
        .iter()
        .map(|(p, _)| {
            if p == root {
                return None;
            }
            let up = parent(p);
            dirs.binary_search_by(|d| d.0.as_slice().cmp(&up)).ok()
        })
        .collect();
    let mut order: Vec<usize> = (0..dirs.len()).collect();
    order.sort_unstable_by_key(|&i| {
        std::cmp::Reverse(dirs[i].0.iter().filter(|&&b| b == b'/').count())
    });
    for i in order {
        if let Some(up) = parents[i] {
            let t = dirs[i].1;
            dirs[up].1.add(t);
        }
    }
    let mut insert = conn.prepare_cached(
        "INSERT INTO dirs (root_id, path, parent, files, bytes, newest, first_scan, last_scan, current)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 1)",
    )?;
    let mut all = Totals::default();
    for (path, t) in &dirs {
        let up = (path != root).then(|| parent(path));
        insert.execute(params![
            root_id,
            path,
            up,
            t.files as i64,
            t.bytes as i64,
            t.newest,
            scan
        ])?;
        if path == root {
            all = *t;
        }
    }
    Ok(all)
}

// ---------- hashes.db

struct Matcher<'a> {
    /// every volume's (legacy root, root now)
    roots: &'a [(Vec<u8>, Vec<u8>)],
    /// legacy roots of the volumes being imported
    chosen: &'a [Vec<u8>],
    maps: &'a [(Vec<u8>, Vec<u8>)],
    dry_run: bool,
}

impl Matcher<'_> {
    /// A legacy path as the index holds it: through its volume's root, else through `maps`.
    fn rewrite(&self, path: &[u8]) -> Vec<u8> {
        match self
            .roots
            .iter()
            .filter(|(legacy, _)| below(path, legacy))
            .max_by_key(|(legacy, _)| legacy.len())
        {
            Some((legacy, now)) => [now.as_slice(), &path[legacy.len()..]].concat(),
            None => map(path, self.maps).unwrap_or_else(|| path.to_vec()),
        }
    }

    /// Attach each legacy hash to the current file with the same path, size and mtime. Only fills
    /// in what is missing, so running it again changes nothing.
    fn attach(
        &self,
        db: &Db,
        file: &Path,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<HashTally> {
        let legacy = open_hashes(file)?;
        let total: i64 = legacy.query_row("SELECT COUNT(*) FROM h", [], |r| r.get(0))?;
        let mut rows_stmt = legacy.prepare("SELECT path, size, mtime, sha256 FROM h")?;
        let mut rows = rows_stmt.query([])?;
        let conn = &db.conn;
        let mut find = conn.prepare_cached(
            "SELECT id, size, mtime, sha256 FROM files WHERE path = ?1 AND current = 1",
        )?;
        let mut set = conn.prepare_cached(
            "UPDATE files SET sha256 = ?2, sample = COALESCE(sample, ?3) WHERE id = ?1",
        )?;
        let mut t = HashTally::default();
        let mut p = Progress {
            volume: file.display().to_string(),
            stage: "hashes",
            total: total as u64,
            ..Progress::default()
        };
        let mut last = Instant::now();
        if !self.dry_run {
            conn.execute_batch("BEGIN")?;
        }
        while let Some(r) = rows.next()? {
            t.rows += 1;
            if !self.dry_run && t.rows.is_multiple_of(BATCH) {
                conn.execute_batch("COMMIT; BEGIN")?;
            }
            if last.elapsed() >= Duration::from_millis(250) {
                p.done = t.rows;
                p.files = t.attached;
                progress(&p);
                last = Instant::now();
            }
            let path = match r.get_ref(0)? {
                ValueRef::Text(b) | ValueRef::Blob(b) if !b.is_empty() => b.to_vec(),
                _ => {
                    t.unusable += 1;
                    continue;
                }
            };
            let size = match r.get_ref(1)? {
                ValueRef::Integer(i) => i,
                ValueRef::Real(f) if f.fract() == 0.0 => f as i64,
                _ => -1,
            };
            let mtime = match r.get_ref(2)? {
                ValueRef::Real(f) => Some(f),
                ValueRef::Integer(i) => Some(i as f64),
                _ => None,
            };
            let sha = match r.get_ref(3)? {
                ValueRef::Text(s) => unhex(s),
                _ => None,
            };
            let (Some(mtime), Some(sha)) = (mtime, sha) else {
                t.unusable += 1;
                continue;
            };
            if size < 0 {
                t.unusable += 1;
                continue;
            }
            if size == 0 {
                t.empty += 1;
                continue;
            }
            if self.chosen.iter().any(|root| below(&path, root)) {
                t.for_volumes += 1;
            }
            if self.dry_run {
                continue;
            }
            let found: Option<(i64, i64, i64, Option<Vec<u8>>)> = find
                .query_row(params![self.rewrite(&path)], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .optional()?;
            match found {
                None => t.unmatched += 1,
                Some((_, s, m, _)) if s != size || !same_second(m, mtime) => t.stale += 1,
                Some((.., Some(have))) if have == sha => t.already += 1,
                Some((.., Some(_))) => t.conflicting += 1,
                Some((id, ..)) => {
                    // a file sampled whole has its full hash as its sample (see `hash`)
                    let sample = (size as u64 <= hash::SMALL).then_some(&sha[..]);
                    set.execute(params![id, &sha[..], sample])?;
                    t.attached += 1;
                }
            }
        }
        if !self.dry_run {
            conn.execute_batch("COMMIT")?;
        }
        p.done = t.rows;
        p.files = t.attached;
        progress(&p);
        Ok(t)
    }
}

fn open_hashes(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(10))?;
    let cols: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('h')")
        .and_then(|mut s| {
            s.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()
        })
        .with_context(|| format!("reading {}", path.display()))?;
    if !["path", "size", "mtime", "sha256"]
        .iter()
        .all(|want| cols.iter().any(|c| c == want))
    {
        bail!(
            "{} isn't bd-hash.py's hashes.db (no table h with path, size, mtime and sha256)",
            path.display()
        );
    }
    Ok(conn)
}

/// Does a legacy mtime (a float) fall in the whole second `index` stored? A fraction a hair under
/// 1 rounds up to the next second in a float, so that second counts too.
fn same_second(stored: i64, legacy: f64) -> bool {
    if legacy < 0.0 {
        return stored == 0;
    }
    let s = legacy.floor() as i64;
    stored == s || (stored == s - 1 && legacy - (s as f64) < 1e-6)
}

fn unhex(s: &[u8]) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = [0u8; 32];
    for (i, pair) in s.chunks(2).enumerate() {
        out[i] = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_find_lines() {
        assert_eq!(
            parse(b"12\t1700000000.9876543210\t/mnt/x/a b"),
            Some((12, 1_700_000_000, &b"/mnt/x/a b"[..]))
        );
        // a tab in a name stays in the path
        assert_eq!(
            parse(b"0\t5.0000000000\t/mnt/x/tab\there"),
            Some((0, 5, &b"/mnt/x/tab\there"[..]))
        );
        // GNU find before 1970 (-1.5 s prints as -2.5); `index` stores 0
        assert_eq!(
            parse(b"3\t-2.5000000000\t/mnt/x/old"),
            Some((3, 0, &b"/mnt/x/old"[..]))
        );
        assert_eq!(
            parse(b"3\t1600000000\t/p"),
            Some((3, 1_600_000_000, &b"/p"[..]))
        );
        assert_eq!(parse(b"x\t1.0\t/p"), None);
        assert_eq!(parse(b"1\t1.0x\t/p"), None);
        assert_eq!(parse(b"1\t1.0"), None);
    }

    fn read_all(tsv: &[u8], from: u64) -> (Vec<(Record, u64)>, Tally) {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x.tsv");
        fs::write(&f, tsv).unwrap();
        let mut rd = Records::open(&f, from, b"/mnt/x").unwrap();
        let mut t = Tally::default();
        let mut out = Vec::new();
        while let Some(r) = rd.next(&mut t).unwrap() {
            out.push(r);
        }
        (out, t)
    }

    #[test]
    fn rejoins_names_with_newlines_and_counts_the_rest() {
        let tsv = b"junk at the start\n\
                    1\t10.5\t/mnt/x/a\n\
                    2\t20.0\t/mnt/x/two\n\
                    lines\n\
                    3\t30.0\t/elsewhere/b\n\
                    continued\n\
                    bad\tline\n\
                    4\t40.0\t/mnt/x/c\n\
                    5\t50.0\t/mnt/x/cut";
        let (recs, t) = read_all(tsv, 0);
        let paths: Vec<&[u8]> = recs.iter().map(|(r, _)| &r.path[..]).collect();
        assert_eq!(paths, [&b"/mnt/x/a"[..], b"/mnt/x/two\nlines", b"/mnt/x/c"]);
        assert!(recs[1].0.rejoined);
        assert_eq!((t.malformed, t.outside_root, t.truncated), (2, 1, 1));

        // each record's end is where the next one starts: resuming there reads the rest once
        let (rest, t2) = read_all(tsv, recs[1].1);
        assert_eq!(rest.len(), 1);
        assert_eq!((t2.malformed, t2.outside_root, t2.truncated), (1, 1, 1));
    }

    #[test]
    fn mtimes_match_to_the_second() {
        assert!(same_second(1_700_000_000, 1_700_000_000.999));
        assert!(!same_second(1_700_000_000, 1_700_000_001.5));
        // "1700000000.9999999990" parses to exactly 1700000001.0
        assert!(same_second(
            1_700_000_000,
            "1700000000.9999999990".parse().unwrap()
        ));
        assert!(same_second(0, -2.5));
    }

    #[test]
    fn maps_the_longest_prefix() {
        let maps = vec![
            (b"/media/old".to_vec(), b"/mnt/a".to_vec()),
            (b"/media/old/Disk".to_vec(), b"/mnt/disk".to_vec()),
        ];
        assert_eq!(map(b"/media/old/Disk/x", &maps).unwrap(), b"/mnt/disk/x");
        assert_eq!(map(b"/media/old/Other", &maps).unwrap(), b"/mnt/a/Other");
        assert_eq!(map(b"/media/old/Disk", &maps).unwrap(), b"/mnt/disk");
        assert_eq!(map(b"/media/older", &maps), None);
    }

    #[test]
    fn decodes_hex() {
        let h = unhex(&[b'a'; 64]).unwrap();
        assert_eq!(h, [0xaa; 32]);
        assert!(unhex(b"abc").is_none());
        assert!(unhex(&[b'g'; 64]).is_none());
    }
}
