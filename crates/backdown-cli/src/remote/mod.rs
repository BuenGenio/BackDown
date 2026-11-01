//! Other machines: `backdown remote add|list|remove|sync|test`, their side of the daemon
//! (a sync job in the side lane, on a schedule and on request), the browser's endpoints, and
//! `backdown serve-stdio`, the end ssh runs on the other machine.
//!
//! Nothing here listens on the network: machines are reached with `ssh` as the person running
//! BackDown (ssh.rs), and the other end only reads (stdio.rs). What each machine's index holds is
//! cached in this index, and the rules for when a copy there counts are in
//! backdown_core::remote.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use backdown_core::alerts::Note;
use backdown_core::remote::{self as core, Machine, Peer, SyncOptions};
use backdown_core::{Db, fmt};
use clap::Subcommand;
use serde_json::{Value, json};

use crate::daemon::{Daemon, JobOpts};
use crate::{client, locations};

pub mod ssh;
pub mod stdio;
pub mod swarm_stdio;

/// Several machines are "multi-machine" in tiers.rs's table: Pro until 1 Nov 2026, free since,
/// with the swarm that's built on it. Reaching machines (add, test, sync, browse, a wipe's
/// re-reads, the daemon's schedule) asks here; listing and forgetting them never do. Without it,
/// what was cached stops being refreshed, and so stops counting once the proof window has passed.
pub fn allowed() -> bool {
    crate::tiers::feature("multi-machine").is_none_or(|f| crate::tiers::access().allows(f))
}

fn gate() -> Result<()> {
    if !allowed() {
        bail!(
            "reaching other machines needs a licence that includes it: add one with `backdown license add <key>` (see {})",
            crate::tiers::PRICING_URL
        );
    }
    Ok(())
}

#[derive(Subcommand)]
pub enum RemoteCmd {
    /// Add a machine whose BackDown index is read over ssh (with your keys; never a password)
    Add {
        /// What to call it, like nas or office-pc
        name: String,
        /// ssh://[user@]host[:port] (a host alias from ~/.ssh/config works too)
        url: String,
        /// Its index there, if not its default (or the one its BackDown service uses). (The global
        /// --db is this machine's index)
        #[arg(long, value_name = "PATH")]
        remote_db: Option<String>,
        /// Where backdown is there, if not on the PATH ssh sessions get
        #[arg(long, value_name = "PATH")]
        backdown: Option<String>,
        /// Don't try to reach it now
        #[arg(long)]
        no_test: bool,
    },
    /// The machines, their drives, and when they were last synced
    List {
        #[arg(long)]
        json: bool,
    },
    /// Forget a machine and what was cached about it (nothing there is touched)
    Remove { name: String },
    /// Fetch what changed in a machine's index (all of them without NAME); files there whose size
    /// matches a file here are hashed there, so copies can be proved
    Sync { name: Option<String> },
    /// Try reaching a machine: ssh, BackDown there, its index
    Test { name: String },
    /// How long a copy on another machine counts as proof after it was last confirmed there, and
    /// how often machines are synced
    Settings {
        /// Days (1–90; 7 unless changed)
        #[arg(long)]
        proof_days: Option<i64>,
        /// Hours between syncs (6 unless changed)
        #[arg(long)]
        sync_hours: Option<i64>,
    },
}

/// The index these commands work on: --db, or a running daemon's, or the default.
pub(crate) fn index_path(db: Option<PathBuf>, local: bool) -> PathBuf {
    db.or_else(|| {
        (!local)
            .then(|| client::Client::connect(None).map(|c| PathBuf::from(c.db)))
            .flatten()
    })
    .unwrap_or_else(locations::default_db)
}

pub(crate) fn open(db: &Path) -> Result<Db> {
    if let Some(dir) = db.parent() {
        locations::ensure_private_dir(dir, false)
            .with_context(|| format!("can't create {}", dir.display()))?;
    }
    Db::open(db)
}

fn ago(t: Option<i64>) -> String {
    let Some(t) = t else { return "never".into() };
    let s = core::now() - t;
    match s {
        i64::MIN..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86_400 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

pub fn command(db: Option<PathBuf>, local: bool, cmd: RemoteCmd) -> Result<ExitCode> {
    let db_path = index_path(db, local);
    match cmd {
        RemoteCmd::Add {
            name,
            url,
            remote_db,
            backdown,
            no_test,
        } => {
            gate()?;
            let db = open(&db_path)?;
            let m = core::add(&db, &name, &url, remote_db.as_deref(), backdown.as_deref())?;
            println!("Added {} ({}).", m.name, m.url);
            if !no_test {
                match test(&db, &m) {
                    Ok(s) => println!("{s}"),
                    Err(e) => {
                        println!("But it can't be reached yet: {e:#}");
                        println!("Fix that, then: backdown remote test {}", m.name);
                        return Ok(ExitCode::SUCCESS);
                    }
                }
            }
            println!(
                "Next: backdown remote sync {} (the BackDown service also syncs it every {} h)",
                m.name,
                core::settings(&db).sync_hours
            );
        }
        RemoteCmd::List { json } => {
            let db = Db::open_read_only(&db_path).or_else(|_| open(&db_path))?;
            let o = core::overview(&db)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&o)?);
                return Ok(ExitCode::SUCCESS);
            }
            let ms = o["machines"].as_array().cloned().unwrap_or_default();
            if ms.is_empty() {
                println!("No machines yet: backdown remote add nas ssh://nas.example.org");
            }
            for m in &ms {
                println!(
                    "{}  {}{}",
                    m["name"].as_str().unwrap_or(""),
                    m["url"].as_str().unwrap_or(""),
                    m["db"]
                        .as_str()
                        .map(|d| format!("  (index {d})"))
                        .unwrap_or_default()
                );
                println!(
                    "  synced {}{}",
                    ago(m["last_sync"].as_i64()),
                    m["last_error"]
                        .as_str()
                        .map(|e| format!(" · last try failed: {e}"))
                        .unwrap_or_default()
                );
                for r in m["roots"].as_array().into_iter().flatten() {
                    println!(
                        "  {}  {} files, {} · scanned there {} · {}",
                        r["path"].as_str().unwrap_or(""),
                        fmt::count(r["files"].as_u64().unwrap_or(0)),
                        fmt::bytes(r["bytes"].as_u64().unwrap_or(0)),
                        ago(r["seen_at"].as_i64()),
                        if r["counts"] == true {
                            "its copies count"
                        } else {
                            "too long ago for its copies to count"
                        }
                    );
                }
            }
            println!(
                "Copies on another machine count when confirmed there within {} days (backdown remote settings --proof-days N).",
                o["settings"]["proof_days"]
            );
        }
        RemoteCmd::Remove { name } => {
            let db = open(&db_path)?;
            let m = core::find(&db, &name)?;
            core::remove(&db, m.id)?;
            println!(
                "Forgot {}; nothing on it was touched, and its copies no longer count here.",
                m.name
            );
        }
        RemoteCmd::Test { name } => {
            gate()?;
            let db = open(&db_path)?;
            let m = core::find(&db, &name)?;
            println!("{}", ssh::describe(&m)?);
            match test(&db, &m) {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("backdown: {e:#}");
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
        RemoteCmd::Sync { name } => {
            gate()?;
            let db = open(&db_path)?;
            let machines = match &name {
                Some(n) => vec![core::find(&db, n)?],
                None => core::list(&db)?,
            };
            if machines.is_empty() {
                println!("No machines yet: backdown remote add nas ssh://nas.example.org");
                return Ok(ExitCode::SUCCESS);
            }
            // a running daemon owns the index: it syncs, and hashes here afterwards
            if !local && let Some(mut c) = client::Client::connect(Some(&db_path)) {
                drop(db);
                let mut failed = false;
                for m in &machines {
                    let r = c.request(
                        &json!({ "cmd": "enqueue", "kind": "remote-sync", "machine": m.id }),
                    )?;
                    let what = format!("syncing {}", m.name);
                    match c.follow(r["job"].as_u64().unwrap_or(0), |j| {
                        crate::live(&format!("{what}: {}", j["progress"].as_str().unwrap_or("")))
                    }) {
                        Ok(j) => {
                            crate::clear();
                            println!("{}", j["summary"].as_str().unwrap_or(""));
                        }
                        Err(e) => {
                            crate::clear();
                            eprintln!("{}: {e:#}", m.name);
                            failed = true;
                        }
                    }
                }
                return Ok(if failed {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                });
            }
            let mut failed = false;
            for m in &machines {
                match sync_one(&db, m, None, &mut |s| {
                    crate::live(&format!("{}: {s}", m.name))
                }) {
                    Ok(summary) => {
                        crate::clear();
                        println!("{summary}");
                    }
                    Err(e) => {
                        crate::clear();
                        eprintln!("{}: {e:#}", m.name);
                        failed = true;
                    }
                }
            }
            if !failed {
                println!(
                    "(backdown hash now checks files here against what's there; the BackDown service does that by itself)"
                );
            }
            return Ok(if failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            });
        }
        RemoteCmd::Settings {
            proof_days,
            sync_hours,
        } => {
            // seeing them is never gated, like the list; changing them is the feature's
            if proof_days.is_some() || sync_hours.is_some() {
                gate()?;
            }
            let db = open(&db_path)?;
            if let Some(d) = proof_days {
                core::set_setting(&db, "proof_days", d)?;
            }
            if let Some(h) = sync_hours {
                core::set_setting(&db, "sync_hours", h)?;
            }
            let s = core::settings(&db);
            println!(
                "A copy on another machine counts as proof when that machine confirmed it within {} days; machines are synced every {} h.",
                s.proof_days, s.sync_hours
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Reach a machine and say what's there (recording what it says, or why it can't be reached).
pub fn test(db: &Db, m: &Machine) -> Result<String> {
    let r = (|| -> Result<String> {
        let mut p = ssh::SshPeer::open(m)?;
        let h = core::hello(db, m, &mut p)?;
        let roots = p.call("roots", json!({}), core::ASK)?;
        let roots = roots["roots"].as_array().cloned().unwrap_or_default();
        Ok(format!(
            "Reached {} ({}): BackDown {}, index {}, {} drive{} indexed{}",
            m.name,
            h["host"].as_str().filter(|s| !s.is_empty()).unwrap_or("?"),
            h["version"].as_str().unwrap_or("?"),
            h["db"].as_str().unwrap_or("?"),
            roots.len(),
            if roots.len() == 1 { "" } else { "s" },
            if roots.is_empty() {
                String::new()
            } else {
                format!(
                    ": {}",
                    roots
                        .iter()
                        .filter_map(|r| r["path"].as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        ))
    })();
    let _ = core::record_try(
        db,
        m.id,
        r.as_ref().err().map(|e| format!("{e:#}")).as_deref(),
    );
    r
}

/// What a sync found of the machine's drive health, for its summary ("" when it wasn't asked).
fn health_words(m: &Machine, st: &core::SyncStats) -> String {
    match st.health {
        "ok" => format!(
            "; drive health of {} drive{}{}",
            st.health_drives,
            if st.health_drives == 1 { "" } else { "s" },
            if st.health_notes > 0 {
                format!(
                    ", {} worse since the last sync (told in Activity)",
                    st.health_notes
                )
            } else {
                String::new()
            }
        ),
        "unsupported" => format!("; update BackDown on {} to see its drive health", m.name),
        "error" => {
            "; its drive health couldn't be fetched (`backdown health --machines` says why)".into()
        }
        _ => String::new(),
    }
}

/// Sync one machine over a fresh ssh connection: its drives, what changed there, hashes there,
/// and its drive health (Team), whose notes are listed in Activity (not popped up: there's no
/// service at hand to show them).
pub fn sync_one(
    db: &Db,
    m: &Machine,
    stop: Option<&AtomicBool>,
    progress: &mut dyn FnMut(&str),
) -> Result<String> {
    let r = (|| -> Result<core::SyncStats> {
        let mut p = ssh::SshPeer::open(m)?;
        let opts = SyncOptions {
            stop,
            health: crate::health::machines_allowed(),
            ..SyncOptions::default()
        };
        core::sync(db, m, &mut p, &opts, progress)
    })();
    match &r {
        Ok(_) => {}
        Err(e) => {
            let _ = core::record_try(db, m.id, Some(&format!("{e:#}")));
        }
    }
    let st = r?;
    Ok(format!(
        "{}: {} drive{}, {} files ({} new or changed, {} gone); {} sampled and {} hashed there for comparison{}{}",
        m.name,
        st.drives,
        if st.drives == 1 { "" } else { "s" },
        fmt::count(st.files),
        fmt::count(st.added),
        fmt::count(st.removed),
        fmt::count(st.sampled),
        fmt::count(st.hashed),
        if st.left > 0 {
            format!("; {} still to do next time", fmt::count(st.left))
        } else {
            String::new()
        },
        health_words(m, &st)
    ))
}

// ---------- the daemon's side

/// The side lane's sync job for machine `id`: returns its summary, and whether hashing here could
/// now find more (so the daemon hashes next). Notes about its drives getting worse that may pop up
/// now go in `show`, if they may at all (even when the sync fails after its drive health).
pub fn sync_job(
    db: &Db,
    id: i64,
    stop: &AtomicBool,
    show: Option<&Mutex<Vec<Note>>>,
    progress: &mut dyn FnMut(&str),
) -> Result<(String, bool)> {
    gate()?;
    let m = core::get(db, id)?;
    let r = (|| -> Result<core::SyncStats> {
        let mut p = ssh::SshPeer::open(&m)?;
        let opts = SyncOptions {
            stop: Some(stop),
            health: crate::health::machines_allowed(),
            show,
            ..SyncOptions::default()
        };
        core::sync(db, &m, &mut p, &opts, progress)
    })();
    if let Err(e) = &r {
        let _ = core::record_try(db, m.id, Some(&format!("{e:#}")));
    }
    let st = r?;
    Ok((
        format!(
            "{}: {} files there ({} new or changed, {} gone); {} sampled, {} hashed there{}{}",
            m.name,
            fmt::count(st.files),
            fmt::count(st.added),
            fmt::count(st.removed),
            fmt::count(st.sampled),
            fmt::count(st.hashed),
            if st.left > 0 {
                format!(", {} left for next time", fmt::count(st.left))
            } else {
                String::new()
            },
            health_words(&m, &st)
        ),
        st.changed,
    ))
}

/// The scheduler's turn: queue a sync of each machine that's due.
pub fn schedule(d: &Daemon, db: &Db) {
    if !allowed() || d.shared {
        return;
    }
    let Ok(machines) = core::list(db) else { return };
    let every = core::settings(db).sync_hours * 3600;
    let now = core::now();
    for m in machines {
        // a sync still running (the first one can take a while) isn't due again
        if m.last_try.is_none_or(|t| now - t >= every) && !d.pending("remote-sync", &label(&m)) {
            queue(d, &m, "scheduled");
        }
    }
}

/// After hashing here: machines with files now worth hashing there (same sample as a file here).
pub fn after_hash(d: &Daemon, db: &Db) {
    if !allowed() || d.shared {
        return;
    }
    for m in core::list(db).unwrap_or_default() {
        if m.last_error.is_none() && core::waiting(db, &m).unwrap_or(false) {
            queue(d, &m, "after-hash");
        }
    }
}

/// A sync job's label: the machine's name (unique, and what people call it).
pub fn label(m: &Machine) -> String {
    m.name.clone()
}

pub fn queue(d: &Daemon, m: &Machine, reason: &'static str) -> u64 {
    let id = m.id;
    d.enqueue(
        "remote-sync",
        Some(label(m)),
        reason,
        JobOpts {
            machine: Some(id),
            ..JobOpts::default()
        },
    )
}

/// For a wipe: copies on other machines, re-read there before anything is deleted here.
pub fn verifier_connect() -> Option<ssh::Ssh> {
    allowed().then_some(ssh::Ssh)
}

// ---------- live conversations for the browser, kept open a little while

struct Pooled {
    peer: ssh::SshPeer,
    used: Instant,
}

static POOL: LazyLock<Mutex<HashMap<i64, Pooled>>> = LazyLock::new(|| {
    // close conversations nobody has used for a while
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(Duration::from_secs(20));
            POOL.lock()
                .unwrap()
                .retain(|_, p| p.used.elapsed() < Duration::from_secs(90));
        }
    });
    Mutex::new(HashMap::new())
});

/// Ask machine `m` something now, reusing an open conversation when there is one. A new one is
/// noted in the index (what the machine said, or why it couldn't be reached): a short write of
/// its own, as the browser's reads use read-only connections.
pub(crate) fn live(db_path: &Path, m: &Machine, op: &str, args: Value) -> Result<Value> {
    let pooled = POOL.lock().unwrap().remove(&m.id);
    let fresh = pooled.is_none();
    let mut peer = match pooled {
        Some(p) => p.peer,
        None => {
            let r = ssh::SshPeer::open(m).and_then(|mut p| core::greet(m, &mut p).map(|h| (p, h)));
            let note = Db::open(db_path);
            match r {
                Ok((p, (h, asked))) => {
                    if let Ok(w) = &note {
                        let _ = core::record_hello(w, m.id, &h, asked);
                        let _ = core::record_try(w, m.id, None);
                    }
                    p
                }
                Err(e) => {
                    if let Ok(w) = &note {
                        let _ = core::record_try(w, m.id, Some(&format!("{e:#}")));
                    }
                    return Err(e);
                }
            }
        }
    };
    match peer.call(op, args.clone(), Duration::from_secs(30)) {
        Ok(v) => {
            POOL.lock().unwrap().insert(
                m.id,
                Pooled {
                    peer,
                    used: Instant::now(),
                },
            );
            Ok(v)
        }
        // a conversation left open may have died meanwhile: once more, afresh
        Err(_) if !fresh => {
            drop(peer);
            live(db_path, m, op, args)
        }
        Err(e) => Err(e),
    }
}

// ---------- the browser's endpoints

pub fn handles(path: &str) -> bool {
    path.starts_with("/api/machine")
}

fn param_id(param: &dyn Fn(&str) -> Option<String>, name: &str) -> Result<i64> {
    param(name)
        .with_context(|| format!("missing {name}"))?
        .parse()
        .with_context(|| format!("bad {name}"))
}

/// Why this browser can't reach machines itself, if it can't.
pub(crate) fn blocker(desktop: bool) -> Option<&'static str> {
    if !allowed() {
        Some("reaching other machines isn't included in this licence (backdown.io/pricing)")
    } else if !desktop {
        Some(
            "machines are reached with your own ssh keys: use your own BackDown (backdown service install, without --system)",
        )
    } else {
        None
    }
}

/// GET /api/machines, /api/machine-ls, /api/machine-file, /api/machine-find.
pub fn get(
    d: Option<&Daemon>,
    desktop: bool,
    db_path: &Path,
    path: &str,
    param: &dyn Fn(&str) -> Option<String>,
) -> Result<Value> {
    let db = Db::open_read_only(db_path)?;
    Ok(match path {
        "/api/machines" => {
            let mut o = core::overview(&db)?;
            // syncs waiting or running, by machine
            if let Some(d) = d {
                let snap = d.snapshot();
                let busy: Vec<Value> = snap["queued"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .chain(std::iter::once(&snap["side"]))
                    .filter(|j| j["kind"] == "remote-sync")
                    .cloned()
                    .collect();
                for m in o["machines"].as_array_mut().into_iter().flatten() {
                    let l = m["name"].clone();
                    m["job"] = busy
                        .iter()
                        .find(|j| j["path"] == l)
                        .cloned()
                        .unwrap_or(Value::Null);
                }
            }
            o["allowed"] = json!(allowed());
            o["why_not"] = json!(blocker(desktop));
            o["daemon"] = json!(d.is_some());
            o
        }
        "/api/machine-ls" => {
            let m = core::get(&db, param_id(param, "m")?)?;
            let dir = param("dir").filter(|d| !d.is_empty());
            let want_live = param("live").as_deref() != Some("0");
            let mut why = None;
            if want_live {
                if let Some(b) = blocker(desktop) {
                    why = Some(b.to_string());
                } else {
                    let r = match &dir {
                        None => live(db_path, &m, "roots", json!({})),
                        Some(k) => live(db_path, &m, "ls", json!({ "dir": k })),
                    };
                    match r {
                        Ok(mut v) => {
                            v["live"] = json!(true);
                            return Ok(v);
                        }
                        Err(e) => why = Some(format!("{e:#}")),
                    }
                }
            }
            let mut v = match &dir {
                None => {
                    let o = core::overview(&db)?;
                    let roots = o["machines"]
                        .as_array()
                        .and_then(|a| a.iter().find(|x| x["id"] == json!(m.id)))
                        .map(|x| x["roots"].clone())
                        .unwrap_or(json!([]));
                    json!({ "roots": roots, "cached": true, "synced_at": m.last_sync })
                }
                Some(k) => {
                    let raw = base64_path(k)?;
                    core::cached_list(&db, &m, &raw)?
                }
            };
            v["live"] = json!(false);
            v["why_not_live"] = json!(why);
            v
        }
        "/api/machine-file" => {
            let m = core::get(&db, param_id(param, "m")?)?;
            let rid = param_id(param, "id")?;
            let live_file = match blocker(desktop) {
                Some(b) => Err(anyhow::anyhow!(b)),
                None => live(db_path, &m, "file", json!({ "file": rid })),
            };
            let mut v = match live_file {
                Ok(mut v) => {
                    v["live"] = json!(true);
                    // its hash, when that machine computed it for us rather than for itself
                    if v["sha256"].is_null()
                        && let Ok(c) = core::cached_file(&db, &m, rid)
                        && c["size"] == v["size"]
                        && c["mtime"] == v["mtime"]
                        && c["sha256"].is_string()
                    {
                        v["sha256"] = c["sha256"].clone();
                        v["hashed_at"] = c["hashed_at"].clone();
                    }
                    v
                }
                Err(e) => {
                    let mut v = core::cached_file(&db, &m, rid)?;
                    v["live"] = json!(false);
                    v["why_not_live"] = json!(format!("{e:#}"));
                    v
                }
            };
            // the same content here, and on the other machines
            if let (Some(h), Some(size)) = (
                v["sha256"].as_str().and_then(core::unhex),
                v["size"].as_u64(),
            ) {
                v["local_copies"] = json!(core::local_copies(&db, &h, size)?);
                v["remote_copies"] = json!(
                    core::copies_of(&db, &h, size)?
                        .into_iter()
                        .filter(|c| !(c.machine_id == m.id && c.rid == rid))
                        .collect::<Vec<_>>()
                );
            }
            v["machine"] = json!({ "id": m.id, "name": m.name, "url": m.url });
            v
        }
        "/api/machine-find" => {
            if let Some(b) = blocker(desktop) {
                bail!(b);
            }
            let m = core::get(&db, param_id(param, "m")?)?;
            let q = param("q").unwrap_or_default();
            let mut args = json!({ "q": q, "limit": 200 });
            if let Some(dir) = param("dir").filter(|d| !d.is_empty()) {
                args["within"] = json!(dir);
            }
            live(db_path, &m, "find", args)?
        }
        _ => bail!("no such endpoint"),
    })
}

fn base64_path(key: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(key)
        .context("not a path key")
}

/// POST /api/machine-add, /api/machine-remove, /api/machine-test, /api/machine-sync,
/// /api/machine-settings.
pub fn post(
    d: Option<&Daemon>,
    desktop: bool,
    db_path: &Path,
    path: &str,
    param: &dyn Fn(&str) -> Option<String>,
) -> Result<Value> {
    let needs_ssh = matches!(
        path,
        "/api/machine-add" | "/api/machine-test" | "/api/machine-sync"
    );
    if needs_ssh && let Some(b) = blocker(desktop) {
        bail!(b);
    }
    // short writes alongside the daemon, like saving a plan
    let db = Db::open(db_path)?;
    Ok(match path {
        "/api/machine-add" => {
            let name = param("name").context("what should it be called?")?;
            let url = param("url").context("its address, ssh://…")?;
            let opt = |k: &str| {
                param(k)
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            };
            let m = core::add(
                &db,
                name.trim(),
                url.trim(),
                opt("db").as_deref(),
                opt("command").as_deref(),
            )?;
            let t = test(&db, &m);
            let syncing = t.is_ok() && d.is_some();
            if t.is_ok()
                && let Some(d) = d
            {
                queue(d, &m, "requested");
            }
            json!({ "ok": true, "machine": core::get(&db, m.id)?, "reached": t.is_ok(), "syncing": syncing,
                    "test": t.as_ref().ok(), "error": Value::Null, "test_error": t.err().map(|e| format!("{e:#}")) })
        }
        "/api/machine-remove" => {
            let id = param_id(param, "m")?;
            POOL.lock().unwrap().remove(&id);
            core::remove(&db, id)?;
            json!({ "ok": true })
        }
        "/api/machine-test" => {
            let m = core::get(&db, param_id(param, "m")?)?;
            POOL.lock().unwrap().remove(&m.id);
            match test(&db, &m) {
                Ok(s) => json!({ "ok": true, "reached": true, "test": s }),
                Err(e) => json!({ "ok": true, "reached": false, "test_error": format!("{e:#}") }),
            }
        }
        "/api/machine-sync" => {
            let d = d.context(
                "syncing runs in the BackDown service: `backdown remote sync` in a terminal",
            )?;
            let machines: Vec<Machine> = match param("m") {
                Some(_) => vec![core::get(&db, param_id(param, "m")?)?],
                None => core::list(&db)?,
            };
            let jobs: Vec<u64> = machines.iter().map(|m| queue(d, m, "requested")).collect();
            json!({ "ok": true, "jobs": jobs })
        }
        "/api/machine-settings" => {
            for k in ["proof_days", "sync_hours"] {
                if let Some(v) = param(k) {
                    core::set_setting(
                        &db,
                        k,
                        v.trim().parse().with_context(|| format!("bad {k}"))?,
                    )?;
                }
            }
            json!({ "ok": true, "settings": core::settings(&db) })
        }
        _ => bail!("no such endpoint"),
    })
}

/// The other machines' copies of a file here, for its details (`/api/file`).
pub fn copies_for(db: &Db, info: &mut Value) -> Result<()> {
    if let (Some(h), Some(size)) = (
        info["sha256"].as_str().and_then(core::unhex),
        info["size"].as_u64(),
    ) {
        let c = core::copies_of(db, &h, size)?;
        if !c.is_empty() {
            info["remote_copies"] = json!(c);
            info["proof_days"] = json!(core::settings(db).proof_days);
        }
    }
    Ok(())
}
