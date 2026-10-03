//! Scheduled attestations: with `attest` set to after-scan, the daemon signs a manifest of a drive
//! once its scan and hashing finish, chains the next one to it, names the job in Activity, and
//! offers both in the browser to download.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use backdown_core::manifest;
use serde_json::{Value, json};

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(60);
    while !f() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn ask(sock: &Path, req: &Value) -> Value {
    let s = UnixStream::connect(sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut w = s.try_clone().unwrap();
    writeln!(w, "{req}").unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

/// One HTTP request to the daemon's web UI, with its session cookie: the status line, headers and
/// body.
fn http_raw(port: u64, token: &str, method: &str, path: &str) -> (String, String) {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port as u16)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: bd={token}\r\nX-BackDown: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap();
    // tiny_http may send the body chunked
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut b = String::new();
        let mut rest = body;
        while let Some((size, tail)) = rest.split_once("\r\n") {
            let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
            if n == 0 {
                break;
            }
            b.push_str(&tail[..n]);
            rest = &tail[n + 2..];
        }
        b
    } else {
        body.to_string()
    };
    (head.to_string(), body)
}

fn http(port: u64, token: &str, method: &str, path: &str) -> Value {
    let (head, body) = http_raw(port, token, method, path);
    assert!(
        head.starts_with("HTTP/1.1 200"),
        "{method} {path}: {head}\n{body}"
    );
    serde_json::from_str(&body).unwrap()
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn the_daemon_signs_a_manifest_after_each_scan() {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let dir = tempfile::Builder::new()
        .prefix("attest-daemon")
        .tempdir_in(&base)
        .unwrap();
    let d = dir.path();
    for sub in ["data/backdown", "home"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
    }
    // the control socket's path must be short (a Unix socket's limit): an empty folder in /tmp
    let run = tempfile::tempdir().unwrap();
    std::fs::write(
        d.join("data/backdown/settings.json"),
        json!({ "attest": "after-scan" }).to_string(),
    )
    .unwrap();
    // no drives to read the health of
    let readings = d.join("readings.json");
    std::fs::write(&readings, r#"{"drives": [], "notes": []}"#).unwrap();
    let drive = d.join("old-drive");
    write(&drive.join("docs/report.txt"), b"the first draft");
    write(&drive.join("docs/notes.txt"), b"same size here!");
    write(&drive.join("photos/beach.jpg"), &[7u8; 5000]);

    let child = Command::new(env!("CARGO_BIN_EXE_backdown"))
        .args(["--db"])
        .arg(d.join("index.db"))
        .args(["daemon", "--port", "0", "--rescan-every", "never"])
        .env("XDG_RUNTIME_DIR", run.path())
        .env("XDG_DATA_HOME", d.join("data"))
        .env("HOME", d.join("home"))
        .env("BACKDOWN_FAKE_HEALTH", &readings)
        .env_remove("NOTIFY_SOCKET")
        .env_remove("BACKDOWN_PRETEND_RELEASE")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _daemon = Daemon(child);
    let sock = run.path().join("backdown/daemon.sock");
    let ui = d.join("data/backdown/ui.json");
    wait_for("the daemon", || sock.exists() && ui.exists());
    let ui: Value = serde_json::from_slice(&std::fs::read(&ui).unwrap()).unwrap();
    let (port, token) = (ui["port"].as_u64().unwrap(), ui["token"].as_str().unwrap());
    // the Settings view says so, and has no key to show before the first manifest
    let s = http(port, token, "GET", "/api/settings");
    assert_eq!(s["settings"]["attest"], "after-scan");
    assert_eq!(s["signing_key"]["fingerprint"], Value::Null);

    let scan = |what: &str| {
        let canon = std::fs::canonicalize(&drive).unwrap();
        let r = ask(
            &sock,
            &json!({ "cmd": "enqueue", "kind": "index", "path": canon.display().to_string() }),
        );
        assert_eq!(r["ok"], true, "{what}: {r}");
    };
    let manifests = || -> Vec<Value> {
        http(port, token, "GET", "/api/manifests")["manifests"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };

    scan("first scan");
    wait_for("the first manifest", || manifests().len() == 1);
    let first = manifests()[0].clone();
    assert_eq!(first["files"], 3);
    let (head, body) = http_raw(
        port,
        token,
        "GET",
        &format!("/api/manifest?id={}", first["id"]),
    );
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(
        head.to_ascii_lowercase()
            .contains("content-disposition: attachment"),
        "{head}"
    );
    let doc: Value = serde_json::from_str(&body).unwrap();
    let c = manifest::check(&doc).unwrap();
    assert_eq!(c.files, 3);
    assert_eq!(doc["prev"], Value::Null);
    // signed with the key in the daemon's data folder, made for it
    let key = d.join("data/backdown/signing.key");
    assert!(key.exists());
    let s = http(port, token, "GET", "/api/settings");
    assert_eq!(s["signing_key"]["public_key"], json!(c.key));

    // Activity names the job and says what it did
    wait_for("the attest job to be listed", || {
        ask(&sock, &json!({ "cmd": "status", "index": false }))["daemon"]["recent"]
            .as_array()
            .is_some_and(|r| r.iter().any(|j| j["kind"] == "attest"))
    });
    let st = ask(&sock, &json!({ "cmd": "status", "index": false }));
    let job = st["daemon"]["recent"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["kind"] == "attest")
        .unwrap()
        .clone();
    assert_eq!(job["state"], "done", "{job}");
    assert_eq!(job["reason"], "after-scan");
    assert!(
        job["summary"].as_str().unwrap().contains("3 files"),
        "{job}"
    );

    // a new scan: a new manifest, chained to the first
    write(&drive.join("photos/sunset.jpg"), b"new photo bytes");
    scan("second scan");
    wait_for("the second manifest", || manifests().len() == 2);
    let newest = manifests()[0].clone();
    assert_eq!(newest["files"], 4);
    let (_, body) = http_raw(
        port,
        token,
        "GET",
        &format!("/api/manifest?id={}", newest["id"]),
    );
    let second: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(second["prev"].as_str().unwrap(), manifest::chain_hash(&doc));
    assert!(manifest::check_chain(&[doc, second]).is_empty());
    // one drive's manifests only, when asked
    let root = first["root_id"].as_i64().unwrap();
    let one = http(port, token, "GET", &format!("/api/manifests?root={root}"));
    assert_eq!(one["manifests"].as_array().unwrap().len(), 2);
    let none = http(
        port,
        token,
        "GET",
        &format!("/api/manifests?root={}", root + 1),
    );
    assert_eq!(none["manifests"].as_array().unwrap().len(), 0);

    // turned off from Settings: no more
    let s = http(port, token, "POST", "/api/settings?attest=off");
    assert_eq!(s["settings"]["attest"], "off");
    let (head, _) = http_raw(port, token, "POST", "/api/settings?attest=hourly");
    assert!(head.starts_with("HTTP/1.1 400"), "{head}");
}
