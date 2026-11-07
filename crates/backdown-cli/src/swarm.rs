//! The swarm (docs/SWARM.md): `backdown swarm` (every machine's drives, how full they are and
//! what's indexed on them), `backdown swarm where NAME` (which machine, which drive, where), and
//! the browser's Swarm view (/api/swarm*).
//!
//! The machines are the ones `backdown remote` reaches: asked live over ssh when they can be (each
//! at once, on a thread of its own), and as last kept when they can't. This machine is probed now.
//! Each machine's own index answers for its files; nothing here is a second index.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Result, bail};
use backdown_core::remote::{self as core, Machine};
use backdown_core::{Db, find, fmt, swarm};
use clap::Subcommand;
use serde_json::{Value, json};

use crate::daemon::Daemon;
use crate::remote;

/// How long the cached files of a machine that can't be asked are searched, at most.
const CACHED_SEARCH: Duration = Duration::from_secs(5);

#[derive(Subcommand)]
pub enum SwarmCmd {
    /// Find files and folders by name on every machine: which machine, which drive, where
    Where {
        /// Words in the name (all of them must be in it)
        #[arg(required = true)]
        name: Vec<String>,
        /// List this many at most
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Don't ask the other machines now: search what was last synced of them
        #[arg(long)]
        cached: bool,
        /// Print them as JSON
        #[arg(long)]
        json: bool,
    },
}

/// This machine's name, the way the others' say theirs.
pub fn here_name() -> String {
    let n = remote::stdio::host_name();
    if n.is_empty() {
        "this machine".into()
    } else {
        n
    }
}

/// Ask machine `m` about its storage now, and keep the answer (or why there was none).
pub fn refresh(db_path: &Path, m: &Machine) -> Result<Value> {
    let answer = remote::live(db_path, m, "volumes", json!({}));
    let db = Db::open(db_path)?;
    core::store_volumes(&db, m.id, &answer)?;
    answer
}

/// Every machine asked about its storage at once (those that can be reached answer; why the
/// others didn't is kept).
fn refresh_all(db_path: &Path, machines: &[Machine]) {
    std::thread::scope(|s| {
        for m in machines {
            s.spawn(move || {
                let _ = refresh(db_path, m);
            });
        }
    });
}

/// The swarm at a glance: this machine probed now, the others as kept (asked first, with `ask`).
pub fn overview(db: &Db, db_path: &Path, ask: bool) -> Result<Value> {
    if ask && remote::allowed() {
        refresh_all(db_path, &core::list(db)?);
    }
    swarm::overview(db, &here_name(), swarm::storage(db))
}

/// One hit for the merged list: which machine, and the drive it's on.
fn tag(mut hit: Value, m: Option<&Machine>, storage: Option<&Value>) -> Value {
    hit["machine"] = json!(m.map_or_else(here_name, |m| m.name.clone()));
    hit["machine_id"] = json!(m.map(|m| m.id));
    hit["here"] = json!(m.is_none());
    swarm::with_place(hit, storage)
}

/// Files and folders named like `q` on one other machine (the search box asks each machine on its
/// own, so this machine's answer and each other one's show as they come): asked live (with `ask`)
/// or from its cached files.
pub fn where_one(db_path: &Path, q: &str, id: i64, limit: usize, ask: bool) -> Result<Value> {
    if swarm::terms(q).is_empty() {
        bail!("find what? Give some letters of the name");
    }
    let limit = limit.clamp(1, 500);
    let db = Db::open_read_only(db_path)?;
    let Some(m) = core::list(&db)?.into_iter().find(|m| m.id == id) else {
        bail!("there's no machine {id}");
    };
    let (info, hits) = search_one(db_path, &m, q, limit, ask && remote::allowed());
    Ok(json!({ "q": q, "hits": hits, "machines": [info] }))
}

/// Files and folders named like `q` on every machine: this one's index, each other machine's
/// asked live (with `ask`), or its cached files when it can't be; the best `limit` of them all,
/// each with its machine and drive, and how each machine was searched.
pub fn where_is(db_path: &Path, q: &str, limit: usize, ask: bool) -> Result<Value> {
    if swarm::terms(q).is_empty() {
        bail!("find what? Give some letters of the name");
    }
    let limit = limit.clamp(1, 500);
    let db = Db::open_read_only(db_path)?;
    let mut hits = Vec::new();
    let mut searched = Vec::new();
    let here = swarm::storage(&db).ok();
    let found = find::find(
        &db,
        &find::Query {
            q,
            within: None,
            earlier: false,
            limit,
        },
    )?;
    searched.push(
        json!({ "name": here_name(), "id": null, "here": true, "how": "here",
                          "total": found.total, "complete": !found.more, "ms": found.ms }),
    );
    hits.extend(
        found
            .hits
            .into_iter()
            .map(|h| tag(json!(h), None, here.as_ref())),
    );
    let machines = core::list(&db)?;
    let ask = ask && remote::allowed();
    let answers: Vec<(Value, Vec<Value>)> = std::thread::scope(|s| {
        let handles: Vec<_> = machines
            .iter()
            .map(|m| s.spawn(move || search_one(db_path, m, q, limit, ask)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join().unwrap_or_else(|_| {
                    (
                        json!({ "how": "none", "error": "the search failed" }),
                        Vec::new(),
                    )
                })
            })
            .collect()
    });
    for (info, h) in answers {
        searched.push(info);
        hits.extend(h);
    }
    hits.sort_by(|a, b| {
        b["rank"]
            .as_u64()
            .cmp(&a["rank"].as_u64())
            .then(b["mtime"].as_i64().cmp(&a["mtime"].as_i64()))
    });
    hits.truncate(limit);
    Ok(json!({ "q": q, "hits": hits, "machines": searched }))
}

/// Search one other machine: asked live, else its cached files. (how it went, its hits)
fn search_one(
    db_path: &Path,
    m: &Machine,
    q: &str,
    limit: usize,
    ask: bool,
) -> (Value, Vec<Value>) {
    let Ok(db) = Db::open_read_only(db_path) else {
        return (
            json!({ "name": m.name, "id": m.id, "how": "none", "error": "the index can't be read" }),
            Vec::new(),
        );
    };
    let storage = core::kept_volumes(&db, m.id)
        .ok()
        .flatten()
        .and_then(|k| k.1);
    let mut why = None;
    if ask {
        match remote::live(
            db_path,
            m,
            "find",
            json!({ "q": q, "limit": limit.min(500) }),
        ) {
            Ok(found) => {
                let hits = found["hits"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|h| tag(h, Some(m), storage.as_ref()))
                    .collect();
                return (
                    json!({ "name": m.name, "id": m.id, "here": false, "how": "live",
                            "total": found["total"], "complete": found["more"] != true, "ms": found["ms"] }),
                    hits,
                );
            }
            Err(e) => why = Some(format!("{e:#}")),
        }
    }
    match swarm::cached_find(&db, m, q, limit, CACHED_SEARCH) {
        Ok((hits, total, complete)) => (
            json!({ "name": m.name, "id": m.id, "here": false, "how": "cached", "total": total,
                    "complete": complete, "error": why, "synced_at": m.last_sync }),
            hits.into_iter()
                .map(|h| tag(h, Some(m), storage.as_ref()))
                .collect(),
        ),
        Err(e) => (
            json!({ "name": m.name, "id": m.id, "here": false, "how": "none",
                    "error": why.unwrap_or_else(|| format!("{e:#}")) }),
            Vec::new(),
        ),
    }
}

// ---------- the command line

pub fn command(
    db: Option<PathBuf>,
    local: bool,
    cmd: Option<SwarmCmd>,
    cached: bool,
    json_out: bool,
) -> Result<ExitCode> {
    let path = remote::index_path(db, local);
    let db = remote::open(&path)?;
    match cmd {
        None => {
            if !cached && remote::allowed() && !core::list(&db)?.is_empty() {
                eprintln!("Asking the other machines…");
            }
            let o = overview(&db, &path, !cached)?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&o)?);
            } else {
                print_overview(&o);
            }
        }
        Some(SwarmCmd::Where {
            name,
            limit,
            cached,
            json,
        }) => {
            drop(db);
            let w = where_is(&path, &name.join(" "), limit, !cached)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&w)?);
            } else {
                print_where(&w);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
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

fn n(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

/// "████████░░ 80 % used"
fn bar(total: u64, free: u64) -> String {
    let used = total.saturating_sub(free);
    let filled = if total == 0 {
        0
    } else {
        ((used as f64 / total as f64) * 10.0).round() as usize
    };
    format!(
        "{}{} {:>3} % used",
        "█".repeat(filled.min(10)),
        "░".repeat(10 - filled.min(10)),
        (used * 100).checked_div(total).unwrap_or(0)
    )
}

/// A drive's name for people: its label, else its model.
fn drive_words(d: &Value) -> String {
    let label = d["volume"]["label"].as_str().filter(|s| !s.is_empty());
    let model = d["disk"]["model"].as_str().filter(|s| !s.is_empty());
    let serial = d["disk"]["serial"].as_str().filter(|s| !s.is_empty());
    match (label, model) {
        (Some(l), Some(m)) => format!(
            "{l} ({m}{})",
            serial.map(|s| format!(" s/n {s}")).unwrap_or_default()
        ),
        (Some(l), None) => l.to_string(),
        (None, Some(m)) => format!(
            "{m}{}",
            serial.map(|s| format!(" s/n {s}")).unwrap_or_default()
        ),
        (None, None) => d["volume"]["device"].as_str().unwrap_or("?").to_string(),
    }
}

fn print_overview(o: &Value) {
    for m in o["machines"].as_array().into_iter().flatten() {
        let name = m["name"].as_str().unwrap_or("?");
        let state = match m["state"].as_str().unwrap_or("") {
            "here" => "this machine".to_string(),
            "ok" => format!("drives as of {}", ago(m["ok_at"].as_i64())),
            "unsupported" => format!(
                "BackDown {} there doesn't report its drives: update it",
                m["version"].as_str().unwrap_or("(older)")
            ),
            "error" => format!(
                "couldn't be asked ({}){}",
                m["error"].as_str().unwrap_or("?"),
                m["ok_at"]
                    .as_i64()
                    .map(|t| format!("; drives as of {}", ago(Some(t))))
                    .unwrap_or_default()
            ),
            _ => "not asked yet: `backdown remote sync`".to_string(),
        };
        println!("{name}  {state}");
        for d in m["disks"].as_array().into_iter().flatten() {
            let mut what = vec![d["name"].as_str().unwrap_or("?").to_string()];
            if let Some(model) = d["model"].as_str() {
                what.push(model.to_string());
            }
            if let Some(s) = d["serial"].as_str() {
                what.push(format!("s/n {s}"));
            }
            what.push(fmt::bytes(n(&d["size"])));
            if let Some(t) = d["transport"].as_str() {
                what.push(t.to_string());
            }
            if let Some(h) = d["health"].as_str() {
                what.push(format!("health {h}"));
            }
            println!("  {}", what.join("  "));
            for v in d["volumes"].as_array().into_iter().flatten() {
                let label = v["label"]
                    .as_str()
                    .or(v["part_label"].as_str())
                    .unwrap_or(v["name"].as_str().unwrap_or(""));
                let fs = v["fs_name"].as_str().unwrap_or("");
                let mounts: Vec<&str> = v["mounts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let room = match (v["space"]["total"].as_u64(), v["space"]["free"].as_u64()) {
                    (Some(t), Some(f)) => {
                        format!("{} free of {}  {}", fmt::bytes(f), fmt::bytes(t), bar(t, f))
                    }
                    _ => format!("{}, not mounted", fmt::bytes(n(&v["size"]))),
                };
                println!(
                    "    {label} {fs}{}  {room}",
                    if mounts.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", mounts.join(", "))
                    }
                );
                for i in v["indexed"].as_array().into_iter().flatten() {
                    println!(
                        "      indexed: {}  {} files, {}",
                        i["path"].as_str().unwrap_or("?"),
                        fmt::count(n(&i["files"])),
                        fmt::bytes(n(&i["bytes"]))
                    );
                }
            }
        }
        for r in m["elsewhere"].as_array().into_iter().flatten() {
            let room = match (r["space"]["total"].as_u64(), r["space"]["free"].as_u64()) {
                (Some(t), Some(f)) => format!("  {} free of {}", fmt::bytes(f), fmt::bytes(t)),
                _ => String::new(),
            };
            println!(
                "  {}  {} files, {}{room}",
                r["path"].as_str().unwrap_or("?"),
                fmt::count(n(&r["files"])),
                fmt::bytes(n(&r["bytes"]))
            );
        }
        for r in m["away"].as_array().into_iter().flatten() {
            let drive = if r["drive"].is_object() {
                format!(" ({})", drive_words(&r["drive"]))
            } else {
                String::new()
            };
            println!(
                "  not connected: {}{drive}  {} files, {}, scanned {}",
                r["path"].as_str().unwrap_or("?"),
                fmt::count(n(&r["files"])),
                fmt::bytes(n(&r["bytes"])),
                r["scanned_at"].as_i64().map_or("never".into(), fmt::day)
            );
        }
        for r in m["unanswered"].as_array().into_iter().flatten() {
            println!(
                "  not answering: {}  {} files, {}",
                r["path"].as_str().unwrap_or("?"),
                fmt::count(n(&r["files"])),
                fmt::bytes(n(&r["bytes"]))
            );
        }
        println!();
    }
    let t = &o["totals"];
    println!(
        "{} machine{} ({} reporting their drives), {} drives: {} free of {}; {} files indexed ({})",
        n(&t["machines"]),
        if n(&t["machines"]) == 1 { "" } else { "s" },
        n(&t["reporting"]),
        n(&t["disks"]),
        fmt::bytes(n(&t["free"])),
        fmt::bytes(n(&t["capacity"])),
        fmt::count(n(&t["files"])),
        fmt::bytes(n(&t["bytes"]))
    );
}

fn print_where(w: &Value) {
    let hits = w["hits"].as_array().cloned().unwrap_or_default();
    if hits.is_empty() {
        println!("Nothing named like that on any machine.");
    }
    for h in &hits {
        let drive = if h["place"].is_object() {
            let mut s = drive_words(&h["place"]);
            if h["place"]["present"] == json!(false) {
                s.push_str(", not connected");
            }
            s
        } else {
            h["drive"].as_str().unwrap_or("").to_string()
        };
        let what = if h["kind"] == "dir" {
            "folder".to_string()
        } else {
            fmt::bytes(n(&h["size"]))
        };
        println!(
            "{}  {}  {}  ({what}, {})",
            h["machine"].as_str().unwrap_or("?"),
            drive,
            h["path"].as_str().unwrap_or("?"),
            h["mtime"].as_i64().map_or("?".into(), fmt::day)
        );
    }
    for m in w["machines"].as_array().into_iter().flatten() {
        let name = m["name"].as_str().unwrap_or("?");
        match m["how"].as_str().unwrap_or("") {
            "cached" => eprintln!(
                "{name} couldn't be asked{}: searched its files as synced {}{}",
                m["error"]
                    .as_str()
                    .map(|e| format!(" ({e})"))
                    .unwrap_or_default(),
                ago(m["synced_at"].as_i64()),
                if m["complete"] == json!(false) {
                    " (only some of them, in the time there was)"
                } else {
                    ""
                }
            ),
            "none" => eprintln!(
                "{name} wasn't searched: {}",
                m["error"].as_str().unwrap_or("?")
            ),
            _ => {}
        }
    }
}

// ---------- a drive on another machine into the pool

/// Ask machine `m`'s `swarm-stdio` for `op` with `args` (its own drive into the pool: `plan`,
/// `plan-get`, `choose`, `send`, `progress`); the answer's data.
pub fn remote_pool(
    db_path: &Path,
    m_id: i64,
    op: &str,
    args: Value,
    timeout: Duration,
) -> Result<Value> {
    let db = Db::open_read_only(db_path)?;
    let m = core::get(&db, m_id)?;
    let mut peer = remote::ssh::SshPeer::open_for(&m, "swarm-stdio")?;
    let h = peer.exchange(
        "hello",
        json!({ "from": here_name() }),
        None,
        Duration::from_secs(60),
    )?;
    if h["ok"] != true {
        bail!(
            "{}: {}",
            m.name,
            h["error"].as_str().unwrap_or("it refused")
        );
    }
    let v = peer.exchange(op, args, None, timeout)?;
    if v["ok"] != true {
        let e = v["error"].as_str().unwrap_or("it refused");
        if e.contains("unknown variant") {
            bail!(
                "the BackDown on {} is too old to plan its drives into the pool from here: update it",
                m.name
            );
        }
        bail!("{}: {e}", m.name);
    }
    Ok(v["data"].clone())
}

// ---------- the browser's endpoints

pub fn handles(path: &str) -> bool {
    path == "/api/swarm" || path.starts_with("/api/swarm-")
}

/// POST /api/swarm-pool?m=&op=plan&root= | op=choose&plan=&i=&to=|skip=1 | op=send&plan=: a drive
/// on another machine into the pool, done there (its `swarm-stdio`, over ssh with your keys).
pub fn post(
    desktop: bool,
    db_path: &Path,
    path: &str,
    param: &dyn Fn(&str) -> Option<String>,
) -> Result<Value> {
    if path != "/api/swarm-pool" {
        bail!("no such endpoint");
    }
    if let Some(b) = remote::blocker(desktop) {
        bail!(b);
    }
    let num = |k: &str| -> Result<i64> {
        param(k)
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| anyhow::anyhow!("missing {k}"))
    };
    let m = num("m")?;
    match param("op").as_deref() {
        // recognising what's on the drive there may take a while the first time
        Some("plan") => remote_pool(
            db_path,
            m,
            "plan",
            json!({ "root": num("root")? }),
            Duration::from_secs(1800),
        ),
        Some("choose") => {
            let mut args = json!({ "plan": num("plan")?, "i": num("i")?, "skip": param("skip").as_deref() == Some("1") });
            if let Some(t) = param("to").filter(|t| !t.is_empty()) {
                args["to"] = json!(t);
            }
            remote_pool(db_path, m, "choose", args, Duration::from_secs(300))
        }
        Some("send") => remote_pool(
            db_path,
            m,
            "send",
            json!({ "plan": num("plan")? }),
            Duration::from_secs(60),
        ),
        _ => bail!("ask for plan, choose or send"),
    }
}

/// GET /api/swarm (the overview: this machine now, the others as kept), /api/swarm-machine?m=
/// (ask one machine about its storage now), /api/swarm-where?q= (where is it, on every machine).
pub fn get(
    d: Option<&Daemon>,
    desktop: bool,
    db_path: &Path,
    path: &str,
    param: &dyn Fn(&str) -> Option<String>,
) -> Result<Value> {
    let blocker = remote::blocker(desktop);
    Ok(match path {
        "/api/swarm" => {
            let db = Db::open_read_only(db_path)?;
            let mut o = swarm::overview(&db, &here_name(), swarm::storage(&db))?;
            o["can_ask"] = json!(blocker.is_none());
            o["why_not"] = json!(blocker);
            o["daemon"] = json!(d.is_some());
            o
        }
        "/api/swarm-machine" => {
            let id: i64 = param("m")
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("which machine?"))?;
            let db = Db::open_read_only(db_path)?;
            let m = core::get(&db, id)?;
            let asked = match blocker {
                Some(b) => Err(anyhow::anyhow!(b)),
                None => refresh(db_path, &m),
            };
            let db = Db::open_read_only(db_path)?;
            let mut v = swarm::one(&db, &core::get(&db, id)?)?;
            v["asked"] = json!(asked.is_ok());
            v["ask_error"] = json!(asked.err().map(|e| format!("{e:#}")));
            v
        }
        // another machine's drive into the pool: its plan and its copy, as that machine has them
        "/api/swarm-pool" => {
            if let Some(b) = blocker {
                bail!(b);
            }
            let m: i64 = param("m")
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("which machine?"))?;
            let plan: i64 = param("plan")
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("which plan?"))?;
            let op = param("op").unwrap_or_default();
            if !matches!(op.as_str(), "plan-get" | "progress") {
                bail!("ask for plan-get or progress");
            }
            remote_pool(
                db_path,
                m,
                &op,
                json!({ "plan": plan }),
                Duration::from_secs(60),
            )?
        }
        "/api/swarm-where" => {
            let q = param("q").unwrap_or_default();
            let limit = param("limit").and_then(|v| v.parse().ok()).unwrap_or(200);
            // one machine (`m`), asked live or (`ask=0`) from what's cached of it
            match param("m").and_then(|v| v.parse::<i64>().ok()) {
                Some(id) => where_one(
                    db_path,
                    &q,
                    id,
                    limit,
                    blocker.is_none() && param("ask").as_deref() != Some("0"),
                )?,
                None => where_is(db_path, &q, limit, blocker.is_none())?,
            }
        }
        _ => bail!("no such endpoint"),
    })
}
