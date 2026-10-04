//! `backdown update` against a local fake of GitHub's releases API and downloads: a newer, signed
//! release installs; one whose signature doesn't check, or that isn't signed although it should
//! be, is refused; a download that doesn't match SHA256SUMS.txt is refused; a new binary that fails
//! `--version` once installed is rolled back; answers are cached (and asked again with their ETag);
//! GitHub's rate limit is respected. And `update --from FOLDER`, from a folder of carried files,
//! without any network: it installs a signed release, refuses an unsigned one unless
//! --allow-unsigned, refuses an older one unless --allow-older, and with --check changes nothing.
//!
//! Nothing here touches the real network or the real services: BACKDOWN_UPDATE_URL points at the
//! fake, HOME and the XDG folders at a temporary one, and a fake `systemctl` that always fails comes
//! first on PATH. Releases are signed with a throwaway key made here, which debug builds take from
//! BACKDOWN_TEST_RELEASE_PUBKEY; release builds trust only BackDown's own release key, so under
//! `cargo test --release` the cases that need a good signature say so and skip.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

const CURRENT: &str = env!("CARGO_PKG_VERSION");
const NEW: &str = "9.9.9";
const SUMS: &str = "SHA256SUMS.txt";
const SIG: &str = "SHA256SUMS.txt.sig";

/// The binary under test trusts the test key (a debug build).
const SIGNS: bool = cfg!(debug_assertions);

macro_rules! needs_test_key {
    () => {
        if !SIGNS {
            eprintln!(
                "skipped: a release build trusts only BackDown's release key, not this test's"
            );
            return;
        }
    };
}

/// The throwaway key this test signs releases with.
fn test_key() -> SigningKey {
    SigningKey::from_bytes(&[0x5a; 32])
}

/// Another throwaway key: what an impostor would sign with.
fn other_key() -> SigningKey {
    SigningKey::from_bytes(&[0xa5; 32])
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn public_hex(k: &SigningKey) -> String {
    hex(k.verifying_key().as_bytes())
}

/// SHA256SUMS.txt.sig as sign-release.mjs writes it.
fn signature(k: &SigningKey, sums: &[u8]) -> String {
    format!("{}\n", URL_SAFE_NO_PAD.encode(k.sign(sums).to_bytes()))
}

fn platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-arm64",
        ("macos", _) => "macos-universal",
        other => panic!("no release builds for {other:?}"),
    }
}

fn archive_name(version: &str) -> String {
    format!("backdown-v{version}-{}.tar.gz", platform())
}

/// A release archive laid out like build-local.sh's: ./backdown, ./README.txt
fn tar_gz(program: &str) -> Vec<u8> {
    let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (name, data, mode) in [
        ("./README.txt", "read me".as_bytes(), 0o644),
        ("./backdown", program.as_bytes(), 0o755),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(mode);
        h.set_cksum();
        b.append_data(&mut h, name, data).unwrap();
    }
    b.into_inner().unwrap().finish().unwrap()
}

/// A stand-in for a release's binary that says it's `version`.
fn program(version: &str) -> String {
    format!("#!/bin/sh\necho 'backdown {version}'\n")
}

/// How a release's SHA256SUMS.txt is signed.
#[derive(Clone, Copy, Default, PartialEq)]
enum Sig {
    /// with the test key
    #[default]
    Good,
    /// not at all: no SHA256SUMS.txt.sig
    Missing,
    /// with another key
    Impostor,
    /// with the test key, but SHA256SUMS.txt was changed after it was signed
    SumsChanged,
}

/// A release's SHA256SUMS.txt (with a line for another platform's archive, as real ones have) and
/// its signature, if it has one.
fn sums_and_sig(version: &str, archive_hash: &str, sig: Sig) -> (String, Option<String>) {
    let sums = format!(
        "{archive_hash}  {}\n{}  backdown-v{version}-windows-x86_64.zip\n",
        archive_name(version),
        "0".repeat(64),
    );
    let signed = match sig {
        Sig::Missing => None,
        Sig::Good | Sig::SumsChanged => Some(signature(&test_key(), sums.as_bytes())),
        Sig::Impostor => Some(signature(&other_key(), sums.as_bytes())),
    };
    let sums = if sig == Sig::SumsChanged {
        format!("{sums}{}  extra.bin\n", "1".repeat(64))
    } else {
        sums
    };
    (sums, signed)
}

#[derive(Clone, Default)]
struct Script {
    /// the release's version (default: NEW)
    version: Option<String>,
    /// what SHA256SUMS.txt claims for the archive (default: the truth)
    sums_hash: Option<String>,
    sig: Sig,
    /// answer the releases list with GitHub's rate limit
    limited: bool,
}

struct Fake {
    port: u16,
    /// every request, whatever it asked for
    hits: Arc<AtomicUsize>,
    lists: Arc<AtomicUsize>,
    etag_hits: Arc<AtomicUsize>,
    server: Arc<tiny_http::Server>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

/// GitHub, as far as `backdown update` sees it.
fn fake_github(program: &str, script: Script) -> Fake {
    let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
    let port = server.server_addr().to_ip().unwrap().port();
    let version = script.version.clone().unwrap_or_else(|| NEW.to_string());
    let archive = tar_gz(program);
    let (sums, sig) = sums_and_sig(
        &version,
        &script
            .sums_hash
            .clone()
            .unwrap_or_else(|| hex(&Sha256::digest(&archive))),
        script.sig,
    );
    let base = format!("http://127.0.0.1:{port}");
    let mut assets = vec![
        serde_json::json!({ "name": archive_name(&version), "browser_download_url": format!("{base}/download/{}", archive_name(&version)), "size": archive.len() }),
        serde_json::json!({ "name": SUMS, "browser_download_url": format!("{base}/download/{SUMS}"), "size": sums.len() }),
    ];
    if let Some(s) = &sig {
        assets.push(serde_json::json!({ "name": SIG, "browser_download_url": format!("{base}/download/{SIG}"), "size": s.len() }));
    }
    let releases = serde_json::json!([
        { "tag_name": format!("v{version}"), "prerelease": false, "draft": false,
          "html_url": format!("{base}/releases/tag/v{version}"), "published_at": "2026-10-01T10:00:00Z",
          "assets": assets },
        { "tag_name": format!("v{CURRENT}"), "prerelease": true, "draft": false, "html_url": "", "assets": [] }
    ])
    .to_string();
    let hits = Arc::new(AtomicUsize::new(0));
    let lists = Arc::new(AtomicUsize::new(0));
    let etag_hits = Arc::new(AtomicUsize::new(0));
    let (s, h, l, e) = (
        Arc::clone(&server),
        Arc::clone(&hits),
        Arc::clone(&lists),
        Arc::clone(&etag_hits),
    );
    let served = Mutex::new(());
    let archive_path = format!("/download/{}", archive_name(&version));
    std::thread::spawn(move || {
        for req in s.incoming_requests() {
            let _g = served.lock().unwrap();
            h.fetch_add(1, Ordering::SeqCst);
            let url = req.url().to_string();
            let hdr = |k: &str, v: &str| {
                tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
            };
            let resp = if url.starts_with("/repos/BuenGenio/backdown-releases/releases?") {
                l.fetch_add(1, Ordering::SeqCst);
                let etag = "\"v1-releases\"";
                let conditional = req
                    .headers()
                    .iter()
                    .any(|h| h.field.equiv("If-None-Match") && h.value.as_str() == etag);
                if script.limited {
                    tiny_http::Response::from_string(r#"{"message":"API rate limit exceeded"}"#)
                        .with_status_code(403)
                        .with_header(hdr("X-RateLimit-Remaining", "0"))
                        .with_header(hdr("X-RateLimit-Reset", "4102444800"))
                } else if conditional {
                    e.fetch_add(1, Ordering::SeqCst);
                    tiny_http::Response::from_string("")
                        .with_status_code(304)
                        .with_header(hdr("ETag", etag))
                } else {
                    tiny_http::Response::from_string(releases.clone())
                        .with_header(hdr("ETag", etag))
                }
            } else if url == archive_path {
                tiny_http::Response::from_data(archive.clone())
            } else if url == format!("/download/{SUMS}") {
                tiny_http::Response::from_string(sums.clone())
            } else if url == format!("/download/{SIG}") && sig.is_some() {
                tiny_http::Response::from_string(sig.clone().unwrap_or_default())
            } else {
                tiny_http::Response::from_string("not found").with_status_code(404)
            };
            let _ = req.respond(resp);
        }
    });
    Fake {
        port,
        hits,
        lists,
        etag_hits,
        server,
    }
}

/// A folder of carried files, as `update --from` takes it: this platform's archive of `version`,
/// SHA256SUMS.txt and (unless `sig` is Missing) SHA256SUMS.txt.sig.
fn bundle(dir: &Path, version: &str, program: &str, sig: Sig) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let archive = tar_gz(program);
    std::fs::write(dir.join(archive_name(version)), &archive).unwrap();
    let (sums, signed) = sums_and_sig(version, &hex(&Sha256::digest(&archive)), sig);
    std::fs::write(dir.join(SUMS), sums).unwrap();
    if let Some(s) = signed {
        std::fs::write(dir.join(SIG), s).unwrap();
    }
    dir.to_path_buf()
}

/// A folder holding `files`, with a SHA256SUMS.txt for them signed with the test key.
fn signed_folder(dir: &Path, files: &[(&str, &[u8])]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let mut sums = String::new();
    for (name, data) in files {
        std::fs::write(dir.join(name), data).unwrap();
        sums.push_str(&format!("{}  {name}\n", hex(&Sha256::digest(data))));
    }
    std::fs::write(dir.join(SIG), signature(&test_key(), sums.as_bytes())).unwrap();
    std::fs::write(dir.join(SUMS), sums).unwrap();
    dir.to_path_buf()
}

/// What's in a folder: names and contents.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap_or_default(),
            )
        })
        .collect();
    v.sort();
    v
}

/// A folder with `bin/backdown` (a copy of the built binary: what gets replaced), a home, and a
/// fake systemctl.
struct Place {
    _dir: tempfile::TempDir,
    root: PathBuf,
    exe: PathBuf,
    original: Vec<u8>,
}

fn place() -> Place {
    // short: the daemon's socket path lives under it
    let dir = tempfile::Builder::new()
        .prefix("bdup")
        .tempdir_in("/tmp")
        .unwrap();
    let root = dir.path().to_path_buf();
    for d in ["bin", "home", "run", "fakebin"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::set_permissions(root.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let exe = root.join("bin/backdown");
    std::fs::copy(env!("CARGO_BIN_EXE_backdown"), &exe).unwrap();
    let systemctl = root.join("fakebin/systemctl");
    std::fs::write(&systemctl, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let original = std::fs::read(&exe).unwrap();
    Place {
        _dir: dir,
        root,
        exe,
        original,
    }
}

impl Place {
    fn run(&self, fake: &Fake, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut c = Command::new(&self.exe);
        c.args(args)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("fakebin").display()),
            )
            .env("HOME", self.root.join("home"))
            .env("XDG_DATA_HOME", self.root.join("home/.local/share"))
            .env("XDG_CONFIG_HOME", self.root.join("home/.config"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("BACKDOWN_TEST_RELEASE_PUBKEY", public_hex(&test_key()))
            .env(
                "BACKDOWN_UPDATE_URL",
                format!(
                    "http://127.0.0.1:{}/repos/BuenGenio/backdown-releases",
                    fake.port
                ),
            );
        for (k, v) in extra {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn data(&self) -> PathBuf {
        self.root.join("home/.local/share/backdown")
    }

    fn installed(&self) -> Vec<u8> {
        std::fs::read(&self.exe).unwrap()
    }

    /// A `curl` first on PATH that leaves a mark and fails: proof nothing went to the network.
    fn no_curl(&self) -> PathBuf {
        let mark = self.root.join("curl-ran");
        let curl = self.root.join("fakebin/curl");
        std::fs::write(
            &curl,
            format!("#!/bin/sh\n: > '{}'\nexit 7\n", mark.display()),
        )
        .unwrap();
        std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).unwrap();
        mark
    }

    /// Nothing was installed and nothing kept: the binary is the one we started with.
    fn unchanged(&self) {
        assert_eq!(self.installed(), self.original, "the binary is untouched");
        assert!(
            !self.data().join("previous").exists(),
            "nothing was replaced, so nothing was kept"
        );
        assert!(!self.root.join("bin/.backdown-update").exists());
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// A stand-in for the new release's binary: says it's 9.9.9. With BD_TEST_MARK set, it works once
/// and then fails, like a build that runs from the download folder but not once installed.
const NEW_PROGRAM: &str = "#!/bin/sh\n\
    if [ -n \"$BD_TEST_MARK\" ]; then\n  if [ -e \"$BD_TEST_MARK\" ]; then echo 'broken' >&2; exit 1; fi\n  : > \"$BD_TEST_MARK\"\nfi\n\
    echo 'backdown 9.9.9'\n";

#[test]
fn check_reports_a_newer_release_and_caches_the_answer() {
    let fake = fake_github(NEW_PROGRAM, Script::default());
    let p = place();
    let o = p.run(&fake, &["update", "--check"], &[]);
    assert_eq!(o.status.code(), Some(10), "{}", text(&o));
    let out = text(&o);
    assert!(
        out.contains(&format!("BackDown {NEW} is available (this is {CURRENT})")),
        "{out}"
    );
    assert!(out.contains(&format!("/releases/tag/v{NEW}")), "{out}");
    assert_eq!(fake.lists.load(Ordering::SeqCst), 1);
    // today's answer is kept: asking again doesn't ask GitHub
    let again = p.run(&fake, &["update", "--check"], &[]);
    assert_eq!(again.status.code(), Some(10));
    assert_eq!(
        fake.lists.load(Ordering::SeqCst),
        1,
        "answered from the cache"
    );
    // --refresh asks, with the ETag, which GitHub answers 304 without counting it
    let fresh = p.run(&fake, &["update", "--check", "--refresh", "--json"], &[]);
    assert_eq!(fake.lists.load(Ordering::SeqCst), 2);
    assert_eq!(fake.etag_hits.load(Ordering::SeqCst), 1);
    let v: serde_json::Value = serde_json::from_slice(&fresh.stdout).unwrap();
    assert_eq!(v["newer"], true);
    assert_eq!(v["latest"]["version"], NEW);
    assert_eq!(v["latest"]["signed"], true);
    assert!(p.data().join("update.json").is_file());
    // nothing was installed by checking
    assert_eq!(p.installed(), p.original);
}

#[test]
fn update_installs_the_verified_release_and_keeps_the_old_one() {
    needs_test_key!();
    let fake = fake_github(NEW_PROGRAM, Script::default());
    let p = place();
    let o = p.run(&fake, &["update"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        out.contains("SHA256SUMS.txt is signed with the test key"),
        "{out}"
    );
    assert!(out.contains("SHA-256 matches"), "{out}");
    assert!(
        out.contains(&format!("Updated BackDown {CURRENT} → {NEW}")),
        "{out}"
    );
    assert_eq!(String::from_utf8(p.installed()).unwrap(), NEW_PROGRAM);
    let v = Command::new(&p.exe).arg("--version").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&v.stdout).trim(),
        format!("backdown {NEW}")
    );
    // the replaced binary is kept, for --rollback
    let kept = p.data().join(format!("previous/backdown-{CURRENT}"));
    assert_eq!(std::fs::read(&kept).unwrap(), p.original);
    assert!(
        !p.root.join("bin/.backdown-update").exists(),
        "the download folder is cleaned up"
    );
}

#[test]
fn a_release_whose_signature_does_not_check_is_refused() {
    needs_test_key!();
    for (sig, why) in [
        (Sig::Impostor, "signed by another key"),
        (
            Sig::SumsChanged,
            "SHA256SUMS.txt changed after it was signed",
        ),
    ] {
        let fake = fake_github(
            NEW_PROGRAM,
            Script {
                sig,
                ..Script::default()
            },
        );
        let p = place();
        let o = p.run(&fake, &["update"], &[]);
        let out = text(&o);
        assert!(!o.status.success(), "{why}: {out}");
        assert!(
            out.contains("SHA256SUMS.txt.sig doesn't match SHA256SUMS.txt"),
            "{why}: {out}"
        );
        assert!(out.contains("weren't signed with the test key"), "{out}");
        assert!(out.contains("nothing was changed"), "{out}");
        p.unchanged();
    }
}

#[test]
fn a_release_since_signing_began_without_a_signature_is_refused() {
    let fake = fake_github(
        NEW_PROGRAM,
        Script {
            sig: Sig::Missing,
            ..Script::default()
        },
    );
    let p = place();
    let o = p.run(&fake, &["update"], &[]);
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.contains("has no SHA256SUMS.txt.sig, but every release since v1.0.0-beta.4 is signed"),
        "{out}"
    );
    p.unchanged();
    // and nothing was downloaded to find out
    assert_eq!(fake.hits.load(Ordering::SeqCst), 1, "only the list");
}

/// 1.0.0-beta.3.1 sorts after 1.0.0-beta.3 and before 1.0.0-beta.4, the first signed release: a
/// build from before beta.4 can be offered an unsigned release newer than itself, a later one can't.
fn unsigned_era_version() -> Option<String> {
    let (core, pre) = CURRENT.split_once('-')?;
    let n: u64 = pre.strip_prefix("beta.")?.split('.').next()?.parse().ok()?;
    (core == "1.0.0" && n < 4).then(|| format!("{CURRENT}.1"))
}

#[test]
fn an_unsigned_release_from_before_signing_installs_with_a_warning() {
    let Some(version) = unsigned_era_version() else {
        eprintln!("skipped: {CURRENT} is newer than every unsigned release");
        return;
    };
    let fake = fake_github(
        &program(&version),
        Script {
            version: Some(version.clone()),
            sig: Sig::Missing,
            ..Script::default()
        },
    );
    let p = place();
    let o = p.run(&fake, &["update"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        out.contains("this release isn't signed (releases before v1.0.0-beta.4 weren't)"),
        "{out}"
    );
    assert!(out.contains("SHA-256 matches"), "{out}");
    assert!(
        out.contains(&format!("Updated BackDown {CURRENT} → {version}")),
        "{out}"
    );
    assert_eq!(String::from_utf8(p.installed()).unwrap(), program(&version));
}

#[test]
fn a_download_that_does_not_match_its_checksum_is_refused() {
    needs_test_key!();
    let fake = fake_github(
        NEW_PROGRAM,
        Script {
            sums_hash: Some("ab".repeat(32)),
            ..Script::default()
        },
    );
    let p = place();
    let o = p.run(&fake, &["update"], &[]);
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(out.contains("doesn't match SHA256SUMS.txt"), "{out}");
    assert!(out.contains("nothing was changed"), "{out}");
    p.unchanged();
}

#[test]
fn a_new_binary_that_fails_once_installed_is_rolled_back() {
    needs_test_key!();
    let fake = fake_github(NEW_PROGRAM, Script::default());
    let p = place();
    let mark = p.root.join("ran-once");
    let o = p.run(
        &fake,
        &["update"],
        &[("BD_TEST_MARK", mark.to_str().unwrap())],
    );
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(out.contains("rolled back"), "{out}");
    assert!(
        mark.exists(),
        "the new binary passed its check before it was installed"
    );
    assert_eq!(p.installed(), p.original, "the previous binary is back");
    let v = Command::new(&p.exe).arg("--version").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&v.stdout).trim(),
        format!("backdown {CURRENT}")
    );
}

#[test]
fn a_program_reporting_the_wrong_version_is_not_installed() {
    needs_test_key!();
    let fake = fake_github("#!/bin/sh\necho 'backdown 1.0.0'\n", Script::default());
    let p = place();
    let o = p.run(&fake, &["update"], &[]);
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(out.contains("not \"backdown 9.9.9\""), "{out}");
    assert_eq!(p.installed(), p.original);
}

#[test]
fn githubs_rate_limit_is_respected() {
    let fake = fake_github(
        NEW_PROGRAM,
        Script {
            limited: true,
            ..Script::default()
        },
    );
    let p = place();
    let o = p.run(&fake, &["update", "--check"], &[]);
    let out = text(&o);
    assert!(out.contains("limit"), "{out}");
    assert_eq!(fake.lists.load(Ordering::SeqCst), 1);
    // not asked again, even with --refresh, until GitHub's reset time
    let _ = p.run(&fake, &["update", "--check", "--refresh"], &[]);
    assert_eq!(fake.lists.load(Ordering::SeqCst), 1);
    let cache: serde_json::Value =
        serde_json::from_slice(&std::fs::read(p.data().join("update.json")).unwrap()).unwrap();
    assert_eq!(cache["blocked_until"], 4_102_444_800i64);
    assert_eq!(p.installed(), p.original);
}

#[test]
fn downloads_are_https_only_beyond_this_machine() {
    let fake = fake_github(NEW_PROGRAM, Script::default());
    let p = place();
    let mut c = Command::new(&p.exe);
    let o = c
        .args(["update", "--check"])
        .env("HOME", p.root.join("home"))
        .env("XDG_DATA_HOME", p.root.join("home/.local/share"))
        .env("XDG_RUNTIME_DIR", p.root.join("run"))
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", p.root.join("fakebin").display()),
        )
        .env("BACKDOWN_UPDATE_URL", "http://example.com/repos/x/y")
        .output()
        .unwrap();
    let out = text(&o);
    assert!(out.contains("only https"), "{out}");
    assert_eq!(fake.lists.load(Ordering::SeqCst), 0);
}

// ---------- update --from FOLDER

#[test]
fn update_from_a_folder_installs_the_signed_release_without_any_network() {
    needs_test_key!();
    let fake = fake_github(NEW_PROGRAM, Script::default());
    let p = place();
    let curl = p.no_curl();
    let usb = bundle(&p.root.join("usb"), NEW, NEW_PROGRAM, Sig::Good);
    let o = p.run(&fake, &["update", "--from", usb.to_str().unwrap()], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        out.contains("SHA256SUMS.txt is signed with the test key"),
        "{out}"
    );
    assert!(
        out.contains(&format!("SHA-256 of {} matches", archive_name(NEW))),
        "{out}"
    );
    assert!(
        out.contains(&format!("Updated BackDown {CURRENT} → {NEW} (from ")),
        "{out}"
    );
    assert_eq!(String::from_utf8(p.installed()).unwrap(), NEW_PROGRAM);
    let kept = p.data().join(format!("previous/backdown-{CURRENT}"));
    assert_eq!(std::fs::read(&kept).unwrap(), p.original);
    assert!(!p.root.join("bin/.backdown-update").exists());
    // no network: curl never ran, the fake GitHub was never asked, nothing was cached
    assert!(!curl.exists(), "curl ran");
    assert_eq!(fake.hits.load(Ordering::SeqCst), 0);
    assert!(!p.data().join("update.json").exists());
    // kept for --rollback, like an online update's
    let rec: serde_json::Value =
        serde_json::from_slice(&std::fs::read(p.data().join("previous.json")).unwrap()).unwrap();
    assert_eq!(rec["version"], CURRENT);
}

#[test]
fn update_from_needs_a_signature_unless_allow_unsigned() {
    let fake = fake_github(NEW_PROGRAM, Script::default());
    let p = place();
    let curl = p.no_curl();
    let usb = bundle(&p.root.join("usb"), NEW, NEW_PROGRAM, Sig::Missing);
    let o = p.run(&fake, &["update", "--from", usb.to_str().unwrap()], &[]);
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(out.contains("has no SHA256SUMS.txt.sig"), "{out}");
    assert!(out.contains("--allow-unsigned"), "{out}");
    p.unchanged();
    // allowed, it says what that means, then checks the checksum and installs
    let o = p.run(
        &fake,
        &[
            "update",
            "--from",
            usb.to_str().unwrap(),
            "--allow-unsigned",
        ],
        &[],
    );
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        out.contains(
            "warning: there's no SHA256SUMS.txt.sig, so nothing shows these files are BackDown's"
        ),
        "{out}"
    );
    assert!(out.contains("shows only that they arrived whole"), "{out}");
    assert!(
        out.contains(&format!("SHA-256 of {} matches", archive_name(NEW))),
        "{out}"
    );
    assert_eq!(String::from_utf8(p.installed()).unwrap(), NEW_PROGRAM);
    assert!(!curl.exists(), "curl ran");
    assert_eq!(fake.hits.load(Ordering::SeqCst), 0);
}

#[test]
fn update_from_refuses_a_signature_that_does_not_check_even_with_allow_unsigned() {
    let fake = fake_github(NEW_PROGRAM, Script::default());
    let p = place();
    for (n, sig) in [Sig::Impostor, Sig::SumsChanged].into_iter().enumerate() {
        let usb = bundle(&p.root.join(format!("usb{n}")), NEW, NEW_PROGRAM, sig);
        for extra in [&[][..], &["--allow-unsigned"][..]] {
            let mut args = vec!["update", "--from", usb.to_str().unwrap()];
            args.extend_from_slice(extra);
            let o = p.run(&fake, &args, &[]);
            let out = text(&o);
            assert!(!o.status.success(), "{out}");
            assert!(
                out.contains("SHA256SUMS.txt.sig doesn't match SHA256SUMS.txt"),
                "{out}"
            );
            assert!(out.contains("Nothing was installed"), "{out}");
            p.unchanged();
        }
    }
    // a damaged signature file is refused too
    let usb = bundle(&p.root.join("usb-damaged"), NEW, NEW_PROGRAM, Sig::Good);
    std::fs::write(usb.join(SIG), "this is not a signature\n").unwrap();
    let o = p.run(&fake, &["update", "--from", usb.to_str().unwrap()], &[]);
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(out.contains("SHA256SUMS.txt.sig is damaged"), "{out}");
    p.unchanged();
}

#[test]
fn update_from_refuses_an_archive_that_does_not_match_its_checksum() {
    needs_test_key!();
    let fake = fake_github(NEW_PROGRAM, Script::default());
    let p = place();
    let usb = bundle(&p.root.join("usb"), NEW, NEW_PROGRAM, Sig::Good);
    // the archive swapped for another after SHA256SUMS.txt was signed
    std::fs::write(usb.join(archive_name(NEW)), tar_gz(&program("9.9.9 evil"))).unwrap();
    for check in [false, true] {
        let mut args = vec!["update", "--from", usb.to_str().unwrap()];
        if check {
            args.push("--check");
        }
        let o = p.run(&fake, &args, &[]);
        let out = text(&o);
        assert!(!o.status.success(), "{out}");
        assert!(
            out.contains(&format!(
                "{} doesn't match SHA256SUMS.txt",
                archive_name(NEW)
            )),
            "{out}"
        );
        p.unchanged();
    }
}

#[test]
fn update_from_refuses_an_older_version_unless_allow_older() {
    needs_test_key!();
    let fake = fake_github(NEW_PROGRAM, Script::default());
    let p = place();
    let usb = bundle(&p.root.join("usb"), "0.1.0", &program("0.1.0"), Sig::Good);
    let o = p.run(&fake, &["update", "--from", usb.to_str().unwrap()], &[]);
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.contains(&format!(
            "BackDown 0.1.0 isn't newer than this one ({CURRENT}): nothing was changed"
        )),
        "{out}"
    );
    assert!(out.contains("--allow-older"), "{out}");
    p.unchanged();
    // the same version isn't newer either
    let same = bundle(&p.root.join("same"), CURRENT, &program(CURRENT), Sig::Good);
    let o = p.run(&fake, &["update", "--from", same.to_str().unwrap()], &[]);
    assert!(!o.status.success(), "{}", text(&o));
    p.unchanged();
    let o = p.run(
        &fake,
        &["update", "--from", usb.to_str().unwrap(), "--allow-older"],
        &[],
    );
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        out.contains(&format!("Updated BackDown {CURRENT} → 0.1.0")),
        "{out}"
    );
    assert_eq!(String::from_utf8(p.installed()).unwrap(), program("0.1.0"));
    assert_eq!(fake.hits.load(Ordering::SeqCst), 0);
}

#[test]
fn update_from_check_only_verifies_and_changes_nothing() {
    needs_test_key!();
    let fake = fake_github(NEW_PROGRAM, Script::default());
    let p = place();
    let curl = p.no_curl();
    let usb = bundle(&p.root.join("usb"), NEW, NEW_PROGRAM, Sig::Good);
    std::fs::write(usb.join("notes.txt"), "carried by hand").unwrap();
    let before = snapshot(&usb);
    let o = p.run(
        &fake,
        &["update", "--from", usb.to_str().unwrap(), "--check"],
        &[],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(out.contains(&format!("ok  {}", archive_name(NEW))), "{out}");
    assert!(
        out.contains("1 file in")
            && out.contains("matches SHA256SUMS.txt, which is signed with the test key"),
        "{out}"
    );
    assert!(
        out.contains("not in SHA256SUMS.txt, so not checked: notes.txt"),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "BackDown {NEW} is newer than this one ({CURRENT})"
        )),
        "{out}"
    );
    assert!(out.contains("Nothing was changed."), "{out}");
    // nothing installed, kept, staged, cached or fetched; the folder as it was
    p.unchanged();
    assert_eq!(snapshot(&usb), before);
    assert!(!p.data().join("update.json").exists());
    assert!(!curl.exists(), "curl ran");
    assert_eq!(fake.hits.load(Ordering::SeqCst), 0);
    // a bundle for another system checks out here too, though it can't update this one
    let zip = format!("backdown-v{NEW}-windows-x86_64.zip");
    let other = signed_folder(&p.root.join("other"), &[(&zip, b"PK not really a zip")]);
    let o = p.run(
        &fake,
        &["update", "--from", other.to_str().unwrap(), "--check"],
        &[],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(out.contains(&format!("ok  {zip}")), "{out}");
    assert!(
        out.contains(&format!("there's no {} in the folder", archive_name(NEW))),
        "{out}"
    );
    // and one that has none of the files its SHA256SUMS.txt lists is refused
    std::fs::remove_file(other.join(&zip)).unwrap();
    let o = p.run(
        &fake,
        &["update", "--from", other.to_str().unwrap(), "--check"],
        &[],
    );
    assert!(!o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("none of the files SHA256SUMS.txt lists"));
    p.unchanged();
}

#[test]
fn update_from_is_part_of_forensic_in_a_release() {
    let fake = fake_github(NEW_PROGRAM, Script::default());
    let p = place();
    let usb = bundle(&p.root.join("usb"), NEW, NEW_PROGRAM, Sig::Good);
    // installing from it is the Forensic feature (refused before any signature is checked)
    let o = p.run(
        &fake,
        &["update", "--from", usb.to_str().unwrap()],
        &[("BACKDOWN_PRETEND_RELEASE", "1")],
    );
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.contains(
            "`backdown update --from` (Air-gapped deployment) is part of BackDown Forensic"
        ),
        "{out}"
    );
    p.unchanged();
    // checking a carried bundle is free, like checking any download
    needs_test_key!();
    let o = p.run(
        &fake,
        &["update", "--from", usb.to_str().unwrap(), "--check"],
        &[("BACKDOWN_PRETEND_RELEASE", "1")],
    );
    assert!(o.status.success(), "{}", text(&o));
    p.unchanged();
}

#[test]
fn update_from_options_need_from_and_exclude_the_online_ones() {
    let fake = fake_github(NEW_PROGRAM, Script::default());
    let p = place();
    for args in [
        &["update", "--allow-unsigned"][..],
        &["update", "--allow-older"][..],
        &["update", "--from", "/nowhere", "--rollback"][..],
        &["update", "--from", "/nowhere", "--channel", "stable"][..],
    ] {
        let o = p.run(&fake, args, &[]);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", text(&o));
    }
    let o = p.run(&fake, &["update", "--from", "/nowhere/at/all"], &[]);
    assert!(
        text(&o).contains("can't open /nowhere/at/all"),
        "{}",
        text(&o)
    );
    assert_eq!(fake.hits.load(Ordering::SeqCst), 0);
    p.unchanged();
}

/// What scripts/sign-release.mjs signs, `update --from` accepts: the same bytes, the same encoding.
#[test]
fn a_bundle_signed_by_sign_release_mjs_checks_out() {
    needs_test_key!();
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipped: no node here to run scripts/sign-release.mjs");
        return;
    }
    let fake = fake_github(NEW_PROGRAM, Script::default());
    let p = place();
    let usb = bundle(&p.root.join("usb"), NEW, NEW_PROGRAM, Sig::Missing);
    // the test key as a PKCS#8 PEM file, the form sign-release.mjs reads
    let mut der = vec![
        0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04,
        0x20,
    ];
    der.extend_from_slice(&test_key().to_bytes());
    let pem = p.root.join("throwaway-release-key.pem");
    std::fs::write(
        &pem,
        format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            STANDARD.encode(&der)
        ),
    )
    .unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/sign-release.mjs");
    let o = Command::new("node")
        .arg(&script)
        .arg(usb.join(SUMS))
        .env("BACKDOWN_RELEASE_KEY", &pem)
        .env("BACKDOWN_TEST_RELEASE_PUBKEY", public_hex(&test_key()))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o));
    assert!(usb.join(SIG).is_file());
    let o = p.run(
        &fake,
        &["update", "--from", usb.to_str().unwrap(), "--check"],
        &[],
    );
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(out.contains("signed with the test key"), "{out}");
}
