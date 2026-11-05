//! A drive's folder that no longer holds the drive: `backdown index` refuses to record it, saying
//! why and what to do, whether it works on the index itself or through the daemon; the daemon
//! shows the reason in its job and leaves the drive alone after it; and an explicit
//! `--allow-empty` still records a drive that really was emptied.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use backdown_core::Db;
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

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A test folder with its own home, data and runtime folders, so nothing here reaches this
/// user's own BackDown (its index, its daemon).
struct Place {
    dir: tempfile::TempDir,
    /// the control socket's path must be short (a Unix socket's limit): a folder in /tmp
    run: tempfile::TempDir,
}

impl Place {
    fn new(prefix: &str) -> Place {
        let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        let dir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(&base)
            .unwrap();
        for sub in ["data/backdown", "home", "config"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        // no drives to read the health of
        std::fs::write(
            dir.path().join("readings.json"),
            r#"{"drives": [], "notes": []}"#,
        )
        .unwrap();
        Place {
            dir,
            run: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn db(&self) -> PathBuf {
        self.path().join("index.db")
    }

    fn backdown(&self) -> Command {
        let d = self.path();
        let mut c = Command::new(env!("CARGO_BIN_EXE_backdown"));
        c.arg("--db")
            .arg(self.db())
            .env("XDG_RUNTIME_DIR", self.run.path())
            .env("XDG_DATA_HOME", d.join("data"))
            .env("XDG_CONFIG_HOME", d.join("config"))
            .env("HOME", d.join("home"))
            .env("BACKDOWN_FAKE_HEALTH", d.join("readings.json"))
            .env_remove("NOTIFY_SOCKET")
            .env_remove("BACKDOWN_PRETEND_RELEASE");
        c
    }

    /// A drive with two files, then what's left in its place when it isn't mounted: folders made
    /// there, with no files in them.
    fn drive(&self) -> PathBuf {
        let drive = self.path().join("media/BIG");
        write(&drive.join("Archive/2019/a.jpg"), &[7u8; 3000]);
        write(&drive.join("Archive/2019/b.jpg"), &[8u8; 3000]);
        std::fs::canonicalize(&drive).unwrap()
    }
}

fn unmounted(drive: &Path) {
    std::fs::remove_dir_all(drive.join("Archive")).unwrap();
    for sub in ["etc", "data", "certs"] {
        std::fs::create_dir_all(drive.join("Archive/_stalwart").join(sub)).unwrap();
    }
}

fn current_files(db: &Path) -> i64 {
    Db::open_read_only(db)
        .unwrap()
        .conn
        .query_row("SELECT COUNT(*) FROM files WHERE current = 1", [], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn backdown_index_refuses_a_drive_folder_without_its_drive() {
    let p = Place::new("scan-guards-cli");
    let drive = p.drive();
    let index = |extra: &[&str]| {
        p.backdown()
            .args(["--local", "index"])
            .args(extra)
            .arg(&drive)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let out = index(&[]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(current_files(&p.db()), 2);

    unmounted(&drive);
    let out = index(&[]);
    assert!(!out.status.success(), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("holds no files at all") && err.contains("--allow-empty"),
        "{err}"
    );
    assert_eq!(current_files(&p.db()), 2, "nothing was recorded as deleted");

    // the drive really was emptied
    let out = index(&["--allow-empty"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(current_files(&p.db()), 0);
    // the flag for a new drive in its place is there too
    let help = p.backdown().args(["index", "--help"]).output().unwrap();
    assert!(String::from_utf8_lossy(&help.stdout).contains("--allow-other-drive"));
}

#[test]
fn the_daemon_says_why_a_scan_was_refused_and_leaves_the_drive_alone() {
    let p = Place::new("scan-guards-daemon");
    let drive = p.drive();
    let child = p
        .backdown()
        .args(["daemon", "--port", "0", "--rescan-every", "never"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _daemon = Daemon(child);
    let sock = p.run.path().join("backdown/daemon.sock");
    wait_for("the daemon", || sock.exists());

    let scan = |extra: Value| -> Value {
        let mut req =
            json!({ "cmd": "enqueue", "kind": "index", "path": drive.display().to_string() });
        if let (Some(r), Some(e)) = (req.as_object_mut(), extra.as_object()) {
            r.extend(e.clone());
        }
        let r = ask(&sock, &req);
        assert_eq!(r["ok"], true, "{r}");
        let id = r["job"].as_u64().unwrap();
        let mut job = Value::Null;
        wait_for("the scan", || {
            job = ask(&sock, &json!({ "cmd": "job", "job": id }))["job"].clone();
            job["state"] == "done" || job["state"] == "failed"
        });
        job
    };
    let job = scan(json!({}));
    assert_eq!(job["state"], "done", "{job}");

    unmounted(&drive);
    let job = scan(json!({}));
    assert_eq!(job["state"], "failed", "{job}");
    let error = job["error"].as_str().unwrap();
    assert!(
        error.starts_with(&format!("{} holds no files at all", drive.display())),
        "the reason, plainly: {error}"
    );
    assert_eq!(current_files(&p.db()), 2, "nothing was recorded as deleted");

    // the scheduler leaves it alone, and says why
    let mine = |st: &Value| -> Value {
        st["daemon"]["drives"]
            .as_array()
            .and_then(|ds| {
                ds.iter()
                    .find(|d| d["path"] == drive.display().to_string())
                    .cloned()
            })
            .unwrap_or_default()
    };
    wait_for("the drive to count as away", || {
        let d = mine(&ask(&sock, &json!({ "cmd": "status", "index": false })));
        d["state"] == "away"
    });
    let d = mine(&ask(&sock, &json!({ "cmd": "status", "index": false })));
    assert_eq!(d["present"], false);
    assert!(
        d["why_away"]
            .as_str()
            .is_some_and(|w| w.contains("holds no files at all")),
        "{d}"
    );

    // asked for explicitly, it's recorded as emptied
    let job = scan(json!({ "allow_empty": true }));
    assert_eq!(job["state"], "done", "{job}");
    assert_eq!(current_files(&p.db()), 0);
}
