//! `backdown serve`: the file browser, a local web UI over the index.
//!
//! Security model: listen on 127.0.0.1 only; reject any Host header but our own (DNS rebinding);
//! require a random per-run token, passed once in the URL and then kept in a SameSite=Strict cookie;
//! serve file bytes only by index id, never by arbitrary path; never serve active content (HTML is
//! sent as text, and every file response carries a CSP sandbox), and open SQLite files immutable.
//! Nothing here writes to a source: the index is opened read-only per request.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use backdown_core::{Db, browse, fmt, report};
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
}

pub fn serve(db: &Path, port: u16, open_browser: bool) -> Result<()> {
    Db::open(db)?; // create/migrate once; requests then use read-only connections
    let db = dunce::canonicalize(db)?;
    let server = Server::http(("127.0.0.1", port))
        .map_err(|e| anyhow::anyhow!("can't listen on 127.0.0.1:{port}: {e}"))?;
    let port = server.server_addr().to_ip().map_or(port, |a| a.port());
    let mut raw = [0u8; 24];
    getrandom::fill(&mut raw)
        .map_err(|e| anyhow::anyhow!("no randomness for the session token: {e}"))?;
    let token = fmt::hex(&raw);
    let ctx = Arc::new(Ctx {
        db: db.clone(),
        token: token.clone(),
        hosts: vec![
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
        ],
    });
    let url = format!("http://127.0.0.1:{port}/?t={token}");
    eprintln!(
        "BackDown browser for {} at\n  {url}\n(only this link, only on this computer; Ctrl-C to stop)",
        db.display()
    );
    if open_browser {
        open(&url);
    }
    for req in server.incoming_requests() {
        let ctx = Arc::clone(&ctx);
        std::thread::spawn(move || handle(&ctx, req));
    }
    Ok(())
}

fn open(url: &str) {
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
    // the link carries the token once; swap it for a cookie and drop it from the address bar
    if path == "/" && param("t") == Some(ctx.token.as_str()) {
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
            "Open BackDown with the link `backdown serve` printed.",
        ));
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
    if path == "/api/content" {
        let range = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("Range"))
            .map(|h| h.value.as_str().to_string());
        return content(ctx, req, param("id"), range.as_deref());
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

fn ro_db(ctx: &Ctx) -> Result<Db> {
    let conn = rusqlite::Connection::open_with_flags(
        &ctx.db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(Db { conn })
}

fn api(ctx: &Ctx, path: &str, params: &[(String, String)]) -> Result<Value> {
    let param = |name: &str| get(params, name);
    let db = ro_db(ctx)?;
    let id = || -> Result<i64> { param("id").context("missing id")?.parse().context("bad id") };
    let dir = || -> Result<PathBuf> { browse::path_of(param("dir").context("missing dir")?) };
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
        "/api/search" => json!(browse::search(&db, &dir()?, param("q").unwrap_or(""), 300)?),
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

fn content(ctx: &Ctx, req: Request, id: Option<&str>, range: Option<&str>) {
    let result = (|| -> Result<(PathBuf, u64)> {
        let db = ro_db(ctx)?;
        let id: i64 = id.context("missing id")?.parse()?;
        browse::resolve_content(&db, id)?
            .context("this version's bytes aren't on any indexed drive any more")
    })();
    let (path, size) = match result {
        Ok(x) => x,
        Err(e) => {
            let _ = req.respond(plain(404, &format!("{e:#}")));
            return;
        }
    };
    let mut file = match File::open(&path) {
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
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut resp = Response::new(
        StatusCode(if range.is_some() && size > 0 {
            206
        } else {
            200
        }),
        vec![
            header("Content-Type", mime(&name)),
            header("Accept-Ranges", "bytes"),
            header("Content-Security-Policy", FILE_CSP),
            header("X-Content-Type-Options", "nosniff"),
            header("Cache-Control", "private, no-store"),
        ],
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
    let _ = req.respond(resp);
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
    let (path, size) = browse::resolve_content(db, id)?
        .context("this version's bytes aren't on any indexed drive any more")?;
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
    let (path, _) = browse::resolve_content(db, id)?
        .context("this version's bytes aren't on any indexed drive any more")?;
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
