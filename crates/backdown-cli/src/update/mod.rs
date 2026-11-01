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
//! checked, no ~/.curlrc), checks SHA256SUMS.txt's signature (SHA256SUMS.txt.sig, made with the
//! release key, RELEASE_KEY), refuses unless the archive's SHA-256 matches, unpacks the binary beside
//! the running one, checks it runs and reports the expected version, keeps a copy of the old one
//! in the data folder, and swaps them with a rename (Windows can't overwrite a running .exe, but it
//! can rename it out of the way). If the installed binary then fails `--version`, the old one is put
//! back. Last, the per-user daemon is restarted so it runs the new version. `--rollback` puts the
//! kept copy back later. Nothing here ever runs as a different user.
//!
//! A signature that doesn't check is always refused. Releases before v1.0.0-beta.4 weren't signed,
//! and are installed on their checksums alone; from then on, a release without its signature isn't.
//!
//! `--from FOLDER` does the same for a machine without a network, from a folder carried over (the
//! archive, SHA256SUMS.txt and SHA256SUMS.txt.sig): the signature is required (or --allow-unsigned),
//! then the checksum, then a newer version (or --allow-older), then the same install. It makes no
//! network request at all. With `--check` it only verifies the folder, on any system.
//!
//! BACKDOWN_UPDATE_URL points it at another API root (the tests' local fake server; plain http is
//! allowed for 127.0.0.1 and localhost only). In debug builds (the tests'),
//! BACKDOWN_TEST_RELEASE_PUBKEY replaces the release key with a throwaway one; release builds trust
//! only their own.

use std::cmp::Ordering;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::settings;

mod background;
mod check;
mod install;
mod offline;

pub use background::*;
pub use check::*;
pub use install::*;
pub use offline::*;

pub const RELEASES_API: &str = "https://api.github.com/repos/BuenGenio/backdown-releases";
const DAY: i64 = 86_400;
/// after a failed check (offline, GitHub down), don't ask again for this long
const RETRY_AFTER_FAILURE: i64 = 6 * 3600;
const SUMS: &str = "SHA256SUMS.txt";
const SIG: &str = "SHA256SUMS.txt.sig";

/// BackDown's release-signing public key. scripts/sign-release.mjs signs each release's
/// SHA256SUMS.txt with its private half (which stays with whoever publishes releases) into
/// SHA256SUMS.txt.sig, a base64url Ed25519 signature over the file's exact bytes.
pub const RELEASE_KEY: [u8; 32] =
    crate::license::hex32("b88db195d0c4084e5ae25f07fbf2f046939021deca58dce2d4e2591875d9e2a8");
/// The first release published with SHA256SUMS.txt.sig: earlier ones are installed on their
/// checksums alone, this one and later ones never without a signature that checks.
const FIRST_SIGNED: &str = "1.0.0-beta.4";
const UNSIGNED: &str = "this release isn't signed (releases before v1.0.0-beta.4 weren't)";

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

/// Every platform releases have an archive for.
const PLATFORMS: [&str; 4] = [
    "linux-x86_64",
    "linux-arm64",
    "macos-universal",
    "windows-x86_64",
];

fn archive_ext(platform: &str) -> &'static str {
    if platform.starts_with("windows") {
        "zip"
    } else {
        "tar.gz"
    }
}

pub fn archive_name(tag: &str, platform: &str) -> String {
    format!("backdown-{tag}-{platform}.{}", archive_ext(platform))
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

/// The files SHA256SUMS.txt lists, with their hashes (sha256sum's format: "<hex>  <name>", or
/// " *<name>"). Only plain file names: nothing in another folder.
fn listed(sums: &str) -> Vec<(String, String)> {
    sums.lines()
        .filter_map(|l| {
            let (hash, file) = l.trim().split_once(char::is_whitespace)?;
            let file = file.trim_start().trim_start_matches('*');
            let plain = !file.is_empty() && !file.contains(['/', '\\']) && file != "..";
            (plain && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
                .then(|| (file.to_string(), hash.to_ascii_lowercase()))
        })
        .collect()
}

/// The hash SHA256SUMS.txt gives for `name`.
pub fn expected_sum(sums: &str, name: &str) -> Option<String> {
    listed(sums)
        .into_iter()
        .find_map(|(file, hash)| (file == name).then_some(hash))
}

// ---------- signatures

/// The key releases are checked against, and what to call it. Debug builds (the tests') take a
/// throwaway one from BACKDOWN_TEST_RELEASE_PUBKEY (64 hex digits) if it's set; release builds
/// trust RELEASE_KEY alone, whatever the environment says.
fn release_key() -> ([u8; 32], &'static str) {
    #[cfg(debug_assertions)]
    if let Ok(h) = std::env::var("BACKDOWN_TEST_RELEASE_PUBKEY") {
        let h = h.trim();
        let mut k = [0u8; 32];
        if h.len() == 64 && h.bytes().all(|c| c.is_ascii_hexdigit()) {
            for (i, b) in k.iter_mut().enumerate() {
                *b = u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap_or_default();
            }
            return (k, "the test key in BACKDOWN_TEST_RELEASE_PUBKEY");
        }
    }
    (RELEASE_KEY, "BackDown's release key")
}

/// Check SHA256SUMS.txt.sig (`sig`: a base64url Ed25519 signature, padding and surrounding
/// whitespace allowed) over the exact bytes of SHA256SUMS.txt (`sums`), against `key`.
pub fn verify_sums(sums: &[u8], sig: &str, key: &[u8; 32]) -> Result<()> {
    let sig = URL_SAFE_NO_PAD
        .decode(sig.trim().trim_end_matches('='))
        .ok()
        .and_then(|b| Signature::from_slice(&b).ok())
        .with_context(|| format!("{SIG} is damaged (it isn't a base64url Ed25519 signature)"))?;
    let vk = VerifyingKey::from_bytes(key).context("the release key isn't an Ed25519 key")?;
    vk.verify_strict(sums, &sig)
        .map_err(|_| anyhow::anyhow!("{SIG} doesn't match {SUMS}"))
}

/// SHA256SUMS.txt's signature, checked against the release key: who signed it, or why it doesn't
/// count.
fn check_signature(sums: &[u8], sig: &[u8]) -> Result<&'static str> {
    let (key, who) = release_key();
    match verify_sums(sums, &String::from_utf8_lossy(sig), &key) {
        Ok(()) => Ok(who),
        Err(e) => bail!(
            "{e}: these files weren't signed with {who}, or they were changed after they were signed"
        ),
    }
}

/// Whether a release must come with SHA256SUMS.txt.sig: every one since the first signed one.
fn needs_signature(v: &Version) -> bool {
    *v >= Version::parse(FIRST_SIGNED).expect("FIRST_SIGNED is semver")
}

#[cfg(test)]
mod tests;
