//! Is there a newer release: asking GitHub (through curl) at most once a day and remembering the
//! answer, so the browser and the CLI can say so without waiting.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use backdown_core::clock::now;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::settings;

use super::*;

// ---------- asking, and remembering the answer

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct Cache {
    /// the API root asked
    pub(super) source: String,
    /// when GitHub last answered (200 or 304)
    pub(super) checked_at: Option<i64>,
    /// when it was last asked
    pub(super) tried_at: Option<i64>,
    pub(super) etag: Option<String>,
    pub(super) releases: Vec<Release>,
    /// what went wrong the last time
    pub(super) error: Option<String>,
    /// GitHub's rate limit: not before then
    pub(super) blocked_until: Option<i64>,
}

pub(super) fn cache_file() -> PathBuf {
    settings::dir().join("update.json")
}

pub(super) fn load_cache() -> Cache {
    std::fs::read(cache_file())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub(super) fn save_cache(c: &Cache) {
    if let Ok(b) = serde_json::to_vec_pretty(c) {
        let _ = settings::write_private(&cache_file(), &b, settings::shared());
    }
}

pub fn api_root() -> String {
    std::env::var("BACKDOWN_UPDATE_URL")
        .ok()
        .filter(|u| !u.trim().is_empty())
        .map_or_else(
            || RELEASES_API.to_string(),
            |u| u.trim_end_matches('/').to_string(),
        )
}

#[derive(Clone, Debug, Serialize)]
pub struct Found {
    pub tag: String,
    pub version: String,
    /// the release page, with its notes
    pub notes_url: String,
    pub prerelease: bool,
    pub published_at: Option<String>,
    pub archive: String,
    pub size: u64,
    /// the release has SHA256SUMS.txt.sig (checked when it's installed)
    pub signed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub current: String,
    pub channel: Channel,
    pub platform: Option<&'static str>,
    /// when GitHub last answered
    pub checked_at: Option<i64>,
    /// the newest release for this channel and platform (maybe this one)
    pub latest: Option<Found>,
    pub newer: bool,
    pub error: Option<String>,
    pub blocked_until: Option<i64>,
    pub source: String,
}

/// Whether a newer BackDown is out: from what's remembered while it's less than a day old (or
/// always, if GitHub's limit says not to ask), else asked now.
pub fn check(force: bool, channel: Channel) -> Status {
    let (c, _) = refresh(force);
    status_of(&c, channel)
}

pub(super) fn refresh(force: bool) -> (Cache, bool) {
    let root = api_root();
    let mut c = load_cache();
    if c.source != root {
        c = Cache {
            source: root,
            ..Cache::default()
        };
    }
    let t = now();
    let fresh = c.checked_at.is_some_and(|x| t - x < DAY);
    let blocked = c.blocked_until.is_some_and(|x| t < x);
    let failed_lately =
        c.error.is_some() && c.tried_at.is_some_and(|x| t - x < RETRY_AFTER_FAILURE);
    let ask = !blocked && (force || (!fresh && !failed_lately));
    if ask {
        fetch(&mut c);
        save_cache(&c);
    }
    (c, ask)
}

pub(super) fn fetch(c: &mut Cache) {
    let t = now();
    c.tried_at = Some(t);
    let url = format!("{}/releases?per_page=30", c.source);
    let mut headers = vec![
        ("Accept", "application/vnd.github+json".to_string()),
        ("X-GitHub-Api-Version", "2022-11-28".to_string()),
    ];
    if let Some(e) = c.etag.as_ref().filter(|_| !c.releases.is_empty()) {
        headers.push(("If-None-Match", e.clone()));
    }
    match http(&url, &headers, None, 30) {
        Ok(r) if r.status == 200 => match serde_json::from_slice::<Value>(&r.body) {
            Ok(v) => {
                c.releases = parse_releases(&v);
                c.etag = r.header("etag").map(String::from);
                c.checked_at = Some(t);
                c.error = None;
                c.blocked_until = None;
            }
            Err(e) => c.error = Some(format!("GitHub's answer isn't JSON: {e}")),
        },
        Ok(r) if r.status == 304 => {
            c.checked_at = Some(t);
            c.error = None;
            c.blocked_until = None;
        }
        Ok(r) if r.limited() => {
            let until = r.limit_reset(t);
            c.blocked_until = Some(until);
            c.error = Some(format!(
                "GitHub's limit for update checks from this network is reached; BackDown asks again after {}",
                crate::utc(until)
            ));
        }
        Ok(r) => {
            let msg = serde_json::from_slice::<Value>(&r.body)
                .ok()
                .and_then(|v| v["message"].as_str().map(String::from))
                .unwrap_or_default();
            c.error = Some(format!(
                "GitHub answered {}{}",
                r.status,
                if msg.is_empty() {
                    String::new()
                } else {
                    format!(": {msg}")
                }
            ));
        }
        Err(e) => c.error = Some(format!("{e:#}")),
    }
}

pub(super) fn status_of(c: &Cache, channel: Channel) -> Status {
    let cur = current();
    let plat = platform();
    let latest = plat
        .and_then(|p| pick(&c.releases, channel, p))
        .map(|(v, r)| {
            let name = archive_name(&r.tag, plat.unwrap_or_default());
            Found {
                tag: r.tag.clone(),
                version: r.tag.trim_start_matches('v').to_string(),
                notes_url: r.url.clone(),
                prerelease: r.prerelease || v.is_pre(),
                published_at: r.published_at.clone(),
                size: r
                    .assets
                    .iter()
                    .find(|a| a.name == name)
                    .map_or(0, |a| a.size),
                archive: name,
                signed: r.assets.iter().any(|a| a.name == SIG),
            }
        });
    let newer = latest
        .as_ref()
        .and_then(|f| Version::parse(&f.version))
        .is_some_and(|v| v > cur);
    Status {
        current: env!("CARGO_PKG_VERSION").to_string(),
        channel,
        platform: plat,
        checked_at: c.checked_at,
        latest,
        newer,
        error: c.error.clone(),
        blocked_until: c.blocked_until.filter(|t| *t > now()),
        source: c.source.clone(),
    }
}

// ---------- HTTP, through curl

pub(super) struct Fetched {
    pub(super) status: u16,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: Vec<u8>,
}

impl Fetched {
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// GitHub's "slow down": 429, or 403 with no requests left (or a Retry-After)
    pub(super) fn limited(&self) -> bool {
        self.status == 429
            || (self.status == 403
                && (self.header("x-ratelimit-remaining") == Some("0")
                    || self.header("retry-after").is_some()))
    }

    /// When asking is allowed again (an hour if GitHub doesn't say).
    pub(super) fn limit_reset(&self, now: i64) -> i64 {
        self.header("retry-after")
            .and_then(|s| s.trim().parse::<i64>().ok())
            .map(|s| now + s)
            .or_else(|| {
                self.header("x-ratelimit-reset")
                    .and_then(|s| s.trim().parse::<i64>().ok())
            })
            .filter(|t| *t > now)
            .unwrap_or(now + 3600)
    }
}

/// https anywhere; plain http only to this machine (tests).
pub(super) fn allowed(url: &str) -> Result<&'static str> {
    if url.starts_with("https://") {
        return Ok("=https");
    }
    let rest = url.strip_prefix("http://").unwrap_or_default();
    let host = rest.split(['/', '?']).next().unwrap_or_default();
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
    if url.starts_with("http://") && ["127.0.0.1", "localhost", "[::1]"].contains(&host) {
        return Ok("=http");
    }
    bail!("refusing to download from {url}: only https")
}

/// The last response's status line and headers from curl's --dump-header (which lists every
/// response when redirects are followed).
pub(super) fn parse_headers(dump: &str) -> (u16, Vec<(String, String)>) {
    let last = dump
        .split("\r\n\r\n")
        .flat_map(|b| b.split("\n\n"))
        .filter(|b| b.trim_start().starts_with("HTTP/"))
        .last()
        .unwrap_or_default();
    let mut lines = last.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    (status, headers)
}

pub(super) static TMP: Mutex<u64> = Mutex::new(0);

pub(super) fn tmp_path(what: &str) -> PathBuf {
    let mut n = TMP.lock().unwrap_or_else(|e| e.into_inner());
    *n += 1;
    settings::dir().join(format!(".update-{}-{}.{what}", std::process::id(), *n))
}

/// GET `url` with curl; the body in memory, or written to `to`.
pub(super) fn http(
    url: &str,
    headers: &[(&str, String)],
    to: Option<&Path>,
    max_secs: u64,
) -> Result<Fetched> {
    let proto = allowed(url)?;
    let _ = locations_dir();
    let head = tmp_path("headers");
    let body = match to {
        Some(p) => p.to_path_buf(),
        None => tmp_path("body"),
    };
    let mut c = Command::new("curl");
    c.args([
        "-q",
        "--silent",
        "--show-error",
        "--location",
        "--max-redirs",
        "5",
    ])
    .args(["--proto", proto, "--proto-redir", proto])
    .args([
        "--connect-timeout",
        "20",
        "--max-time",
        &max_secs.to_string(),
    ])
    .args([
        "--user-agent",
        &format!(
            "backdown/{} (+https://backdown.io)",
            env!("CARGO_PKG_VERSION")
        ),
    ])
    .arg("--dump-header")
    .arg(&head)
    .arg("--output")
    .arg(&body)
    .args(["--write-out", "%{http_code}"]);
    if proto == "=https" {
        c.arg("--tlsv1.2");
    }
    for (k, v) in headers {
        c.arg("--header").arg(format!("{k}: {v}"));
    }
    c.arg("--url").arg(url);
    let out = c.stdin(Stdio::null()).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!(
                "checking for updates needs curl, which comes with macOS, Windows 10 and later and most Linux systems; install it, or download BackDown from https://backdown.io/download"
            )
        } else {
            anyhow::anyhow!("can't run curl: {e}")
        }
    })?;
    let dump = std::fs::read_to_string(&head).unwrap_or_default();
    let _ = std::fs::remove_file(&head);
    let data = if to.is_none() {
        let d = std::fs::read(&body).unwrap_or_default();
        let _ = std::fs::remove_file(&body);
        d
    } else {
        Vec::new()
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!(
            "couldn't reach {}: {}",
            url.split('/').take(3).collect::<Vec<_>>().join("/"),
            err.trim().trim_start_matches("curl: ")
        );
    }
    let (status, hdrs) = parse_headers(&dump);
    let code: u16 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(status);
    Ok(Fetched {
        status: if code != 0 { code } else { status },
        headers: hdrs,
        body: data,
    })
}

pub(super) fn locations_dir() -> Result<()> {
    crate::locations::ensure_private_dir(&settings::dir(), settings::shared())
        .with_context(|| format!("can't create {}", settings::dir().display()))
}
