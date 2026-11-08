//! `backdown app` and `backdown desktop`, in a temporary home: the app finds (or starts) the daemon
//! and opens it in a fake browser that records its arguments and holds its profile lock like
//! Chromium, without ever opening a window; a second launch sees the open window; the desktop
//! files land where the desktop looks. A fake `systemctl` that always fails keeps the real
//! services out of it.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Home {
    _dir: tempfile::TempDir,
    root: PathBuf,
    daemons: Vec<Child>,
}

impl Drop for Home {
    fn drop(&mut self) {
        for d in &mut self.daemons {
            let _ = d.kill();
            let _ = d.wait();
        }
        // a daemon `backdown app` started itself
        if let Ok(pid) = std::fs::read_to_string(self.root.join("daemon.pid")) {
            let _ = Command::new("kill").arg(pid.trim()).status();
        }
    }
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn home() -> Home {
    // short: the daemon's socket goes in run/
    let dir = tempfile::Builder::new()
        .prefix("bdapp")
        .tempdir_in("/tmp")
        .unwrap();
    let root = dir.path().to_path_buf();
    for d in ["home", "run", "bin"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::set_permissions(root.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
    script(&root.join("bin/systemctl"), "#!/bin/sh\nexit 1\n");
    // Chrome, as far as BackDown can tell: it records its arguments, takes the profile lock the
    // way Chromium does (a symlink "host-pid"), and stays up a while
    // records its arguments to browser.NNNN (a sequence, so launches stay in order without the
    // GNU-only `date +%s%N`), takes the profile lock the way Chromium does, and stays up a while.
    // The in-progress file is "browser-writing.*", which the launches() filter ignores.
    script(
        &root.join("bin/google-chrome"),
        "#!/bin/sh\ntmp=\"$FAKE_LOG-writing.$$\"\nfor a in \"$@\"; do echo \"$a\"; done > \"$tmp\"\n\
         n=$(ls -1 \"$FAKE_LOG\".* 2>/dev/null | grep -cE '\\.[0-9]+$')\n\
         mv \"$tmp\" \"$(printf '%s.%04d' \"$FAKE_LOG\" \"$n\")\"\n\
         for a in \"$@\"; do case $a in --user-data-dir=*) d=${a#--user-data-dir=}; mkdir -p \"$d\"; ln -sfn \"testhost-$$\" \"$d/SingletonLock\";; esac; done\n\
         sleep 20\n",
    );
    // the default-browser fallback: xdg-open on Linux, `open` on macOS — fake both
    for opener in ["bin/xdg-open", "bin/open"] {
        script(
            &root.join(opener),
            "#!/bin/sh\necho \"$1\" > \"$FAKE_LOG.opened\"\n",
        );
    }
    Home {
        _dir: dir,
        root,
        daemons: Vec::new(),
    }
}

impl Home {
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_backdown"));
        c.args(args)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("HOME", self.root.join("home"))
            .env("XDG_DATA_HOME", self.root.join("home/.local/share"))
            .env("XDG_CONFIG_HOME", self.root.join("home/.config"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("BACKDOWN_NO_UPDATE_CHECK", "1")
            .env("BACKDOWN_PORT", "0")
            .env("FAKE_LOG", self.root.join("browser"));
        if !cfg!(target_os = "linux") {
            // elsewhere browsers are found in /Applications or Program Files, not on PATH
            c.env("BACKDOWN_BROWSER", self.root.join("bin/google-chrome"));
        }
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn data(&self) -> PathBuf {
        // where the daemon keeps its state, mirroring locations::state_dir()
        let home = self.root.join("home");
        if cfg!(target_os = "macos") {
            home.join("Library/Application Support/BackDown")
        } else {
            home.join(".local/share/backdown")
        }
    }

    /// Each browser launch's arguments, oldest first (waits for `n` of them).
    fn launches(&self, n: usize) -> Vec<Vec<String>> {
        let start = Instant::now();
        loop {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&self.root)
                .unwrap()
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    let n = p.file_name().unwrap().to_string_lossy().to_string();
                    n.starts_with("browser.") && n[8..].chars().all(|c| c.is_ascii_digit())
                })
                .collect();
            files.sort();
            if files.len() >= n || start.elapsed() > Duration::from_secs(10) {
                return files
                    .iter()
                    .map(|f| {
                        std::fs::read_to_string(f)
                            .unwrap()
                            .lines()
                            .map(String::from)
                            .collect()
                    })
                    .collect();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn app_opens_the_daemon_in_an_app_window_and_finds_it_again() {
    let mut h = home();
    let d = h
        .cmd(&["daemon"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    h.daemons.push(d);
    let ui = h.data().join("ui.json");
    let start = Instant::now();
    while !ui.exists() && start.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let port =
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&ui).unwrap()).unwrap()["port"]
            .as_u64()
            .unwrap();
    let token = serde_json::from_slice::<serde_json::Value>(&std::fs::read(&ui).unwrap()).unwrap()
        ["token"]
        .as_str()
        .unwrap()
        .to_string();

    let o = h.run(&["app"]);
    assert!(o.status.success(), "{}", text(&o));
    let first = &h.launches(1)[0];
    let url = first[0].strip_prefix("--app=").expect("app mode");
    assert!(
        url.starts_with(&format!("http://127.0.0.1:{port}/?c=")),
        "{url}"
    );
    assert!(
        !first.iter().any(|a| a.contains(&token)),
        "the token never goes on a command line"
    );
    let profile = h.data().join("app-window");
    assert!(
        first.contains(&format!("--user-data-dir={}", profile.display())),
        "{first:?}"
    );
    assert!(first.contains(&"--no-first-run".to_string()));
    assert!(
        first.contains(&"--window-size=1280,860".to_string()),
        "a first size, the first time"
    );
    if cfg!(target_os = "linux") {
        assert!(first.contains(&"--class=BackDown".to_string()));
    }

    // the code opens the UI once: a cookie and a clean address…
    let code_url = url
        .strip_prefix(&format!("http://127.0.0.1:{port}"))
        .unwrap();
    let (status, head) = get(port as u16, code_url);
    assert_eq!(status, 303, "{head}");
    assert!(head.contains(&format!("bd={token}")), "{head}");
    // …and never again
    assert_eq!(get(port as u16, code_url).0, 401);

    // launched again while the window is open: no new daemon, and the open window is found
    let again = h.run(&["app"]);
    let out = text(&again);
    assert!(again.status.success(), "{out}");
    assert!(out.contains("open already"), "{out}");
    let second = &h.launches(2)[1];
    assert!(
        !second.iter().any(|a| a.starts_with("--window-size")),
        "Chromium keeps the size now: {second:?}"
    );

    // --no-window only makes sure the daemon runs
    let bg = h.run(&["app", "--no-window"]);
    assert!(
        text(&bg).contains(&format!("127.0.0.1:{port}")),
        "{}",
        text(&bg)
    );

    // `backdown open` goes through a one-time code too
    let o = h.run(&["open"]);
    assert!(o.status.success(), "{}", text(&o));
    let opened = std::fs::read_to_string(h.root.join("browser.opened")).unwrap();
    assert!(
        opened
            .trim()
            .starts_with(&format!("http://127.0.0.1:{port}/?c=")),
        "{opened}"
    );
}

#[test]
fn app_starts_a_daemon_when_none_runs() {
    let h = home();
    let o = h.run(&["app", "--no-window"]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(out.contains("started the BackDown daemon"), "{out}");
    let log = std::fs::read_to_string(h.data().join("daemon.log")).unwrap();
    assert!(log.contains("daemon: index"), "{log}");
    // remember it, to stop it when the test ends
    let ui: serde_json::Value =
        serde_json::from_slice(&std::fs::read(h.data().join("ui.json")).unwrap()).unwrap();
    let pid = daemon_pid(
        ui["port"].as_u64().unwrap() as u16,
        ui["token"].as_str().unwrap(),
    );
    std::fs::write(h.root.join("daemon.pid"), pid.to_string()).unwrap();
    // a second launch finds it
    let again = h.run(&["app", "--no-window"]);
    assert!(!text(&again).contains("started"), "{}", text(&again));
}

#[test]
fn a_browser_that_fails_at_once_falls_back_to_the_default_browser() {
    let h = home();
    // a Chrome with no display to open on
    script(
        &h.root.join("bin/google-chrome"),
        "#!/bin/sh\necho 'cannot open display' >&2\nexit 1\n",
    );
    let o = h.run(&["app"]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        out.contains("opening your default browser instead"),
        "{out}"
    );
    let ui: serde_json::Value =
        serde_json::from_slice(&std::fs::read(h.data().join("ui.json")).unwrap()).unwrap();
    let port = ui["port"].as_u64().unwrap() as u16;
    std::fs::write(
        h.root.join("daemon.pid"),
        daemon_pid(port, ui["token"].as_str().unwrap()).to_string(),
    )
    .unwrap();
    let opened = std::fs::read_to_string(h.root.join("browser.opened")).unwrap();
    let path = opened
        .trim()
        .strip_prefix(&format!("http://127.0.0.1:{port}"))
        .unwrap();
    assert!(path.starts_with("/?c="), "{opened}");
    // the code the failed browser never used still opens the UI
    assert_eq!(get(port, path).0, 303);
}

fn get(port: u16, path: &str) -> (u16, String) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, out)
}

fn daemon_pid(port: u16, token: &str) -> u64 {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(s, "GET /api/daemon HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: bd={token}\r\nConnection: close\r\n\r\n").unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let body = &out[out.find("\r\n\r\n").unwrap() + 4..];
    serde_json::from_str::<serde_json::Value>(body).unwrap()["pid"]
        .as_u64()
        .unwrap()
}

#[cfg(target_os = "linux")]
#[test]
fn desktop_install_and_uninstall_for_this_user() {
    let h = home();
    let o = h.run(&["desktop", "install", "--autostart"]);
    assert!(o.status.success(), "{}", text(&o));
    let share = h.root.join("home/.local/share");
    let entry = share.join("applications/io.backdown.BackDown.desktop");
    let text_ = std::fs::read_to_string(&entry).unwrap();
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_backdown")).unwrap();
    assert!(
        text_.contains(&format!("Exec={} app\n", exe.display())),
        "{text_}"
    );
    for n in [16, 32, 48, 128, 256, 512] {
        assert!(
            share
                .join(format!(
                    "icons/hicolor/{n}x{n}/apps/io.backdown.BackDown.png"
                ))
                .is_file()
        );
    }
    assert!(
        share
            .join("icons/hicolor/scalable/apps/io.backdown.BackDown.svg")
            .is_file()
    );
    let auto = h
        .root
        .join("home/.config/autostart/io.backdown.BackDown.desktop");
    assert!(
        std::fs::read_to_string(&auto)
            .unwrap()
            .contains("app --no-window")
    );
    // what freedesktop's own checker says, where it's installed
    if let Ok(v) = Command::new("desktop-file-validate")
        .arg(&entry)
        .arg(&auto)
        .output()
    {
        assert!(v.status.success() && v.stdout.is_empty(), "{}", text(&v));
    }
    let o = h.run(&["desktop", "uninstall"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!entry.exists() && !auto.exists());
    assert!(
        !share
            .join("icons/hicolor/256x256/apps/io.backdown.BackDown.png")
            .exists()
    );
}

#[test]
fn desktop_files_for_every_installer() {
    let h = home();
    for (platform, must) in [
        (
            "linux",
            &[
                "usr/share/applications/io.backdown.BackDown.desktop",
                "usr/lib/systemd/user/backdown.service",
                "usr/share/metainfo/io.backdown.BackDown.metainfo.xml",
            ][..],
        ),
        (
            "appimage",
            &[
                "AppRun",
                "io.backdown.BackDown.desktop",
                "io.backdown.BackDown.png",
                ".DirIcon",
            ][..],
        ),
        (
            "macos",
            &[
                "BackDown.app/Contents/Info.plist",
                "BackDown.app/Contents/Resources/BackDown.icns",
                "BackDown.app/Contents/PkgInfo",
            ][..],
        ),
        (
            "windows",
            &[
                "install.ps1",
                "uninstall.ps1",
                "Install BackDown.cmd",
                "Uninstall BackDown.cmd",
                "backdown.ico",
                "backdown.wxs",
            ][..],
        ),
    ] {
        let out = h.root.join(format!("pkg-{platform}"));
        let o = h.run(&[
            "desktop",
            "files",
            "--platform",
            platform,
            "--out",
            out.to_str().unwrap(),
            "--version",
            "0.5.0-beta.1",
        ]);
        assert!(o.status.success(), "{platform}: {}", text(&o));
        for f in must {
            assert!(out.join(f).is_file(), "{platform}: {f}");
        }
    }
    let apprun = h.root.join("pkg-appimage/AppRun");
    assert_eq!(
        std::fs::metadata(&apprun).unwrap().permissions().mode() & 0o111,
        0o111
    );
    if let Ok(v) = Command::new("desktop-file-validate")
        .arg(
            h.root
                .join("pkg-linux/usr/share/applications/io.backdown.BackDown.desktop"),
        )
        .output()
    {
        assert!(v.status.success() && v.stdout.is_empty(), "{}", text(&v));
    }
}
