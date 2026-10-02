//! `backdown daemon`: the long-running owner of the index.
//!
//! One worker thread runs jobs one at a time, so there is a single writer to the index and a
//! single reader per drive; every index job is followed by hashing, so each scan keeps provable
//! fingerprints for the time slider. A scheduler rescans drives when due, or when one that was away
//! comes back. A health thread reads every drive's SMART data (health.rs) every six hours and when
//! a drive appears, keeps what's new, and notifies when a drive, a mirror or a scheduled job goes
//! wrong. Other machines (remote/) are synced over ssh in the side lane, on a schedule. The CLI
//! talks to it over a Unix socket (line-delimited JSON), the browser over the same local web UI as
//! `backdown serve`. Under systemd it reports readiness, liveness and status.
//!
//! Being killed is safe: every job commits in batches and resumes where it stopped.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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
    pub host: String,
    pub rescan_every: Option<Duration>,
    /// system-wide service: files and the socket are shared with the `backdown` group
    pub shared: bool,
    /// --rescan-every was given (it wins over Settings at startup)
    pub rescan_flag: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Job {
    pub id: u64,
    pub kind: &'static str, // "index" | "hash" | "entities"
    pub path: Option<String>,
    pub state: &'static str, // "queued" | "running" | "done" | "failed" | "stopped"
    /// it can be stopped while it runs (and resumed later where that makes sense)
    pub stoppable: bool,
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
    pub(crate) opts: JobOpts,
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
    /// how its last scan was made: the method (walk | journal | table), and for people
    pub how: Option<(String, String)>,
}

#[derive(Clone, Debug, Default)]
pub struct JobOpts {
    pub cross_filesystems: bool,
    pub allow_empty: bool,
    pub jobs_per_device: usize,
    pub retry_errors: bool,
    /// apply: the plan to carry out
    pub plan: Option<i64>,
    /// mirror-sync / mirror-verify: which mirror
    pub mirror: Option<i64>,
    /// wipe: which recorded wipe, and whether to trust the index instead of re-reading
    pub wipe: Option<i64>,
    pub trust_index: bool,
    /// rescue: which recorded rescue
    pub rescue: Option<i64>,
    /// remote-sync: which machine (remote/)
    pub machine: Option<i64>,
    /// index: how to make the scan (Auto: the change log, the file table, or a walk)
    pub method: index::Method,
    /// dedup: which recorded dedup run (tidying an archive); mail-merge: which recorded merge
    pub dedup: Option<i64>,
    pub merge: Option<i64>,
}

/// Jobs in the main lane that can be stopped at their next file (and carried on later).
const STOPPABLE: &[&str] = &["dedup", "mail-merge"];

/// Jobs that can run for hours or days and hardly touch the index (copying a failing drive,
/// searching a card for lost files, syncing another machine over ssh): they go in a lane of their
/// own, one at a time, so scans and hashing carry on meanwhile, and they can be stopped.
const SIDE: &[&str] = &["rescue", "remote-sync"];

#[derive(Default)]
struct State {
    next: u64,
    jobs: VecDeque<Job>,
    queue: VecDeque<u64>,
    /// the side lane's queue (see `SIDE`)
    side: VecDeque<u64>,
    /// how to ask each running stoppable job to stop
    stops: HashMap<u64, Arc<AtomicBool>>,
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
    /// the machine-wide service: its sandbox reads only, so it never wipes (read by `respond`,
    /// which only the Unix control socket calls so far)
    #[cfg_attr(not(unix), allow(dead_code))]
    pub shared: bool,
    /// changeable while it runs (Settings)
    rescan_every: Mutex<Option<Duration>>,
    /// drive health, read in a thread of its own (health.rs), and notifications
    pub health: crate::health::Monitor,
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
    pub fn rescan_every(&self) -> Option<Duration> {
        *self.rescan_every.lock().unwrap()
    }

    /// A new rescan interval (Settings): the scheduler uses it at its next look.
    pub fn set_rescan_every(&self, every: Option<Duration>) {
        *self.rescan_every.lock().unwrap() = every;
        let mut st = self.state.lock().unwrap();
        st.recheck = true;
        st.version += 1;
        self.changed.notify_all();
    }

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
                match reason {
                    "after-scan" => " (after a scan)",
                    "after-apply" => " (after copying into it)",
                    "after-wipe" => " (after wiping)",
                    "after-rescue" => " (what a rescue found)",
                    "after-dedup" => " (after linking duplicates in it)",
                    "after-merge" => " (after merging mail into it)",
                    "scheduled" => " (scheduled)",
                    "returned" => " (the drive is back)",
                    _ => "",
                }
            );
        }
        st.jobs.push_back(Job {
            id,
            kind,
            path,
            state: "queued",
            stoppable: SIDE.contains(&kind) || STOPPABLE.contains(&kind),
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
        if SIDE.contains(&kind) {
            st.side.push_back(id);
        } else {
            st.queue.push_back(id);
        }
        // keep a bounded history of finished jobs
        while st.jobs.len() > 200 {
            match st
                .jobs
                .iter()
                .position(|j| matches!(j.state, "done" | "failed" | "stopped"))
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

    /// Stop job `id`: taken off the queue if it hasn't started; asked to stop at its next
    /// checkpoint if it's running and can be stopped.
    pub fn stop(&self, id: u64) -> Result<&'static str> {
        let mut st = self.state.lock().unwrap();
        let queued = st.queue.iter().position(|x| *x == id).map(|i| (false, i));
        if let Some((side, i)) =
            queued.or_else(|| st.side.iter().position(|x| *x == id).map(|i| (true, i)))
        {
            if side {
                st.side.remove(i);
            } else {
                st.queue.remove(i);
            }
            let mut rescue = None;
            if let Some(j) = st.jobs.iter_mut().find(|j| j.id == id) {
                j.state = "stopped";
                j.summary = "taken off the queue".into();
                j.finished_at = Some(now());
                rescue = j.opts.rescue;
            }
            st.version += 1;
            self.changed.notify_all();
            drop(st);
            // a rescue's record says so too, so it can be started again later
            if let Some(r) = rescue {
                let _ = Db::open(&self.db).and_then(|db| {
                    backdown_core::rescue::set(
                        &db,
                        r,
                        "stopped",
                        Some("taken off the queue before it started"),
                        None,
                    )
                });
            }
            return Ok("taken off the queue");
        }
        match st.stops.get(&id) {
            Some(flag) => {
                flag.store(true, Ordering::Relaxed);
                Ok("stopping")
            }
            None => bail!("job {id} isn't waiting, and can't be stopped while it runs"),
        }
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
    pub(crate) fn progress(&self, id: u64, msg: String, counts: Counts) {
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

    /// Something a viewer would notice changed outside the job queue (drive health, notifications).
    pub fn touch(&self) {
        self.state.lock().unwrap().version += 1;
        self.changed.notify_all();
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

    /// A job of `kind` for `path` is waiting or running.
    pub fn pending(&self, kind: &str, path: &str) -> bool {
        self.state.lock().unwrap().jobs.iter().any(|j| {
            j.kind == kind
                && j.path.as_deref() == Some(path)
                && (j.state == "queued" || j.state == "running")
        })
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
        let current = st
            .jobs
            .iter()
            .find(|j| j.state == "running" && !SIDE.contains(&j.kind));
        // the side lane's running job (a rescue), alongside the main one
        let side = st
            .jobs
            .iter()
            .find(|j| j.state == "running" && SIDE.contains(&j.kind));
        let queued: Vec<&Job> = st
            .queue
            .iter()
            .chain(st.side.iter())
            .filter_map(|id| st.jobs.iter().find(|j| j.id == *id))
            .collect();
        let recent: Vec<&Job> = st
            .jobs
            .iter()
            .rev()
            .filter(|j| matches!(j.state, "done" | "failed" | "stopped"))
            .take(30)
            .collect();
        // the plan: when each drive is scanned next, and why not if it isn't
        let now = now();
        let every = self.rescan_every().map(|d| d.as_secs() as i64);
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
                        "files": d.files, "bytes": d.bytes, "next_at": next_at, "state": state,
                        "last_method": d.how.as_ref().map(|h| &h.0),
                        "last_how": d.how.as_ref().map(|h| &h.1) })
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
            "rescan_every_secs": self.rescan_every().map(|d| d.as_secs()),
            "checked_at": st.checked_at,
            "current": current,
            "side": side,
            "queued": queued,
            "recent": recent,
            "drives": drives,
            "health": self.health.snapshot(),
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
    crate::settings::init(&opts.state_dir, opts.shared, opts.rescan_flag);
    let d = Arc::new(Daemon {
        db: db.clone(),
        started_at: now(),
        shared: opts.shared,
        rescan_every: Mutex::new(opts.rescan_every),
        health: crate::health::Monitor::new(
            crate::health::source_from_env(),
            &opts.state_dir,
            !opts.shared,
        ),
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
    // after an update on Windows the old daemon may still hold the port for a moment
    let (servers, port) = {
        let mut tries = if std::env::var_os("BACKDOWN_BIND_WAIT").is_some() {
            40
        } else {
            1
        };
        loop {
            match serve::bind_all(&opts.host, opts.port) {
                Ok(x) => break x,
                Err(_) if tries > 1 => {
                    tries -= 1;
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(e) => return Err(e),
            }
        }
    };
    // with --host, the address other devices use (`backdown token` adds the token; the
    // log doesn't, since more people can read a log than this private file)
    let network = (!serve::local_host(&opts.host))
        .then(|| format!("http://{}:{port}/", serve::network_name(&opts.host)));
    write_private(
        &ui_file,
        &serde_json::to_vec_pretty(&json!({
            "port": port, "token": token, "url": format!("http://{}:{port}/", opts.host),
            "network_url": network,
        }))?,
        opts.shared,
    )?;
    if let Some(n) = &network {
        eprintln!(
            "from other devices (look only): {n} (the link with its token: `backdown token --links`)"
        );
    }

    eprintln!(
        "backdown {} daemon: index {}, web UI on {}:{port} (`backdown open`)",
        env!("CARGO_PKG_VERSION"),
        db.display(),
        opts.host
    );
    // once, after an upgrade: the indexes that make big indexes quick (folders, duplicates),
    // first in the queue. Minutes on a big index, so they're a job once the service is up rather
    // than part of opening the index (which systemd would time out); until then the views they
    // speed up read the tables instead.
    if !Db::open_read_only(&db)?.missing_later_indexes()?.is_empty() {
        d.enqueue("indexes", None, "scheduled", JobOpts::default());
    }
    {
        let d = Arc::clone(&d);
        std::thread::Builder::new()
            .name("worker".into())
            .spawn(move || worker(&d))?;
    }
    {
        let d = Arc::clone(&d);
        std::thread::Builder::new()
            .name("side".into())
            .spawn(move || side_worker(&d))?;
    }
    {
        let d = Arc::clone(&d);
        std::thread::Builder::new()
            .name("scheduler".into())
            .spawn(move || scheduler(&d))?;
    }
    {
        let d = Arc::clone(&d);
        std::thread::Builder::new()
            .name("health".into())
            .spawn(move || crate::health::run(&d))?;
    }
    #[cfg(unix)]
    {
        let d = Arc::clone(&d);
        std::thread::Builder::new()
            .name("control".into())
            .spawn(move || control::serve(&d, socket))?;
    }
    crate::update::spawn_checker(opts.shared);
    notify::send("READY=1\nSTATUS=idle");
    serve::run(
        servers,
        db,
        token,
        port,
        Some(d),
        !opts.shared,
        serve::extra_hosts(&opts.host, port),
    );
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

// ---------- the side lane: long jobs that hardly write to the index, one at a time, stoppable

fn side_worker(d: &Daemon) {
    loop {
        let id = {
            let mut st = d.state.lock().unwrap();
            while st.side.is_empty() {
                st = d.wake.wait(st).unwrap();
            }
            st.side.pop_front().expect("checked non-empty")
        };
        let Some(job) = d.job(id) else { continue };
        let stop = Arc::new(AtomicBool::new(false));
        d.state.lock().unwrap().stops.insert(id, Arc::clone(&stop));
        d.update(id, |j| {
            j.state = "running";
            j.started_at = Some(now());
        });
        // its own connection: the rescue record's short writes wait their turn behind the worker's
        let result = Db::open(&d.db).and_then(|db| match job.kind {
            "rescue" => run_rescue(d, &db, &job, &stop),
            "remote-sync" => run_remote_sync(d, &db, &job, &stop),
            k => bail!("{k} doesn't run in the side lane"),
        });
        d.state.lock().unwrap().stops.remove(&id);
        let stopped = stop.load(Ordering::Relaxed);
        crate::health::after_job(
            d,
            &job,
            result.as_ref().err().map(|e| format!("{e:#}")).as_deref(),
        );
        match result {
            Ok(summary) => {
                eprintln!("{}: {summary}", if stopped { "stopped" } else { "done" });
                d.update(id, |j| {
                    j.state = if stopped { "stopped" } else { "done" };
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
    }
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
        // tidying and merging can be stopped at their next file
        let stop = Arc::new(AtomicBool::new(false));
        if STOPPABLE.contains(&job.kind) {
            d.state.lock().unwrap().stops.insert(id, Arc::clone(&stop));
        }
        let result = match job.kind {
            "index" => run_index(d, &mut db, &job),
            "entities" => run_entities(d, &db, &job),
            "apply" => run_apply(d, &mut db, &job),
            "mirror-sync" | "mirror-verify" => run_mirror(d, &db, &job),
            "wipe" => run_wipe(d, &mut db, &job),
            "dedup" => crate::tidy::run_dedup_job(d, &db, &job, &stop),
            "mail-merge" => crate::tidy::run_merge_job(d, &db, &job, &stop),
            "indexes" => run_indexes(d, &db, &job),
            _ => run_hash(d, &mut db, &job),
        };
        d.state.lock().unwrap().stops.remove(&id);
        serve::forget_index_status();
        let stopped = stop.load(Ordering::Relaxed);
        let ok = result.is_ok();
        // a mirror that no longer matches, or a scheduled job that failed: tell someone
        crate::health::after_job(
            d,
            &job,
            result.as_ref().err().map(|e| format!("{e:#}")).as_deref(),
        );
        match result {
            Ok(summary) => {
                eprintln!("{}: {summary}", if stopped { "stopped" } else { "done" });
                d.update(id, |j| {
                    j.state = if stopped { "stopped" } else { "done" };
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
            // hash after every scan, so this scan's versions keep a provable fingerprint (with
            // the readers per drive, and retries, from Settings)
            let s = crate::settings::load();
            d.enqueue(
                "hash",
                None,
                "after-scan",
                JobOpts {
                    jobs_per_device: s.hash_jobs.max(1),
                    retry_errors: s.hash_retry_errors,
                    ..JobOpts::default()
                },
            );
        }
        if job.kind == "wipe" {
            // rescan what was wiped (and where things were moved), so the index sees it
            if let Some(id) = job.opts.wipe
                && let Ok((paths, move_to, _)) = backdown_core::wipe::load(&db, id)
            {
                let mut roots: Vec<(String, bool)> = Vec::new();
                for (p, wiped) in paths
                    .iter()
                    .map(|p| (p, true))
                    .chain(move_to.iter().map(|m| (m, false)))
                {
                    if let Some(r) = root_holding(&db, p) {
                        match roots.iter_mut().find(|(x, _)| *x == r) {
                            Some(e) => e.1 |= wiped,
                            None => roots.push((r, wiped)),
                        }
                    }
                }
                for (r, wiped) in roots {
                    // a drive this wipe emptied is empty on purpose: the scan mustn't take it for
                    // an unplugged drive's bare mount point (it's still there, or it wouldn't count)
                    let emptied = wiped && ok && Path::new(&r).is_dir();
                    d.enqueue(
                        "index",
                        Some(r),
                        "after-wipe",
                        JobOpts {
                            allow_empty: emptied,
                            ..JobOpts::default()
                        },
                    );
                }
            }
        }
        if job.kind == "apply" {
            // the archive's copies only count as proof once its drive is indexed and hashed again
            if let Some(root) = job.opts.plan.and_then(|id| archive_root(&db, id)) {
                d.enqueue("index", Some(root), "after-apply", JobOpts::default());
            }
        }
        // linked or merged files are new to the index: rescan where they are
        if let Some((place, reason)) = crate::tidy::changed_place(&db, &job)
            && let Some(root) = root_holding(&db, &place)
        {
            d.enqueue("index", Some(root), reason, JobOpts::default());
        }
        if ok && job.kind == "hash" {
            // then say what the drives hold, now that duplicates are known
            d.enqueue("entities", None, "after-scan", JobOpts::default());
            // and have other machines hash what now matches a file here by sample (remote/)
            crate::remote::after_hash(d, &db);
        }
        if d.state.lock().unwrap().queue.is_empty() {
            notify::send("STATUS=idle");
        }
    }
}

/// Carry out a filing plan: journaled, verified copies (see backdown_core::apply).
fn run_apply(d: &Daemon, db: &mut Db, job: &Job) -> Result<String> {
    use backdown_core::apply;
    let id = job.opts.plan.context("apply without a plan")?;
    let st = apply::apply(db, id, false, &mut |p| {
        let counts = Counts {
            stage: "copying",
            files: p.files,
            total_files: 0,
            bytes: p.bytes,
            total_bytes: p.total_bytes,
        };
        d.progress(
            job.id,
            format!(
                "{}/{} {} · {} files, {}",
                p.item,
                p.items,
                p.what,
                fmt::count(p.files),
                fmt::bytes(p.bytes)
            ),
            counts,
        );
    })?;
    let mut s = format!(
        "plan {id}: copied and verified {} files ({}); {} already there, {} duplicates, {} resumed",
        fmt::count(st.copied),
        fmt::bytes(st.copied_bytes),
        fmt::count(st.already_there),
        fmt::count(st.duplicates),
        fmt::count(st.resumed)
    );
    if st.merged > 0 {
        s += &format!("; {} messages in merged mailboxes", fmt::count(st.merged));
    }
    if st.renamed > 0 {
        s += &format!(
            "; {} renamed where the archive's drive takes names for one",
            fmt::count(st.renamed)
        );
    }
    if st.extras_not_kept > 0 {
        s += &format!(
            "; {} without some streams or attributes (see the journal)",
            fmt::count(st.extras_not_kept)
        );
    }
    if st.conflicts + st.failed > 0 {
        s += &format!(
            "; {} conflicts and {} failures need a look",
            st.conflicts, st.failed
        );
    }
    Ok(s)
}

/// Sync a mirror, or re-read a sample of it.
fn run_mirror(d: &Daemon, db: &Db, job: &Job) -> Result<String> {
    use backdown_core::mirror;
    let id = job.opts.mirror.context("mirror job without a mirror")?;
    let mut tick = |n: u64, b: u64| {
        let counts = Counts {
            stage: if job.kind == "mirror-sync" {
                "copying"
            } else {
                "re-reading"
            },
            files: n,
            bytes: b,
            ..Counts::default()
        };
        d.progress(
            job.id,
            format!("{} files, {}", fmt::count(n), fmt::bytes(b)),
            counts,
        );
    };
    if job.kind == "mirror-sync" {
        let st = mirror::sync(db, id, &mut tick)?;
        Ok(format!(
            "mirror {id}: {} copied ({}), {} updated, {} unchanged{}",
            fmt::count(st.copied),
            fmt::bytes(st.copied_bytes),
            fmt::count(st.updated),
            fmt::count(st.unchanged),
            if st.failed > 0 {
                format!(", {} failed", st.failed)
            } else {
                String::new()
            }
        ))
    } else {
        let st = mirror::verify(db, id, 5, &mut tick)?;
        let bad = st.missing + st.differs + st.unreadable;
        if bad > 0 {
            bail!(
                "mirror {id}: {} missing, {} different and {} unreadable of {} files re-read: the mirror drive needs attention",
                st.missing,
                st.differs,
                st.unreadable,
                st.checked
            );
        }
        Ok(format!(
            "mirror {id}: re-read {} files ({}), all identical",
            fmt::count(st.checked),
            fmt::bytes(st.checked_bytes)
        ))
    }
}

/// Sync another machine over ssh (remote/); then hash here if what's cached there changed, since
/// files here may now match files there.
fn run_remote_sync(d: &Daemon, db: &Db, job: &Job, stop: &AtomicBool) -> Result<String> {
    let id = job.opts.machine.context("a sync without a machine")?;
    let (summary, changed) = crate::remote::sync_job(db, id, stop, &mut |msg| {
        d.progress(
            job.id,
            msg.to_string(),
            Counts {
                stage: "syncing",
                ..Counts::default()
            },
        );
    })?;
    if changed && !stop.load(Ordering::Relaxed) {
        d.enqueue(
            "hash",
            None,
            "after-sync",
            JobOpts {
                jobs_per_device: 1,
                ..JobOpts::default()
            },
        );
    }
    Ok(summary)
}

/// Wipe what a recorded wipe selected (see backdown_core::wipe for the rules). Copies on other
/// machines count only once that machine has re-read them (remote/).
fn run_wipe(d: &Daemon, db: &mut Db, job: &Job) -> Result<String> {
    let id = job.opts.wipe.context("wipe without a selection")?;
    let mut ssh = crate::remote::verifier_connect();
    let mut verifier = ssh
        .as_mut()
        .map(|s| backdown_core::remote::Verifier::new(s));
    let st = backdown_core::wipe::wipe_with(
        db,
        id,
        job.opts.trust_index,
        Some(&d.db),
        verifier.as_mut(),
        &mut |n, b| {
            let counts = Counts {
                stage: "wiping",
                files: n,
                bytes: b,
                ..Counts::default()
            };
            d.progress(
                job.id,
                format!("{} files, {}", fmt::count(n), fmt::bytes(b)),
                counts,
            );
        },
    )?;
    Ok(format!(
        "deleted {} files ({}), {} moved first{}; left {} changed, {} with no copy, {} unreadable{}",
        fmt::count(st.deleted),
        fmt::bytes(st.deleted_bytes),
        fmt::count(st.moved),
        if st.remote > 0 {
            format!(
                ", {} kept only on another machine (re-read there first)",
                fmt::count(st.remote)
            )
        } else {
            String::new()
        },
        fmt::count(st.left_changed),
        fmt::count(st.left_no_copy),
        fmt::count(st.left_unreadable),
        if st.failed > 0 {
            format!("; {} failed", st.failed)
        } else {
            String::new()
        }
    ))
}

/// Carry out a recorded rescue (see backdown_core::rescue), keeping its record up to date: what
/// it's doing while it runs, then how it ended and what it found.
fn run_rescue(d: &Daemon, db: &Db, job: &Job, stop: &AtomicBool) -> Result<String> {
    use backdown_core::rescue;
    let id = job.opts.rescue.context("rescue without a record")?;
    let r = rescue::load(db, id)?;
    rescue::set(db, id, "running", None, None)?;
    let out = match r.mode.as_str() {
        "image" => rescue_image(d, db, job, &r, stop),
        "parts" => rescue_parts(d, job, &r, stop),
        "carve" => rescue_carve(d, db, job, &r, stop),
        "files" => rescue_files(d, db, job, &r, stop),
        m => Err(anyhow::anyhow!("{m} can't run yet")),
    };
    match out {
        Ok((summary, result)) => {
            let state = if stop.load(Ordering::Relaxed) {
                "stopped"
            } else {
                "done"
            };
            rescue::set(db, id, state, Some(&summary), Some(&result))?;
            Ok(summary)
        }
        Err(e) => {
            let _ = rescue::set(db, id, "failed", Some(&format!("{e:#}")), None);
            Err(e)
        }
    }
}

/// Find files by their content and save them, sorted by kind; then index them so they open in the
/// browser. Stopping and running it again carries on from where it got to.
fn rescue_carve(
    d: &Daemon,
    db: &Db,
    job: &Job,
    r: &backdown_core::rescue::Rescue,
    stop: &AtomicBool,
) -> Result<(String, Value)> {
    use backdown_core::rescue::{self, carve};
    let src = PathBuf::from(&r.source);
    let dest = PathBuf::from(
        r.dest
            .as_deref()
            .context("found files with nowhere to go")?,
    );
    let kinds: Option<Vec<String>> = r.options["kinds"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .filter(|k| !k.is_empty() && !carve::GROUPS.iter().all(|g| k.iter().any(|x| x == g)));
    let opts = carve::Options {
        kinds,
        start: r.options["start"].as_u64().unwrap_or(0),
        end: r.options["end"].as_u64(),
        ..carve::Options::default()
    };
    // what's been found so far, on the record now and then for whoever has the Rescue view open
    let mut kept = Instant::now();
    let st = carve::scan_and_save(
        &src,
        &opts,
        &dest,
        &mut |p| {
            d.progress(
                job.id,
                format!(
                    "{} of {} looked through · {} found ({})",
                    fmt::bytes(p.done),
                    fmt::bytes(p.size),
                    fmt::count(p.found),
                    fmt::bytes(p.found_bytes)
                ),
                Counts {
                    stage: "looking",
                    files: p.found,
                    total_files: 0,
                    bytes: p.done,
                    total_bytes: p.size,
                },
            );
            if kept.elapsed() >= Duration::from_secs(15) {
                kept = Instant::now();
                let so_far = json!({ "pos": p.pos, "found": p.found, "found_bytes": p.found_bytes, "by_group": p.by_group });
                let _ = rescue::set(db, r.id, "running", None, Some(&so_far));
            }
        },
        stop,
    )?;
    // where to carry on from: the end of what was looked through, on a sector
    let pos = opts.start + st.scanned / opts.sector * opts.sector;
    let result = json!({
        "pos": pos, "found": st.found, "found_bytes": st.found_bytes, "by_group": st.by_group,
        "incomplete": st.incomplete, "unreadable": st.unreadable, "problems": st.problems,
    });
    if st.found > 0 {
        // the recovered files open in the browser once they're indexed: the drive they're on, if
        // it's indexed, else the folder by itself
        let target = root_holding(db, &dest).unwrap_or_else(|| dest.display().to_string());
        d.enqueue("index", Some(target), "after-rescue", JobOpts::default());
    }
    let summary = format!(
        "{}found {} file{} ({}){}",
        if st.stopped { "stopped: " } else { "" },
        fmt::count(st.found),
        if st.found == 1 { "" } else { "s" },
        fmt::bytes(st.found_bytes),
        if st.incomplete > 0 {
            format!(", {} of them cut off or damaged", fmt::count(st.incomplete))
        } else {
            String::new()
        }
    );
    Ok((summary, result))
}

/// Copy chosen files and folders (deleted ones too) out of a filesystem on a drive or image,
/// reading it through fsread, each copy checked by reading it back; then index them so they open
/// in the browser. Stopping and running it again leaves what was copied as it is.
fn rescue_files(
    d: &Daemon,
    db: &Db,
    job: &Job,
    r: &backdown_core::rescue::Rescue,
    stop: &AtomicBool,
) -> Result<(String, Value)> {
    use backdown_core::rescue::files;
    let dest = PathBuf::from(r.dest.as_deref().context("files with nowhere to go")?);
    let opts: files::Options = serde_json::from_value(r.options.clone())?;
    let sum = files::copy_out(
        Path::new(&r.source),
        &opts,
        &dest,
        &mut |p| {
            d.progress(
                job.id,
                format!(
                    "{} of {} files copied ({} of {})",
                    fmt::count(p.files),
                    fmt::count(p.total_files),
                    fmt::bytes(p.bytes),
                    fmt::bytes(p.total_bytes)
                ),
                Counts {
                    stage: "copying",
                    files: p.files,
                    total_files: p.total_files,
                    bytes: p.bytes,
                    total_bytes: p.total_bytes,
                },
            )
        },
        stop,
    )?;
    if sum.files > 0 {
        let target = root_holding(db, &dest).unwrap_or_else(|| dest.display().to_string());
        d.enqueue("index", Some(target), "after-rescue", JobOpts::default());
    }
    let summary = format!(
        "{}copied {} file{} ({}){}{}",
        if sum.stopped { "stopped: " } else { "" },
        fmt::count(sum.files),
        if sum.files == 1 { "" } else { "s" },
        fmt::bytes(sum.bytes),
        if sum.partly > 0 {
            format!(
                ", {} of them deleted and partly overwritten",
                fmt::count(sum.partly)
            )
        } else {
            String::new()
        },
        if sum.failed > 0 {
            format!("; {} couldn't be copied", fmt::count(sum.failed))
        } else {
            String::new()
        }
    );
    Ok((summary, serde_json::to_value(&sum)?))
}

/// Look for file systems the partition table doesn't list (reads only).
fn rescue_parts(
    d: &Daemon,
    job: &Job,
    r: &backdown_core::rescue::Rescue,
    stop: &AtomicBool,
) -> Result<(String, Value)> {
    use backdown_core::rescue::parts;
    let opts = parts::Options {
        deep: r.options["deep"].as_bool().unwrap_or(false),
        ..parts::Options::default()
    };
    let s = parts::search(
        Path::new(&r.source),
        &opts,
        &mut |p| {
            d.progress(
                job.id,
                format!(
                    "{}: {} of {} read · {} found",
                    if opts.deep {
                        "reading every sector"
                    } else {
                        "looking where partitions start"
                    },
                    fmt::bytes(p.pos),
                    fmt::bytes(p.size),
                    fmt::count(p.found)
                ),
                Counts {
                    stage: "searching",
                    files: p.found,
                    total_files: 0,
                    bytes: p.pos,
                    total_bytes: p.size,
                },
            )
        },
        stop,
    )?;
    let lost = s.found.iter().filter(|f| !f.in_table && !f.nested).count();
    let summary = format!(
        "{}{} file system{} found{}",
        if s.stopped { "stopped: " } else { "" },
        s.found.len(),
        if s.found.len() == 1 { "" } else { "s" },
        if lost > 0 {
            format!(", {lost} not in the partition table")
        } else {
            String::new()
        }
    );
    Ok((summary, serde_json::to_value(&s)?))
}

/// Copy a (failing) drive into an image, ddrescue-style; stopping and running it again carries on.
fn rescue_image(
    d: &Daemon,
    db: &Db,
    job: &Job,
    r: &backdown_core::rescue::Rescue,
    stop: &AtomicBool,
) -> Result<(String, Value)> {
    use backdown_core::rescue::{self, image};
    let src = PathBuf::from(&r.source);
    let dest = PathBuf::from(r.dest.as_deref().context("a copy with nowhere to go")?);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    }
    let map = rescue::map_path(&dest);
    let device = rescue::access(&src).device;
    let opts = image::Options {
        retries: r.options["retries"].as_u64().unwrap_or(1) as u32,
        // an image file is usually a copy made earlier, not the failing drive itself
        allow_same_drive: !device,
        // Linux reads a drive a 4 KiB page at a time: smaller sectors only multiply failed reads
        sector: if device && cfg!(target_os = "linux") {
            4096
        } else {
            512
        },
        ..image::Options::default()
    };
    let mut kept = Instant::now();
    let sum = image::image(
        &src,
        &dest,
        &map,
        &opts,
        &mut |p| {
            let bad = if p.bad > 0 {
                format!(
                    " · {} unreadable in {} place{}",
                    fmt::bytes(p.bad),
                    fmt::count(p.bad_areas),
                    if p.bad_areas == 1 { "" } else { "s" }
                )
            } else {
                String::new()
            };
            d.progress(
                job.id,
                format!(
                    "{}: {} of {} copied{bad} · {}/s",
                    p.pass,
                    fmt::bytes(p.rescued),
                    fmt::bytes(p.size),
                    fmt::bytes(p.read_rate as u64)
                ),
                Counts {
                    stage: p.pass,
                    files: p.bad_areas,
                    total_files: 0,
                    bytes: p.size - p.todo,
                    total_bytes: p.size,
                },
            );
            // the record's strip, now and then, for whoever has the Rescue view open
            if kept.elapsed() >= Duration::from_secs(30) {
                kept = Instant::now();
                if let Ok(m) = image::read_map(&map) {
                    let _ = rescue::set(
                        db,
                        r.id,
                        "running",
                        None,
                        Some(&rescue::image_state(&m, 160)),
                    );
                }
            }
        },
        stop,
    )?;
    let result = rescue::image_state(&image::read_map(&map)?, 160);
    let summary = if sum.complete {
        format!(
            "copied all {} into {}",
            fmt::bytes(sum.size),
            dest.display()
        )
    } else if sum.stopped {
        format!(
            "stopped with {} of {} copied: resume to carry on",
            fmt::bytes(sum.rescued),
            fmt::bytes(sum.size)
        )
    } else {
        format!(
            "copied {} of {}; {} couldn't be read, in {} place{}",
            fmt::bytes(sum.rescued),
            fmt::bytes(sum.size),
            fmt::bytes(sum.bad),
            fmt::count(sum.bad_areas),
            if sum.bad_areas == 1 { "" } else { "s" }
        )
    };
    Ok((summary, result))
}

/// The indexed root holding `path`, if any.
fn root_holding(db: &Db, path: &Path) -> Option<String> {
    let mut stmt = db.conn.prepare("SELECT path FROM roots").ok()?;
    let roots: Vec<PathBuf> = stmt
        .query_map([], |r| r.get::<_, Vec<u8>>(0))
        .ok()?
        .filter_map(|r| r.ok().map(|b| paths::from_bytes(&b)))
        .collect();
    roots
        .into_iter()
        .filter(|r| path.starts_with(r))
        .max_by_key(|r| r.as_os_str().len())
        .map(|r| r.display().to_string())
}

/// The indexed root that holds a plan's archive folder, if any.
fn archive_root(db: &Db, plan: i64) -> Option<String> {
    let (p, _) = backdown_core::plan::load(db, plan).ok()?;
    let target = PathBuf::from(&p.target);
    let mut stmt = db.conn.prepare("SELECT path FROM roots").ok()?;
    let roots: Vec<PathBuf> = stmt
        .query_map([], |r| r.get::<_, Vec<u8>>(0))
        .ok()?
        .filter_map(|r| r.ok().map(|b| paths::from_bytes(&b)))
        .collect();
    roots
        .into_iter()
        .filter(|r| target.starts_with(r))
        .max_by_key(|r| r.as_os_str().len())
        .map(|r| r.display().to_string())
}

/// The indexes that make a big index quick (db.rs `LATER_INDEXES`): built once, in the worker
/// lane so no scan or hash writes alongside them.
fn run_indexes(d: &Daemon, db: &Db, job: &Job) -> Result<String> {
    let built = db.build_later_indexes(&mut |what| {
        d.progress(
            job.id,
            format!("preparing the index for {what} (once; minutes on a big index)"),
            Counts {
                stage: "preparing",
                files: 0,
                total_files: 0,
                bytes: 0,
                total_bytes: 0,
            },
        );
    })?;
    Ok(if built.is_empty() {
        "the indexes were there already".into()
    } else {
        format!("quick from now on: {}", built.join(", "))
    })
}

/// Recognise what's on every drive whose stored results are older than its latest scan.
fn run_entities(d: &Daemon, db: &Db, job: &Job) -> Result<String> {
    use backdown_core::recognise;
    let ids = recognise::root_ids(db)?;
    let mut done = 0;
    let mut things = 0;
    for (i, id) in ids.iter().enumerate() {
        // after a scan: only drives whose results are out of date; when asked for: all of them
        if job.reason != "requested" && recognise::load(db, *id)?.is_some_and(|(_, fresh)| fresh) {
            continue;
        }
        let r = recognise::recognise(db, *id, true, &mut |s| {
            let counts = Counts {
                stage: "recognising",
                files: i as u64,
                total_files: ids.len() as u64,
                ..Counts::default()
            };
            d.progress(
                job.id,
                format!("drive {} of {}: {s}", i + 1, ids.len()),
                counts,
            );
        })?;
        recognise::store(db, &r)?;
        things += r.entities.len();
        done += 1;
    }
    Ok(if done == 0 {
        "recognition is up to date".into()
    } else {
        format!(
            "recognised {things} thing{} on {done} drive{}",
            if things == 1 { "" } else { "s" },
            if done == 1 { "" } else { "s" }
        )
    })
}

fn run_index(d: &Daemon, db: &mut Db, job: &Job) -> Result<String> {
    let path = PathBuf::from(job.path.as_deref().context("index job without a path")?);
    let opts = index::Options {
        one_file_system: !job.opts.cross_filesystems,
        allow_empty: job.opts.allow_empty,
    };
    // the drive's previous scan is the best guess at how far this one has to go
    let before = d.drive(&path.display().to_string());
    // the change log, the file table or a walk, whichever is safe and fastest (index.rs)
    let st = index::scan(db, &path, &opts, job.opts.method, &mut |s| {
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
        "{}: {} · {} files, {} (new {}, changed {}, gone {}) · scan {}{}",
        st.root.display(),
        st.how(),
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
    ) + &crate::hash_absent(st.absent))
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
        "SELECT r.path, s.finished_at, COALESCE(s.files, 0), COALESCE(s.bytes, 0), s.id FROM roots r LEFT JOIN scans s ON s.id = r.last_scan",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, Option<i64>>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Option<i64>>(4)?,
        ))
    })?;
    let now = now();
    let mut drives = Vec::new();
    for row in rows {
        let (path, last, files, bytes, scan) = row?;
        let p = paths::from_bytes(&path);
        let key = p.display().to_string();
        let here = p.is_dir()
            && (files == 0 || std::fs::read_dir(&p).is_ok_and(|mut it| it.next().is_some()));
        let how = scan
            .and_then(|s| index::how(&db, &[s]).ok())
            .and_then(|h| h.first().map(|h| (h.method.clone(), h.phrase())));
        drives.push(Drive {
            path: key.clone(),
            present: here,
            last_scan_at: last,
            files: files as u64,
            bytes: bytes as u64,
            how,
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
            .rescan_every()
            .is_some_and(|every| last.is_none_or(|t| now - t >= every.as_secs() as i64));
        if (back || due) && !d.pending_index(&key) {
            if back {
                eprintln!("{key} is back");
                d.health.ask(); // and read its health
            }
            let reason = if back { "returned" } else { "scheduled" };
            d.enqueue("index", Some(key), reason, JobOpts::default());
        }
    }
    d.set_drives(drives);
    // other machines due a sync (remote/)
    crate::remote::schedule(d, &db);
    for m in backdown_core::mirror::list(&db)? {
        if !Path::new(&m.source).is_dir() || !Path::new(&m.target).is_dir() {
            continue;
        }
        let label = Some(format!("mirror {}", m.id));
        let opts = || JobOpts {
            mirror: Some(m.id),
            ..JobOpts::default()
        };
        if m.last_sync.is_none_or(|t| now - t >= 86_400) {
            d.enqueue("mirror-sync", label.clone(), "scheduled", opts());
        }
        if m.last_verify
            .is_none_or(|t| now - t >= m.verify_every_days * 86_400)
        {
            d.enqueue("mirror-verify", label, "scheduled", opts());
        }
    }
    Ok(())
}

// ---------- requests from the CLI (and the UI's rescan button)

/// One request on the control socket (Unix only so far: on Windows the CLI works on the index itself).
#[cfg_attr(not(unix), allow(dead_code))]
pub fn respond(d: &Daemon, req: &Value) -> Result<Value> {
    let ro = || Db::open_read_only(&d.db);
    Ok(match req["cmd"].as_str().unwrap_or("") {
        "hello" => {
            json!({ "ok": true, "version": env!("CARGO_PKG_VERSION"), "db": d.db.display().to_string(), "pid": std::process::id() })
        }
        // `"index": false`: only what the daemon is doing (the counts can take a while to work out)
        "status" => {
            let index = if req["index"] == false {
                Value::Null
            } else {
                json!(serve::index_status(&ro()?, &d.db)?)
            };
            json!({ "ok": true, "index": index, "daemon": d.snapshot() })
        }
        "enqueue" => {
            let kind = match req["kind"].as_str() {
                Some("index") => "index",
                Some("hash") => "hash",
                Some("entities") => "entities",
                Some("apply") => {
                    if d.shared {
                        bail!(
                            "the machine-wide service reads only; copy from your own BackDown (backdown --local apply …)"
                        );
                    }
                    "apply"
                }
                Some("wipe") => {
                    if d.shared {
                        bail!(
                            "the machine-wide service reads only; wipe from your own BackDown (backdown --local wipe …)"
                        );
                    }
                    "wipe"
                }
                // another machine, over ssh with this user's keys (remote/)
                Some("remote-sync") => {
                    if d.shared {
                        bail!(
                            "the machine-wide service has no ssh keys of yours: sync from your own BackDown (backdown --local remote sync …)"
                        );
                    }
                    let db = Db::open(&d.db)?;
                    let m = backdown_core::remote::get(
                        &db,
                        req["machine"]
                            .as_i64()
                            .context("remote-sync needs a machine")?,
                    )?;
                    return Ok(
                        json!({ "ok": true, "job": crate::remote::queue(d, &m, "requested") }),
                    );
                }
                Some("dedup") => {
                    if d.shared {
                        bail!(
                            "the machine-wide service reads only; tidy from your own BackDown (backdown --local dedup …)"
                        );
                    }
                    "dedup"
                }
                Some("mail-merge") => {
                    if d.shared {
                        bail!(
                            "the machine-wide service reads only; merge from your own BackDown (backdown --local mail merge …)"
                        );
                    }
                    "mail-merge"
                }
                _ => bail!(
                    "kind must be index, hash, entities, apply, wipe, dedup, mail-merge or remote-sync"
                ),
            };
            let plan = req["plan"].as_i64();
            let path = match kind {
                "index" => {
                    let p = req["path"].as_str().context("index needs a path")?;
                    if !Path::new(p).is_absolute() {
                        bail!("path must be absolute");
                    }
                    Some(p.to_string())
                }
                // one queued apply per plan (the label keeps two plans' jobs apart)
                "apply" => Some(format!("plan {}", plan.context("apply needs a plan")?)),
                "wipe" => Some(format!(
                    "wipe {}",
                    req["wipe"].as_i64().context("wipe needs a recorded wipe")?
                )),
                "dedup" => Some(format!(
                    "dedup {}",
                    req["dedup"]
                        .as_i64()
                        .context("dedup needs a recorded run")?
                )),
                "mail-merge" => Some(format!(
                    "merge {}",
                    req["merge"]
                        .as_i64()
                        .context("mail-merge needs a recorded merge")?
                )),
                _ => None,
            };
            let opts = JobOpts {
                cross_filesystems: req["cross_filesystems"].as_bool().unwrap_or(false),
                allow_empty: req["allow_empty"].as_bool().unwrap_or(false),
                jobs_per_device: req["jobs"].as_u64().unwrap_or(1) as usize,
                retry_errors: req["retry_errors"].as_bool().unwrap_or(false),
                plan,
                mirror: None,
                wipe: req["wipe"].as_i64(),
                trust_index: req["trust_index"].as_bool().unwrap_or(false),
                rescue: None,
                machine: None,
                method: req["method"].as_str().unwrap_or("auto").parse()?,
                dedup: req["dedup"].as_i64(),
                merge: req["merge"].as_i64(),
            };
            json!({ "ok": true, "job": d.enqueue(kind, path, "requested", opts) })
        }
        "job" => json!({ "ok": true, "job": d.job(req["job"].as_u64().context("missing job")?) }),
        "stop" => {
            json!({ "ok": true, "state": d.stop(req["job"].as_u64().context("missing job")?)? })
        }
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
        // read every drive's health now (and keep it): `backdown health`
        "health" => crate::health::respond(d)?,
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
            let finished = matches!(job.state, "done" | "failed" | "stopped");
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
