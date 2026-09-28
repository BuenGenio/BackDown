//! Settings: what the browser's Settings view changes, kept where the daemon reads it.
//!
//! One small JSON file, settings.json, in the daemon's data folder (~/.local/share/backdown,
//! ~/Library/Application Support/BackDown, %LOCALAPPDATA%\BackDown, or /var/lib/backdown for the
//! machine-wide service), with the filing schema beside it as schema.toml. Changes apply without a
//! restart: the rescan interval at the scheduler's next look (every 30 s), the hashing settings with
//! the next hash job, junk rules at the next recognition, the schema with the next plan, update
//! checks at the next check.
//!
//! The endpoints here (`handle`) follow serve.rs's rules: the session cookie is checked before
//! they're reached, and every POST needs the X-BackDown header, which a cross-site page can't send.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use backdown_core::{plan, recognise};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tiny_http::{Header, Request, Response};

use crate::daemon::{self, Daemon};
use crate::locations;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// rescan each drive this often: "12h", "1d", "7d", or "never" (only when asked, or when a
    /// drive comes back)
    pub rescan_every: String,
    /// readers per drive for the hashing after each scan (1 suits spinning disks; SSDs take more)
    pub hash_jobs: usize,
    /// the hashing after each scan also retries files it couldn't read before
    pub hash_retry_errors: bool,
    /// ask GitHub once a day whether a newer BackDown is out
    pub update_check: bool,
    /// "stable" or "preview"; unset: previews while this is a preview, releases otherwise
    pub update_channel: Option<String>,
    /// more folder names to count as regenerable (caches and the like), besides the built-in rules
    pub junk_names: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            rescan_every: "24h".into(),
            hash_jobs: 1,
            hash_retry_errors: false,
            update_check: true,
            update_channel: None,
            junk_names: Vec::new(),
        }
    }
}

struct Place {
    dir: PathBuf,
    /// the machine-wide service's folder, shared with the `backdown` group
    shared: bool,
    /// the running binary, as it was started (re-executed after an update)
    exe: Option<PathBuf>,
    /// the daemon was started with --rescan-every, which wins at startup
    rescan_flag: bool,
}

static PLACE: OnceLock<Place> = OnceLock::new();

/// Where settings live, for a daemon (its state folder). Everything else uses the per-user folder.
pub fn init(dir: &Path, shared: bool, rescan_flag: bool) {
    let _ = PLACE.set(Place {
        dir: dir.to_path_buf(),
        shared,
        exe: std::env::current_exe()
            .ok()
            .and_then(|e| dunce::canonicalize(e).ok()),
        rescan_flag,
    });
    apply_to_core(&load());
}

pub fn dir() -> PathBuf {
    PLACE
        .get()
        .map_or_else(locations::state_dir, |p| p.dir.clone())
}

pub fn shared() -> bool {
    PLACE.get().is_some_and(|p| p.shared)
}

/// The binary this daemon was started from (its path now holds any update).
pub fn started_exe() -> Option<PathBuf> {
    PLACE.get().and_then(|p| p.exe.clone())
}

fn file() -> PathBuf {
    dir().join("settings.json")
}

fn schema_file() -> PathBuf {
    dir().join("schema.toml")
}

/// The settings (defaults for anything missing; all defaults if the file is missing or damaged).
pub fn load() -> Settings {
    std::fs::read(file())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(s: &Settings) -> Result<()> {
    write_private(&file(), &serde_json::to_vec_pretty(s)?, shared())
}

/// Settings that live in backdown-core (junk rules) take effect in this process.
pub fn apply_to_core(s: &Settings) {
    recognise::set_extra_junk(&s.junk_names);
}

/// Write a file only its owner (or, for the system service, the `backdown` group) can read,
/// replacing it in one step.
pub fn write_private(path: &Path, data: &[u8], shared: bool) -> Result<()> {
    if let Some(d) = path.parent() {
        locations::ensure_private_dir(d, shared)
            .with_context(|| format!("can't create {}", d.display()))?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(if shared { 0o640 } else { 0o600 });
        }
        use std::io::Write;
        o.open(&tmp)
            .with_context(|| format!("can't write {}", tmp.display()))?
            .write_all(data)?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("can't write {}", path.display()))?;
    Ok(())
}

// ---------- the filing schema

/// The schema plans use: yours from Settings (schema.toml), else the built-in one. The bool says
/// which.
pub fn schema() -> Result<(plan::Schema, bool)> {
    match std::fs::read_to_string(schema_file()) {
        Ok(t) => Ok((
            plan::parse_schema(&t).with_context(|| {
                format!(
                    "your filing schema ({}) has a mistake; fix or reset it in Settings",
                    schema_file().display()
                )
            })?,
            true,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((plan::Schema::default(), false)),
        Err(e) => Err(e).with_context(|| format!("can't read {}", schema_file().display())),
    }
}

/// Check a schema: an error for what `plan` would refuse, warnings for what it would ignore.
pub fn check_schema(text: &str) -> Result<Vec<String>> {
    let s = plan::parse_schema(text)?;
    let known: Vec<&str> = recognise::KINDS
        .iter()
        .map(|(k, _)| *k)
        .chain(["unsorted"])
        .collect();
    let mut warnings: Vec<String> = s
        .place
        .keys()
        .filter(|k| !known.contains(&k.as_str()))
        .map(|k| format!("“{k}” isn't a kind BackDown recognises, so nothing is placed by it"))
        .collect();
    for k in &known {
        if !s.place.contains_key(*k) {
            warnings.push(format!(
                "no place for “{k}”: such things go under Unsorted/{{drive}}"
            ));
        }
    }
    if !(0.5..=1.0).contains(&s.merge_overlap) {
        warnings.push(format!(
            "merge_overlap is {}: between 0.5 and 1 makes sense (0.9 is the default)",
            s.merge_overlap
        ));
    }
    Ok(warnings)
}

// ---------- the web endpoints

pub struct Env<'a> {
    pub daemon: Option<&'a Arc<Daemon>>,
    /// running as the person at the screen (not the machine-wide service)
    pub desktop: bool,
    pub db: &'a Path,
}

const PATHS: &[&str] = &[
    "/api/settings",
    "/api/features",
    "/api/license",
    "/api/schema",
    "/api/update",
    "/api/update-check",
    "/api/update-install",
    "/api/restart",
    "/api/launch-code",
];

pub fn handles(path: &str) -> bool {
    PATHS.contains(&path)
}

type Resp = Response<std::io::Cursor<Vec<u8>>>;

fn reply(v: &Value, status: u16) -> Resp {
    Response::from_data(serde_json::to_vec(v).unwrap_or_default())
        .with_status_code(status)
        .with_header(Header::from_bytes(b"Content-Type", b"application/json").expect("header"))
        .with_header(Header::from_bytes(b"Cache-Control", b"no-store").expect("header"))
}

/// Answer one of `PATHS` (already authenticated by serve.rs).
pub fn handle(mut req: Request, path: &str, params: &[(String, String)], env: Env) {
    let post = *req.method() == tiny_http::Method::Post;
    let marked = req.headers().iter().any(|h| h.field.equiv("X-BackDown"));
    if post && !marked {
        let _ = req.respond(reply(&json!({ "error": "missing X-BackDown header" }), 403));
        return;
    }
    if !post && *req.method() != tiny_http::Method::Get {
        let _ = req.respond(reply(&json!({ "error": "GET or POST" }), 405));
        return;
    }
    let param = |n: &str| params.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
    let mut body = String::new();
    if post && let Err(e) = req.as_reader().take(256 << 10).read_to_string(&mut body) {
        let _ = req.respond(reply(&json!({ "error": format!("{e}") }), 400));
        return;
    }
    let r: Result<Value> = match (post, path) {
        (false, "/api/settings") => Ok(settings_json(&env)),
        (true, "/api/settings") => change(&env, params).map(|()| settings_json(&env)),
        (false, "/api/features") => Ok(crate::tiers::features_json()),
        (true, "/api/license") => {
            if env.daemon.is_some() && !env.desktop {
                Err(anyhow::anyhow!(
                    "the machine-wide service keeps no licence of its own: add it with `backdown license add <key>` as the person using BackDown"
                ))
            } else {
                crate::license::store(&body).map(|_| crate::tiers::features_json())
            }
        }
        (false, "/api/schema") => schema_json(),
        (true, "/api/schema") => post_schema(
            &body,
            param("reset") == Some("1"),
            param("check") == Some("1"),
        ),
        (false, "/api/update") => Ok(crate::update::status_json(false, &env)),
        (true, "/api/update-check") => Ok(crate::update::status_json(true, &env)),
        (true, "/api/update-install") => crate::update::start_install(&env),
        (true, "/api/restart") => match env.daemon {
            Some(_) => {
                restart_soon();
                Ok(json!({ "ok": true }))
            }
            None => Err(anyhow::anyhow!("no daemon here to restart")),
        },
        (true, "/api/launch-code") => crate::app::issue_code().map(|c| json!({ "code": c })),
        _ => Err(anyhow::anyhow!("no such endpoint")),
    };
    let resp = match r {
        Ok(v) => reply(&v, 200),
        Err(e) => reply(&json!({ "error": format!("{e:#}") }), 400),
    };
    let _ = req.respond(resp);
}

fn settings_json(env: &Env) -> Value {
    let s = load();
    let flag = PLACE.get().is_some_and(|p| p.rescan_flag);
    let running_every = env.daemon.map(|d| d.rescan_every().map(|x| x.as_secs()));
    json!({
        "settings": s,
        "defaults": Settings::default(),
        "daemon": env.daemon.is_some(),
        "shared": env.daemon.is_some() && !env.desktop,
        // what the running daemon uses now (it can differ when it was started with --rescan-every)
        "rescan_every_secs": running_every,
        "rescan_flag": flag,
        "junk_rules": recognise::JUNK_NAMES.iter().map(|(n, w)| json!({ "name": n, "why": w })).collect::<Vec<_>>(),
        "junk_places": recognise::JUNK_PLACES.iter().map(|(n, w)| json!({ "name": n, "why": w })).collect::<Vec<_>>(),
        "hashing": {
            "sample_kib": 64,
            "small_kib": 192,
        },
        "paths": {
            "data": dir().display().to_string(),
            "index": env.db.display().to_string(),
            "settings": file().display().to_string(),
            "schema": schema_file().display().to_string(),
            "licence": crate::license::path().display().to_string(),
            "log": dir().join("daemon.log").display().to_string(),
        },
        "about": {
            "version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "exe": started_exe().or_else(|| std::env::current_exe().ok()).map(|p| p.display().to_string()),
            "pid": std::process::id(),
        },
    })
}

/// POST /api/settings?key=value…: check each, save, and apply what the daemon holds in memory.
fn change(env: &Env, params: &[(String, String)]) -> Result<()> {
    let mut s = load();
    let mut every = None;
    for (k, v) in params {
        let v = v.trim();
        match k.as_str() {
            "rescan_every" => {
                let d = daemon::parse_every(v)?;
                if d.is_some_and(|d| d < Duration::from_secs(15 * 60)) {
                    bail!(
                        "rescanning more often than every 15 minutes would keep your drives busy"
                    );
                }
                s.rescan_every = if d.is_none() {
                    "never".into()
                } else {
                    v.into()
                };
                every = Some(d);
            }
            "hash_jobs" => {
                let n: usize = v.parse().context("readers per drive is a number")?;
                if !(1..=16).contains(&n) {
                    bail!("readers per drive: 1 to 16");
                }
                s.hash_jobs = n;
            }
            "hash_retry_errors" => s.hash_retry_errors = v == "1" || v == "true",
            "update_check" => s.update_check = v == "1" || v == "true",
            "update_channel" => {
                s.update_channel = match v {
                    "" | "auto" => None,
                    "stable" | "preview" => Some(v.into()),
                    _ => bail!("the update channel is stable or preview"),
                }
            }
            "junk_names" => {
                let mut names: Vec<String> = Vec::new();
                for n in v
                    .split([',', '\n'])
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                {
                    if n.contains(['/', '\\']) || n == "." || n == ".." {
                        bail!("“{n}”: a folder name, not a path");
                    }
                    if !names.iter().any(|x| x.eq_ignore_ascii_case(n)) {
                        names.push(n.to_string());
                    }
                }
                s.junk_names = names;
            }
            _ => bail!("unknown setting {k}"),
        }
    }
    save(&s)?;
    apply_to_core(&s);
    if let (Some(d), Some(every)) = (env.daemon, every) {
        d.set_rescan_every(every);
    }
    Ok(())
}

fn schema_json() -> Result<Value> {
    let custom = std::fs::read_to_string(schema_file()).ok();
    let text = custom
        .clone()
        .unwrap_or_else(|| plan::DEFAULT_SCHEMA.to_string());
    let (problem, warnings) = match check_schema(&text) {
        Ok(w) => (None, w),
        Err(e) => (Some(format!("{e:#}")), Vec::new()),
    };
    Ok(json!({
        "text": text,
        "custom": custom.is_some(),
        "builtin": plan::DEFAULT_SCHEMA,
        "problem": problem,
        "warnings": warnings,
        "file": schema_file().display().to_string(),
        "kinds": recognise::KINDS.iter().map(|(k, l)| json!({ "kind": k, "label": l })).collect::<Vec<_>>(),
    }))
}

/// POST /api/schema: check (`check=1`), save, or go back to the built-in schema (`reset=1`).
fn post_schema(text: &str, reset: bool, check_only: bool) -> Result<Value> {
    if reset {
        match std::fs::remove_file(schema_file()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context("can't remove your schema"),
        }
        return schema_json();
    }
    let warnings = check_schema(text)?;
    if check_only {
        return Ok(json!({ "ok": true, "warnings": warnings }));
    }
    let text = if text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    };
    write_private(&schema_file(), text.as_bytes(), shared())?;
    let mut v = schema_json()?;
    v["saved"] = json!(true);
    Ok(v)
}

// ---------- restarting the daemon (after an update)

static RESTARTING: Mutex<bool> = Mutex::new(false);

/// Answer first, then start again from the binary on disk: under systemd by exiting so it
/// restarts the service; elsewhere by running the same command line again in this process (Unix)
/// or in a new one (Windows).
pub fn restart_soon() {
    let mut r = RESTARTING.lock().unwrap_or_else(|e| e.into_inner());
    if *r {
        return;
    }
    *r = true;
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(400));
        restart_now();
    });
}

fn restart_now() -> ! {
    let exe = started_exe()
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("backdown"));
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    eprintln!("restarting from {}", exe.display());
    if std::env::var_os("INVOCATION_ID").is_some() && std::env::var_os("NOTIFY_SOCKET").is_some() {
        // systemd restarts a service that fails (Restart=on-failure): that's the restart
        std::process::exit(75);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let e = std::process::Command::new(&exe).args(&args).exec();
        eprintln!("couldn't start {} again: {e}", exe.display());
        std::process::exit(1);
    }
    #[cfg(not(unix))]
    {
        let mut c = std::process::Command::new(&exe);
        c.args(&args).env("BACKDOWN_BIND_WAIT", "1");
        crate::app::detach(&mut c);
        if let Err(e) = c.spawn() {
            eprintln!("couldn't start {} again: {e}", exe.display());
        }
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_schema_checks_clean() {
        assert_eq!(
            check_schema(plan::DEFAULT_SCHEMA).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn schema_mistakes_are_errors_and_oddities_warnings() {
        assert!(
            check_schema("[place]\nmail-store = /abs\n")
                .unwrap_err()
                .to_string()
                .contains("quoted path")
        );
        assert!(check_schema("[wrong]\na = 1\n").is_err());
        let w = check_schema(&plan::DEFAULT_SCHEMA.replace(
            "mail-store = \"Mail/{origin}\"",
            "mail-stores = \"Mail/{origin}\"",
        ))
        .unwrap();
        assert!(
            w.iter().any(|w| w.contains("“mail-stores” isn't a kind")),
            "{w:?}"
        );
        assert!(
            w.iter().any(|w| w.contains("no place for “mail-store”")),
            "{w:?}"
        );
    }

    #[test]
    fn settings_default_for_what_is_missing() {
        let s: Settings = serde_json::from_str(r#"{"hash_jobs": 3}"#).unwrap();
        assert_eq!(s.hash_jobs, 3);
        assert_eq!(s.rescan_every, "24h");
        assert!(s.update_check);
        let back: Settings = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
