//! The pool (docs/SWARM.md §3): `backdown pool add|list|set|remove` for this machine's pool
//! folders, and the browser's /api/pool* requests. Every machine's pool folders together are the
//! swarm's archive: `pool list` shows them all (the others' as they last said).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, bail};
use backdown_core::pool;
use backdown_core::{Db, fmt, swarm};
use clap::Subcommand;
use serde_json::{Value, json};

use crate::daemon::Daemon;
use crate::remote;

#[derive(Subcommand)]
pub enum PoolCmd {
    /// Make a folder on one of this machine's drives part of the pool (it's created if need be)
    Add {
        path: PathBuf,
        /// Room to keep free on its drive: a size (50G) or a share (5%); 5 % of the drive, at
        /// least 20 GB, unless said
        #[arg(long, value_name = "SIZE|PCT")]
        reserve: Option<String>,
        /// The top-level places it takes, like Mail,Media (unless said, anything)
        #[arg(long, value_name = "PLACES", value_delimiter = ',')]
        takes: Vec<String>,
    },
    /// Every pool folder of the swarm: this machine's, and the others' as they last said
    List {
        #[arg(long)]
        json: bool,
    },
    /// Change what a pool folder of this machine keeps free or takes
    Set {
        /// Its path, or its number from `pool list`
        folder: String,
        #[arg(long, value_name = "SIZE|PCT")]
        reserve: Option<String>,
        /// The top-level places it takes (an empty value: anything)
        #[arg(long, value_name = "PLACES", value_delimiter = ',')]
        takes: Option<Vec<String>>,
    },
    /// Stop treating a folder of this machine as part of the pool (nothing in it is touched)
    Remove {
        /// Its path, or its number from `pool list`
        folder: String,
    },
    /// Where a drive's things would go in the pool: what's in it already (by content, on any
    /// machine), what's new, and which pool folder each thing goes into. Kept as a pool plan
    Plan {
        /// The drive: its path as indexed here, or its folder name
        drive: String,
        #[arg(long)]
        json: bool,
    },
    /// Copy a drive into the pool as its plan says: each file read back there and compared by
    /// SHA-256; what's in the pool already, or not as the drive was last scanned, isn't sent.
    /// Stopped or interrupted, it carries on where it was
    Send {
        /// The plan's number (backdown pool plans)
        plan: i64,
    },
    /// The pool plans made here, or one of them
    Plans {
        /// A plan's number, to show it whole
        id: Option<i64>,
        #[arg(long)]
        json: bool,
    },
}

/// Ask this machine's BackDown service, if one runs, to index `path` (a new pool folder that no
/// indexed drive covers yet). Whether it was asked.
fn ask_index(db_path: &Path, path: &str) -> bool {
    let Some(mut c) = crate::client::Client::connect(Some(db_path)) else {
        return false;
    };
    c.request(&json!({ "cmd": "enqueue", "kind": "index", "path": path }))
        .is_ok_and(|r| r["ok"] == true)
}

fn area_line(a: &Value) -> String {
    let room = match (a["space"]["free"].as_u64(), a["room"].as_u64()) {
        (Some(free), Some(room)) => format!(
            "{} to fill ({} free, {} kept free)",
            fmt::bytes(room),
            fmt::bytes(free),
            fmt::bytes(a["reserve"].as_u64().unwrap_or(0))
        ),
        _ if a["answering"] == json!(false) => "its drive isn't answering".into(),
        _ => "its drive isn't there".into(),
    };
    let takes: Vec<&str> = a["takes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    format!(
        "{}  holds {} files, {}; {room}; takes {}{}",
        a["path"].as_str().unwrap_or("?"),
        fmt::count(a["files"].as_u64().unwrap_or(0)),
        fmt::bytes(a["bytes"].as_u64().unwrap_or(0)),
        if takes.is_empty() {
            "anything".to_string()
        } else {
            takes.join(", ")
        },
        if a["indexed"] == json!(false) {
            "; not indexed yet"
        } else {
            ""
        }
    )
}

pub fn command(db: Option<PathBuf>, local: bool, cmd: PoolCmd) -> Result<ExitCode> {
    let path = remote::index_path(db, local);
    let db = remote::open(&path)?;
    let parse = |r: &Option<String>| r.as_deref().map(pool::parse_reserve).transpose();
    match cmd {
        PoolCmd::Add {
            path: folder,
            reserve,
            takes,
        } => {
            let a = pool::add(&db, &folder, parse(&reserve)?, &takes)?;
            let r = pool::report(&db)?;
            let v = r
                .as_array()
                .and_then(|l| l.iter().find(|x| x["id"] == json!(a.id)))
                .cloned()
                .unwrap_or(Value::Null);
            println!("Pool folder {}: {}", a.id, area_line(&v));
            if v["indexed"] == json!(false) {
                if ask_index(&path, &a.path) {
                    println!("Indexing it now (BackDown's service does that in the background).");
                } else {
                    println!(
                        "Nothing can be filed into it until it's indexed: backdown index {}",
                        a.path
                    );
                }
            }
        }
        PoolCmd::List { json } => {
            let o = swarm::overview(&db, &crate::swarm::here_name(), swarm::storage(&db))?;
            let mut all = Vec::new();
            for m in o["machines"].as_array().into_iter().flatten() {
                for a in m["pool"].as_array().into_iter().flatten() {
                    let mut a = a.clone();
                    a["machine"] = m["name"].clone();
                    a["here"] = m["here"].clone();
                    all.push(a);
                }
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &json!({ "pool": all, "totals": o["totals"]["pool"] })
                    )?
                );
            } else if all.is_empty() {
                println!(
                    "No pool folders yet. On the machine whose drive it's on: backdown pool add /path/to/folder"
                );
            } else {
                for a in &all {
                    println!(
                        "{} {}  {}",
                        a["machine"].as_str().unwrap_or("?"),
                        a["id"],
                        area_line(a)
                    );
                }
                let t = &o["totals"]["pool"];
                println!(
                    "\nThe pool: {} files, {}; {} to fill",
                    fmt::count(t["files"].as_u64().unwrap_or(0)),
                    fmt::bytes(t["bytes"].as_u64().unwrap_or(0)),
                    fmt::bytes(t["room"].as_u64().unwrap_or(0))
                );
            }
        }
        PoolCmd::Set {
            folder,
            reserve,
            takes,
        } => {
            let a = pool::find(&db, &folder)?;
            if reserve.is_none() && takes.is_none() {
                bail!("change what? --reserve SIZE|PCT, --takes PLACES");
            }
            let a = pool::set(&db, a.id, parse(&reserve)?, takes.as_deref())?;
            println!("Pool folder {}: {}", a.id, a.path);
        }
        PoolCmd::Plan { drive, json } => {
            let id = drive_id(&db, &drive)?;
            let pp = make_plan(&db, id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&pp)?);
            } else {
                print_plan(&pp);
            }
        }
        PoolCmd::Send { plan } => {
            let mut last = std::time::Instant::now();
            let (st, to_index) = run_send(&path, &db, plan, None, &mut |p| {
                if last.elapsed().as_millis() >= 500 {
                    last = std::time::Instant::now();
                    eprint!(
                        "\r{}: {} files, {} of {} copied   ",
                        p.thing,
                        fmt::count(p.files),
                        fmt::bytes(p.bytes),
                        fmt::bytes(p.total_bytes)
                    );
                }
            })?;
            eprintln!();
            println!("Pool plan {plan}: {}", send_words(&st));
            for d in &st.done {
                println!("  {d}");
            }
            for p in &st.first_problems {
                eprintln!("  {p}");
            }
            // this machine's pool folders that got files: indexed now (or by its service)
            for f in to_index {
                let root = covering(&db, &f)?;
                let shown = root.display().to_string();
                let queued = crate::client::Client::connect(Some(&path)).is_some_and(|mut c| {
                    c.request(&json!({ "cmd": "enqueue", "kind": "index", "path": shown }))
                        .is_ok_and(|r| r["ok"] == true)
                });
                if queued {
                    println!("  BackDown's service is indexing {shown}");
                } else {
                    let mut w = Db::open(&path)?;
                    backdown_core::index::scan(
                        &mut w,
                        &root,
                        &backdown_core::index::Options {
                            one_file_system: true,
                            allow_empty: false,
                            allow_other_drive: false,
                        },
                        backdown_core::index::Method::Auto,
                        &mut |_| {},
                    )?;
                    let n = pool::receive::adopt(&w)?;
                    println!("  indexed {shown} ({n} files' hashes taken from their copies)");
                }
            }
        }
        PoolCmd::Plans { id: Some(id), json } => {
            let pp = pool::plan::load(&db, id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&pp)?);
            } else {
                print_plan(&pp);
            }
        }
        PoolCmd::Plans { id: None, json } => {
            let l = pool::plan::list(&db)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&l)?);
            } else if l.is_empty() {
                println!("No pool plans yet: backdown pool plan DRIVE");
            } else {
                for p in &l {
                    println!(
                        "{:>4}  {}  {}  {} to copy  {}",
                        p["id"],
                        fmt::day(p["created_at"].as_i64().unwrap_or(0)),
                        p["drive"].as_str().unwrap_or("?"),
                        fmt::bytes(
                            p["new_bytes"].as_u64().unwrap_or(0)
                                + p["unknown_bytes"].as_u64().unwrap_or(0)
                        ),
                        p["state"].as_str().unwrap_or("")
                    );
                }
            }
        }
        PoolCmd::Remove { folder } => {
            let a = pool::find(&db, &folder)?;
            pool::remove(&db, a.id)?;
            println!(
                "{} isn't part of the pool any more (nothing in it was touched)",
                a.path
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

// ---------- planning

/// The indexed drive here called `which`: its path, or its name.
fn drive_id(db: &Db, which: &str) -> Result<i64> {
    let want = std::fs::canonicalize(which).unwrap_or_else(|_| PathBuf::from(which));
    let roots = backdown_core::browse::roots(db)?;
    if let Some(r) = roots.iter().find(|r| Path::new(&r.path) == want) {
        return Ok(r.id);
    }
    let named: Vec<_> = roots.iter().filter(|r| r.name == which).collect();
    match named.as_slice() {
        [r] => Ok(r.id),
        [] => bail!("{which} isn't an indexed drive here (backdown scans)"),
        _ => bail!("several indexed drives are called {which}: give its path"),
    }
}

/// Make the plan for drive `root_id` here and keep it: this machine's filing schema, what's
/// recognised on the drive (worked out now if it's out of date), and every pool folder the swarm
/// has (this machine's measured now, the others' as last said).
pub fn make_plan(db: &Db, root_id: i64) -> Result<pool::plan::PoolPlan> {
    if !backdown_core::browse::roots(db)?
        .iter()
        .any(|r| r.id == root_id)
    {
        bail!("there's no indexed drive {root_id} here");
    }
    let (schema, _) = crate::settings::schema()?;
    let rec = crate::insight::recognised(db, &[root_id], false, false)?;
    let list: Vec<_> = rec.into_iter().map(|(r, _)| r).collect();
    let here = swarm::storage(db)?;
    let name = crate::swarm::here_name();
    let areas = pool::plan::areas(db, &name, &here)?;
    let mut pp = pool::plan::make(db, &name, root_id, &schema, &list, &areas)?;
    pool::plan::save(db, &mut pp)?;
    Ok(pp)
}

fn print_plan(pp: &pool::plan::PoolPlan) {
    let b = fmt::bytes;
    println!(
        "Pool plan {} for {} ({}): {} files, {}",
        pp.id.unwrap_or(0),
        pp.drive,
        pp.root,
        fmt::count(pp.files),
        b(pp.bytes)
    );
    println!(
        "  in the pool already {} · twice on the drive {} · left out {} · not compared yet {}",
        b(pp.in_pool_bytes),
        b(pp.twice_bytes),
        b(pp.left_bytes),
        b(pp.unknown_bytes)
    );
    println!(
        "  to copy: {}{}",
        b(pp.copy_bytes),
        if pp.unplaced_bytes > 0 {
            format!(", of which {} fits nowhere", b(pp.unplaced_bytes))
        } else {
            String::new()
        }
    );
    for u in pp.areas.iter().filter(|u| u.assigned_bytes > 0) {
        if let Some(at) = &u.at {
            println!(
                "  → {}  {} ({} room there)",
                at.label(),
                b(u.assigned_bytes),
                b(u.room)
            );
        }
    }
    println!();
    for (i, t) in pp.things.iter().enumerate() {
        let to = match &t.to {
            Some(at) => format!("→ {}", at.label()),
            None => format!("({})", t.why_to),
        };
        println!(
            "{:>3}. {}  {}  {} new{}  {}{}",
            i + 1,
            t.item.what,
            t.item.dest,
            b(t.new_bytes + t.unknown_bytes),
            if t.in_pool_bytes > 0 {
                format!(", {} in the pool already", b(t.in_pool_bytes))
            } else {
                String::new()
            },
            to,
            if t.to.is_some() && !t.why_to.is_empty() {
                format!(" ({})", t.why_to)
            } else {
                String::new()
            }
        );
    }
    for n in &pp.notes {
        eprintln!("note: {n}");
    }
    if pp.unknown_bytes > 0 {
        eprintln!(
            "{} couldn't be compared with the pool yet: hash this drive (backdown hash) and sync the pool's machines (backdown remote sync), then plan again",
            b(pp.unknown_bytes)
        );
    }
}

/// The person's choice for thing `i` of plan `id`: a pool folder (`to`: "here:ID" for one of
/// this machine's, "MACHINE:ID" for another's, by this index's numbers), or leaving it out.
pub fn choose_in(
    db: &Db,
    id: i64,
    i: usize,
    to: Option<&str>,
    skip: bool,
) -> Result<pool::plan::PoolPlan> {
    let mut pp = pool::plan::load(db, id)?;
    let here = swarm::storage(db)?;
    let areas = pool::plan::areas(db, &crate::swarm::here_name(), &here)?;
    let to = match to.filter(|t| !t.is_empty()) {
        None => None,
        Some(t) => {
            let (m, a) = t
                .split_once(':')
                .ok_or_else(|| anyhow::anyhow!("to is MACHINE:FOLDER"))?;
            let machine_id = if m == "here" {
                None
            } else {
                Some(m.parse::<i64>()?)
            };
            let area: i64 = a.parse()?;
            Some(
                areas
                    .iter()
                    .find(|x| x.at.machine_id == machine_id && x.at.area == area)
                    .map(|x| x.at.clone())
                    .ok_or_else(|| anyhow::anyhow!("that pool folder isn't there (any more)"))?,
            )
        }
    };
    pool::plan::choose(db, &mut pp, &areas, i, to, skip)?;
    pool::plan::save(db, &mut pp)?;
    Ok(pp)
}

/// Plan `id` is of a drive on this machine (only there can it be carried out).
fn check_here(db: &Db, id: i64) -> Result<()> {
    let pp = pool::plan::load(db, id)?;
    if pp.machine != crate::swarm::here_name() {
        bail!(
            "this plan is of a drive on {}: copy it from there",
            pp.machine
        );
    }
    Ok(())
}

/// Queue plan `id`'s copy in this machine's BackDown service (its side lane).
pub fn queue_send(d: &Daemon, id: i64) -> u64 {
    d.enqueue(
        "pool-send",
        Some(format!("pool plan {id}")),
        "requested",
        crate::daemon::JobOpts {
            plan: Some(id),
            ..Default::default()
        },
    )
}

/// Start plan `id`'s copy on this machine, for another machine that asked (`swarm-stdio`): in this
/// machine's BackDown service when one runs on this index, else in a `backdown pool send` of its
/// own, started now (it outlives the conversation). How it runs.
pub fn start_send(db_path: &Path, db: &Db, id: i64) -> Result<String> {
    check_here(db, id)?;
    if let Some(mut c) = crate::client::Client::connect(Some(db_path))
        && c.request(&json!({ "cmd": "enqueue", "kind": "pool-send", "plan": id }))
            .is_ok_and(|r| r["ok"] == true)
    {
        return Ok("service".into());
    }
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--db")
        .arg(db_path)
        .arg("--local")
        .arg("pool")
        .arg("send")
        .arg(id.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0); // not ended with the ssh session that started it
    }
    cmd.spawn()?;
    Ok("process".into())
}

/// How plan `id`'s copy is going: the plan's state (`sending` while it runs), what's to copy, and
/// its log so far by state.
pub fn send_progress(db: &Db, id: i64) -> Result<Value> {
    let pp = pool::plan::load(db, id)?;
    let by: Vec<Value> = pool::send::summary(db, id)?
        .into_iter()
        .map(|(state, files, bytes)| json!({ "state": state, "files": files, "bytes": bytes }))
        .collect();
    Ok(json!({ "id": id, "state": pp.state, "copy_bytes": pp.copy_bytes, "log": by }))
}

// ---------- copying into the pool

/// Carry out pool plan `id` (see backdown_core::pool::send): this machine's pool folders written
/// in-process, the others' through `swarm-stdio` there, over ssh. What happened, and this
/// machine's pool folders that received files (to be indexed).
pub fn run_send(
    db_path: &Path,
    db: &Db,
    id: i64,
    stop: Option<&std::sync::atomic::AtomicBool>,
    progress: &mut dyn FnMut(&pool::send::Progress),
) -> Result<(pool::send::Stats, Vec<PathBuf>)> {
    let name = crate::swarm::here_name();
    let storage = swarm::storage(db)?;
    let areas = pool::plan::areas(db, &name, &storage)?;
    let mut here = pool::send::Here {
        db,
        to_index: Vec::new(),
    };
    let mut ssh = crate::remote::swarm_stdio::Ssh {
        db: db_path.to_path_buf(),
        name: name.clone(),
    };
    let st = pool::send::send(db, id, &name, &areas, &mut here, &mut ssh, stop, progress)?;
    Ok((st, here.to_index))
}

/// The indexed drive whose scans cover `folder` (else the folder itself): what's indexed again
/// after files land in it.
fn covering(db: &Db, folder: &Path) -> Result<PathBuf> {
    Ok(backdown_core::browse::roots(db)?
        .into_iter()
        .filter(|r| backdown_core::names::within(folder, Path::new(&r.path)))
        .max_by_key(|r| r.path.len())
        .map_or(folder.to_path_buf(), |r| PathBuf::from(r.path)))
}

/// "4 copied (1.2 GB), 1 in the pool already, …"
pub fn send_words(st: &pool::send::Stats) -> String {
    let mut parts = vec![format!(
        "{} copied ({})",
        fmt::count(st.copied),
        fmt::bytes(st.copied_bytes)
    )];
    for (n, what) in [
        (st.renamed, "under a numbered name"),
        (st.same, "there already"),
        (st.in_pool, "in the pool already"),
        (st.twice, "twice on the drive"),
        (st.resumed, "done before"),
        (st.changed, "not as last scanned"),
        (st.failed, "failed"),
    ] {
        if n > 0 {
            parts.push(format!("{} {what}", fmt::count(n)));
        }
    }
    parts.join(", ")
}

/// The daemon's side-lane job: carry out pool plan `job.opts.plan`, then index this machine's
/// pool folders that received files.
pub fn run_send_job(
    d: &Daemon,
    db: &Db,
    job: &crate::daemon::Job,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<String> {
    let id = job
        .opts
        .plan
        .ok_or_else(|| anyhow::anyhow!("a pool send without a plan"))?;
    let (st, to_index) = run_send(&d.db, db, id, Some(stop), &mut |p| {
        d.progress(
            job.id,
            format!(
                "{}: {} files, {} copied",
                p.thing,
                fmt::count(p.files),
                fmt::bytes(p.bytes)
            ),
            crate::daemon::Counts {
                stage: "copying",
                files: p.files,
                total_files: 0,
                bytes: p.bytes,
                total_bytes: p.total_bytes,
            },
        )
    })?;
    for f in to_index {
        let root = covering(db, &f)?;
        d.enqueue(
            "index",
            Some(root.display().to_string()),
            "after-pool",
            Default::default(),
        );
    }
    let mut words = send_words(&st);
    if !st.done.is_empty() {
        words.push_str(&format!("; {}", st.done.join("; ")));
    }
    Ok(words)
}

// ---------- the browser's requests

pub fn handles(path: &str) -> bool {
    path.starts_with("/api/pool-")
}

/// GET /api/pool-plans (the plans made here), /api/pool-plan?id= (one of them).
pub fn get(db_path: &Path, path: &str, param: &dyn Fn(&str) -> Option<String>) -> Result<Value> {
    let db = Db::open_read_only(db_path)?;
    Ok(match path {
        "/api/pool-plans" => json!({ "plans": pool::plan::list(&db)? }),
        "/api/pool-plan" => {
            let id: i64 = param("id")
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("which plan?"))?;
            json!(pool::plan::load(&db, id)?)
        }
        _ => bail!("no such endpoint"),
    })
}

/// POST /api/pool-area-add?path=&reserve=&takes=, /api/pool-area-set?id=&reserve=&takes=,
/// /api/pool-area-remove?id= (this machine's own pool folders only: another machine's are changed
/// there); /api/pool-plan-make?root= and /api/pool-plan-choose?id=&i=&to=|skip=1 (a drive here into
/// the pool).
pub fn post(
    d: Option<&Daemon>,
    db_path: &Path,
    path: &str,
    param: &dyn Fn(&str) -> Option<String>,
) -> Result<Value> {
    let db = Db::open(db_path)?;
    let reserve = param("reserve")
        .filter(|r| !r.trim().is_empty())
        .map(|r| pool::parse_reserve(&r))
        .transpose()?;
    let takes = || -> Vec<String> {
        param("takes")
            .map(|t| t.split(',').map(str::to_string).collect())
            .unwrap_or_default()
    };
    let id = || -> Result<i64> {
        param("id")
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| anyhow::anyhow!("which pool folder?"))
    };
    Ok(match path {
        "/api/pool-area-add" => {
            let folder = param("path").unwrap_or_default();
            let a = pool::add(&db, Path::new(folder.trim()), reserve, &takes())?;
            let mut indexing = None;
            let covered = pool::report(&db)?
                .as_array()
                .and_then(|l| l.iter().find(|x| x["id"] == json!(a.id)))
                .is_some_and(|x| x["indexed"] == json!(true));
            if !covered && let Some(d) = d {
                indexing = Some(d.enqueue(
                    "index",
                    Some(a.path.clone()),
                    "requested",
                    Default::default(),
                ));
            }
            json!({ "ok": true, "area": a, "indexing": indexing })
        }
        "/api/pool-area-set" => {
            let t = param("takes").map(|_| takes());
            json!({ "ok": true, "area": pool::set(&db, id()?, reserve, t.as_deref())? })
        }
        "/api/pool-area-remove" => {
            pool::remove(&db, id()?)?;
            json!({ "ok": true })
        }
        // copying a drive into the pool: a job in the BackDown service's side lane
        "/api/pool-send" => {
            let d = d.ok_or_else(|| {
                anyhow::anyhow!(
                    "copying runs in BackDown's service: `backdown pool send PLAN` in a terminal"
                )
            })?;
            let id = id()?;
            check_here(&db, id)?;
            json!({ "ok": true, "job": queue_send(d, id) })
        }
        // a drive here into the pool: its plan, made now and kept
        "/api/pool-plan-make" => {
            let root: i64 = param("root")
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("which drive?"))?;
            json!(make_plan(&db, root)?)
        }
        // the person's choice for one thing: a pool folder (to=here:ID or MACHINE_ID:ID), or
        // leaving it out (skip=1)
        "/api/pool-plan-choose" => {
            let i: usize = param("i")
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("which thing?"))?;
            let skip = param("skip").as_deref() == Some("1");
            json!(choose_in(&db, id()?, i, param("to").as_deref(), skip)?)
        }
        _ => bail!("no such endpoint"),
    })
}
