//! `backdown serve`: the file browser, a local web UI over the index.
//!
//! Security model: listen on 127.0.0.1 only; reject any Host header but our own (DNS rebinding);
//! require a random per-run token, passed once in the URL and then kept in a SameSite=Strict cookie;
//! serve file bytes only by index id, never by arbitrary path; never serve active content (HTML is
//! sent as text, and every file response carries a CSP sandbox), and open SQLite files immutable.
//! Inside containers (archives, disk images, mail), peek.rs reads bounded ranges on a time budget,
//! turns HTML mail into plain text, and picks an entry only by its name in the container's own
//! listing; entries go out with the same headers as the file bytes. mailbox.rs threads an mbox (by
//! id) or a Maildir (an indexed folder, its messages read by their index ids) from headers alone,
//! on the same kind of budget. A filesystem inside a disk image (/api/fs, and /api/entry with
//! `part`) is picked by its partition number in the image's own table, and a file in it by its
//! path through that filesystem's own folder listings.
//! Nothing here writes to a source: the index is opened read-only per request, and GET /api/volumes
//! runs the same unprivileged, read-only disk probe as `backdown volumes`. The requests that do
//! something, POST /api/rescan (daemon only) and POST /api/open (hand a file or folder to the desktop),
//! also need an `X-BackDown` header, which a cross-site page can't send without a CORS preflight this
//! server never grants. /api/open never opens anything executable, and is off for the system service.
//! POST /api/wipe (daemon only, never the system service) deletes: it re-checks the selection the
//! way `backdown wipe` does and needs the drive names typed back, and the daemon re-reads every file
//! and its copy before deleting it.
//! POST /api/dedup (daemon only, never the system service) records a run that links identical
//! files inside one folder, which the daemon does after re-reading both of each pair; POST
//! /api/plan-merge changes a saved plan that hasn't started, so it merges two snapshots of a
//! mailbox instead of copying both. Both need the same header.
//! GET /api/health reads drive health the same unprivileged way as `backdown health` (in the daemon,
//! its latest round); the POSTs beside it (read now, mark notifications read, notification
//! settings, a test notification) need the same header, change nothing but BackDown's own records
//! and settings file, and a notification's text never goes through a shell.
//! Drives (drives.rs; the person at the screen's BackDown only, never the system service, and each
//! with the X-BackDown header): POST /api/mount mounts read-only (never read-write) a partition the
//! volume probe calls mountable, and /api/attach a disk image picked by index id; /api/unmount and
//! /api/detach undo only what BackDown mounted; /api/rescue-allow asks udisks (and so polkit's
//! password dialog) for a read-only file for a drive the probe lists. A filesystem Rescue found is
//! browsed (/api/fs and /api/entry with `rescue` and `at`) and copied out of (/api/rescue with mode
//! `files`) at the place its record gives, on the drive or image the record names: the browser
//! never names a device or a path, and picks files through the filesystem's own listings (deleted
//! ones by the segment their listing gave them).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use backdown_core::{
    Db, apply, browse, dedup, fmt, names, overlap, paths, plan, recognise, report, rescue, summary,
    volumes, wipe,
};

use crate::daemon::Daemon;
use crate::peek;
use rusqlite::{OpenFlags, types::ValueRef};
use serde_json::{Value, json};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const UI_CSP: &str = "default-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; style-src 'self' 'unsafe-inline'; \
    script-src 'self' 'wasm-unsafe-eval'; worker-src 'self' blob:; connect-src 'self'; font-src 'self' data:; \
    frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const FILE_CSP: &str = "sandbox; default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'; media-src 'self'";
const TEXT_LIMIT: u64 = 8 << 20;

struct Ctx {
    db: PathBuf,
    token: String,
    hosts: Vec<String>,
    daemon: Option<Arc<Daemon>>,
    /// running as the person at the screen (not the system service), so files can be handed to
    /// their desktop's apps
    desktop: bool,
}

pub fn new_token() -> Result<String> {
    let mut raw = [0u8; 24];
    getrandom::fill(&mut raw)
        .map_err(|e| anyhow::anyhow!("no randomness for the session token: {e}"))?;
    Ok(fmt::hex(&raw))
}

pub fn bind(port: u16) -> Result<(Server, u16)> {
    let server = Server::http(("127.0.0.1", port))
        .map_err(|e| anyhow::anyhow!("can't listen on 127.0.0.1:{port}: {e}"))?;
    let port = server.server_addr().to_ip().map_or(port, |a| a.port());
    Ok((server, port))
}

/// Serve the UI until the process ends. With a daemon, the UI also shows its activity and can
/// queue rescans.
pub fn run(
    server: Server,
    db: PathBuf,
    token: String,
    port: u16,
    daemon: Option<Arc<Daemon>>,
    desktop: bool,
) {
    let ctx = Arc::new(Ctx {
        db,
        token,
        hosts: vec![
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
        ],
        daemon,
        desktop,
    });
    for req in server.incoming_requests() {
        let ctx = Arc::clone(&ctx);
        std::thread::spawn(move || handle(&ctx, req));
    }
}

/// `backdown serve`: the UI alone, in the foreground, with a one-run link.
pub fn serve(db: &Path, port: u16, open_browser: bool) -> Result<()> {
    Db::open(db)?; // create/migrate once; requests then use read-only connections
    let db = dunce::canonicalize(db)?;
    let (server, port) = bind(port)?;
    let token = new_token()?;
    let url = format!("http://127.0.0.1:{port}/?t={token}");
    eprintln!(
        "BackDown browser for {} at\n  {url}\n(only this link, only on this computer; Ctrl-C to stop)",
        db.display()
    );
    if open_browser {
        open(&url);
    }
    run(server, db, token, port, None, true);
    Ok(())
}

pub fn open(url: &str) {
    let spawned = if cfg!(target_os = "macos") {
        Command::new("open").arg(url).spawn()
    } else if cfg!(windows) {
        Command::new("cmd").args(["/C", "start", "", url]).spawn()
    } else {
        Command::new("xdg-open").arg(url).spawn()
    };
    if spawned.is_err() {
        eprintln!("(couldn't open a browser; paste the link above)");
    }
}

type Resp = Response<std::io::Cursor<Vec<u8>>>;

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).expect("static header")
}

fn plain(status: u16, msg: &str) -> Resp {
    Response::from_string(msg)
        .with_status_code(status)
        .with_header(header("Content-Type", "text/plain; charset=utf-8"))
}

fn json_resp(v: &impl serde::Serialize) -> Resp {
    Response::from_data(serde_json::to_vec(v).unwrap_or_default())
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Cache-Control", "no-store"))
}

fn handle(ctx: &Ctx, req: Request) {
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let params: Vec<(String, String)> = query
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (decode(k), decode(v))
        })
        .collect();
    let param = |name: &str| get(&params, name);

    let host = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Host"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();
    if !ctx.hosts.contains(&host) {
        let _ = req.respond(plain(403, "wrong host"));
        return;
    }
    // the link carries the token once (or a one-time code from `backdown app`/`open`, which keeps
    // the token off command lines); swap it for a cookie and drop it from the address bar
    if path == "/"
        && (param("t") == Some(ctx.token.as_str()) || param("c").is_some_and(crate::app::redeem))
    {
        let resp = Response::empty(StatusCode(303))
            .with_header(header("Location", "/"))
            .with_header(header(
                "Set-Cookie",
                &format!("bd={}; Path=/; HttpOnly; SameSite=Strict", ctx.token),
            ));
        let _ = req.respond(resp);
        return;
    }
    let cookie_ok = req
        .headers()
        .iter()
        .filter(|h| h.field.equiv("Cookie"))
        .flat_map(|h| {
            h.value
                .as_str()
                .split(';')
                .map(str::trim)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .any(|c| c == format!("bd={}", ctx.token));
    if !cookie_ok {
        let _ = req.respond(plain(
            401,
            "This BackDown window has expired: open BackDown again from your applications (or run `backdown open`, or use the link `backdown serve` printed).",
        ));
        return;
    }
    // Pro features, as tiers.rs's table says: a 402 that explains what's needed
    if let Some(refusal) = crate::tiers::gate_endpoint(path) {
        let _ = req.respond(json_resp(&refusal).with_status_code(402));
        return;
    }
    // Settings, licence, updates and launching (settings.rs, with the same POST rules)
    if crate::settings::handles(path) {
        let env = crate::settings::Env {
            daemon: ctx.daemon.as_ref(),
            desktop: ctx.desktop,
            db: &ctx.db,
        };
        crate::settings::handle(req, path, &params, env);
        return;
    }
    if *req.method() == Method::Post && path == "/api/open" {
        let marked = req.headers().iter().any(|h| h.field.equiv("X-BackDown"));
        let resp = if !marked {
            plain(403, "missing X-BackDown header")
        } else {
            match open_on_desktop(ctx, param("id"), param("dir"), param("reveal") == Some("1")) {
                Ok(v) => json_resp(&v),
                Err(e) => json_resp(&json!({ "error": format!("{e:#}") })).with_status_code(400),
            }
        };
        let _ = req.respond(resp);
        return;
    }
    if *req.method() == Method::Post && (path == "/api/plan" || path == "/api/apply") {
        let marked = req.headers().iter().any(|h| h.field.equiv("X-BackDown"));
        let resp = if !marked {
            plain(403, "missing X-BackDown header")
        } else {
            let r = if path == "/api/plan" {
                new_plan(
                    ctx,
                    param("to"),
                    param("from"),
                    param("skip_junk") == Some("1"),
                )
            } else {
                start_apply(ctx, param("id"))
            };
            match r {
                Ok(v) => json_resp(&v),
                Err(e) => json_resp(&json!({ "error": format!("{e:#}") })).with_status_code(400),
            }
        };
        let _ = req.respond(resp);
        return;
    }
    if *req.method() == Method::Post && path == "/api/wipe" {
        let marked = req.headers().iter().any(|h| h.field.equiv("X-BackDown"));
        let resp = if !marked {
            plain(403, "missing X-BackDown header")
        } else {
            match start_wipe(
                ctx,
                param("keys"),
                param("move_to"),
                param("confirm"),
                param("trust") == Some("1"),
            ) {
                Ok(v) => json_resp(&v),
                Err(e) => json_resp(&json!({ "error": format!("{e:#}") })).with_status_code(400),
            }
        };
        let _ = req.respond(resp);
        return;
    }
    if *req.method() == Method::Post && (path == "/api/dedup" || path == "/api/plan-merge") {
        // tidy an archive (the daemon links, after re-reading both files), or turn a plan's
        // overlapping mailboxes into one merged mailbox
        let marked = req.headers().iter().any(|h| h.field.equiv("X-BackDown"));
        let resp = if !marked {
            plain(403, "missing X-BackDown header")
        } else {
            let r = if path == "/api/dedup" {
                start_dedup(ctx, param("folder"), param("link"), param("min"))
            } else {
                plan_merge(ctx, param("id"), param("item"))
            };
            match r {
                Ok(v) => json_resp(&v),
                Err(e) => json_resp(&json!({ "error": format!("{e:#}") })).with_status_code(400),
            }
        };
        let _ = req.respond(resp);
        return;
    }
    if *req.method() == Method::Post && path == "/api/rescue" {
        let marked = req.headers().iter().any(|h| h.field.equiv("X-BackDown"));
        let resp = if !marked {
            plain(403, "missing X-BackDown header")
        } else {
            match start_rescue(ctx, &params) {
                Ok(v) => json_resp(&v),
                Err(e) => json_resp(&json!({ "error": format!("{e:#}") })).with_status_code(400),
            }
        };
        let _ = req.respond(resp);
        return;
    }
    if *req.method() == Method::Post && path == "/api/job-stop" {
        // stop a queued job, or a running one that can stop (a rescue): the map or results so far stay
        let marked = req.headers().iter().any(|h| h.field.equiv("X-BackDown"));
        let resp = match (
            marked,
            &ctx.daemon,
            param("id").and_then(|v| v.parse::<u64>().ok()),
        ) {
            (false, _, _) => plain(403, "missing X-BackDown header"),
            (true, None, _) => json_resp(&json!({ "error": "no daemon" })).with_status_code(409),
            (true, Some(_), None) => {
                json_resp(&json!({ "error": "missing id" })).with_status_code(400)
            }
            (true, Some(d), Some(id)) => match d.stop(id) {
                Ok(state) => json_resp(&json!({ "ok": true, "state": state })),
                Err(e) => json_resp(&json!({ "error": format!("{e:#}") })).with_status_code(409),
            },
        };
        let _ = req.respond(resp);
        return;
    }
    if *req.method() == Method::Post && path == "/api/index" {
        // index a folder that isn't indexed yet (a newly plugged-in drive): reading only
        let marked = req.headers().iter().any(|h| h.field.equiv("X-BackDown"));
        let resp = match (marked, &ctx.daemon, param("path")) {
            (false, _, _) => plain(403, "missing X-BackDown header"),
            (true, None, _) => json_resp(&json!({ "error": "indexing runs in the BackDown service: `backdown index PATH` in a terminal" })).with_status_code(409),
            (true, Some(_), None) => json_resp(&json!({ "error": "missing path" })).with_status_code(400),
            (true, Some(d), Some(p)) => {
                let path = PathBuf::from(p);
                if !path.is_absolute() || !path.is_dir() {
                    json_resp(&json!({ "error": format!("{p} isn't a folder here") })).with_status_code(400)
                } else {
                    json_resp(&json!({ "ok": true, "job": d.enqueue("index", Some(path.display().to_string()), "requested", Default::default()) }))
                }
            }
        };
        let _ = req.respond(resp);
        return;
    }
    if *req.method() == Method::Post && path == "/api/recognise" {
        let marked = req.headers().iter().any(|h| h.field.equiv("X-BackDown"));
        let resp = match (marked, &ctx.daemon) {
            (false, _) => plain(403, "missing X-BackDown header"),
            (true, None) => {
                json_resp(&json!({ "error": "no daemon: run `backdown entities` in a terminal" }))
                    .with_status_code(409)
            }
            (true, Some(d)) => json_resp(
                &json!({ "ok": true, "job": d.enqueue("entities", None, "requested", Default::default()) }),
            ),
        };
        let _ = req.respond(resp);
        return;
    }
    if *req.method() == Method::Post && crate::health::handles(path) {
        // drive health and notifications: read the drives now, mark notifications read, change
        // or try the notification settings (health.rs)
        let marked = req.headers().iter().any(|h| h.field.equiv("X-BackDown"));
        let resp = if !marked {
            plain(403, "missing X-BackDown header")
        } else {
            let p = |k: &str| param(k).map(str::to_string);
            match crate::health::post(ctx.daemon.as_deref(), &ctx.db, path, &p) {
                Ok(v) => json_resp(&v),
                Err(e) => json_resp(&json!({ "error": format!("{e:#}") })).with_status_code(400),
            }
        };
        let _ = req.respond(resp);
        return;
    }
    if *req.method() == Method::Post && path == "/api/rescan" {
        let marked = req.headers().iter().any(|h| h.field.equiv("X-BackDown"));
        let resp = match (marked, &ctx.daemon) {
            (false, _) => plain(403, "missing X-BackDown header"),
            (true, None) => json_resp(
                &json!({ "error": "no daemon: this browser was started with `backdown serve`" }),
            )
            .with_status_code(409),
            (true, Some(d)) => match rescan(ctx, d, param("root")) {
                Ok(v) => json_resp(&v),
                Err(e) => json_resp(&json!({ "error": format!("{e:#}") })).with_status_code(400),
            },
        };
        let _ = req.respond(resp);
        return;
    }
    if *req.method() == Method::Post
        && matches!(
            path,
            "/api/mount" | "/api/unmount" | "/api/attach" | "/api/detach" | "/api/rescue-allow"
        )
    {
        // read-only mounts, and reading a drive through udisks (drives.rs): the person at the
        // screen's own BackDown only
        let marked = req.headers().iter().any(|h| h.field.equiv("X-BackDown"));
        let resp = if !marked {
            plain(403, "missing X-BackDown header")
        } else if !ctx.desktop {
            json_resp(&json!({ "error": "the machine-wide service only reads what's mounted: mount drives from your own BackDown" }))
                .with_status_code(409)
        } else {
            let p = |n: &str| get(&params, n).map(str::to_string);
            match crate::drives::post(&ctx.db, path, &p) {
                Ok(v) => json_resp(&v),
                Err(e) => json_resp(&crate::drives::error_json(&e)).with_status_code(400),
            }
        };
        let _ = req.respond(resp);
        return;
    }
    if *req.method() != Method::Get {
        let _ = req.respond(plain(405, "read-only"));
        return;
    }

    if let Some(asset) = path
        .strip_prefix('/')
        .and_then(|p| ui_asset(if p.is_empty() { "index.html" } else { p }))
    {
        let (body, ct) = asset;
        let mut resp = Response::from_data(body.to_vec())
            .with_header(header("Content-Type", ct))
            .with_header(header("X-Content-Type-Options", "nosniff"));
        if ct.starts_with("text/html") {
            resp = resp
                .with_header(header("Content-Security-Policy", UI_CSP))
                .with_header(header("Cache-Control", "no-store"));
        }
        let _ = req.respond(resp);
        return;
    }
    if path == "/api/report" {
        let resp = match report(ctx, param("format") == Some("md")) {
            Ok((body, md)) => {
                let r = Response::from_data(body.into_bytes())
                    .with_header(header(
                        "Content-Type",
                        if md {
                            "text/markdown; charset=utf-8"
                        } else {
                            "text/html; charset=utf-8"
                        },
                    ))
                    .with_header(header("Content-Security-Policy", UI_CSP))
                    .with_header(header("X-Content-Type-Options", "nosniff"))
                    .with_header(header("Cache-Control", "no-store"));
                if md {
                    r.with_header(header(
                        "Content-Disposition",
                        "attachment; filename=\"backdown-report.md\"",
                    ))
                } else {
                    r
                }
            }
            Err(e) => plain(500, &format!("{e:#}")),
        };
        let _ = req.respond(resp);
        return;
    }
    if path == "/api/content" {
        let range = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("Range"))
            .map(|h| h.value.as_str().to_string());
        let download = param("download") == Some("1");
        return content(ctx, req, param("id"), range.as_deref(), download);
    }
    if path == "/api/entry" || path == "/api/attachment" {
        return inner(ctx, req, path, &params);
    }
    let result = api(ctx, path, &params);
    let resp = match result {
        Ok(v) => json_resp(&v),
        Err(e) => json_resp(&json!({ "error": format!("{e:#}") })).with_status_code(400),
    };
    let _ = req.respond(resp);
}

fn get<'a>(params: &'a [(String, String)], name: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' => match b
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                Some(v) => {
                    out.push(v);
                    i += 2;
                }
                None => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Make and save a filing plan into the folder `to`, from the drives `from` (root ids; default all).
fn new_plan(ctx: &Ctx, to: Option<&str>, from: Option<&str>, skip_junk: bool) -> Result<Value> {
    let to = PathBuf::from(to.context("where should the archive go?")?.trim());
    if !to.is_absolute() {
        bail!("the archive folder must be a full path, like /media/archive/Archive");
    }
    let only: Option<Vec<i64>> = from
        .filter(|f| !f.is_empty())
        .map(|f| {
            f.split(',')
                .map(|x| x.parse().context("bad drive id"))
                .collect::<Result<_>>()
        })
        .transpose()?;
    // plans are written to the index: a short write alongside the daemon, which waits its turn
    let db = Db::open(&ctx.db)?;
    let list: Vec<_> = recognise::root_ids(&db)?
        .into_iter()
        .filter_map(|id| recognise::load(&db, id).ok().flatten().map(|(r, _)| r))
        .collect();
    let ov = overlap::overlaps(&db, &list, &mut |_| {})?;
    // caches, previews and build output can be left out: they're regenerable, but then the drives
    // they're on can't be wiped whole (those files have no copy). The schema is yours from
    // Settings if you've edited it.
    let (mut schema, custom) = crate::settings::schema()?;
    schema.skip_junk |= skip_junk;
    let mut p = plan::make(&db, &to, only.as_deref(), &schema, &list, &ov)?;
    p.schema = match (custom, schema.skip_junk) {
        (false, true) => "built-in, caches left out".into(),
        (false, false) => "built-in".into(),
        (true, true) => "yours (Settings), caches left out".into(),
        (true, false) => "yours (Settings)".into(),
    };
    if p.items.is_empty() {
        bail!("nothing to file: are the drives recognised yet (What's here)?");
    }
    Ok(json!({ "ok": true, "id": plan::save(&db, &p)? }))
}

/// Queue a plan for the daemon to carry out.
fn start_apply(ctx: &Ctx, id: Option<&str>) -> Result<Value> {
    let id: i64 = id.context("missing id")?.parse().context("bad id")?;
    let d = ctx
        .daemon
        .as_ref()
        .context("copying runs in the BackDown service: `backdown apply ID --go` in a terminal")?;
    plan::load(&ro_db(ctx)?, id)?;
    let job = d.enqueue(
        "apply",
        Some(format!("plan {id}")),
        "requested",
        crate::daemon::JobOpts {
            plan: Some(id),
            ..Default::default()
        },
    );
    Ok(json!({ "ok": true, "job": job }))
}

/// Why this browser can't start a wipe, if it can't.
fn wipe_blocker(ctx: &Ctx) -> Option<&'static str> {
    if ctx.daemon.is_none() {
        Some("wiping runs in the BackDown service: `backdown wipe PATH` in a terminal")
    } else if !ctx.desktop {
        Some(
            "the machine-wide service only reads: wipe from your own BackDown (`backdown --local wipe PATH`)",
        )
    } else {
        None
    }
}

/// A selection as the browser sends it: index keys, comma-separated.
fn wipe_paths(keys: Option<&str>) -> Result<Vec<PathBuf>> {
    let paths: Vec<PathBuf> = keys
        .unwrap_or("")
        .split(',')
        .filter(|k| !k.is_empty())
        .map(browse::path_of)
        .collect::<Result<_>>()?;
    if paths.is_empty() {
        bail!("nothing selected");
    }
    Ok(paths)
}

/// What wiping the selection would do, as `backdown wipe` prints it, plus whether it can be
/// started from here.
fn wipe_assess(ctx: &Ctx, db: &Db, keys: Option<&str>) -> Result<Value> {
    let paths = wipe_paths(keys)?;
    let a = wipe::assess(db, &paths, Some(&ctx.db))?;
    let mut v = json!(a);
    for (it, p) in v["items"]
        .as_array_mut()
        .context("assessment")?
        .iter_mut()
        .zip(&paths)
    {
        it["connected"] = json!(p.exists());
        it["key"] = json!(browse::key(p));
    }
    v["why_not"] = json!(wipe_blocker(ctx));
    Ok(v)
}

/// Record a wipe and queue it: the checks of `backdown wipe --go`, confirmed by typing the drive
/// names the assessment asked for.
fn start_wipe(
    ctx: &Ctx,
    keys: Option<&str>,
    move_to: Option<&str>,
    confirm: Option<&str>,
    trust_index: bool,
) -> Result<Value> {
    if let Some(why) = wipe_blocker(ctx) {
        bail!(why);
    }
    let d = ctx.daemon.as_ref().context("no daemon")?;
    let paths = wipe_paths(keys)?;
    let move_to = move_to
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(PathBuf::from);
    if let Some(m) = &move_to {
        if !m.is_absolute() {
            bail!("the folder to move things to must be a full path");
        }
        if paths.iter().any(|p| m.starts_with(p)) {
            bail!("the folder to move things to is inside what's being wiped");
        }
    }
    let db = Db::open(&ctx.db)?;
    let a = wipe::assess(&db, &paths, Some(&ctx.db))?;
    if let Some(it) = a.items.iter().find(|i| i.refused.is_some()) {
        bail!(
            "{} can't be wiped: {}",
            it.path,
            it.refused.as_deref().unwrap_or_default()
        );
    }
    if let Some(p) = paths.iter().find(|p| !p.exists()) {
        bail!("{} isn't there: connect its drive", p.display());
    }
    if confirm.map(str::trim) != Some(a.confirm.as_str()) {
        bail!("type {} to confirm", a.confirm);
    }
    let id = wipe::record(&db, &paths, move_to.as_deref())?;
    let job = d.enqueue(
        "wipe",
        Some(format!("wipe {id}")),
        "requested",
        crate::daemon::JobOpts {
            wipe: Some(id),
            trust_index,
            ..Default::default()
        },
    );
    Ok(json!({ "ok": true, "id": id, "job": job }))
}

/// Why this browser can't start tidying an archive, if it can't.
fn tidy_blocker(ctx: &Ctx) -> Option<&'static str> {
    if ctx.daemon.is_none() {
        Some("tidying runs in the BackDown service: `backdown dedup FOLDER --go` in a terminal")
    } else if !ctx.desktop {
        Some(
            "the machine-wide service only reads: tidy from your own BackDown (`backdown --local dedup FOLDER --go`)",
        )
    } else {
        None
    }
}

/// The folder, link and smallest size of a dedup request, checked.
fn dedup_args(
    ctx: &Ctx,
    folder: Option<&str>,
    link: Option<&str>,
    min: Option<&str>,
) -> Result<(PathBuf, dedup::Link, u64)> {
    let folder = folder
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .context("which folder?")?;
    let folder = dedup::check_folder(Path::new(folder), Some(&ctx.db))?;
    let link = dedup::Link::parse(link.unwrap_or("hard"))?;
    let min = min
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(fmt::parse_size)
        .transpose()?
        .map_or_else(|| fmt::parse_size("4K"), Ok)?;
    Ok((folder, link, min))
}

/// What tidying a folder would do (from the index and the files' metadata), and whether it can
/// be started from here.
fn dedup_assess(
    ctx: &Ctx,
    db: &Db,
    folder: Option<&str>,
    link: Option<&str>,
    min: Option<&str>,
) -> Result<Value> {
    let (folder, link, min) = dedup_args(ctx, folder, link, min)?;
    let mut v = json!(dedup::assess(db, &folder, link, min)?);
    v["why_not"] = json!(tidy_blocker(ctx));
    v["min_size"] = json!(min);
    Ok(v)
}

/// Record a dedup run and queue it for the daemon.
fn start_dedup(
    ctx: &Ctx,
    folder: Option<&str>,
    link: Option<&str>,
    min: Option<&str>,
) -> Result<Value> {
    if let Some(why) = tidy_blocker(ctx) {
        bail!(why);
    }
    let d = ctx.daemon.as_ref().context("no daemon")?;
    let (folder, link, min) = dedup_args(ctx, folder, link, min)?;
    let db = Db::open(&ctx.db)?;
    let id = dedup::record(&db, &folder, link, min)?;
    let job = d.enqueue(
        "dedup",
        Some(format!("dedup {id}")),
        "requested",
        crate::daemon::JobOpts {
            dedup: Some(id),
            ..Default::default()
        },
    );
    Ok(json!({ "ok": true, "id": id, "job": job }))
}

/// Merge a plan item (a mailbox) with its twin, before the plan is applied.
fn plan_merge(ctx: &Ctx, id: Option<&str>, item: Option<&str>) -> Result<Value> {
    let id: i64 = id.context("missing id")?.parse().context("bad id")?;
    let item: usize = item.context("missing item")?.parse().context("bad item")?;
    // a short write to the plan, alongside the daemon
    let db = Db::open(&ctx.db)?;
    let p = plan::merge_mail(&db, id, item)?;
    Ok(json!({ "ok": true, "plan": p }))
}

/// Start a rescue, or carry one on (`resume`): the checks that can be made before it runs, then a
/// job in the daemon's side lane. Only reads the source; writes only to the destination.
fn start_rescue(ctx: &Ctx, params: &[(String, String)]) -> Result<Value> {
    let param = |n: &str| get(params, n).map(str::trim).filter(|v| !v.is_empty());
    let num = |n: &str| -> Result<Option<u64>> {
        param(n)
            .map(|v| v.parse().with_context(|| format!("bad {n}")))
            .transpose()
    };
    let d = ctx
        .daemon
        .as_ref()
        .context("rescues run in the BackDown service: `backdown rescue …` in a terminal")?;
    if !ctx.desktop {
        bail!(
            "the machine-wide service only reads: rescue from your own BackDown (`backdown --local rescue …`)"
        );
    }
    let db = Db::open(&ctx.db)?;
    let id = if let Some(resume) = param("resume") {
        let id: i64 = resume.parse().context("bad rescue")?;
        let r = rescue::load(&db, id)?;
        if d.pending("rescue", &format!("rescue {id}")) {
            bail!("that rescue is already running or waiting its turn");
        }
        if r.mode == "carve" {
            // carry on from where it got to
            let mut o = r.options.clone();
            o["start"] = r.result["pos"].clone();
            rescue::set_options(&db, id, &o)?;
        }
        if r.mode == "files" {
            // what was copied (and checked) before the stop is left as it is
            let mut o = r.options.clone();
            o["resume"] = json!(true);
            rescue::set_options(&db, id, &o)?;
        }
        rescue::set(&db, id, "new", None, None)?;
        id
    } else if param("mode") == Some("files") {
        // copy files out of a filesystem: the source is looked up (drives.rs), never sent
        let p = |n: &str| get(params, n).map(str::to_string);
        let (src, dest, options) = crate::drives::files_request(&db, &p)?;
        rescue::record(&db, "files", &src, Some(&dest), &options)?
    } else {
        let mode = param("mode").context("missing mode")?;
        let source = PathBuf::from(param("source").context("which drive or image?")?);
        let a = rescue::access(&source);
        if !a.readable {
            bail!(
                "{} can't be read: {}",
                source.display(),
                a.why.unwrap_or_default()
            );
        }
        let dest = |what: &str| -> Result<PathBuf> {
            let p =
                PathBuf::from(param("dest").with_context(|| format!("where should {what} go?"))?);
            if !p.is_absolute() {
                bail!("{what} needs a full path, on another drive");
            }
            Ok(p)
        };
        let (dest, options) = match mode {
            "image" => {
                let to = dest("the copy")?;
                if to.is_dir() {
                    bail!(
                        "{} is a folder: give the copy a file name, like card.img",
                        to.display()
                    );
                }
                rescue::image::check_places(&source, &to, &rescue::map_path(&to), !a.device)?;
                (Some(to), json!({ "retries": num("retries")?.unwrap_or(1) }))
            }
            "carve" => {
                let to = dest("what's found")?;
                // an image file can't be overwritten by what's saved beside it; a drive can
                if a.device && rescue::carve::same_device(&source, &to) {
                    bail!(
                        "{} is on the drive being rescued: saving there could overwrite the very files being looked for. Choose a folder on another drive",
                        to.display()
                    );
                }
                let kinds: Vec<&str> = param("kinds")
                    .unwrap_or("")
                    .split(',')
                    .filter(|k| !k.is_empty())
                    .collect();
                if kinds.is_empty() {
                    bail!("choose at least one kind of file to look for");
                }
                (
                    Some(to),
                    json!({ "kinds": kinds, "start": num("start")?.unwrap_or(0), "end": num("end")? }),
                )
            }
            "parts" => (None, json!({ "deep": param("deep") == Some("1") })),
            _ => bail!("mode must be image, carve or parts"),
        };
        rescue::record(&db, mode, &source, dest.as_deref(), &options)?
    };
    let job = d.enqueue(
        "rescue",
        Some(format!("rescue {id}")),
        "requested",
        crate::daemon::JobOpts {
            rescue: Some(id),
            ..Default::default()
        },
    );
    Ok(json!({ "ok": true, "id": id, "job": job }))
}

/// The report as HTML (to view) or Markdown (to save), from what's stored and indexed now.
fn report(ctx: &Ctx, md: bool) -> Result<(String, bool)> {
    let db = ro_db(ctx)?;
    let mut rec: Vec<_> = recognise::root_ids(&db)?
        .into_iter()
        .filter_map(|id| recognise::load(&db, id).ok().flatten())
        .collect();
    crate::tiers::strip_provenance(rec.iter_mut().map(|(r, _)| r));
    let list: Vec<_> = rec.iter().map(|(r, _)| r.clone()).collect();
    let ov = overlap::overlaps(&db, &list, &mut |_| {})?;
    let s = summary::build(&db, &rec, &ov, &mut |_| {})?;
    Ok((
        if md {
            summary::markdown(&s)
        } else {
            summary::html(&s)
        },
        md,
    ))
}

/// Queue a scan of one indexed drive (by root id) or of all of them.
fn rescan(ctx: &Ctx, d: &Daemon, root: Option<&str>) -> Result<Value> {
    let roots = browse::roots(&ro_db(ctx)?)?;
    let chosen: Vec<_> = match root {
        Some(id) => {
            let id: i64 = id.parse().context("bad root")?;
            roots.into_iter().filter(|r| r.id == id).collect()
        }
        None => roots,
    };
    if chosen.is_empty() {
        bail!("no such drive in the index");
    }
    let jobs: Vec<u64> = chosen
        .into_iter()
        .map(|r| d.enqueue("index", Some(r.path), "requested", Default::default()))
        .collect();
    Ok(json!({ "ok": true, "jobs": jobs }))
}

fn ro_db(ctx: &Ctx) -> Result<Db> {
    Db::open_read_only(&ctx.db)
}

fn api(ctx: &Ctx, path: &str, params: &[(String, String)]) -> Result<Value> {
    let param = |name: &str| get(params, name);
    if path == "/api/daemon" {
        return Ok(match &ctx.daemon {
            None => json!({ "running": false }),
            Some(d) => {
                // long poll: with ?since=<change>&wait=<s>, answer when something changes (or at
                // the timeout), so an idle UI costs one request a minute instead of a steady poll
                if let (Some(since), Some(wait)) = (
                    param("since").and_then(|v| v.parse().ok()),
                    param("wait").and_then(|v| v.parse::<u64>().ok()),
                ) {
                    d.wait_change(since, std::time::Duration::from_secs(wait.min(55)));
                }
                d.snapshot()
            }
        });
    }
    if crate::health::handles(path) {
        // drive health, notifications and their settings (health.rs)
        let p = |k: &str| param(k).map(str::to_string);
        return crate::health::get(ctx.daemon.as_deref(), &ctx.db, path, &p);
    }
    let db = ro_db(ctx)?;
    let id = || -> Result<i64> { param("id").context("missing id")?.parse().context("bad id") };
    let dir = || -> Result<PathBuf> { browse::path_of(param("dir").context("missing dir")?) };
    let num = |name: &str, default: u64| -> Result<u64> {
        param(name).map_or(Ok(default), |v| {
            v.parse().with_context(|| format!("bad {name}"))
        })
    };
    // the container viewers (peek.rs) read the file an index id resolves to, like `content()`
    let file = || -> Result<PathBuf> { Ok(source(&db, id()?)?.0) };
    Ok(match path {
        "/api/roots" => json!({ "roots": browse::roots(&db)?, "status": db.status()? }),
        "/api/timeline" => json!(browse::timeline(
            &db,
            param("root").context("missing root")?.parse()?
        )?),
        "/api/ls" => {
            let scan = param("scan")
                .filter(|s| !s.is_empty())
                .map(str::parse)
                .transpose()?;
            json!(browse::list(&db, &dir()?, scan)?)
        }
        "/api/file" => json!(browse::file(&db, id()?)?),
        // what's recognised on each drive (as stored: the daemon refreshes it after each scan)
        "/api/entities" => {
            let mut roots = Vec::new();
            for id in recognise::root_ids(&db)? {
                if let Some((r, fresh)) = recognise::load(&db, id)? {
                    let mut v = serde_json::to_value(&r)?;
                    v["fresh"] = json!(fresh);
                    roots.push(v);
                }
            }
            let mut v = json!({ "roots": roots, "daemon": ctx.daemon.is_some() });
            crate::tiers::redact_provenance(&mut v);
            v
        }
        "/api/mirrors" => json!(
            backdown_core::mirror::list(&db)?
                .into_iter()
                .map(|m| {
                    let here = std::path::Path::new(&m.source).is_dir()
                        && std::path::Path::new(&m.target).is_dir();
                    let mut v = json!(m);
                    v["connected"] = json!(here);
                    v
                })
                .collect::<Vec<_>>()
        ),
        "/api/plans" => {
            let mut out = Vec::new();
            for (id, _, _, state) in apply::plans(&db)? {
                let (p, _) = plan::load(&db, id)?;
                out.push(json!({ "id": id, "state": state, "created_at": p.created_at, "target": p.target,
                    "copy_bytes": p.copy_bytes, "copy_files": p.copy_files, "items": p.items.len(),
                    "drives": p.drives.iter().map(|d| json!({ "drive": d.drive, "covered": d.covered })).collect::<Vec<_>>() }));
            }
            json!(out)
        }
        "/api/plan" => {
            let id: i64 = id()?;
            let (p, state) = plan::load(&db, id)?;
            // mailboxes that could be merged with their twin instead
            let mergeable: Vec<usize> = (0..p.items.len())
                .filter(|&i| plan::merge_target(&p, i).is_some())
                .collect();
            json!({ "id": id, "state": state, "plan": p, "journal": apply::journal_summary(&db, id)?, "mergeable": mergeable })
        }
        "/api/dedup-assess" => {
            dedup_assess(ctx, &db, param("folder"), param("link"), param("min"))?
        }
        "/api/dedups" => json!(dedup::history(&db)?),
        "/api/mail-merges" => json!(backdown_core::mailmerge::history(&db)?),
        "/api/plan-dry" => {
            let mut db = db;
            json!(apply::apply(&mut db, id()?, true, &mut |_| {})?)
        }
        "/api/wipe-assess" => wipe_assess(ctx, &db, param("keys"))?,
        "/api/wipes" => json!(wipe::history(&db)?),
        // can the daemon read this drive (or image file) directly, and if not, what would let it
        "/api/rescue-access" => {
            let a = rescue::access(Path::new(param("path").context("missing path")?.trim()));
            let mut v = json!(a);
            // the system can open it for this BackDown (udisks, a password dialog): drives.rs
            v["allow"] = json!(ctx.desktop && cfg!(target_os = "linux") && a.device && !a.readable);
            v
        }
        "/api/rescues" => json!(rescue::history(&db)?),
        // free space where things would be moved to (the nearest folder up that exists)
        "/api/room" => {
            let p = PathBuf::from(param("path").context("missing path")?.trim());
            json!({ "free": p.is_absolute().then(|| wipe::room(&p)).flatten() })
        }
        "/api/overlaps" => {
            let stored: Vec<_> = recognise::root_ids(&db)?
                .into_iter()
                .filter_map(|id| recognise::load(&db, id).ok().flatten().map(|(r, _)| r))
                .collect();
            json!(overlap::overlaps(&db, &stored, &mut |_| {})?)
        }
        // a typed path -> the folder to show (and the file to select), if it's in the index
        "/api/resolve" => {
            // a typed path, or an entity's key (exact bytes, whatever the OS's path encoding)
            let typed = match param("key") {
                Some(k) => browse::path_of(k)?,
                None => PathBuf::from(param("path").context("missing path")?.trim()),
            };
            // a file lists as an empty folder, so ask about files first
            let is_file: bool = db
                .conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM files WHERE path = ?1 AND current = 1)",
                    [paths::to_bytes(&typed)],
                    |r| r.get(0),
                )
                .unwrap_or(false);
            if !is_file && browse::list(&db, &typed, None).is_ok() {
                json!({ "dir": browse::key(&typed) })
            } else {
                let parent = typed.parent().context("not in the index")?;
                let listing = browse::list(&db, parent, None).context("that isn't in the index")?;
                let key = browse::key(&typed);
                if !listing.entries.iter().any(|e| e.key == key) {
                    bail!("{} isn't in the index", typed.display());
                }
                json!({ "dir": browse::key(parent), "select": key })
            }
        }
        "/api/search" => json!(browse::search(&db, &dir()?, param("q").unwrap_or(""), 300)?),
        // ranked search by name (find.rs): under `dir`, or on every drive without it
        "/api/find" => {
            let within = param("dir")
                .filter(|d| !d.is_empty())
                .map(browse::path_of)
                .transpose()?;
            json!(backdown_core::find::find(
                &db,
                &backdown_core::find::Query {
                    q: param("q").unwrap_or(""),
                    within: within.as_deref(),
                    earlier: param("earlier") == Some("1"),
                    limit: num("limit", 200)? as usize,
                }
            )?)
        }
        "/api/dupes" => {
            let min = fmt::parse_size(param("min").unwrap_or("1M"))?;
            json!(report::dupes(&db, min, 200)?)
        }
        "/api/sqlite" => sqlite(
            &db,
            id()?,
            param("table"),
            param("offset").unwrap_or("0").parse()?,
        )?,
        "/api/sheet" => peek::sheet(
            &file()?,
            param("sheet"),
            num("offset", 0)?,
            num("limit", 500)?,
        )?,
        "/api/slides" => peek::slides(&file()?)?,
        "/api/mail" => peek::mail(&file()?, param("at").map(str::parse).transpose()?)?,
        "/api/mbox" => peek::mbox(
            &file()?,
            num("offset", 0)?,
            param("q").unwrap_or(""),
            num("limit", 200)? as usize,
        )?,
        "/api/threads" | "/api/thread" | "/api/mail-kind" => {
            crate::mailbox::api(&db, path, &|name| get(params, name))?
        }
        "/api/archive" => peek::archive(
            &file()?,
            num("limit", 10_000)? as usize,
            param("quick").is_some(),
        )?,
        "/api/disk" => peek::disk(&file()?, param("path"))?,
        "/api/fs" => match param("rescue") {
            // a filesystem a Rescue found: its place comes from the record, not from here
            Some(r) => {
                let (src, start, len, _) =
                    crate::drives::found_region(&db, r.parse()?, num("at", 0)?)?;
                peek::region_list(
                    &src,
                    start,
                    len,
                    param("path"),
                    param("deleted") == Some("1"),
                )?
            }
            None => peek::fs_list(
                &file()?,
                param("part").unwrap_or("0"),
                param("path"),
                param("deleted") == Some("1"),
            )?,
        },
        // what BackDown mounted or attached (drives.rs), and so can undo; with an id, that image's
        "/api/mounts" => {
            let image = param("id").map(|i| Ok::<_, anyhow::Error>(source(&db, i.parse()?)?.0));
            crate::drives::mounts(ctx.desktop, image.transpose()?.as_deref())
        }
        // a SQL dump's schema and data sizes (sqldump.rs), a few seconds of reading at a time
        "/api/sqldump" => crate::sqldump::schema(id()?, &file()?, param("more"))?,
        // this machine's disks and what's on them: the same read-only probe as `backdown volumes`
        // (sysfs and udev, diskutil, or PowerShell), with the volumes the index covers marked
        "/api/volumes" => {
            let mut r = volumes::probe()?;
            volumes::mark_indexed(&mut r, &db)?;
            r.index = Some(ctx.db.display().to_string());
            json!(r)
        }
        "/api/git" => git(&db, &dir()?, param("rev"))?,
        "/api/diff" => diff(
            &db,
            param("a").context("missing a")?.parse()?,
            param("b").context("missing b")?.parse()?,
        )?,
        _ => bail!("no such endpoint"),
    })
}

// ---- file bytes

fn mime(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml", // safe: <img> never runs its scripts, and the sandbox CSP covers direct opens
        "pdf" => "application/pdf",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "m4a" | "aac" => "audio/mp4",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        _ => "application/octet-stream", // everything else, HTML included, is read by the viewers as data
    }
}

/// Where an index id's bytes can be read now (the file itself, or an identical current copy),
/// and their size. Every file-reading endpoint starts here.
fn source(db: &Db, id: i64) -> Result<(PathBuf, u64)> {
    browse::resolve_content(db, id)?
        .context("this version's bytes aren't on any indexed drive any more")
}

/// The headers every response carrying file bytes gets: a type from the safe list (HTML and
/// anything else unknown go out as opaque data), a sandbox CSP, no sniffing, no caching.
fn file_headers(name: &str) -> Vec<Header> {
    vec![
        header("Content-Type", mime(name)),
        header("Content-Security-Policy", FILE_CSP),
        header("X-Content-Type-Options", "nosniff"),
        header("Cache-Control", "private, no-store"),
    ]
}

/// `attachment` with an ASCII fallback name and the real one percent-encoded (RFC 6266).
fn disposition(name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || " ._-()[]+,".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"._-".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

fn content(ctx: &Ctx, req: Request, id: Option<&str>, range: Option<&str>, download: bool) {
    let result = (|| -> Result<(PathBuf, u64)> {
        let db = ro_db(ctx)?;
        source(&db, id.context("missing id")?.parse()?)
    })();
    let (path, size) = match result {
        Ok(x) => x,
        Err(e) => {
            let _ = req.respond(plain(404, &format!("{e:#}")));
            return;
        }
    };
    // verbatim on Windows (long paths), with backup semantics when elevated
    let mut file = match paths::open_read(&path) {
        Ok(f) => f,
        Err(e) => {
            let _ = req.respond(plain(404, &e.to_string()));
            return;
        }
    };
    let (start, end) = parse_range(range, size).unwrap_or((0, size.saturating_sub(1)));
    let len = if size == 0 { 0 } else { end - start + 1 };
    if file.seek(SeekFrom::Start(start)).is_err() {
        let _ = req.respond(plain(500, "seek failed"));
        return;
    }
    let name = path
        .file_name()
        .map(|n| names::nfc(&n.to_string_lossy()).into_owned())
        .unwrap_or_default();
    let mut headers = file_headers(&name);
    headers.push(header("Accept-Ranges", "bytes"));
    let mut resp = Response::new(
        StatusCode(if range.is_some() && size > 0 {
            206
        } else {
            200
        }),
        headers,
        Box::new(file.take(len)) as Box<dyn Read + Send>,
        Some(len as usize),
        None,
    );
    if range.is_some() && size > 0 {
        resp.add_header(header(
            "Content-Range",
            &format!("bytes {start}-{end}/{size}"),
        ));
    }
    if download {
        // "download a copy": saved under its own name
        resp.add_header(header("Content-Disposition", &disposition(&name)));
    }
    let _ = req.respond(resp);
}

fn pct_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Kinds of file the desktop would run rather than show: never opened from the browser.
const RUNNABLE: &[&str] = &[
    "exe",
    "msi",
    "msix",
    "bat",
    "cmd",
    "com",
    "scr",
    "pif",
    "cpl",
    "ps1",
    "psm1",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
    "hta",
    "lnk",
    "url",
    "reg",
    "jar",
    "sh",
    "bash",
    "zsh",
    "csh",
    "ksh",
    "fish",
    "command",
    "tool",
    "app",
    "desktop",
    "appimage",
    "run",
    "bin",
    "py",
    "pyw",
    "pl",
    "rb",
    "php",
    "deb",
    "rpm",
    "pkg",
    "mpkg",
    "apk",
    "xpi",
    "workflow",
    "terminal",
    "scpt",
    "applescript",
];

/// POST /api/open?id=<file>|dir=<key>[&reveal=1]: open a file with its default app, or show a file
/// or folder in the file manager. Only indexed things that are still on disk.
fn open_on_desktop(ctx: &Ctx, id: Option<&str>, dir: Option<&str>, reveal: bool) -> Result<Value> {
    if !ctx.desktop {
        bail!("the system-wide BackDown service has no desktop to open things on");
    }
    let db = ro_db(ctx)?;
    let (path, is_dir) = match (id, dir) {
        (Some(id), _) => {
            let (path, _) = browse::resolve_content(&db, id.parse().context("bad id")?)?
                .context("this version isn't on any connected drive any more")?;
            (path, false)
        }
        (None, Some(key)) => {
            let path = browse::path_of(key)?;
            browse::list(&db, &path, None).context("that folder isn't in the index")?;
            (path, true)
        }
        _ => bail!("missing id or dir"),
    };
    if !path.exists() {
        bail!("{} isn't there any more", path.display());
    }
    if !is_dir && !reveal {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        #[cfg(unix)]
        // an executable bit alone proves little (FAT and NTFS mounts set it on every file), so
        // it counts only on a program or a script
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            let mut magic = [0u8; 4];
            std::fs::metadata(&path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
                && std::fs::File::open(&path)
                    .and_then(|mut f| f.read(&mut magic))
                    .is_ok()
                && (magic.starts_with(b"#!")
                    || magic == *b"\x7fELF"
                    || [0xfeedface_u32, 0xfeedfacf, 0xcafebabe]
                        .iter()
                        .any(|m| magic == m.to_be_bytes() || magic == m.to_le_bytes()))
        };
        #[cfg(not(unix))]
        let executable = false;
        if RUNNABLE.contains(&ext.as_str()) || executable {
            bail!("BackDown doesn't open programs or scripts; use “Show in file manager” instead");
        }
    }
    desktop_open(&path, reveal && !is_dir)?;
    Ok(json!({ "ok": true, "path": path.display().to_string() }))
}

fn desktop_open(path: &Path, reveal: bool) -> Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        if reveal {
            c.arg("-R");
        }
        c.arg(path);
        c
    } else if cfg!(windows) {
        // explorer.exe, not `cmd /c start`: no shell ever parses the file name
        let mut c = Command::new("explorer.exe");
        if reveal {
            c.arg(format!("/select,{}", path.display()));
        } else {
            c.arg(path);
        }
        c
    } else {
        // freedesktop: select the file in the file manager if one answers on D-Bus, else open its folder
        if reveal {
            let uri = format!(
                "file://{}",
                pct_encode(&path.display().to_string()).replace("%2F", "/")
            );
            let shown = Command::new("gdbus")
                .args([
                    "call",
                    "--session",
                    "--dest",
                    "org.freedesktop.FileManager1",
                    "--object-path",
                    "/org/freedesktop/FileManager1",
                    "--method",
                    "org.freedesktop.FileManager1.ShowItems",
                ])
                .arg(format!("['{uri}']"))
                .arg("")
                .output()
                .is_ok_and(|o| o.status.success());
            if shown {
                return Ok(());
            }
        }
        let mut c = Command::new("xdg-open");
        c.arg(if reveal {
            path.parent().unwrap_or(path)
        } else {
            path
        });
        c
    };
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("no desktop opener here")?;
    // openers hand off and exit quickly; a failure (no display, no app for the type) shows up here
    for _ in 0..40 {
        if let Some(status) = child.try_wait()? {
            if status.success() || cfg!(windows) {
                return Ok(()); // explorer.exe exits 1 even when it worked
            }
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut err);
            }
            bail!(
                "the desktop couldn't open it{}",
                if err.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", err.trim())
                }
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Ok(()) // still running after 4 s: it's showing the file
}

/// One file from inside an indexed container: an archive or disk-image entry (`/api/entry`, by
/// its name in the container's own listing, never a filesystem path) or a mail attachment
/// (`/api/attachment`). Same headers as `content()`; `dl=1` makes it a download.
fn inner(ctx: &Ctx, req: Request, path: &str, params: &[(String, String)]) {
    let param = |name: &str| get(params, name);
    let result = (|| -> Result<peek::Part> {
        let db = ro_db(ctx)?;
        let max = param("max").map(str::parse).transpose()?;
        if path == "/api/entry"
            && let Some(r) = param("rescue")
        {
            // a file in a filesystem a Rescue found, at the place its record says
            let at = param("at").context("missing at")?.parse()?;
            let (src, start, len, _) = crate::drives::found_region(&db, r.parse()?, at)?;
            return peek::region_entry(
                &src,
                start,
                len,
                param("name").context("missing name")?,
                max,
            );
        }
        let (file, _) = source(&db, param("id").context("missing id")?.parse()?)?;
        if path == "/api/entry" {
            let name = param("name").context("missing name")?;
            match param("part") {
                Some(part) => peek::fs_entry(&file, part, name, max),
                None => peek::entry(&file, name, max),
            }
        } else {
            peek::attachment(
                &file,
                param("at").map(str::parse).transpose()?,
                param("part").context("missing part")?.parse()?,
            )
        }
    })();
    let part = match result {
        Ok(p) => p,
        Err(e) => {
            let _ = req.respond(plain(404, &format!("{e:#}")));
            return;
        }
    };
    let mut headers = file_headers(&part.name);
    if param("dl").is_some() {
        headers.push(header("Content-Disposition", &disposition(&part.name)));
    }
    let len = part.len.map(|n| n as usize);
    let _ = req.respond(Response::new(
        StatusCode(200),
        headers,
        part.body,
        len,
        None,
    ));
}

fn parse_range(range: Option<&str>, size: u64) -> Option<(u64, u64)> {
    let spec = range?.strip_prefix("bytes=")?;
    let (a, b) = spec.split(',').next()?.split_once('-')?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => (size.saturating_sub(n.parse().ok()?), size.checked_sub(1)?),
        (s, "") => (s.parse().ok()?, size.checked_sub(1)?),
        (s, e) => (
            s.parse().ok()?,
            e.parse::<u64>().ok()?.min(size.checked_sub(1)?),
        ),
    };
    (start <= end).then_some((start, end))
}

fn read_text(db: &Db, id: i64) -> Result<(PathBuf, String)> {
    let (path, size) = source(db, id)?;
    if size > TEXT_LIMIT {
        bail!("too large to compare ({})", fmt::bytes(size));
    }
    let bytes = std::fs::read(&path)?;
    let text =
        String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("{} isn't text", path.display()))?;
    Ok((path, text))
}

// ---- viewers that need the server

fn diff(db: &Db, a: i64, b: i64) -> Result<Value> {
    let (pa, ta) = read_text(db, a)?;
    let (pb, tb) = read_text(db, b)?;
    let d = similar::TextDiff::from_lines(&ta, &tb);
    let unified = d
        .unified_diff()
        .context_radius(3)
        .header(&pa.display().to_string(), &pb.display().to_string())
        .to_string();
    Ok(
        json!({ "a": pa.display().to_string(), "b": pb.display().to_string(), "ratio": d.ratio(), "unified": unified }),
    )
}

/// SQLite files are opened read-only *and* immutable, so SQLite never takes a lock or touches a
/// journal on the source drive.
fn sqlite(db: &Db, id: i64, table: Option<&str>, offset: i64) -> Result<Value> {
    let (path, _) = source(db, id)?;
    let uri = format!("file:{}?mode=ro&immutable=1", uri_path(&path));
    let conn = rusqlite::Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("not a SQLite database")?;
    let mut tables = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT name, type, sql FROM sqlite_schema WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' ORDER BY name")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        for row in rows {
            let (name, kind, sql) = row?;
            let count: Option<i64> = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM \"{}\"", name.replace('"', "\"\"")),
                    [],
                    |r| r.get(0),
                )
                .ok();
            tables.push(json!({ "name": name, "kind": kind, "sql": sql, "rows": count }));
        }
    }
    let mut data = Value::Null;
    if let Some(t) = table {
        let quoted = format!("\"{}\"", t.replace('"', "\"\""));
        let mut stmt = conn.prepare(&format!("SELECT * FROM {quoted} LIMIT 200 OFFSET ?1"))?;
        let columns: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
        let n = columns.len();
        let mut rows = stmt.query([offset])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let mut cells = Vec::with_capacity(n);
            for i in 0..n {
                cells.push(match r.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(v) => json!(v),
                    ValueRef::Real(v) => json!(v),
                    ValueRef::Text(t) => {
                        let s = String::from_utf8_lossy(t);
                        json!(if s.len() > 2000 {
                            format!("{}…", &s[..s.floor_char_boundary(2000)])
                        } else {
                            s.into_owned()
                        })
                    }
                    ValueRef::Blob(b) => json!(format!("<blob {}>", fmt::bytes(b.len() as u64))),
                });
            }
            out.push(cells);
        }
        data = json!({ "table": t, "columns": columns, "rows": out, "offset": offset });
    }
    Ok(json!({ "path": path.display().to_string(), "tables": tables, "data": data }))
}

fn uri_path(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '%' => out.push_str("%25"),
            '?' => out.push_str("%3f"),
            '#' => out.push_str("%23"),
            c => out.push(c),
        }
    }
    if cfg!(windows) {
        format!("/{out}")
    } else {
        out
    }
}

/// A repository in an indexed folder, read with git itself: log, or one commit with its patch.
/// Read-only by construction: only `log`/`show`, no optional locks, revisions validated.
fn git(db: &Db, dir: &Path, rev: Option<&str>) -> Result<Value> {
    browse::list(db, dir, None).context("not an indexed folder")?;
    if !dir.join(".git").exists() {
        bail!("{} isn't a git repository", dir.display());
    }
    let run = |args: &[&str]| -> Result<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["--no-pager", "-c", "core.quotepath=off"])
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .context("git isn't installed")?;
        if !out.status.success() {
            bail!("git: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        if s.len() > 4 << 20 {
            s.truncate(s.floor_char_boundary(4 << 20));
            s.push_str("\n… (truncated)");
        }
        Ok(s)
    };
    match rev {
        Some(r) => {
            if r.is_empty() || r.len() > 64 || !r.chars().all(|c| c.is_ascii_hexdigit()) {
                bail!("not a commit id");
            }
            let show = run(&[
                "show",
                "--no-color",
                "--stat",
                "--patch",
                "--format=%H%x1f%an%x1f%ae%x1f%at%x1f%B%x1e",
                r,
            ])?;
            let (head, patch) = show.split_once('\u{1e}').unwrap_or((&show, ""));
            let f: Vec<&str> = head.splitn(5, '\u{1f}').collect();
            Ok(
                json!({ "commit": { "id": f.first(), "author": f.get(1), "email": f.get(2), "time": f.get(3).and_then(|t| t.trim().parse::<i64>().ok()), "message": f.get(4).map(|m| m.trim()) }, "patch": patch.trim_start() }),
            )
        }
        None => {
            let branch = run(&["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
            let log = run(&[
                "log",
                "--no-color",
                "--max-count=300",
                "--format=%H%x1f%an%x1f%at%x1f%s",
            ])
            .unwrap_or_default();
            let commits: Vec<Value> = log
                .lines()
                .filter_map(|l| {
                    let f: Vec<&str> = l.splitn(4, '\u{1f}').collect();
                    (f.len() == 4).then(|| json!({ "id": f[0], "author": f[1], "time": f[2].parse::<i64>().ok(), "subject": f[3] }))
                })
                .collect();
            Ok(json!({ "branch": branch.trim(), "commits": commits }))
        }
    }
}

// ---- the UI, compiled into the binary

fn ui_asset(name: &str) -> Option<(&'static [u8], &'static str)> {
    macro_rules! ui {
        ($($file:literal => $ct:literal),* $(,)?) => {
            match name { $($file => Some((include_bytes!(concat!("../ui/", $file)) as &[u8], $ct)),)* _ => None }
        };
    }
    ui! {
        "index.html" => "text/html; charset=utf-8",
        "app.css" => "text/css; charset=utf-8",
        "app.js" => "text/javascript; charset=utf-8",
        "viewers.js" => "text/javascript; charset=utf-8",
        "icons.js" => "text/javascript; charset=utf-8",
        "extras.js" => "text/javascript; charset=utf-8",
        "mailbox.js" => "text/javascript; charset=utf-8",
        "sqldump.js" => "text/javascript; charset=utf-8",
        "search.js" => "text/javascript; charset=utf-8",
        "settings.js" => "text/javascript; charset=utf-8",
        "tidy.js" => "text/javascript; charset=utf-8",
        "health.js" => "text/javascript; charset=utf-8",
        "fsbrowse.js" => "text/javascript; charset=utf-8",
        "drives.js" => "text/javascript; charset=utf-8",
        "favicon.svg" => "image/svg+xml",
        "vendor/pdf.min.mjs" => "text/javascript; charset=utf-8",
        "vendor/pdf.worker.min.mjs" => "text/javascript; charset=utf-8",
        "vendor/docx-preview.min.js" => "text/javascript; charset=utf-8",
        "vendor/jszip.min.js" => "text/javascript; charset=utf-8",
        "vendor/highlight.min.js" => "text/javascript; charset=utf-8",
        "vendor/hljs-light.css" => "text/css; charset=utf-8",
        "vendor/hljs-dark.css" => "text/css; charset=utf-8",
    }
}
