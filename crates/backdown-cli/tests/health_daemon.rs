//! The daemon reading drive health from a sequence of (fake) readings: it keeps them, notices the
//! drive getting worse, records a notification, shows it on the "desktop" (a fake notify-send
//! that writes down what it was asked to show), and serves it all to the browser.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(30);
    while !f() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn readings(file: &Path, reallocated: u64, pending: u64) {
    let v = json!({ "drives": [
        { "device": "/dev/sdx", "name": "sdx", "model": "FAKEDISK HD4000", "serial": "FAKE-0001",
          "transport": "sata", "rotational": true, "protocol": "ata", "reported": "ok",
          "temperature": 36, "power_on_hours": 20000, "reallocated": reallocated, "pending": pending,
          "uncorrectable": 0, "crc_errors": 0, "mounts": ["/media/someone/Mirror"] },
        { "device": "/dev/nvme9n1", "name": "nvme9n1", "model": "Fakedisk NV970 1TB",
          "serial": "S0FAKE0NV970123", "rotational": false, "protocol": "nvme",
          "critical_warning": 0, "available_spare": 100, "spare_threshold": 10,
          "percentage_used": 3, "media_errors": 0, "temperature": 35 }
    ], "notes": [] });
    std::fs::write(file, serde_json::to_vec(&v).unwrap()).unwrap();
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

/// One HTTP request to the daemon's web UI, with its session cookie.
fn http(port: u64, token: &str, method: &str, path: &str) -> Value {
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
    assert!(
        head.starts_with("HTTP/1.1 200"),
        "{method} {path}: {head}\n{body}"
    );
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
    serde_json::from_str(&body).unwrap()
}

#[test]
fn a_drive_getting_worse_is_kept_told_and_shown() {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let dir = tempfile::Builder::new()
        .prefix("health-daemon")
        .tempdir_in(&base)
        .unwrap();
    let d = dir.path();
    for sub in ["data", "home", "bin"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
    }
    // the control socket's path must be short (a Unix socket's limit): an empty folder in /tmp
    let run = tempfile::tempdir().unwrap();
    // the "desktop": writes down each notification it's asked to show
    let shown = d.join("shown.txt");
    let fake = d.join("bin/notify-send");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\t' \"$a\"; done >> '{}'\necho >> '{}'\n",
            shown.display(),
            shown.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let file = d.join("readings.json");
    readings(&file, 0, 0);

    let path = format!(
        "{}:{}",
        d.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let child = Command::new(env!("CARGO_BIN_EXE_backdown"))
        .args(["--db"])
        .arg(d.join("index.db"))
        .args(["daemon", "--port", "0", "--rescan-every", "never"])
        .env("XDG_RUNTIME_DIR", run.path())
        .env("XDG_DATA_HOME", d.join("data"))
        .env("HOME", d.join("home"))
        .env("PATH", path)
        .env("BACKDOWN_FAKE_HEALTH", &file)
        .env_remove("NOTIFY_SOCKET")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _daemon = Daemon(child);
    let sock = run.path().join("backdown/daemon.sock");
    let ui = d.join("data/backdown/ui.json");
    wait_for("the daemon", || sock.exists() && ui.exists());

    // first reading: both fine, nothing to tell
    let r = ask(&sock, &json!({ "cmd": "health" }));
    assert_eq!(r["ok"], true, "{r}");
    let drives = r["health"]["drives"].as_array().unwrap();
    assert_eq!(drives.len(), 2);
    assert!(drives.iter().all(|x| x["verdict"] == "ok"), "{r}");

    // the disk starts losing sectors
    readings(&file, 6, 2);
    let r = ask(&sock, &json!({ "cmd": "health" }));
    let hdd = r["health"]["drives"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == "sn:FAKE0001")
        .unwrap()
        .clone();
    assert_eq!(hdd["verdict"], "warning", "{hdd}");
    assert_eq!(hdd["readings"], 2);
    assert_eq!(hdd["trend"], json!(["+6 reallocated", "+2 pending"]));
    let codes: Vec<&str> = hdd["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["verdict", "reallocated", "pending"]);

    // told once, on the desktop
    wait_for("the notification", || {
        std::fs::read_to_string(&shown).is_ok_and(|s| s.lines().count() >= 1)
    });
    let s = std::fs::read_to_string(&shown).unwrap();
    assert!(s.contains("--app-name=BackDown"), "{s}");
    assert!(s.contains("FAKEDISK HD4000 needs watching"), "{s}");
    assert!(s.contains("6 more sectors have gone bad"), "{s}");

    // the same again: kept quiet
    let r = ask(&sock, &json!({ "cmd": "health" }));
    assert_eq!(r["ok"], true);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(std::fs::read_to_string(&shown).unwrap().lines().count(), 1);

    // the browser sees the same, and can mark it read
    let ui: Value = serde_json::from_slice(&std::fs::read(&ui).unwrap()).unwrap();
    let (port, token) = (ui["port"].as_u64().unwrap(), ui["token"].as_str().unwrap());
    let h = http(port, token, "GET", "/api/health");
    assert_eq!(h["drives"][0]["verdict"], "warning");
    let n = http(port, token, "GET", "/api/notifications");
    assert_eq!(n["unread"], 1);
    assert_eq!(n["notes"][0]["kind"], "health");
    assert_eq!(n["notes"][0]["drive"], "sn:FAKE0001");
    assert_eq!(n["notes"][0]["shown"], true);
    let status = http(port, token, "GET", "/api/daemon");
    assert_eq!(status["health"]["unread"], 1);
    assert_eq!(status["health"]["worst"], "warning");
    let m = http(port, token, "POST", "/api/notifications?read=all");
    assert_eq!(m["unread"], 0);
    // notifications can be turned off from the browser (a Settings view hooks here)
    let s = http(port, token, "POST", "/api/notify-settings?desktop=0");
    assert_eq!(s["desktop"], false);
    assert_eq!(s["health"], true);
    assert_eq!(
        http(port, token, "GET", "/api/notify-settings")["desktop"],
        false
    );

    // worse still, with desktop pop-ups off: listed, not shown
    readings(&file, 6, 14);
    let r = ask(&sock, &json!({ "cmd": "health" }));
    assert_eq!(r["health"]["drives"][0]["verdict"], "failing");
    let n = http(port, token, "GET", "/api/notifications");
    assert_eq!(n["unread"], 1);
    assert_eq!(n["notes"][0]["level"], "failing");
    assert_eq!(n["notes"][0]["shown"], false);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(std::fs::read_to_string(&shown).unwrap().lines().count(), 1);
}

/// A stand-in for OpenSSH: skip the options and the host, then be the other machine's shell.
const FAKE_SSH: &str = r#"#!/bin/sh
while [ $# -gt 0 ]; do
  case "$1" in
    -o|-p|-l) shift 2 ;;
    --) shift; break ;;
    *) shift ;;
  esac
done
shift
exec sh -c "$1"
"#;

#[test]
fn a_drive_on_another_machine_getting_worse_is_told_by_the_service() {
    use std::os::unix::fs::PermissionsExt;
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let dir = tempfile::Builder::new()
        .prefix("health-machines")
        .tempdir_in(&base)
        .unwrap();
    let d = dir.path();
    for sub in ["data", "home", "bin"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
    }
    let run = tempfile::tempdir().unwrap();
    let shown = d.join("shown.txt");
    let fake = d.join("bin/notify-send");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\t' \"$a\"; done >> '{}'\necho >> '{}'\n",
            shown.display(),
            shown.display()
        ),
    )
    .unwrap();
    std::fs::write(d.join("bin/ssh"), FAKE_SSH).unwrap();
    for f in ["notify-send", "ssh"] {
        std::fs::set_permissions(
            d.join("bin").join(f),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_backdown"), d.join("bin/backdown")).unwrap();
    let path = format!(
        "{}:{}",
        d.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let backdown = |db: &Path, args: &[&str], readings: Option<&Path>| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_backdown"));
        c.arg("--db")
            .arg(db)
            .args(args)
            .env("XDG_RUNTIME_DIR", run.path())
            .env("XDG_DATA_HOME", d.join("data"))
            .env("HOME", d.join("home"))
            .env("PATH", &path)
            .env_remove("BACKDOWN_PRETEND_RELEASE");
        if let Some(r) = readings {
            c.env("BACKDOWN_FAKE_HEALTH", r);
        }
        c.output().unwrap()
    };
    // this machine's own drives are fine; the NAS's are read into its own index
    let here = d.join("here.json");
    readings(&here, 0, 0);
    let there = d.join("there.json");
    let nas_db = d.join("nas.db");
    readings(&there, 0, 0);
    assert!(
        backdown(&nas_db, &["--local", "health"], Some(&there))
            .status
            .success()
    );

    let db = d.join("index.db");
    let child = Command::new(env!("CARGO_BIN_EXE_backdown"))
        .args(["--db"])
        .arg(&db)
        .args(["daemon", "--port", "0", "--rescan-every", "never"])
        .env("XDG_RUNTIME_DIR", run.path())
        .env("XDG_DATA_HOME", d.join("data"))
        .env("HOME", d.join("home"))
        .env("PATH", &path)
        .env("BACKDOWN_FAKE_HEALTH", &here)
        .env_remove("BACKDOWN_PRETEND_RELEASE")
        .env_remove("NOTIFY_SOCKET")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _daemon = Daemon(child);
    let sock = run.path().join("backdown/daemon.sock");
    let ui = d.join("data/backdown/ui.json");
    wait_for("the daemon", || sock.exists() && ui.exists());

    let nas = nas_db.to_str().unwrap();
    let o = backdown(
        &db,
        &[
            "remote",
            "add",
            "nas",
            "ssh://nas.example.org",
            "--remote-db",
            nas,
        ],
        None,
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // synced by the service: the first sight of its drives tells nothing
    let o = backdown(&db, &["remote", "sync", "nas"], None);
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(
        o.status.success() && out.contains("drive health of 2 drives"),
        "{out}"
    );
    // a drive there fails; the next sync tells, on the desktop too
    readings(&there, 150, 0);
    backdown(&nas_db, &["--local", "health"], Some(&there));
    let o = backdown(&db, &["remote", "sync", "nas"], None);
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(out.contains("1 worse since the last sync"), "{out}");
    wait_for("the notification", || {
        std::fs::read_to_string(&shown).is_ok_and(|s| s.contains("A drive on nas is failing"))
    });
    let s = std::fs::read_to_string(&shown).unwrap();
    assert!(
        s.contains("FAKEDISK HD4000 (S/N FAKE-0001): 150 more sectors"),
        "{s}"
    );
    assert_eq!(s.lines().count(), 1, "{s}");

    // the browser: the machine's drives, and the notification linked to its drive there
    let ui: Value = serde_json::from_slice(&std::fs::read(&ui).unwrap()).unwrap();
    let (port, token) = (ui["port"].as_u64().unwrap(), ui["token"].as_str().unwrap());
    let hm = http(port, token, "GET", "/api/health-machines");
    assert_eq!(hm["worst"], "failing", "{hm}");
    let m = &hm["machines"][0];
    assert_eq!(m["name"], "nas");
    assert!(m["last_sync"].is_i64());
    assert_eq!(m["drives"][0]["verdict"], "failing");
    assert_eq!(m["drives"][0]["id"], "sn:FAKE0001");
    assert_eq!(m["drives"][1]["verdict"], "ok");
    let n = http(port, token, "GET", "/api/notifications");
    assert_eq!(n["notes"][0]["title"], "A drive on nas is failing");
    assert_eq!(n["notes"][0]["drive"], "@nas:sn:FAKE0001");
    assert_eq!(n["notes"][0]["shown"], true);
    assert_eq!(
        http(port, token, "GET", "/api/daemon")["health"]["unread"],
        1
    );
    // and a sync with nothing new tells nothing more
    backdown(&db, &["remote", "sync", "nas"], None);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(std::fs::read_to_string(&shown).unwrap().lines().count(), 1);
    assert_eq!(http(port, token, "GET", "/api/notifications")["unread"], 1);
}
