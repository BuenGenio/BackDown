//! Drive health in the daemon, the browser and the terminal (the readings themselves are
//! backdown_core::health), and the notifications that come with it.
//!
//! The daemon reads every connected drive's health in a thread of its own (a reading takes a
//! second or two and never waits for a scan): shortly after it starts, every six hours, when a
//! drive appears (the set of disks changed, or an indexed drive came back), and when asked
//! (`backdown health`, the browser's "Check now"). Each reading is kept in the index when it
//! says something new ([`health::record`]); what got worse becomes a notification
//! (backdown_core::alerts decides whether it's news and whether it may reach the desktop, and
//! [`notifier`] shows it). Failed scheduled jobs and mirrors that no longer match their source
//! ([`after_job`]) are told the same way.
//!
//! Where readings come from is a [`Source`]: this machine's drives, or, with
//! `BACKDOWN_FAKE_HEALTH=<file>`, a JSON file of readings read afresh each round (for tests and
//! demos; it changes nothing else).
//!
//! The other machines' drives (Team: [`machines_allowed`]) come with each machine's sync
//! (backdown_core::remote fetches them from that machine's own index, over the person's ssh, and
//! tells when one got worse); here they're shown (`backdown health --machines`, GET
//! /api/health-machines), and a sync's notes about them may pop up ([`after_remote_sync`]).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use backdown_core::alerts::{self, Limits, Note};
use backdown_core::health::{self, Probe, Summary, Verdict};
use backdown_core::{Db, fmt, mirror};
use serde_json::{Value, json};

use crate::daemon::{Daemon, Job};
use crate::notifier::{self, Settings};
use crate::{client, locations};
use backdown_core::clock::now;

/// How often every connected drive is read.
pub const EVERY: i64 = 6 * 3600;

/// Gathering other machines' drive health here is the Team feature "health-alerts" (tiers.rs; a
/// preview unlocks it): fetching it during a sync, the notifications about their drives,
/// `backdown health --machines` and GET /api/health-machines. This machine's own stay free.
pub fn machines_allowed() -> bool {
    crate::tiers::feature("health-alerts").is_none_or(|f| crate::tiers::access().allows(f))
}

// ---------- where readings come from

pub trait Source: Send + Sync {
    fn read(&self, now: i64) -> Result<Probe>;
}

/// This machine's drives.
pub struct System;

impl Source for System {
    fn read(&self, now: i64) -> Result<Probe> {
        health::probe(now)
    }
}

/// Readings from a JSON file (a `Probe`: `{"drives": [...], "notes": [...]}`), read afresh each
/// round, so a test can make a drive get worse.
pub struct FromFile(pub PathBuf);

impl Source for FromFile {
    fn read(&self, now: i64) -> Result<Probe> {
        let bytes =
            std::fs::read(&self.0).with_context(|| format!("can't read {}", self.0.display()))?;
        let mut p: Probe = serde_json::from_slice(&bytes)
            .with_context(|| format!("{} isn't a list of readings", self.0.display()))?;
        for h in &mut p.drives {
            if h.read_at == 0 {
                h.read_at = now;
            }
            if h.id.is_empty() {
                h.id = health::identity(h.serial.as_deref(), h.wwn.as_deref());
            }
            if h.source.is_empty() {
                h.source = "test readings".into();
            }
        }
        Ok(p)
    }
}

pub fn source_from_env() -> Box<dyn Source> {
    match std::env::var_os("BACKDOWN_FAKE_HEALTH") {
        Some(p) if !p.is_empty() => {
            eprintln!(
                "drive health: reading {} instead of the drives (BACKDOWN_FAKE_HEALTH)",
                Path::new(&p).display()
            );
            Box::new(FromFile(p.into()))
        }
        _ => Box::new(System),
    }
}

// ---------- the daemon's side

#[derive(Default)]
struct State {
    asked: bool,
    running: bool,
    checked_at: Option<i64>,
    next_at: Option<i64>,
    drives: Vec<Summary>,
    notes: Vec<String>,
    error: Option<String>,
    unread: u64,
    /// bumped whenever what the browser shows changes
    change: u64,
}

/// The daemon's view of drive health: the latest readings, when they were taken, and what's
/// unread.
pub struct Monitor {
    source: Box<dyn Source>,
    pub settings_path: PathBuf,
    /// runs as the person at the screen, so it may show desktop notifications
    pub desktop: bool,
    state: Mutex<State>,
    wake: Condvar,
    round: Mutex<()>,
}

impl Monitor {
    pub fn new(source: Box<dyn Source>, state_dir: &Path, desktop: bool) -> Monitor {
        Monitor {
            source,
            settings_path: state_dir.join(notifier::SETTINGS_FILE),
            desktop,
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            round: Mutex::new(()),
        }
    }

    /// Read the drives as soon as the health thread can.
    pub fn ask(&self) {
        self.state.lock().unwrap().asked = true;
        self.wake.notify_all();
    }

    pub fn settings(&self) -> Settings {
        notifier::load(&self.settings_path)
    }

    /// For the daemon's snapshot: enough for the browser to know when to fetch more.
    pub fn snapshot(&self) -> Value {
        let st = self.state.lock().unwrap();
        let worst = st
            .drives
            .iter()
            .filter(|d| d.present)
            .map(|d| d.verdict)
            .max_by_key(|v| v.rank())
            .unwrap_or(Verdict::Unknown);
        json!({
            "checked_at": st.checked_at, "next_at": st.next_at, "running": st.running,
            "change": st.change, "unread": st.unread, "worst": worst,
        })
    }

    fn overview(&self) -> Value {
        let st = self.state.lock().unwrap();
        json!({
            "drives": st.drives, "notes": st.notes, "error": st.error, "checked_at": st.checked_at,
            "next_at": st.next_at, "running": st.running, "every_secs": EVERY, "daemon": true,
        })
    }

    fn set_unread(&self, d: &Daemon, n: u64) {
        {
            let mut st = self.state.lock().unwrap();
            st.unread = n;
            st.change += 1;
        }
        d.touch();
    }
}

/// The health thread: read the drives shortly after the daemon starts, every [`EVERY`], when the
/// set of disks changes, and when asked.
pub fn run(d: &Daemon) {
    let m = &d.health;
    let mut disks = disk_set();
    let mut due = Instant::now() + Duration::from_secs(20);
    // the unread count, before the first reading
    if let Ok(db) = Db::open_read_only(&d.db) {
        let n = alerts::unread(&db).unwrap_or(0);
        m.state.lock().unwrap().unread = n;
    }
    loop {
        let asked = {
            let st = m.state.lock().unwrap();
            let (mut st, _) = m
                .wake
                .wait_timeout_while(st, Duration::from_secs(30), |s| !s.asked)
                .unwrap();
            std::mem::take(&mut st.asked)
        };
        let now_disks = disk_set();
        let arrived = now_disks.is_some() && now_disks != disks;
        disks = now_disks;
        if arrived && !asked {
            // give the system a moment to read the new drive's SMART data
            std::thread::sleep(Duration::from_secs(10));
        }
        if asked || arrived || Instant::now() >= due {
            if let Err(e) = round(d) {
                eprintln!("drive health: {e:#}");
            }
            due = Instant::now() + Duration::from_secs(EVERY as u64);
            m.state.lock().unwrap().next_at = Some(now() + EVERY);
        }
    }
}

/// Which disks are there, cheaply (no tools run): changes when one is plugged in or removed.
pub(crate) fn disk_set() -> Option<Vec<String>> {
    let (dir, keep): (&str, fn(&str) -> bool) = if cfg!(target_os = "linux") {
        ("/sys/block", |n| {
            !["loop", "ram", "zram", "dm-", "md", "sr"]
                .iter()
                .any(|p| n.starts_with(p))
        })
    } else if cfg!(target_os = "macos") {
        ("/dev", |n| {
            n.strip_prefix("disk")
                .is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit()))
        })
    } else {
        return None; // Windows: when an indexed drive comes back (the scheduler asks), and every 6 h
    };
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| keep(n))
        .collect();
    v.sort();
    Some(v)
}

/// Read every drive now, keep what's new, tell what got worse; returns the overview.
pub fn round(d: &Daemon) -> Result<Value> {
    let m = &d.health;
    let _one = m.round.lock().unwrap();
    {
        let mut st = m.state.lock().unwrap();
        st.running = true;
        st.change += 1;
    }
    d.touch();
    let settings = m.settings();
    let out = read_and_record(
        &d.db,
        m.source.as_ref(),
        &settings,
        m.desktop && settings.desktop,
        now(),
    );
    {
        let mut st = m.state.lock().unwrap();
        st.running = false;
        st.checked_at = Some(now());
        st.next_at = Some(now() + EVERY);
        st.change += 1;
        match &out {
            Ok(r) => {
                st.drives = r.drives.clone();
                st.notes = r.notes.clone();
                st.unread = r.unread;
                st.error = None;
            }
            Err(e) => st.error = Some(format!("{e:#}")),
        }
    }
    d.touch();
    let r = out?;
    show(&r.show);
    Ok(m.overview())
}

/// What one round found.
pub struct Round {
    pub drives: Vec<Summary>,
    pub notes: Vec<String>,
    /// notes to show on the desktop now
    pub show: Vec<Note>,
    pub unread: u64,
}

/// Read, keep and tell (without showing anything yet): the part of a round that's the same for
/// the daemon and `backdown health`.
pub fn read_and_record(
    db_path: &Path,
    source: &dyn Source,
    settings: &Settings,
    desktop: bool,
    now: i64,
) -> Result<Round> {
    let probe = source.read(now)?;
    let db = Db::open(db_path)?;
    let mirrors = mirror::list(&db)?;
    let mut show = Vec::new();
    for h in &probe.drives {
        let r = health::record(&db, h)?;
        // Activity lists every note; the settings choose which also pop up on the desktop
        let pop = desktop && settings.wants("health");
        for n in alerts::drive_notes(h, &r, &alerts::mirrors_on(h, &mirrors, &probe.drives)) {
            if let Some((_, true)) = alerts::add(&db, &n, pop, now, &Limits::default())? {
                show.push(n);
            }
        }
    }
    Ok(Round {
        drives: health::overview(Some(&db), &probe.drives)?,
        notes: probe.notes,
        show,
        unread: alerts::unread(&db)?,
    })
}

/// Show notes on the desktop, one after the other, without holding anything up.
fn show(notes: &[Note]) {
    if notes.is_empty() {
        return;
    }
    let notes = notes.to_vec();
    std::thread::spawn(move || {
        for n in notes {
            if let Err(e) = notifier::send(&n.title, &n.body, n.level == "failing") {
                eprintln!("notification: {e:#}");
            }
        }
    });
}

/// Notes of `kind` may pop up on this desktop now: the service runs as the person at the screen,
/// and the settings say so.
pub fn pops_up(d: &Daemon, kind: &str) -> bool {
    let s = d.health.settings();
    d.health.desktop && s.desktop && s.wants(kind)
}

/// After a sync of another machine: the unread count (notes about drives there are listed in
/// Activity), the browser told to fetch again (what's known of its drives changed), and the notes
/// that may pop up now shown.
pub fn after_remote_sync(d: &Daemon, db: &Db, show_now: &[Note]) {
    match alerts::unread(db) {
        Ok(n) => d.health.set_unread(d, n),
        Err(e) => eprintln!("notification: {e:#}"),
    }
    show(show_now);
}

/// A job ended: tell about a mirror that no longer matches its source, or a scheduled job that
/// failed. Recording is quick; showing happens in the background.
pub fn after_job(d: &Daemon, job: &Job, error: Option<&str>) {
    let Some(error) = error else { return };
    let label = job.path.as_deref().unwrap_or("");
    let note = if job.kind == "mirror-verify" {
        Note {
            kind: "drift",
            key: format!("drift:{label}"),
            level: "warning",
            title: "A mirror no longer matches its source".into(),
            body: fmt::capitalised(error),
            drive: None,
            quiet_for: 20 * 3600,
        }
    } else if job.auto {
        let what = match job.kind {
            "index" => format!("scan of {label}"),
            "hash" => "hashing".into(),
            "entities" => "recognition".into(),
            "mirror-sync" => format!("sync of {label}"),
            "remote-sync" => format!("sync of the machine {label}"),
            k => k.to_string(),
        };
        Note {
            kind: "job",
            key: format!("job:{}:{label}", job.kind),
            level: "warning",
            title: format!("A scheduled {what} failed"),
            body: fmt::capitalised(error),
            drive: None,
            quiet_for: 20 * 3600,
        }
    } else {
        return;
    };
    tell(d, note);
}

/// Record a note in Activity (if it's news) and pop it up if its kind is chosen. Recording is
/// quick; showing happens in the background.
pub fn tell(d: &Daemon, note: Note) {
    let settings = d.health.settings();
    // recorded either way (Activity lists it); popped up only if that kind is chosen
    let desktop = d.health.desktop && settings.desktop && settings.wants(note.kind);
    let r = Db::open(&d.db).and_then(|db| {
        let added = alerts::add(&db, &note, desktop, now(), &Limits::default())?;
        Ok((added, alerts::unread(&db)?))
    });
    match r {
        Ok((added, unread)) => {
            d.health.set_unread(d, unread);
            if let Some((_, true)) = added {
                show(&[note]);
            }
        }
        Err(e) => eprintln!("notification: {e:#}"),
    }
}

/// `{"cmd": "health"}` on the control socket: read now and answer with the overview.
#[cfg_attr(not(unix), allow(dead_code))]
pub fn respond(d: &Daemon) -> Result<Value> {
    Ok(json!({ "ok": true, "health": round(d)? }))
}

// ---------- the browser's endpoints

/// The paths [`get`] and [`post`] answer.
pub fn handles(path: &str) -> bool {
    matches!(
        path,
        "/api/health"
            | "/api/health-check"
            | "/api/health-machines"
            | "/api/notifications"
            | "/api/notify-settings"
            | "/api/notify-test"
    )
}

fn settings_path(d: Option<&Daemon>) -> PathBuf {
    d.map_or_else(
        || locations::state_dir().join(notifier::SETTINGS_FILE),
        |d| d.health.settings_path.clone(),
    )
}

fn settings_json(d: Option<&Daemon>) -> Value {
    let s = notifier::load(&settings_path(d));
    let reachable = d.is_none_or(|d| d.health.desktop);
    let mut v = json!(s);
    v["available"] = json!(reachable && notifier::available());
    if !reachable {
        v["why"] = json!(
            "the machine-wide service runs on nobody's desktop: notifications are only listed in Activity"
        );
    } else if !notifier::available() {
        v["why"] =
            json!("this system has no way to show notifications (notify-send isn't installed)");
    }
    v
}

/// GET /api/health, /api/health-machines (the other machines' drives, as last synced: read from
/// the index, nothing is reached), /api/notifications, /api/notify-settings.
pub fn get(
    d: Option<&Daemon>,
    db_path: &Path,
    path: &str,
    param: &dyn Fn(&str) -> Option<String>,
) -> Result<Value> {
    Ok(match path {
        "/api/health" => match d {
            Some(d) => {
                let never = {
                    let st = d.health.state.lock().unwrap();
                    st.checked_at.is_none() && !st.running
                };
                if never {
                    d.health.ask();
                }
                d.health.overview()
            }
            // `backdown serve`: read now, alongside what the index holds
            None => {
                let probe = System.read(now())?;
                let db = Db::open_read_only(db_path)?;
                let stored = health::available(&db).then_some(&db);
                json!({ "drives": health::overview(stored, &probe.drives)?, "notes": probe.notes,
                        "checked_at": now(), "daemon": false, "every_secs": EVERY })
            }
        },
        "/api/health-machines" => {
            let db = Db::open_read_only(db_path)?;
            let mut v = backdown_core::remote::health_overview(&db)?;
            // the machine-wide service never syncs machines: it has no ssh keys of anyone's
            if d.is_some_and(|d| d.shared) {
                v["why_not"] = json!(
                    "other machines are synced with your own ssh keys, by your own BackDown (backdown service install, without --system)"
                );
            }
            v
        }
        "/api/notifications" => {
            let db = Db::open_read_only(db_path)?;
            let limit = param("limit")
                .and_then(|l| l.parse().ok())
                .unwrap_or(50usize);
            if health::available(&db) {
                json!({ "notes": alerts::list(&db, limit.min(500))?, "unread": alerts::unread(&db)? })
            } else {
                json!({ "notes": [], "unread": 0 })
            }
        }
        "/api/notify-settings" => settings_json(d),
        _ => bail!("no such endpoint"),
    })
}

/// POST /api/health-check (read the drives now), /api/notifications (`read=all` or an id),
/// /api/notify-settings (`desktop`, `health`, `drift`, `jobs`: 0 or 1), /api/notify-test.
pub fn post(
    d: Option<&Daemon>,
    db_path: &Path,
    path: &str,
    param: &dyn Fn(&str) -> Option<String>,
) -> Result<Value> {
    Ok(match path {
        "/api/health-check" => {
            let d = d.context("drive health is read by the BackDown service: `backdown health` in a terminal reads it now")?;
            d.health.ask();
            json!({ "ok": true })
        }
        "/api/notifications" => {
            let which = param("read").context("read=all or read=<id>")?;
            let id = match which.as_str() {
                "all" => None,
                n => Some(n.parse::<i64>().context("bad id")?),
            };
            let db = Db::open(db_path)?;
            let n = alerts::mark_read(&db, id)?;
            let unread = alerts::unread(&db)?;
            if let Some(d) = d {
                d.health.set_unread(d, unread);
            }
            json!({ "ok": true, "marked": n, "unread": unread })
        }
        "/api/notify-settings" => {
            let p = settings_path(d);
            let mut s = notifier::load(&p);
            for (k, field) in [
                ("desktop", &mut s.desktop),
                ("health", &mut s.health),
                ("drift", &mut s.drift),
                ("jobs", &mut s.jobs),
            ] {
                match param(k).as_deref() {
                    Some("1" | "true" | "on") => *field = true,
                    Some("0" | "false" | "off") => *field = false,
                    Some(other) => bail!("{k} must be 0 or 1, not {other:?}"),
                    None => {}
                }
            }
            notifier::save(&p, &s)?;
            settings_json(d)
        }
        "/api/notify-test" => {
            if d.is_some_and(|d| !d.health.desktop) {
                bail!("the machine-wide service runs on nobody's desktop");
            }
            notifier::send(
                "Notifications work",
                "BackDown will tell you here when a drive gets worse, a mirror stops matching, or a scheduled job fails.",
                false,
            )?;
            json!({ "ok": true })
        }
        _ => bail!("no such endpoint"),
    })
}

// ---------- `backdown health`

/// `backdown health`: read every drive now (through the daemon when it runs, so it's kept), show
/// each with its counters and how it changed since the first reading. Exits 2 when a connected
/// drive is failing. With `machines`, the other machines' drives instead ([`machines_command`]).
pub fn command(
    db: Option<PathBuf>,
    local: bool,
    as_json: bool,
    machines: bool,
) -> Result<ExitCode> {
    if machines {
        return machines_command(db, local, as_json);
    }
    let overview = match (!local)
        .then(|| client::Client::connect(db.as_deref()))
        .flatten()
    {
        Some(mut c) => {
            let r = c.request(&json!({ "cmd": "health" }))?;
            r["health"].clone()
        }
        None => {
            let db_path = db.unwrap_or_else(locations::default_db);
            if let Some(dir) = db_path.parent() {
                locations::ensure_private_dir(dir, false)
                    .with_context(|| format!("can't create {}", dir.display()))?;
            }
            let settings = notifier::load(&locations::state_dir().join(notifier::SETTINGS_FILE));
            let r = read_and_record(
                &db_path,
                source_from_env().as_ref(),
                &settings,
                false,
                now(),
            )?;
            json!({ "drives": r.drives, "notes": r.notes, "checked_at": now(), "daemon": false })
        }
    };
    let failing = overview["drives"].as_array().is_some_and(|a| {
        a.iter()
            .any(|d| d["present"] == true && d["verdict"] == "failing")
    });
    if as_json {
        println!("{}", serde_json::to_string_pretty(&overview)?);
    } else {
        print(&overview);
    }
    Ok(if failing {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    })
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}

fn num(v: &Value) -> String {
    v.as_u64().map_or("-".into(), fmt::count)
}

fn print(o: &Value) {
    let drives = o["drives"].as_array().cloned().unwrap_or_default();
    if drives.is_empty() {
        println!("no drives found");
    } else {
        println!(
            "{:<10} {:<24} {:<18} {:<8} {:>5} {:>7} {:>7} {:>5} {:>5} {:>5} {:>5}  SINCE FIRST READING",
            "DRIVE",
            "MODEL",
            "SERIAL",
            "HEALTH",
            "TEMP",
            "HOURS",
            "REALLOC",
            "PEND",
            "UNC",
            "CRC",
            "WEAR"
        );
    }
    for d in &drives {
        let h = &d["health"];
        let name = if d["present"] == true {
            h["name"].as_str().unwrap_or("?").to_string()
        } else {
            "(away)".into()
        };
        let pending = if h["pending"].is_null() && !h["bad_sectors"].is_null() {
            format!("{}*", num(&h["bad_sectors"]))
        } else {
            num(&h["pending"])
        };
        let since = match (d["first_at"].as_i64(), d["trend"].as_array()) {
            (Some(t), Some(tr)) if !tr.is_empty() => format!(
                "{}: {}",
                fmt::day(t),
                tr.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            (Some(t), _) if d["readings"].as_u64().unwrap_or(0) > 1 => {
                format!("{}: no change", fmt::day(t))
            }
            _ => "first reading".into(),
        };
        println!(
            "{:<10} {:<24} {:<18} {:<8} {:>5} {:>7} {:>7} {:>5} {:>5} {:>5} {:>5}  {}",
            clip(&name, 10),
            clip(h["model"].as_str().unwrap_or("-"), 24),
            clip(h["serial"].as_str().unwrap_or("-"), 18),
            d["verdict"].as_str().unwrap_or("?"),
            h["temperature"]
                .as_i64()
                .map_or("-".into(), |t| format!("{t}°C")),
            num(&h["power_on_hours"]),
            num(&h["reallocated"]),
            pending,
            num(&h["uncorrectable"]),
            num(&h["crc_errors"]),
            h["percentage_used"]
                .as_u64()
                .map_or("-".into(), |w| format!("{w}%")),
            since
        );
    }
    let mut said = false;
    for d in &drives {
        let h = &d["health"];
        let who = h["name"].as_str().unwrap_or("?");
        let lines: Vec<String> = d["findings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f["text"].as_str().map(str::to_string))
            .chain(h["note"].as_str().map(str::to_string))
            .collect();
        if !lines.is_empty() && !said {
            println!();
            said = true;
        }
        for l in lines {
            println!("{who}: {l}");
        }
    }
    if drives
        .iter()
        .any(|d| d["health"]["pending"].is_null() && !d["health"]["bad_sectors"].is_null())
    {
        println!(
            "* reallocated and pending sectors together (the attribute table wasn't available)"
        );
    }
    for n in o["notes"].as_array().into_iter().flatten() {
        println!("note: {}", n.as_str().unwrap_or(""));
    }
    if o["daemon"] == false {
        println!(
            "(the BackDown service isn't running: nothing re-reads the drives on its own; `backdown service install`)"
        );
    }
}

// ---------- `backdown health --machines`

/// `backdown health --machines`: the other machines' drives as each one's own BackDown last read
/// them, fetched with its last sync (nothing is reached or read now). Exits 2 when a drive there
/// is failing (one still being read there).
fn machines_command(db: Option<PathBuf>, local: bool, as_json: bool) -> Result<ExitCode> {
    if !machines_allowed() {
        let f = crate::tiers::feature("health-alerts").context("no health-alerts feature")?;
        bail!(
            "other machines' drive health, gathered here, is part of BackDown Team: {}\n\
             Add a licence key with `backdown license add <key>` (see {}).",
            f.unlocks,
            crate::tiers::PRICING_URL
        );
    }
    let db_path = crate::remote::index_path(db, local);
    let db = Db::open_read_only(&db_path).or_else(|_| crate::remote::open(&db_path))?;
    let o = backdown_core::remote::health_overview(&db)?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&o)?);
    } else {
        print!("{}", machines_text(&o, now()));
    }
    Ok(if o["worst"] == "failing" {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    })
}

/// "3 h ago", as of `now`.
fn ago_at(t: Option<i64>, now: i64) -> String {
    let Some(t) = t else { return "never".into() };
    match now - t {
        i64::MIN..60 => "just now".into(),
        s @ 60..3600 => format!("{} min ago", s / 60),
        s @ 3600..86_400 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86_400),
    }
}

/// What `backdown health --machines` prints: each machine with its drives, what's wrong with
/// them, a word when it can't say, and its own latest notifications.
fn machines_text(o: &Value, now: i64) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let machines = o["machines"].as_array().cloned().unwrap_or_default();
    if machines.is_empty() {
        s.push_str("No machines yet: backdown remote add nas ssh://nas.example.org\n");
        return s;
    }
    let (mut stale, mut together) = (false, false);
    for (i, m) in machines.iter().enumerate() {
        if i > 0 {
            s.push('\n');
        }
        let name = m["name"].as_str().unwrap_or("?");
        let _ = writeln!(
            s,
            "{name} ({}) · synced {}",
            m["url"].as_str().unwrap_or(""),
            ago_at(m["last_sync"].as_i64(), now)
        );
        let drives = m["drives"].as_array().cloned().unwrap_or_default();
        if !drives.is_empty() {
            let _ = writeln!(
                s,
                "  {:<8} {:<24} {:<18} {:>5} {:>7} {:>7} {:>5} {:>5} {:>5} {:>5}  READ THERE",
                "HEALTH",
                "MODEL",
                "SERIAL",
                "TEMP",
                "HOURS",
                "REALLOC",
                "PEND",
                "UNC",
                "CRC",
                "WEAR"
            );
        }
        for d in &drives {
            let h = &d["health"];
            let pending = if h["pending"].is_null() && !h["bad_sectors"].is_null() {
                together = true;
                format!("{}*", num(&h["bad_sectors"]))
            } else {
                num(&h["pending"])
            };
            let current = d["current"] == true;
            stale |= !current;
            let _ = writeln!(
                s,
                "  {:<8} {:<24} {:<18} {:>5} {:>7} {:>7} {:>5} {:>5} {:>5} {:>5}  {}{}",
                d["verdict"].as_str().unwrap_or("?"),
                clip(
                    h["model"].as_str().or(h["device"].as_str()).unwrap_or("-"),
                    24
                ),
                clip(h["serial"].as_str().unwrap_or("-"), 18),
                h["temperature"]
                    .as_i64()
                    .map_or("-".into(), |t| format!("{t}°C")),
                num(&h["power_on_hours"]),
                num(&h["reallocated"]),
                pending,
                num(&h["uncorrectable"]),
                num(&h["crc_errors"]),
                h["percentage_used"]
                    .as_u64()
                    .map_or("-".into(), |w| format!("{w}%")),
                ago_at(h["read_at"].as_i64(), now),
                if current { "" } else { " †" }
            );
        }
        for d in &drives {
            let h = &d["health"];
            let who = h["model"].as_str().or(h["device"].as_str()).unwrap_or("?");
            for l in d["findings"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| f["text"].as_str())
                .chain(h["note"].as_str())
            {
                let _ = writeln!(s, "  {who}: {l}");
            }
        }
        if let Some(n) = m["health"]["note"].as_str() {
            let _ = writeln!(s, "  {n}");
        }
        for n in m["notes"].as_array().into_iter().flatten().take(3) {
            let _ = writeln!(
                s,
                "  notified on {name} {}: {}",
                ago_at(n["at"].as_i64(), now),
                n["title"].as_str().unwrap_or("")
            );
        }
    }
    if together {
        s.push_str(
            "* reallocated and pending sectors together (the attribute table wasn't available)\n",
        );
    }
    if stale {
        let _ = writeln!(
            s,
            "† not read there for over {} days before its sync: taken out, or its BackDown isn't running",
            o["stale_days"].as_i64().unwrap_or(7)
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_round_from_fake_readings() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("index.db");
        let file = dir.path().join("readings.json");
        let write = |realloc: u64| {
            std::fs::write(
                &file,
                serde_json::to_vec(&json!({ "drives": [ {
                    "device": "/dev/sdb", "name": "sdb", "model": "FAKEDISK HD4000", "serial": "FAKE0001",
                    "rotational": true, "reported": "ok", "reallocated": realloc, "pending": 0 } ] }))
                .unwrap(),
            )
            .unwrap()
        };
        let src = FromFile(file.clone());
        let s = Settings::default();
        write(0);
        let r = read_and_record(&db, &src, &s, true, 1_000).unwrap();
        assert_eq!(r.drives.len(), 1);
        assert_eq!(r.drives[0].id, "sn:FAKE0001");
        assert_eq!(r.drives[0].verdict, Verdict::Ok);
        assert!(r.show.is_empty());
        write(5);
        let r = read_and_record(&db, &src, &s, true, 2_000).unwrap();
        assert_eq!(r.drives[0].verdict, Verdict::Warning);
        assert_eq!(r.show.len(), 1);
        assert_eq!(r.unread, 1);
        // the same again: kept quiet
        write(5);
        let r = read_and_record(&db, &src, &s, true, 3_000).unwrap();
        assert!(r.show.is_empty());
        assert_eq!(r.unread, 1);
        // health pop-ups turned off: listed in Activity, not popped up
        write(9);
        let off = Settings {
            health: false,
            ..Settings::default()
        };
        let r = read_and_record(&db, &src, &off, true, 4_000).unwrap();
        assert!(r.show.is_empty());
        assert_eq!(r.unread, 2);
        assert_eq!(r.drives[0].health.reallocated, Some(9));
    }

    #[test]
    fn other_machines_drives_in_the_terminal() {
        let now = 1_790_000_000;
        let o = json!({ "stale_days": 7, "worst": "failing", "machines": [
            { "name": "nas", "url": "ssh://alice@nas.example.org", "last_sync": now - 7200,
              "health": { "state": "ok", "note": null },
              "drives": [
                { "verdict": "failing", "current": true,
                  "health": { "model": "FAKEDISK HD4000", "serial": "FAKE0001", "temperature": 41,
                              "power_on_hours": 20000, "reallocated": 150, "pending": 0,
                              "uncorrectable": 0, "crc_errors": 0, "read_at": now - 3 * 3600 },
                  "findings": [{ "level": "failing", "code": "reallocated",
                                 "text": "150 sectors had gone bad and have been replaced by spares (reallocated)" }] },
                { "verdict": "ok", "current": false,
                  "health": { "model": "FAKEDISK NV970", "serial": "FAKE0002", "percentage_used": 3,
                              "read_at": now - 40 * 86_400 },
                  "findings": [] }
              ],
              "notes": [{ "at": now - 2 * 86_400, "level": "failing", "title": "FAKEDISK HD4000 is failing",
                          "body": "150 more sectors have gone bad" }] },
            { "name": "old-pc", "url": "ssh://old-pc", "last_sync": now - 3 * 86_400,
              "health": { "state": "unsupported", "note": "update BackDown on old-pc to see its drive health" },
              "drives": [], "notes": [] }
        ]});
        let t = machines_text(&o, now);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(
            lines[0],
            "nas (ssh://alice@nas.example.org) · synced 2 h ago"
        );
        assert!(lines[1].starts_with("  HEALTH   MODEL"), "{t}");
        assert!(lines[2].starts_with("  failing  FAKEDISK HD4000"), "{t}");
        assert!(
            lines[2].contains(" 41°C ") && lines[2].contains(" 150 "),
            "{t}"
        );
        assert!(lines[2].ends_with("3 h ago"), "{t}");
        assert!(lines[3].starts_with("  ok       FAKEDISK NV970"), "{t}");
        assert!(lines[3].ends_with("40 days ago †"), "{t}");
        assert_eq!(
            lines[4],
            "  FAKEDISK HD4000: 150 sectors had gone bad and have been replaced by spares (reallocated)"
        );
        assert_eq!(
            lines[5],
            "  notified on nas 2 days ago: FAKEDISK HD4000 is failing"
        );
        assert!(
            t.contains(
                "\nold-pc (ssh://old-pc) · synced 3 days ago\n  update BackDown on old-pc to see its drive health\n"
            ),
            "{t}"
        );
        assert!(t.ends_with(
            "† not read there for over 7 days before its sync: taken out, or its BackDown isn't running\n"
        ));
        assert_eq!(
            machines_text(&json!({ "machines": [] }), now),
            "No machines yet: backdown remote add nas ssh://nas.example.org\n"
        );
    }
}
