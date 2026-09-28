//! `backdown daemon`: the long-running owner of the index.
//!
//! One worker thread runs jobs one at a time, so there is a single writer to the index and a
//! single reader per drive; every index job is followed by hashing, so each scan keeps provable
//! fingerprints for the time slider. A scheduler rescans drives when due, or when one that was away
//! comes back. The CLI talks to it over a Unix socket (line-delimited JSON), the browser over the
//! same local web UI as `backdown serve`. Under systemd it reports readiness, liveness and status.
//!
//! Being killed is safe: every job commits in batches and resumes where it stopped.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use backdown_core::{Db, fmt, hash, index, paths, report};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{locations, serve};

pub struct Options {
    pub db: PathBuf,
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub port: u16,
    pub rescan_every: Option<Duration>,
    /// system-wide service: files and the socket are shared with the `backdown` group
    pub shared: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Job {
    pub id: u64,
    pub kind: &'static str, // "index" | "hash"
    pub path: Option<String>,
    pub state: &'static str, // "queued" | "running" | "done" | "failed"
    pub progress: String,
    /// the same progress as numbers, for progress bars and time estimates
    pub counts: Option<Counts>,
    pub summary: String,
    pub error: Option<String>,
    /// queued by the scheduler rather than asked for
    pub auto: bool,
    /// why it was queued: "requested" | "scheduled" | "returned" (a drive came back) | "after-scan"
    pub reason: &'static str,
    pub queued_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    #[serde(skip)]
    opts: JobOpts,
}

/// Where a running job is. For a scan the totals are the drive's previous scan (an estimate); for
/// hashing they're exact.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Counts {
    pub stage: &'static str,
    pub files: u64,
    pub total_files: u64,
    pub bytes: u64,
    pub total_bytes: u64,
}

/// An indexed drive as the scheduler last saw it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Drive {
    pub path: String,
    pub present: bool,
    pub last_scan_at: Option<i64>,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct JobOpts {
    pub cross_filesystems: bool,
    pub allow_empty: bool,
    pub jobs_per_device: usize,
    pub retry_errors: bool,
}

#[derive(Default)]
struct State {
    next: u64,
    jobs: VecDeque<Job>,
    queue: VecDeque<u64>,
    /// bumped on every change a viewer would notice, so the web UI can wait for the next one
    /// instead of polling (progress counts at most once a second)
    version: u64,
    progress_bumped: Option<Instant>,
    drives: Vec<Drive>,
    checked_at: Option<i64>,
    /// a scan just finished: the scheduler rereads the drives now rather than at its next check
    recheck: bool,
}

pub struct Daemon {
    pub db: PathBuf,
    pub started_at: i64,
    pub rescan_every: Option<Duration>,
    state: Mutex<State>,
    wake: Condvar,
    changed: Condvar,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Daemon {
    pub fn enqueue(
        &self,
        kind: &'static str,
        path: Option<String>,
        reason: &'static str,
        opts: JobOpts,
    ) -> u64 {
        let auto = reason != "requested";
        let mut st = self.state.lock().unwrap();
        // the same job still waiting to run covers this request
        if let Some(j) = st
            .jobs
            .iter()
            .find(|j| j.state == "queued" && j.kind == kind && j.path == path)
        {
            return j.id;
        }
        st.next += 1;
        let id = st.next;
        if auto || kind == "index" {
            eprintln!(
                "queued {kind}{}{}",
                path.as_deref()
                    .map(|p| format!(" of {p}"))
                    .unwrap_or_default(),
                if !auto {
                    ""
                } else if kind == "hash" {
                    " (after a scan)"
                } else {
                    " (scheduled)"
                }
            );
        }
        st.jobs.push_back(Job {
            id,
            kind,
            path,
            state: "queued",
            progress: String::new(),
            counts: None,
            summary: String::new(),
            error: None,
            auto,
            reason,
            queued_at: now(),
            started_at: None,
            finished_at: None,
            opts,
        });
        st.queue.push_back(id);
        // keep a bounded history of finished jobs
        while st.jobs.len() > 200 {
            match st
                .jobs
                .iter()
                .position(|j| j.state == "done" || j.state == "failed")
            {
                Some(i) => {
                    st.jobs.remove(i);
                }
                None => break,
            }
        }
        st.version += 1;
        self.wake.notify_all();
        self.changed.notify_all();
        id
    }

    pub fn job(&self, id: u64) -> Option<Job> {
        self.state
            .lock()
            .unwrap()
            .jobs
            .iter()
            .find(|j| j.id == id)
            .cloned()
    }

    fn update(&self, id: u64, f: impl FnOnce(&mut Job)) {
        let mut st = self.state.lock().unwrap();
        if let Some(j) = st.jobs.iter_mut().find(|j| j.id == id) {
            f(j);
            st.version += 1;
            self.changed.notify_all();
        }
    }

    /// A running job's progress; wakes waiting viewers at most once a second.
    fn progress(&self, id: u64, msg: String, counts: Counts) {
        let mut st = self.state.lock().unwrap();
        if let Some(j) = st.jobs.iter_mut().find(|j| j.id == id) {
            j.progress = msg;
            j.counts = Some(counts);
            if st
                .progress_bumped
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
            {
                st.progress_bumped = Some(Instant::now());
                st.version += 1;
                self.changed.notify_all();
            }
        }
    }

    /// Wait until something changes after `since` (the `version` of a snapshot), or `timeout`.
    pub fn wait_change(&self, since: u64, timeout: Duration) {
        let st = self.state.lock().unwrap();
        let _ = self
            .changed
            .wait_timeout_while(st, timeout, |s| s.version == since)
            .unwrap();
    }

    fn set_drives(&self, drives: Vec<Drive>) {
        let mut st = self.state.lock().unwrap();
        st.checked_at = Some(now());
        if st.drives != drives {
            st.drives = drives;
            st.version += 1;
            self.changed.notify_all();
        }
    }

    fn drive(&self, path: &str) -> Option<Drive> {
        let st = self.state.lock().unwrap();
        st.drives.iter().find(|d| d.path == path).cloned()
    }

    fn pending_index(&self, path: &str) -> bool {
        self.state.lock().unwrap().jobs.iter().any(|j| {
            j.kind == "index"
                && j.path.as_deref() == Some(path)
                && (j.state == "queued" || j.state == "running")
        })
    }

    /// What the daemon is doing, for the CLI and the web UI.
    pub fn snapshot(&self) -> Value {
        let st = self.state.lock().unwrap();
        let current = st.jobs.iter().find(|j| j.state == "running");
        let queued: Vec<&Job> = st
            .queue
            .iter()
            .filter_map(|id| st.jobs.iter().find(|j| j.id == *id))
            .collect();
        let recent: Vec<&Job> = st
            .jobs
            .iter()
            .rev()
            .filter(|j| j.state == "done" || j.state == "failed")
            .take(30)
            .collect();
        // the plan: when each drive is scanned next, and why not if it isn't
        let now = now();
        let every = self.rescan_every.map(|d| d.as_secs() as i64);
        let drives: Vec<Value> = st
            .drives
            .iter()
            .map(|d| {
                let busy = |state: &str| {
                    st.jobs.iter().any(|j| {
                        j.kind == "index" && j.state == state && j.path.as_deref() == Some(&d.path)
                    })
                };
                let next_at = every.map(|e| d.last_scan_at.map_or(now, |t| t + e));
                let state = if busy("running") {
                    "scanning"
                } else if busy("queued") {
                    "queued"
                } else if !d.present {
                    "away"
                } else if next_at.is_some_and(|n| n <= now) {
                    "due"
                } else if every.is_none() {
                    "on-request"
                } else {
                    "scheduled"
                };
                json!({ "path": d.path, "present": d.present, "last_scan_at": d.last_scan_at,
                        "files": d.files, "bytes": d.bytes, "next_at": next_at, "state": state })
            })
            .collect();
        json!({
            "running": true,
            "version": env!("CARGO_PKG_VERSION"),
            "change": st.version,
            "now": now,
            "db": self.db.display().to_string(),
            "pid": std::process::id(),
            "started_at": self.started_at,
            "rescan_every_secs": self.rescan_every.map(|d| d.as_secs()),
            "checked_at": st.checked_at,
            "current": current,
            "queued": queued,
            "recent": recent,
            "drives": drives,
        })
    }
}

pub fn run(opts: Options) -> Result<()> {
    for dir in [&opts.state_dir, &opts.runtime_dir] {
        locations::ensure_private_dir(dir, opts.shared)
            .with_context(|| format!("can't create {}", dir.display()))?;
    }
    if let Some(parent) = opts.db.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Db::open(&opts.db)?; // create or migrate before anything reads it
    let db = dunce::canonicalize(&opts.db)?;
    let d = Arc::new(Daemon {
        db: db.clone(),
        started_at: now(),
        rescan_every: opts.rescan_every,
        state: Mutex::new(State::default()),
        wake: Condvar::new(),
        changed: Condvar::new(),
    });

    // claim the control socket first: if another daemon is running, stop before touching anything
    #[cfg(unix)]
    let socket = control::listen(&opts.runtime_dir.join(locations::SOCKET), opts.shared)?;

    // the local web UI, with a token that survives restarts so bookmarks keep working
    let ui_file = opts.state_dir.join(locations::UI_FILE);
    let token = existing_token(&ui_file).map_or_else(serve::new_token, Ok)?;
    let (server, port) = serve::bind(opts.port)?;
    write_private(
        &ui_file,
        &serde_json::to_vec_pretty(
            &json!({ "port": port, "token": token, "url": format!("http://127.0.0.1:{port}/") }),
        )?,
        opts.shared,
    )?;

    eprintln!(
        "backdown {} daemon: index {}, web UI on 127.0.0.1:{port} (`backdown open`)",
        env!("CARGO_PKG_VERSION"),
        db.display()
    );
    {
        let d = Arc::clone(&d);
        std::thread::Builder::new()
            .name("worker".into())
            .spawn(move || worker(&d))?;
    }
    {
        let d = Arc::clone(&d);
        std::thread::Builder::new()
            .name("scheduler".into())
            .spawn(move || scheduler(&d))?;
    }
    #[cfg(unix)]
    {
        let d = Arc::clone(&d);
        std::thread::Builder::new()
            .name("control".into())
            .spawn(move || control::serve(&d, socket))?;
    }
    notify::send("READY=1\nSTATUS=idle");
    serve::run(server, db, token, port, Some(d));
    Ok(())
}

fn existing_token(ui_file: &Path) -> Option<String> {
    let v: Value = serde_json::from_slice(&std::fs::read(ui_file).ok()?).ok()?;
    let t = v["token"].as_str()?;
    (t.len() >= 32 && t.chars().all(|c| c.is_ascii_hexdigit())).then(|| t.to_string())
}

/// Write a file only its owner (or, for the system service, the `backdown` group) can read.
fn write_private(path: &Path, data: &[u8], shared: bool) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(if shared { 0o640 } else { 0o600 });
        }
        #[cfg(not(unix))]
        let _ = shared;
        use std::io::Write;
        o.open(&tmp)?.write_all(data)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ---------- the worker: one job at a time, one writer to the index

fn worker(d: &Daemon) {
    let mut db = loop {
        match Db::open(&d.db) {
            Ok(db) => break db,
            Err(e) => {
                eprintln!("can't open the index ({e:#}); retrying in 30 s");
                std::thread::sleep(Duration::from_secs(30));
            }
        }
    };
    loop {
        let id = {
            let mut st = d.state.lock().unwrap();
            while st.queue.is_empty() {
                st = d.wake.wait(st).unwrap();
            }
            st.queue.pop_front().expect("checked non-empty")
        };
        let Some(job) = d.job(id) else { continue };
        d.update(id, |j| {
            j.state = "running";
            j.started_at = Some(now());
        });
        notify::send(&format!(
            "STATUS={}{}",
            job.kind,
            job.path
                .as_deref()
                .map(|p| format!(" {p}"))
                .unwrap_or_default()
        ));
        let result = match job.kind {
            "index" => run_index(d, &mut db, &job),
            _ => run_hash(d, &mut db, &job),
        };
        let ok = result.is_ok();
        match result {
            Ok(summary) => {
                eprintln!("done: {summary}");
                d.update(id, |j| {
                    j.state = "done";
                    j.summary = summary;
                    j.finished_at = Some(now());
                });
            }
            Err(e) => {
                eprintln!(
                    "failed: {} {}: {e:#}",
                    job.kind,
                    job.path.as_deref().unwrap_or("")
                );
                d.update(id, |j| {
                    j.state = "failed";
                    j.error = Some(format!("{e:#}"));
                    j.finished_at = Some(now());
                });
            }
        }
        if ok && job.kind == "index" {
            d.state.lock().unwrap().recheck = true;
            // hash after every scan, so this scan's versions keep a provable fingerprint
            d.enqueue(
                "hash",
                None,
                "after-scan",
                JobOpts {
                    jobs_per_device: 1,
                    ..JobOpts::default()
                },
            );
        }
        if d.state.lock().unwrap().queue.is_empty() {
            notify::send("STATUS=idle");
        }
    }
}

fn run_index(d: &Daemon, db: &mut Db, job: &Job) -> Result<String> {
    let path = PathBuf::from(job.path.as_deref().context("index job without a path")?);
    let opts = index::Options {
        one_file_system: !job.opts.cross_filesystems,
        allow_empty: job.opts.allow_empty,
    };
    // the drive's previous scan is the best guess at how far this one has to go
    let before = d.drive(&path.display().to_string());
    let st = index::index(db, &path, &opts, &mut |s| {
        let msg = format!("{} files, {}", fmt::count(s.files), fmt::bytes(s.bytes));
        let counts = Counts {
            stage: "scanning",
            files: s.files,
            total_files: before.as_ref().map_or(0, |b| b.files),
            bytes: s.bytes,
            total_bytes: before.as_ref().map_or(0, |b| b.bytes),
        };
        d.progress(job.id, msg, counts);
    })?;
    Ok(format!(
        "{}: {} files, {} (new {}, changed {}, gone {}) · scan {}{}",
        st.root.display(),
        fmt::count(st.files),
        fmt::bytes(st.bytes),
        st.new,
        st.changed,
        st.removed,
        st.scan,
        if st.errors > 0 {
            format!(" · {} unreadable entries", st.errors)
        } else {
            String::new()
        }
    ))
}

fn run_hash(d: &Daemon, db: &mut Db, job: &Job) -> Result<String> {
    let opts = hash::Options {
        jobs_per_device: job.opts.jobs_per_device.max(1),
        retry_errors: job.opts.retry_errors,
    };
    let st = hash::hash(db, &opts, &mut |p| {
        let pct = if p.total_bytes > 0 {
            100.0 * p.bytes as f64 / p.total_bytes as f64
        } else {
            100.0
        };
        let msg = format!(
            "{}: {}/{} files, {} of {} ({pct:.0}%)",
            p.stage,
            fmt::count(p.files),
            fmt::count(p.total_files),
            fmt::bytes(p.bytes),
            fmt::bytes(p.total_bytes)
        );
        let counts = Counts {
            stage: p.stage,
            files: p.files,
            total_files: p.total_files,
            bytes: p.bytes,
            total_bytes: p.total_bytes,
        };
        d.progress(job.id, msg, counts);
    })?;
    Ok(format!(
        "hashed: {} sampled, {} full, {} read{}",
        fmt::count(st.sampled),
        fmt::count(st.hashed),
        fmt::bytes(st.bytes_read),
        if st.errors > 0 {
            format!(", {} unreadable", st.errors)
        } else {
            String::new()
        }
    ))
}

// ---------- the scheduler: rescans, drive arrivals, watchdog

fn scheduler(d: &Daemon) {
    let watchdog = notify::watchdog();
    let tick = watchdog.map_or(Duration::from_secs(5), |w| {
        (w / 2).min(Duration::from_secs(5))
    });
    let mut present: HashMap<String, bool> = HashMap::new();
    let mut last_check: Option<Instant> = None;
    loop {
        if watchdog.is_some() {
            notify::send("WATCHDOG=1");
        }
        let recheck = std::mem::take(&mut d.state.lock().unwrap().recheck);
        if recheck || last_check.is_none_or(|t| t.elapsed() >= Duration::from_secs(30)) {
            last_check = Some(Instant::now());
            if let Err(e) = check_roots(d, &mut present) {
                eprintln!("scheduler: {e:#}");
            }
        }
        std::thread::sleep(tick);
    }
}

/// Queue a scan of each drive that is due (per `rescan_every`) or has just come back. A root whose
/// folder is there but empty, after holding files, counts as away: it's an unplugged drive's
/// leftover mount point, and scanning it would record every file as deleted.
fn check_roots(d: &Daemon, present: &mut HashMap<String, bool>) -> Result<()> {
    let db = Db::open_read_only(&d.db)?;
    let mut stmt = db.conn.prepare(
        "SELECT r.path, s.finished_at, COALESCE(s.files, 0), COALESCE(s.bytes, 0) FROM roots r LEFT JOIN scans s ON s.id = r.last_scan",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, Option<i64>>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    let now = now();
    let mut drives = Vec::new();
    for row in rows {
        let (path, last, files, bytes) = row?;
        let p = paths::from_bytes(&path);
        let key = p.display().to_string();
        let here = p.is_dir()
            && (files == 0 || std::fs::read_dir(&p).is_ok_and(|mut it| it.next().is_some()));
        drives.push(Drive {
            path: key.clone(),
            present: here,
            last_scan_at: last,
            files: files as u64,
            bytes: bytes as u64,
        });
        let was = present.insert(key.clone(), here);
        if !here {
            if was == Some(true) {
                eprintln!("{key} went away");
            }
            continue;
        }
        let back = was == Some(false);
        let due = d
            .rescan_every
            .is_some_and(|every| last.is_none_or(|t| now - t >= every.as_secs() as i64));
        if (back || due) && !d.pending_index(&key) {
            if back {
                eprintln!("{key} is back");
            }
            let reason = if back { "returned" } else { "scheduled" };
            d.enqueue("index", Some(key), reason, JobOpts::default());
        }
    }
    d.set_drives(drives);
    Ok(())
}

// ---------- requests from the CLI (and the UI's rescan button)

pub fn respond(d: &Daemon, req: &Value) -> Result<Value> {
    let ro = || Db::open_read_only(&d.db);
    Ok(match req["cmd"].as_str().unwrap_or("") {
        "hello" => {
            json!({ "ok": true, "version": env!("CARGO_PKG_VERSION"), "db": d.db.display().to_string(), "pid": std::process::id() })
        }
        "status" => json!({ "ok": true, "index": ro()?.status()?, "daemon": d.snapshot() }),
        "enqueue" => {
            let kind = match req["kind"].as_str() {
                Some("index") => "index",
                Some("hash") => "hash",
                _ => bail!("kind must be index or hash"),
            };
            let path = match kind {
                "index" => {
                    let p = req["path"].as_str().context("index needs a path")?;
                    if !Path::new(p).is_absolute() {
                        bail!("path must be absolute");
                    }
                    Some(p.to_string())
                }
                _ => None,
            };
            let opts = JobOpts {
                cross_filesystems: req["cross_filesystems"].as_bool().unwrap_or(false),
                allow_empty: req["allow_empty"].as_bool().unwrap_or(false),
                jobs_per_device: req["jobs"].as_u64().unwrap_or(1) as usize,
                retry_errors: req["retry_errors"].as_bool().unwrap_or(false),
            };
            json!({ "ok": true, "job": d.enqueue(kind, path, "requested", opts) })
        }
        "job" => json!({ "ok": true, "job": d.job(req["job"].as_u64().context("missing job")?) }),
        "check" => {
            let p = req["path"].as_str().context("check needs a path")?;
            json!({ "ok": true, "report": report::check(&ro()?, Path::new(p))? })
        }
        "dupes" => {
            let min = req["min"].as_u64().unwrap_or(1_000_000);
            let limit = req["limit"].as_u64().unwrap_or(20) as usize;
            json!({ "ok": true, "report": report::dupes(&ro()?, min, limit)? })
        }
        "scans" => json!({ "ok": true, "scans": report::scans(&ro()?)? }),
        other => bail!("unknown command {other:?}"),
    })
}

#[cfg(unix)]
mod control {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::time::Duration;

    use anyhow::{Context, Result, bail};
    use serde_json::{Value, json};

    use super::{Daemon, respond};

    pub fn listen(path: &Path, shared: bool) -> Result<UnixListener> {
        let len = path.as_os_str().len();
        if len > 100 {
            bail!(
                "the control socket path is too long for a Unix socket ({len} bytes, {}): set XDG_RUNTIME_DIR to a shorter folder",
                path.display()
            );
        }
        if path.exists() {
            if UnixStream::connect(path).is_ok() {
                bail!(
                    "another backdown daemon is already running ({})",
                    path.display()
                );
            }
            std::fs::remove_file(path)?; // left behind by a daemon that was killed
        }
        let l =
            UnixListener::bind(path).with_context(|| format!("can't create {}", path.display()))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(if shared { 0o660 } else { 0o600 }),
        )?;
        Ok(l)
    }

    pub fn serve(d: &std::sync::Arc<Daemon>, l: UnixListener) {
        for conn in l.incoming().flatten() {
            let d = std::sync::Arc::clone(d);
            std::thread::spawn(move || {
                let _ = handle(&d, conn);
            });
        }
    }

    fn handle(d: &Daemon, stream: UnixStream) -> Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut out = stream;
        let mut line = String::new();
        while reader.read_line(&mut line)? > 0 {
            let req: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
            line.clear();
            if req["cmd"] == "follow" {
                follow(d, &mut out, req["job"].as_u64().unwrap_or(0))?;
                continue;
            }
            let resp = respond(d, &req)
                .unwrap_or_else(|e| json!({ "ok": false, "error": format!("{e:#}") }));
            writeln!(out, "{resp}")?;
        }
        Ok(())
    }

    /// Stream a job's progress as it changes, ending with its final state.
    fn follow(d: &Daemon, out: &mut UnixStream, id: u64) -> Result<()> {
        let mut last = String::new();
        loop {
            let Some(job) = d.job(id) else {
                writeln!(
                    out,
                    "{}",
                    json!({ "ok": false, "error": format!("no job {id}") })
                )?;
                return Ok(());
            };
            let finished = job.state == "done" || job.state == "failed";
            let ev = json!({ "ok": true, "job": job });
            let s = ev.to_string();
            if s != last || finished {
                writeln!(out, "{s}")?;
                last = s;
            }
            if finished {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

/// systemd's notify protocol (readiness, watchdog, status), without a library. No-op elsewhere.
pub mod notify {
    use std::time::Duration;

    pub fn watchdog() -> Option<Duration> {
        let usec: u64 = std::env::var("WATCHDOG_USEC").ok()?.parse().ok()?;
        (usec > 0).then(|| Duration::from_micros(usec))
    }

    #[cfg(target_os = "linux")]
    pub fn send(msg: &str) {
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::{SocketAddr, UnixDatagram};
        let Some(path) = std::env::var_os("NOTIFY_SOCKET") else {
            return;
        };
        let Ok(sock) = UnixDatagram::unbound() else {
            return;
        };
        let bytes = path.as_encoded_bytes();
        let _ = match bytes.strip_prefix(b"@") {
            Some(name) => SocketAddr::from_abstract_name(name)
                .and_then(|a| sock.send_to_addr(msg.as_bytes(), &a)),
            None => sock.send_to(msg.as_bytes(), &path),
        };
    }

    #[cfg(not(target_os = "linux"))]
    pub fn send(_: &str) {}
}

/// "30m", "12h", "7d", "3600s" or "0"/"never" (no scheduled rescans).
pub fn parse_every(s: &str) -> Result<Option<Duration>> {
    let s = s.trim().to_ascii_lowercase();
    if s == "0" || s == "never" || s == "off" {
        return Ok(None);
    }
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num
        .parse()
        .with_context(|| format!("not a duration: {s:?}"))?;
    let secs = match unit {
        "" | "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => bail!("unknown duration unit in {s:?} (use m, h or d)"),
    };
    Ok(Some(Duration::from_secs(secs)))
}

#[cfg(test)]
mod tests {
    use super::parse_every;
    use std::time::Duration;

    #[test]
    fn durations() {
        assert_eq!(
            parse_every("24h").unwrap(),
            Some(Duration::from_secs(86_400))
        );
        assert_eq!(
            parse_every("90m").unwrap(),
            Some(Duration::from_secs(5_400))
        );
        assert_eq!(parse_every("never").unwrap(), None);
        assert!(parse_every("3w").is_err());
    }
}
