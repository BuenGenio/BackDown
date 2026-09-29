//! `backdown update`: is there a newer BackDown, and install it.
//!
//! Releases are the public GitHub releases of BuenGenio/backdown-releases, where
//! scripts/publish-release.sh puts them, asked through GitHub's API without a token. The answer is
//! kept for a day (update.json in the data folder) and asked again with its ETag, which GitHub
//! doesn't count against its limit of 60 requests an hour per address; when that limit is reached,
//! nothing is asked until GitHub says it's lifted. The channel is previews while this is a preview
//! (stable releases otherwise); Settings or --channel can change it.
//!
//! Installing downloads this platform's archive and the release's SHA256SUMS.txt over HTTPS with
//! curl (it comes with macOS, Windows 10 and later, and most Linux systems; https only, certificates
//! checked, no ~/.curlrc), refuses unless the archive's SHA-256 matches, unpacks the binary beside
//! the running one, checks it runs and reports the expected version, keeps a copy of the old one
//! in the data folder, and swaps them with a rename (Windows can't overwrite a running .exe, but it
//! can rename it out of the way). If the installed binary then fails `--version`, the old one is put
//! back. Last, the per-user daemon is restarted so it runs the new version. `--rollback` puts the
//! kept copy back later. Nothing here ever runs as a different user.
//!
//! BACKDOWN_UPDATE_URL points it at another API root (the tests' local fake server; plain http is
//! allowed for 127.0.0.1 and localhost only).

use std::cmp::Ordering;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use backdown_core::fmt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::settings;

pub const RELEASES_API: &str = "https://api.github.com/repos/BuenGenio/backdown-releases";
const DAY: i64 = 86_400;
/// after a failed check (offline, GitHub down), don't ask again for this long
const RETRY_AFTER_FAILURE: i64 = 6 * 3600;
const SUMS: &str = "SHA256SUMS.txt";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

// ---------- versions

/// A semantic version: 0.2.0-alpha.4 < 0.2.0-alpha.10 < 0.2.0-beta.1 < 0.2.0-rc.1 < 0.2.0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<Ident>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ident {
    Num(u64),
    Text(String),
}

impl Version {
    /// "v0.2.0-alpha.4" or "0.2.0" (build metadata after + is ignored)
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim().trim_start_matches('v');
        let s = s.split_once('+').map_or(s, |(v, _)| v);
        let (core, pre) = s.split_once('-').map_or((s, ""), |(c, p)| (c, p));
        if s.contains('-') && pre.is_empty() {
            return None;
        }
        let mut n = core.split('.').map(|x| x.parse::<u64>().ok());
        let (major, minor, patch) = (n.next()??, n.next()??, n.next()??);
        if n.next().is_some() {
            return None;
        }
        let pre = if pre.is_empty() {
            Vec::new()
        } else {
            pre.split('.')
                .map(|p| {
                    if p.is_empty() {
                        None
                    } else {
                        Some(match p.parse() {
                            Ok(n) if !p.starts_with('0') || p == "0" => Ident::Num(n),
                            _ => Ident::Text(p.to_string()),
                        })
                    }
                })
                .collect::<Option<_>>()?
        };
        Some(Version {
            major,
            minor,
            patch,
            pre,
        })
    }

    pub fn is_pre(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(o.major, o.minor, o.patch))
            .then_with(|| match (self.pre.is_empty(), o.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater, // 1.0.0 > 1.0.0-rc.1
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (a, b) in self.pre.iter().zip(&o.pre) {
                        let c = match (a, b) {
                            (Ident::Num(a), Ident::Num(b)) => a.cmp(b),
                            (Ident::Num(_), Ident::Text(_)) => Ordering::Less,
                            (Ident::Text(_), Ident::Num(_)) => Ordering::Greater,
                            (Ident::Text(a), Ident::Text(b)) => a.cmp(b),
                        };
                        if c != Ordering::Equal {
                            return c;
                        }
                    }
                    self.pre.len().cmp(&o.pre.len())
                }
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

pub fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml's version is semver")
}

// ---------- releases

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Preview,
}

impl Channel {
    pub fn parse(s: &str) -> Result<Channel> {
        match s {
            "stable" | "release" | "releases" => Ok(Channel::Stable),
            "preview" | "previews" | "pre" => Ok(Channel::Preview),
            _ => bail!("the channel is stable or preview, not {s:?}"),
        }
    }

    /// From Settings, else previews for a preview build and releases for a release.
    pub fn configured() -> Channel {
        match settings::load().update_channel.as_deref() {
            Some(c) => Channel::parse(c).unwrap_or(Channel::Stable),
            None if current().is_pre() => Channel::Preview,
            None => Channel::Stable,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    pub prerelease: bool,
    /// the release page (its notes)
    pub url: String,
    pub published_at: Option<String>,
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// GitHub's list of releases, without drafts.
pub fn parse_releases(v: &Value) -> Vec<Release> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["draft"] != true)
        .filter_map(|r| {
            Some(Release {
                tag: r["tag_name"].as_str()?.to_string(),
                prerelease: r["prerelease"].as_bool().unwrap_or(false),
                url: r["html_url"].as_str().unwrap_or_default().to_string(),
                published_at: r["published_at"].as_str().map(String::from),
                assets: r["assets"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|a| {
                        Some(Asset {
                            name: a["name"].as_str()?.to_string(),
                            url: a["browser_download_url"].as_str()?.to_string(),
                            size: a["size"].as_u64().unwrap_or(0),
                        })
                    })
                    .collect(),
            })
        })
        .collect()
}

/// This build's platform, as release archives are named.
pub fn platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        ("macos", _) => Some("macos-universal"),
        ("windows", "x86_64") => Some("windows-x86_64"),
        _ => None,
    }
}

pub fn archive_name(tag: &str, platform: &str) -> String {
    let ext = if platform.starts_with("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("backdown-{tag}-{platform}.{ext}")
}

/// The newest release on `channel` with an archive for `platform` and checksums.
pub fn pick<'a>(
    releases: &'a [Release],
    channel: Channel,
    platform: &str,
) -> Option<(Version, &'a Release)> {
    releases
        .iter()
        .filter_map(|r| Some((Version::parse(&r.tag)?, r)))
        .filter(|(v, r)| channel == Channel::Preview || (!r.prerelease && !v.is_pre()))
        .filter(|(_, r)| {
            let want = archive_name(&r.tag, platform);
            r.assets.iter().any(|a| a.name == want) && r.assets.iter().any(|a| a.name == SUMS)
        })
        .max_by(|a, b| a.0.cmp(&b.0))
}

/// The hash SHA256SUMS.txt gives for `name` (sha256sum's format: "<hex>  <name>", or " *<name>").
pub fn expected_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hash, file) = l.trim().split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        (file == name && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

// ---------- asking, and remembering the answer

#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    /// the API root asked
    source: String,
    /// when GitHub last answered (200 or 304)
    checked_at: Option<i64>,
    /// when it was last asked
    tried_at: Option<i64>,
    etag: Option<String>,
    releases: Vec<Release>,
    /// what went wrong the last time
    error: Option<String>,
    /// GitHub's rate limit: not before then
    blocked_until: Option<i64>,
}

fn cache_file() -> PathBuf {
    settings::dir().join("update.json")
}

fn load_cache() -> Cache {
    std::fs::read(cache_file())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_cache(c: &Cache) {
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

fn refresh(force: bool) -> (Cache, bool) {
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

fn fetch(c: &mut Cache) {
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

fn status_of(c: &Cache, channel: Channel) -> Status {
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

struct Fetched {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Fetched {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// GitHub's "slow down": 429, or 403 with no requests left (or a Retry-After)
    fn limited(&self) -> bool {
        self.status == 429
            || (self.status == 403
                && (self.header("x-ratelimit-remaining") == Some("0")
                    || self.header("retry-after").is_some()))
    }

    /// When asking is allowed again (an hour if GitHub doesn't say).
    fn limit_reset(&self, now: i64) -> i64 {
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
fn allowed(url: &str) -> Result<&'static str> {
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
fn parse_headers(dump: &str) -> (u16, Vec<(String, String)>) {
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

static TMP: Mutex<u64> = Mutex::new(0);

fn tmp_path(what: &str) -> PathBuf {
    let mut n = TMP.lock().unwrap_or_else(|e| e.into_inner());
    *n += 1;
    settings::dir().join(format!(".update-{}-{}.{what}", std::process::id(), *n))
}

/// GET `url` with curl; the body in memory, or written to `to`.
fn http(
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

fn locations_dir() -> Result<()> {
    crate::locations::ensure_private_dir(&settings::dir(), settings::shared())
        .with_context(|| format!("can't create {}", settings::dir().display()))
}

// ---------- installing

/// Why this binary can't replace itself, if it can't.
pub fn not_replaceable(exe: &Path) -> Option<String> {
    if std::env::var_os("APPIMAGE").is_some() {
        return Some("this BackDown runs from an AppImage, which can't change itself: download the new AppImage from https://backdown.io/download".into());
    }
    let s = exe.to_string_lossy();
    let managed = [
        (
            "/usr/bin/",
            "your package manager (it came in a .deb or another package)",
        ),
        ("/bin/", "your package manager"),
        ("/usr/sbin/", "your package manager"),
        ("/nix/store/", "Nix"),
        ("/snap/", "snap"),
    ];
    for (prefix, who) in managed {
        if s.starts_with(prefix) {
            return Some(format!(
                "{} is managed by {who}: update it that way (the new package is at https://backdown.io/download)",
                exe.display()
            ));
        }
    }
    if s.contains("/Cellar/") {
        return Some(format!(
            "{} is managed by Homebrew: brew upgrade backdown",
            exe.display()
        ));
    }
    None
}

fn bin_name() -> &'static str {
    if cfg!(windows) {
        "backdown.exe"
    } else {
        "backdown"
    }
}

/// `path --version`, waiting at most 30 s.
fn version_of(path: &Path) -> Result<String> {
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("can't run {}", path.display()))?;
    let start = Instant::now();
    loop {
        if let Some(st) = child.try_wait()? {
            let mut out = String::new();
            let mut err = String::new();
            if let Some(mut o) = child.stdout.take() {
                let _ = o.read_to_string(&mut out);
            }
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut err);
            }
            if !st.success() {
                bail!(
                    "it exited with {st}{}",
                    if err.trim().is_empty() {
                        String::new()
                    } else {
                        format!(": {}", err.trim())
                    }
                );
            }
            return Ok(out.lines().next().unwrap_or_default().trim().to_string());
        }
        if start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            bail!("it didn't answer --version within 30 s");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn sha256_file(p: &Path) -> Result<String> {
    let mut f = std::fs::File::open(p).with_context(|| format!("can't read {}", p.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(fmt::hex(&h.finalize()))
}

/// Take `name` out of a release archive (.tar.gz, or .zip for Windows) into `to`.
fn unpack(archive: &Path, name: &str, to: &Path) -> Result<()> {
    if archive.to_string_lossy().ends_with(".zip") {
        return unzip_one(archive, name, to);
    }
    let f = std::fs::File::open(archive)?;
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
    for e in ar.entries().context("the archive is damaged")? {
        let mut e = e.context("the archive is damaged")?;
        let is_it = e.path()?.file_name() == Some(std::ffi::OsStr::new(name));
        if is_it && e.header().entry_type().is_file() {
            let mut out = std::fs::File::create(to)?;
            std::io::copy(&mut e, &mut out).context("the archive is damaged")?;
            return Ok(());
        }
    }
    bail!("there's no {name} in the archive")
}

/// One file from a zip archive (stored or deflated; what 7z and zip write).
fn unzip_one(archive: &Path, name: &str, to: &Path) -> Result<()> {
    let d = std::fs::read(archive)?;
    let u16at = |i: usize| -> Result<usize> {
        Ok(u16::from_le_bytes(d.get(i..i + 2).context("the zip is damaged")?.try_into()?) as usize)
    };
    let u32at = |i: usize| -> Result<usize> {
        Ok(u32::from_le_bytes(d.get(i..i + 4).context("the zip is damaged")?.try_into()?) as usize)
    };
    let lowest = d.len().saturating_sub(22 + 65_535);
    let eocd = (lowest..=d.len().saturating_sub(22))
        .rev()
        .find(|&i| d[i..].starts_with(b"PK\x05\x06"))
        .context("not a zip archive")?;
    let (entries, mut p) = (u16at(eocd + 10)?, u32at(eocd + 16)?);
    for _ in 0..entries {
        if !d.get(p..).is_some_and(|x| x.starts_with(b"PK\x01\x02")) {
            bail!("the zip's directory is damaged");
        }
        let (method, crc, csize, usize_) = (
            u16at(p + 10)?,
            u32at(p + 16)?,
            u32at(p + 20)?,
            u32at(p + 24)?,
        );
        let (nlen, elen, clen, local) = (
            u16at(p + 28)?,
            u16at(p + 30)?,
            u16at(p + 32)?,
            u32at(p + 42)?,
        );
        let fname = d.get(p + 46..p + 46 + nlen).context("the zip is damaged")?;
        p += 46 + nlen + elen + clen;
        let fname = String::from_utf8_lossy(fname).replace('\\', "/");
        if fname.rsplit('/').next() != Some(name) {
            continue;
        }
        if !d.get(local..).is_some_and(|x| x.starts_with(b"PK\x03\x04")) {
            bail!("the zip is damaged");
        }
        let start = local + 30 + u16at(local + 26)? + u16at(local + 28)?;
        let raw = d
            .get(start..start + csize)
            .context("the zip is cut short")?;
        let data = match method {
            0 => raw.to_vec(),
            8 => {
                let mut v = Vec::with_capacity(usize_);
                flate2::read::DeflateDecoder::new(raw).read_to_end(&mut v)?;
                v
            }
            m => bail!("{name} is compressed with method {m}, which this updater doesn't read"),
        };
        let mut c = flate2::Crc::new();
        c.update(&data);
        if data.len() != usize_ || c.sum() as usize != crc {
            bail!("{name} in the zip is damaged (its CRC doesn't match)");
        }
        std::fs::write(to, data)?;
        return Ok(());
    }
    bail!("there's no {name} in the archive")
}

#[cfg(unix)]
fn make_executable(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_: &Path) -> Result<()> {
    Ok(())
}

/// Put `new` where `exe` is, in one rename. On Windows the running .exe is renamed aside first
/// (`backdown.exe.old`, removed the next time); returns that path so a failure can undo it.
fn swap_in(new: &Path, exe: &Path) -> Result<Option<PathBuf>> {
    if cfg!(windows) {
        let old = exe.with_extension("exe.old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(exe, &old)
            .with_context(|| format!("can't move {} aside", exe.display()))?;
        if let Err(e) = std::fs::rename(new, exe) {
            let _ = std::fs::rename(&old, exe);
            return Err(e)
                .with_context(|| format!("can't put the new version in {}", exe.display()));
        }
        Ok(Some(old))
    } else {
        std::fs::rename(new, exe).with_context(|| format!("can't replace {}", exe.display()))?;
        Ok(None)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Previous {
    version: String,
    exe: PathBuf,
    copy: PathBuf,
    replaced_at: i64,
}

fn previous_file() -> PathBuf {
    settings::dir().join("previous.json")
}

/// A copy of the binary about to be replaced, in the data folder, for rolling back.
fn keep_copy(exe: &Path) -> Result<PathBuf> {
    let dir = settings::dir().join("previous");
    crate::locations::ensure_private_dir(&dir, settings::shared())?;
    let copy = dir.join(format!(
        "backdown-{}{}",
        env!("CARGO_PKG_VERSION"),
        if cfg!(windows) { ".exe" } else { "" }
    ));
    std::fs::copy(exe, &copy).with_context(|| format!("can't keep a copy of {}", exe.display()))?;
    make_executable(&copy)?;
    let rec = Previous {
        version: env!("CARGO_PKG_VERSION").into(),
        exe: exe.to_path_buf(),
        copy: copy.clone(),
        replaced_at: now(),
    };
    settings::write_private(
        &previous_file(),
        &serde_json::to_vec_pretty(&rec)?,
        settings::shared(),
    )?;
    Ok(copy)
}

/// Put a kept copy back in place of `exe`, through the same folder so the last step is a rename.
fn restore(copy: &Path, exe: &Path) -> Result<()> {
    let stage = exe.with_file_name(format!(".{}.restore", bin_name()));
    std::fs::copy(copy, &stage).with_context(|| format!("can't copy {}", copy.display()))?;
    make_executable(&stage)?;
    if let Err(e) = swap_in(&stage, exe) {
        let _ = std::fs::remove_file(&stage);
        return Err(e);
    }
    Ok(())
}

pub struct Installed {
    pub from: String,
    pub to: String,
}

/// Download, verify, check and install `found` over `exe`. Any failure before the swap leaves
/// everything as it was; a new binary that fails `--version` once installed is replaced by the old.
pub fn install(found: &Found, exe: &Path, say: &mut dyn FnMut(&str)) -> Result<Installed> {
    let plat = platform().context("there are no BackDown builds for this platform")?;
    if let Some(why) = not_replaceable(exe) {
        bail!(why);
    }
    // the same release, as last listed
    let cache = load_cache();
    let release = cache
        .releases
        .iter()
        .find(|r| r.tag == found.tag)
        .context("check for updates again first")?;
    let archive = release
        .assets
        .iter()
        .find(|a| a.name == archive_name(&release.tag, plat))
        .context("that release has no build for this platform")?;
    let sums = release
        .assets
        .iter()
        .find(|a| a.name == SUMS)
        .context("that release has no SHA256SUMS.txt, so it can't be verified")?;
    let dir = exe.parent().context("where is backdown?")?;
    let stage = dir.join(".backdown-update");
    let _ = std::fs::remove_dir_all(&stage);
    if let Err(e) = std::fs::create_dir(&stage) {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            bail!(
                "can't write to {} ({e}){}",
                dir.display(),
                if cfg!(unix) {
                    ": run `sudo backdown update`, or move backdown somewhere you own (like ~/.local/bin)"
                } else {
                    ""
                }
            );
        }
        return Err(e).with_context(|| format!("can't create {}", stage.display()));
    }
    let result = (|| -> Result<Installed> {
        say(&format!("downloading {SUMS} for {}", release.tag));
        let sums_path = stage.join(SUMS);
        let r = http(&sums.url, &[], Some(&sums_path), 120)?;
        if r.status != 200 {
            bail!("couldn't download {SUMS} (HTTP {})", r.status);
        }
        let expect = expected_sum(&std::fs::read_to_string(&sums_path)?, &archive.name)
            .with_context(|| {
                format!(
                    "{SUMS} doesn't list {}, so it can't be verified",
                    archive.name
                )
            })?;
        say(&format!(
            "downloading {} ({})",
            archive.name,
            fmt::bytes(archive.size)
        ));
        let arch_path = stage.join(&archive.name);
        let r = http(
            &archive.url,
            &[("Accept", "application/octet-stream".into())],
            Some(&arch_path),
            1800,
        )?;
        if r.status != 200 {
            bail!("couldn't download {} (HTTP {})", archive.name, r.status);
        }
        let got = sha256_file(&arch_path)?;
        if got != expect {
            bail!(
                "{} doesn't match {SUMS} (SHA-256 {got}, expected {expect}): it wasn't installed, and nothing was changed",
                archive.name
            );
        }
        say("SHA-256 matches SHA256SUMS.txt");
        let new = stage.join(bin_name());
        unpack(&arch_path, bin_name(), &new)?;
        make_executable(&new)?;
        let want = format!("backdown {}", found.version);
        match version_of(&new) {
            Ok(v) if v == want => {}
            Ok(v) => bail!("the downloaded program says {v:?}, not {want:?}: nothing was changed"),
            Err(e) => bail!("the downloaded program doesn't run here ({e:#}): nothing was changed"),
        }
        let copy = keep_copy(exe)?;
        let aside = swap_in(&new, exe)?;
        // the installed binary must run; if it doesn't, the previous one goes back
        if let Err(why) = version_of(exe).and_then(|v| {
            if v == want {
                Ok(())
            } else {
                Err(anyhow::anyhow!("it says {v:?}"))
            }
        }) {
            match &aside {
                Some(old) => {
                    let _ = std::fs::remove_file(exe);
                    std::fs::rename(old, exe)?;
                }
                None => restore(&copy, exe)?,
            }
            bail!(
                "BackDown {} didn't start once installed ({why:#}), so {} is back in place (rolled back)",
                found.version,
                env!("CARGO_PKG_VERSION")
            );
        }
        say(&format!(
            "installed BackDown {} in {}",
            found.version,
            exe.display()
        ));
        Ok(Installed {
            from: env!("CARGO_PKG_VERSION").into(),
            to: found.version.clone(),
        })
    })();
    let _ = std::fs::remove_dir_all(&stage);
    result
}

/// `backdown update --rollback`: the copy kept by the last update goes back in place.
fn rollback(exe: &Path) -> Result<String> {
    let rec: Previous = serde_json::from_slice(
        &std::fs::read(previous_file())
            .context("no earlier version was kept (nothing was updated here yet)")?,
    )?;
    if rec.exe != exe {
        bail!(
            "the kept copy (BackDown {}) was for {}, not {}",
            rec.version,
            rec.exe.display(),
            exe.display()
        );
    }
    if let Some(why) = not_replaceable(exe) {
        bail!(why);
    }
    let v = version_of(&rec.copy)
        .with_context(|| format!("the kept copy {} doesn't run", rec.copy.display()))?;
    restore(&rec.copy, exe)?;
    Ok(v)
}

/// After an install: the per-user daemon runs the new binary. Under systemd, restart the service;
/// otherwise ask the daemon (through its web UI's local port) to start itself again.
pub fn restart_daemon() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let active = Command::new("systemctl")
            .args(["--user", "is-active", "--quiet", "backdown.service"])
            .status()
            .is_ok_and(|s| s.success());
        if active {
            let ok = Command::new("systemctl")
                .args(["--user", "restart", "backdown.service"])
                .status()
                .is_ok_and(|s| s.success());
            return Some(if ok {
                "restarted the BackDown service".into()
            } else {
                "couldn't restart the service: systemctl --user restart backdown".into()
            });
        }
    }
    let found = crate::app::find_daemon()?;
    if found.system {
        return Some(
            "the machine-wide service still runs the old version: sudo systemctl restart backdown"
                .into(),
        );
    }
    match crate::app::local_request(found.port, "POST", "/api/restart", &found.token) {
        Ok((200, _)) => Some("restarted the background daemon".into()),
        Ok((s, b)) => Some(format!(
            "the daemon didn't restart ({s}: {}); stop and start it",
            b.trim()
        )),
        Err(e) => Some(format!("the daemon didn't restart ({e:#})")),
    }
}

/// Windows leaves the replaced .exe renamed beside the new one until it can go.
pub fn tidy() {
    if cfg!(windows)
        && let Ok(exe) = std::env::current_exe()
    {
        let _ = std::fs::remove_file(exe.with_extension("exe.old"));
    }
}

pub struct Args {
    pub check: bool,
    pub refresh: bool,
    pub rollback: bool,
    pub channel: Option<String>,
    pub json: bool,
}

/// `backdown update [--check] [--rollback]`
pub fn run(a: Args) -> Result<ExitCode> {
    tidy();
    let exe = dunce::canonicalize(std::env::current_exe()?)?;
    if a.rollback {
        let v = rollback(&exe)?;
        println!("put back {v} in {}", exe.display());
        if let Some(r) = restart_daemon() {
            println!("{r}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let channel = match &a.channel {
        Some(c) => Channel::parse(c)?,
        None => Channel::configured(),
    };
    // installing asks afresh unless the list is under an hour old
    let fresh_enough = load_cache()
        .checked_at
        .is_some_and(|t| now() - t < 3600 && load_cache().source == api_root());
    let st = check(a.refresh || (!a.check && !fresh_enough), channel);
    if a.json {
        println!("{}", serde_json::to_string_pretty(&st)?);
        return Ok(if st.newer {
            ExitCode::from(10)
        } else {
            ExitCode::SUCCESS
        });
    }
    if let Some(e) = &st.error {
        eprintln!("note: {e}");
    }
    let Some(latest) = st.latest.as_ref().filter(|_| st.newer) else {
        if st.checked_at.is_none() {
            bail!("couldn't find out whether there's a newer BackDown");
        }
        println!(
            "BackDown {} is the newest {} (checked {}).",
            st.current,
            match channel {
                Channel::Stable => "release",
                Channel::Preview => "version, previews included",
            },
            crate::utc(st.checked_at.unwrap_or_default())
        );
        return Ok(ExitCode::SUCCESS);
    };
    println!(
        "BackDown {} is available (this is {}). Release notes: {}",
        latest.version, st.current, latest.notes_url
    );
    if a.check {
        println!("Install it with `backdown update`.");
        return Ok(ExitCode::from(10));
    }
    let done = install(latest, &exe, &mut |m| eprintln!("  {m}"))?;
    println!("Updated BackDown {} → {}.", done.from, done.to);
    if let Some(r) = restart_daemon() {
        println!("{r}");
    }
    Ok(ExitCode::SUCCESS)
}

// ---------- the daemon's side: a daily look, and installs from the browser

/// Look once a day (per Settings) whether a newer BackDown is out, so the browser can say so.
/// Not for the machine-wide service, which has no network.
pub fn spawn_checker(shared: bool) {
    if shared || std::env::var_os("BACKDOWN_NO_UPDATE_CHECK").is_some() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("updates".into())
        .spawn(|| {
            std::thread::sleep(Duration::from_secs(90));
            loop {
                if settings::load().update_check {
                    let _ = refresh(false);
                }
                std::thread::sleep(Duration::from_secs(3600));
            }
        });
}

#[derive(Clone, Default, Serialize)]
struct Install {
    state: &'static str, // "" | "running" | "done" | "failed"
    message: String,
    version: String,
}

static INSTALL: Mutex<Install> = Mutex::new(Install {
    state: "",
    message: String::new(),
    version: String::new(),
});

/// GET /api/update (what's known), POST /api/update-check (ask GitHub now).
pub fn status_json(ask: bool, env: &settings::Env) -> Value {
    let channel = Channel::configured();
    let st = if ask {
        check(true, channel)
    } else {
        status_of(&load_cache(), channel)
    };
    let exe = settings::started_exe().or_else(|| std::env::current_exe().ok());
    let why_not = if env.daemon.is_none() {
        Some("installing runs in the BackDown service; in a terminal: backdown update".to_string())
    } else if !env.desktop {
        Some(
            "the machine-wide service is updated by whoever installed it: sudo backdown update"
                .to_string(),
        )
    } else {
        exe.as_deref().and_then(not_replaceable)
    };
    let install = INSTALL.lock().unwrap_or_else(|e| e.into_inner()).clone();
    json!({
        "status": st,
        "enabled": settings::load().update_check,
        "can_install": why_not.is_none(),
        "why_not": why_not,
        "install": install,
    })
}

/// POST /api/update-install: install the newer version in the background, then restart.
pub fn start_install(env: &settings::Env) -> Result<Value> {
    if env.daemon.is_none() || !env.desktop {
        bail!(
            "installing updates runs in your own BackDown service; in a terminal: backdown update"
        );
    }
    let exe = settings::started_exe()
        .or_else(|| std::env::current_exe().ok())
        .context("where is backdown?")?;
    if let Some(why) = not_replaceable(&exe) {
        bail!(why);
    }
    let st = status_of(&load_cache(), Channel::configured());
    let found = st
        .latest
        .filter(|_| st.newer)
        .context("there's no newer version to install: check again")?;
    {
        let mut i = INSTALL.lock().unwrap_or_else(|e| e.into_inner());
        if i.state == "running" {
            bail!("an update is being installed already");
        }
        *i = Install {
            state: "running",
            message: "starting".into(),
            version: found.version.clone(),
        };
    }
    std::thread::spawn(move || {
        let r = install(&found, &exe, &mut |m| {
            eprintln!("update: {m}");
            INSTALL.lock().unwrap_or_else(|e| e.into_inner()).message = m.to_string();
        });
        let mut i = INSTALL.lock().unwrap_or_else(|e| e.into_inner());
        match r {
            Ok(done) => {
                i.state = "done";
                i.message = format!("installed {}; restarting", done.to);
                drop(i);
                settings::restart_soon();
            }
            Err(e) => {
                eprintln!("update failed: {e:#}");
                i.state = "failed";
                i.message = format!("{e:#}");
            }
        }
    });
    Ok(json!({ "ok": true, "version": st.current }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn versions_order_like_semver() {
        let order = [
            "0.1.9",
            "0.2.0-alpha.1",
            "0.2.0-alpha.4",
            "0.2.0-alpha.10",
            "0.2.0-beta",
            "0.2.0-beta.2",
            "0.2.0-rc.1",
            "0.2.0",
            "0.2.1",
            "0.10.0",
            "1.0.0",
        ];
        for w in order.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
        assert_eq!(v("v1.2.3+build.9"), v("1.2.3"));
        assert!(Version::parse("1.2").is_none());
        assert!(Version::parse("1.2.3.4").is_none());
        assert!(Version::parse("1.2.3-").is_none());
        assert!(Version::parse("banana").is_none());
        assert!(current() == v(env!("CARGO_PKG_VERSION")));
    }

    fn release(tag: &str, pre: bool, plats: &[&str]) -> Release {
        let mut assets: Vec<Asset> = plats
            .iter()
            .map(|p| Asset {
                name: archive_name(tag, p),
                url: format!("https://example.com/{tag}/{p}"),
                size: 100,
            })
            .collect();
        assets.push(Asset {
            name: SUMS.into(),
            url: format!("https://example.com/{tag}/sums"),
            size: 10,
        });
        Release {
            tag: tag.into(),
            prerelease: pre,
            url: format!("https://example.com/releases/{tag}"),
            published_at: None,
            assets,
        }
    }

    #[test]
    fn picking_the_newest_for_a_channel_and_platform() {
        let all = [
            "linux-x86_64",
            "linux-arm64",
            "macos-universal",
            "windows-x86_64",
        ];
        let list = vec![
            release("v0.2.0-alpha.4", true, &all),
            release("v0.3.0", false, &all),
            release("v0.4.0-beta.1", true, &all),
            release("v0.3.1", false, &["linux-x86_64"]), // a Linux-only fix
        ];
        let (ver, r) = pick(&list, Channel::Stable, "macos-universal").unwrap();
        assert_eq!((ver, r.tag.as_str()), (v("0.3.0"), "v0.3.0"));
        assert_eq!(
            pick(&list, Channel::Stable, "linux-x86_64").unwrap().1.tag,
            "v0.3.1"
        );
        assert_eq!(
            pick(&list, Channel::Preview, "linux-arm64").unwrap().1.tag,
            "v0.4.0-beta.1"
        );
        // a release without checksums is never offered
        let mut unsummed = release("v9.0.0", false, &all);
        unsummed.assets.retain(|a| a.name != SUMS);
        assert_eq!(pick(&[unsummed], Channel::Stable, "linux-arm64"), None);
        // a pre-release tag counts as a preview even if it isn't marked as one
        let sneaky = release("v0.5.0-rc.1", false, &all);
        assert!(pick(&[sneaky], Channel::Stable, "linux-arm64").is_none());
    }

    #[test]
    fn github_release_json() {
        let v: Value = serde_json::json!([
            { "tag_name": "v0.3.0", "prerelease": false, "draft": false, "html_url": "https://github.com/x/y/releases/tag/v0.3.0",
              "published_at": "2026-10-01T10:00:00Z",
              "assets": [{ "name": "SHA256SUMS.txt", "browser_download_url": "https://github.com/x/y/releases/download/v0.3.0/SHA256SUMS.txt", "size": 400 }] },
            { "tag_name": "v0.4.0", "draft": true, "assets": [] },
            { "no": "tag" }
        ]);
        let r = parse_releases(&v);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].tag, "v0.3.0");
        assert_eq!(r[0].assets[0].size, 400);
    }

    #[test]
    fn checksums_file() {
        let sums = "c6418ceda15c13829e11d70ad3ed8d8c7cd5c7a5d7d564b6af737d5a1b12144b  backdown-v0.2.0-alpha.4-linux-x86_64.tar.gz\n\
                    421FB22C42F878B68DDF84EF856285808FB60E165CFCA719F34C2EB562FD451C *backdown-v0.2.0-alpha.4-linux-arm64.tar.gz\n\
                    nothex  other\n";
        assert_eq!(
            expected_sum(sums, "backdown-v0.2.0-alpha.4-linux-arm64.tar.gz").unwrap(),
            "421fb22c42f878b68ddf84ef856285808fb60e165cfca719f34c2eb562fd451c"
        );
        assert!(expected_sum(sums, "backdown-v0.2.0-alpha.4-linux-x86_64.tar.gz").is_some());
        assert!(expected_sum(sums, "other").is_none());
        assert!(expected_sum(sums, "backdown-v0.2.0-alpha.4-windows-x86_64.zip").is_none());
    }

    #[test]
    fn only_https_except_to_this_machine() {
        assert_eq!(allowed("https://api.github.com/x").unwrap(), "=https");
        assert_eq!(allowed("http://127.0.0.1:8080/x").unwrap(), "=http");
        assert_eq!(allowed("http://localhost/x").unwrap(), "=http");
        assert!(allowed("http://example.com/x").is_err());
        assert!(allowed("http://127.0.0.1.example.com/x").is_err());
        assert!(allowed("ftp://127.0.0.1/x").is_err());
        assert!(allowed("file:///etc/passwd").is_err());
    }

    #[test]
    fn headers_of_the_last_response() {
        let dump = "HTTP/1.1 302 Found\r\nLocation: https://x/y\r\n\r\nHTTP/2 403 \r\nX-RateLimit-Remaining: 0\r\nx-ratelimit-reset: 1790003600\r\n\r\n";
        let (status, h) = parse_headers(dump);
        assert_eq!(status, 403);
        let f = Fetched {
            status,
            headers: h,
            body: Vec::new(),
        };
        assert!(f.limited());
        assert_eq!(f.limit_reset(1_790_000_000), 1_790_003_600);
        let retry = Fetched {
            status: 429,
            headers: vec![("retry-after".into(), "120".into())],
            body: Vec::new(),
        };
        assert!(retry.limited());
        assert_eq!(retry.limit_reset(1000), 1120);
        let denied = Fetched {
            status: 403,
            headers: vec![("x-ratelimit-remaining".into(), "42".into())],
            body: Vec::new(),
        };
        assert!(!denied.limited());
    }

    #[test]
    fn archives_are_named_as_published() {
        assert_eq!(
            archive_name("v0.2.0-alpha.4", "windows-x86_64"),
            "backdown-v0.2.0-alpha.4-windows-x86_64.zip"
        );
        assert_eq!(
            archive_name("v0.2.0-alpha.4", "macos-universal"),
            "backdown-v0.2.0-alpha.4-macos-universal.tar.gz"
        );
    }

    #[test]
    fn package_managed_binaries_are_left_alone() {
        if std::env::var_os("APPIMAGE").is_some() {
            return;
        }
        assert!(
            not_replaceable(Path::new("/usr/bin/backdown"))
                .unwrap()
                .contains("package manager")
        );
        assert!(
            not_replaceable(Path::new("/opt/homebrew/Cellar/backdown/1/bin/backdown")).is_some()
        );
        assert!(not_replaceable(Path::new("/home/someone/.local/bin/backdown")).is_none());
    }

    #[test]
    fn a_zip_entry_comes_out_whole() {
        let dir = tempfile::tempdir().unwrap();
        let z = dir.path().join("a.zip");
        {
            let f = std::fs::File::create(&z).unwrap();
            let mut w = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            w.start_file("README.txt", opts).unwrap();
            std::io::Write::write_all(&mut w, b"read me").unwrap();
            w.start_file("backdown.exe", opts).unwrap();
            std::io::Write::write_all(&mut w, &b"MZ program ".repeat(1000)).unwrap();
            w.finish().unwrap();
        }
        let out = dir.path().join("out.exe");
        unzip_one(&z, "backdown.exe", &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"MZ program ".repeat(1000));
        assert!(unzip_one(&z, "missing.exe", &out).is_err());
    }
}
