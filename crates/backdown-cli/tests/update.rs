//! `backdown update` against a local fake of GitHub's releases API and downloads: a newer release
//! installs; a download that doesn't match SHA256SUMS.txt is refused; a new binary that fails
//! `--version` once installed is rolled back; answers are cached (and asked again with their ETag);
//! GitHub's rate limit is respected. Nothing here touches the real network or the real services:
//! BACKDOWN_UPDATE_URL points at the fake, HOME and the XDG folders at a temporary one, and a fake
//! `systemctl` that always fails comes first on PATH.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

const CURRENT: &str = env!("CARGO_PKG_VERSION");
const NEW: &str = "9.9.9";

fn platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-arm64",
        ("macos", _) => "macos-universal",
        other => panic!("no release builds for {other:?}"),
    }
}

fn archive_name() -> String {
    format!("backdown-v{NEW}-{}.tar.gz", platform())
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

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[derive(Clone, Default)]
struct Script {
    /// what SHA256SUMS.txt claims for the archive (default: the truth)
    sums_hash: Option<String>,
    /// answer the releases list with GitHub's rate limit
    limited: bool,
}

struct Fake {
    port: u16,
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
    let archive = tar_gz(program);
    let sums = format!(
        "{}  {}\n0000000000000000000000000000000000000000000000000000000000000000  backdown-v{NEW}-windows-x86_64.zip\n",
        script
            .sums_hash
            .clone()
            .unwrap_or_else(|| hex(&Sha256::digest(&archive))),
        archive_name()
    );
    let base = format!("http://127.0.0.1:{port}");
    let releases = serde_json::json!([
        { "tag_name": format!("v{NEW}"), "prerelease": false, "draft": false,
          "html_url": format!("{base}/releases/tag/v{NEW}"), "published_at": "2026-10-01T10:00:00Z",
          "assets": [
            { "name": archive_name(), "browser_download_url": format!("{base}/download/{}", archive_name()), "size": archive.len() },
            { "name": "SHA256SUMS.txt", "browser_download_url": format!("{base}/download/SHA256SUMS.txt"), "size": sums.len() }
          ] },
        { "tag_name": format!("v{CURRENT}"), "prerelease": true, "draft": false, "html_url": "", "assets": [] }
    ])
    .to_string();
    let lists = Arc::new(AtomicUsize::new(0));
    let etag_hits = Arc::new(AtomicUsize::new(0));
    let (s, l, e) = (
        Arc::clone(&server),
        Arc::clone(&lists),
        Arc::clone(&etag_hits),
    );
    let served = Mutex::new(());
    std::thread::spawn(move || {
        for req in s.incoming_requests() {
            let _g = served.lock().unwrap();
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
            } else if url == format!("/download/{}", archive_name()) {
                tiny_http::Response::from_data(archive.clone())
            } else if url == "/download/SHA256SUMS.txt" {
                tiny_http::Response::from_string(sums.clone())
            } else {
                tiny_http::Response::from_string("not found").with_status_code(404)
            };
            let _ = req.respond(resp);
        }
    });
    Fake {
        port,
        lists,
        etag_hits,
        server,
    }
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
    assert!(p.data().join("update.json").is_file());
    // nothing was installed by checking
    assert_eq!(p.installed(), p.original);
}

#[test]
fn update_installs_the_verified_release_and_keeps_the_old_one() {
    let fake = fake_github(NEW_PROGRAM, Script::default());
    let p = place();
    let o = p.run(&fake, &["update"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
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
fn a_download_that_does_not_match_its_checksum_is_refused() {
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
    assert_eq!(p.installed(), p.original, "the binary is untouched");
    assert!(
        !p.data().join("previous").exists(),
        "nothing was replaced, so nothing was kept"
    );
    assert!(!p.root.join("bin/.backdown-update").exists());
}

#[test]
fn a_new_binary_that_fails_once_installed_is_rolled_back() {
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
