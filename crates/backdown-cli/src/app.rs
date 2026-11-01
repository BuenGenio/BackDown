//! `backdown app`: BackDown as a desktop app; what the launcher, the Start menu entry and
//! BackDown.app run.
//!
//! 1. Find this user's daemon (or the machine-wide one) through its ui.json and a request to its
//!    local port. If none answers, start one: the systemd user service where there is one, else a
//!    detached `backdown daemon` writing to daemon.log in the data folder. Never a second one.
//! 2. Open its web UI in an app window: a Chromium-family browser (Chrome, Chromium, Edge, Brave)
//!    in app mode, with a profile of its own under the data folder (app-window/), so it never mixes
//!    with your browsing and remembers its size and place; with no such browser, the default one.
//! 3. Launched again while the window is open, it brings that window forward where the desktop
//!    lets it (Windows, macOS, X11 with wmctrl or xdotool); where it can't (Wayland), the browser
//!    that has the window opens the UI in a new one. Chromium's own profile lock says whether it's
//!    open.
//!
//! The session token never goes on a command line (other users can read those): the window opens
//! a one-time code, good for one use within a minute, which the daemon exchanges for the usual
//! session cookie (serve.rs). `backdown open` uses the same codes.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::locations;

// ---------- one-time codes (in the daemon)

const CODE_LIFE: Duration = Duration::from_secs(60);
static CODES: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());

/// A code that opens the UI once, within a minute (POST /api/launch-code, which needs the token).
pub fn issue_code() -> Result<String> {
    let code = crate::serve::new_token()?;
    let mut c = CODES.lock().unwrap_or_else(|e| e.into_inner());
    c.retain(|(_, t)| t.elapsed() < CODE_LIFE);
    c.push((code.clone(), Instant::now()));
    if c.len() > 32 {
        c.remove(0);
    }
    Ok(code)
}

/// Use up a code: true if it was issued, unused and still fresh.
pub fn redeem(code: &str) -> bool {
    let mut c = CODES.lock().unwrap_or_else(|e| e.into_inner());
    c.retain(|(_, t)| t.elapsed() < CODE_LIFE);
    let found = c.iter().position(|(k, _)| {
        k.len() == code.len()
            && k.bytes()
                .zip(code.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    });
    found.map(|i| c.remove(i)).is_some()
}

// ---------- the daemon

pub struct Found {
    pub port: u16,
    pub token: String,
    /// the machine-wide service
    pub system: bool,
}

/// One request to the local UI server, as a browser with the session cookie would make it.
pub fn local_request(port: u16, method: &str, path: &str, token: &str) -> Result<(u16, String)> {
    request(port, method, path, Some(token))
}

fn request(port: u16, method: &str, path: &str, token: Option<&str>) -> Result<(u16, String)> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    s.set_write_timeout(Some(Duration::from_secs(5)))?;
    let cookie = token
        .map(|t| format!("Cookie: bd={t}\r\n"))
        .unwrap_or_default();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{cookie}X-BackDown: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .context("not an HTTP answer")?;
    let chunked = head
        .lines()
        .any(|l| l.to_ascii_lowercase().replace(' ', "") == "transfer-encoding:chunked");
    Ok((
        status,
        if chunked {
            dechunk(body)
        } else {
            body.to_string()
        },
    ))
}

fn dechunk(mut s: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = s.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 || rest.len() < n {
            break;
        }
        out.push_str(&rest[..n]);
        s = rest[n..].trim_start_matches("\r\n");
    }
    out
}

/// A running daemon: this user's, then the machine-wide one (whose ui.json the `backdown` group
/// can read). Asked, not assumed: a ui.json can outlive its daemon, and a port can be reused.
pub fn find_daemon() -> Option<Found> {
    for (i, file) in locations::candidate_ui_files().into_iter().enumerate() {
        let Ok(raw) = std::fs::read(&file) else {
            continue;
        };
        let Ok(v) = serde_json::from_slice::<Value>(&raw) else {
            continue;
        };
        let (Some(port), Some(token)) = (v["port"].as_u64(), v["token"].as_str()) else {
            continue;
        };
        let port = port as u16;
        // whatever listens there must be BackDown (it turns strangers away) before it gets the token
        if !request(port, "GET", "/api/daemon", None)
            .is_ok_and(|(status, body)| status == 401 && body.contains("BackDown"))
        {
            continue;
        }
        if let Ok((200, body)) = local_request(port, "GET", "/api/daemon", token)
            && serde_json::from_str::<Value>(&body).is_ok_and(|d| d["running"] == true)
        {
            return Some(Found {
                port,
                token: token.to_string(),
                system: i > 0,
            });
        }
    }
    None
}

/// No daemon of ours answers, yet a BackDown daemon listens on the port ours would take
/// (BACKDOWN_PORT, else 8787): another user's (typically your own, seen from a root shell), whose
/// token only its owner can read. Says so, instead of "none is running"; `cmd` is what to run
/// again as that user.
pub fn someone_elses_daemon(cmd: &str) -> Option<String> {
    let port: u16 = match std::env::var("BACKDOWN_PORT") {
        Ok(p) => p.trim().parse().ok()?,
        Err(_) => 8787,
    };
    // 0: any free port, so nobody can be in the way
    if port == 0
        || !request(port, "GET", "/api/daemon", None)
            .is_ok_and(|(status, body)| status == 401 && body.contains("BackDown"))
    {
        return None;
    }
    Some(match listener_owner(port) {
        Some((theirs, me)) if theirs == me => format!(
            "BackDown is running on port {port} as you, but its token isn't in {} (was it started \
             with a different HOME or XDG_DATA_HOME?)",
            locations::state_dir().display()
        ),
        Some((theirs, me)) => format!(
            "BackDown is running on port {port} as {theirs}, not as {me}: run `{cmd}` as \
             {theirs}, without sudo (only {theirs} can read its token)"
        ),
        None => format!(
            "BackDown is running on port {port}, but not as this user: run `{cmd}` as the user \
             who started it, without sudo"
        ),
    })
}

/// Who listens on `port`, and who we are, as user names.
#[cfg(target_os = "linux")]
fn listener_owner(port: u16) -> Option<(String, String)> {
    let uid = ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .find_map(|t| listener_uid(&t, port))?;
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    let name = |u: u32| user_name(&passwd, u).unwrap_or_else(|| format!("uid {u}"));
    Some((name(uid), name(rustix::process::geteuid().as_raw())))
}

#[cfg(not(target_os = "linux"))]
fn listener_owner(_port: u16) -> Option<(String, String)> {
    None
}

/// The uid listening on `port`, from /proc/net/tcp or tcp6: any local address, state 0A (LISTEN).
#[cfg(target_os = "linux")]
fn listener_uid(table: &str, port: u16) -> Option<u32> {
    table.lines().skip(1).find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        let local = u16::from_str_radix(f.get(1)?.rsplit_once(':')?.1, 16).ok()?;
        if local != port || *f.get(3)? != "0A" {
            return None;
        }
        f.get(7)?.parse().ok()
    })
}

#[cfg(target_os = "linux")]
fn user_name(passwd: &str, uid: u32) -> Option<String> {
    passwd.lines().find_map(|l| {
        let mut f = l.split(':');
        let name = f.next()?;
        (f.nth(1)?.parse::<u32>().ok()? == uid).then(|| name.to_string())
    })
}

/// `backdown token`: the background service's browser token, alone on stdout so scripts can use
/// it, and in a terminal also the links with it in. `links`: only the links, one per line. The
/// token is the one in ui.json, which the service keeps across restarts so bookmarks keep working.
pub fn print_token(links: bool) -> Result<ExitCode> {
    use std::io::IsTerminal;
    let running = find_daemon();
    // the running service's file (this user's, or the machine-wide one), else the first saved one
    let v = locations::candidate_ui_files()
        .into_iter()
        .enumerate()
        .filter(|(i, _)| running.as_ref().is_none_or(|f| f.system == (*i > 0)))
        .find_map(|(_, f)| {
            let v: Value = serde_json::from_slice(&std::fs::read(f).ok()?).ok()?;
            v["token"].as_str().is_some().then_some(v)
        });
    let Some(v) = v else {
        if let Some(why) = someone_elses_daemon("backdown token") {
            bail!(why);
        }
        bail!(
            "no BackDown service has run here yet: start one with `backdown service install` or \
             `backdown app` (`backdown serve` prints its own link)"
        );
    };
    let token = v["token"].as_str().unwrap_or_default();
    let port = v["port"].as_u64().unwrap_or(8787);
    let mut all = vec![(
        "on this computer",
        format!("http://127.0.0.1:{port}/?t={token}"),
    )];
    if let Some(n) = v["network_url"].as_str() {
        all.push(("from other devices (look only)", format!("{n}?t={token}")));
    }
    if links {
        for (_, l) in &all {
            println!("{l}");
        }
    } else {
        println!("{token}");
        if std::io::stdout().is_terminal() {
            for (what, l) in &all {
                eprintln!("{what}: {l}");
            }
        }
    }
    if running.is_none() {
        eprintln!("(the service isn't running now; it uses this token again when it starts)");
    }
    Ok(ExitCode::SUCCESS)
}

/// The program to run for BackDown itself: inside an AppImage, the AppImage (its mount point goes
/// away when the process that mounted it ends).
pub fn self_exe() -> Result<PathBuf> {
    if let Some(a) = std::env::var_os("APPIMAGE").map(PathBuf::from)
        && a.is_file()
    {
        return Ok(a);
    }
    Ok(dunce::canonicalize(std::env::current_exe()?)?)
}

/// Let a child outlive us, without a console window on Windows or our terminal's signals on Unix.
pub fn detach(c: &mut Command) {
    c.stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
}

/// Start this user's daemon: the installed service if there is one (systemd user unit, launchd
/// agent, scheduled task), else `backdown daemon` in the background. Returns the child to watch,
/// if we started one ourselves.
fn start_daemon() -> Result<Option<std::process::Child>> {
    if crate::service::start_installed() {
        eprintln!("started BackDown's background service");
        return Ok(None);
    }
    let dir = locations::state_dir();
    locations::ensure_private_dir(&dir, false)
        .with_context(|| format!("can't create {}", dir.display()))?;
    let log = dir.join("daemon.log");
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > 8 << 20) {
        let _ = std::fs::rename(&log, dir.join("daemon.log.1"));
    }
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("can't write {}", log.display()))?;
    let mut c = Command::new(self_exe()?);
    c.arg("daemon").stdout(out.try_clone()?).stderr(out);
    detach(&mut c);
    let child = c.spawn().context("can't start the BackDown daemon")?;
    eprintln!("started the BackDown daemon (log: {})", log.display());
    Ok(Some(child))
}

/// A running daemon, started if need be.
pub fn ensure_daemon() -> Result<Found> {
    if let Some(f) = find_daemon() {
        return Ok(f);
    }
    // ours couldn't have the port anyway
    if let Some(why) = someone_elses_daemon("backdown app") {
        bail!(why);
    }
    let mut child = start_daemon()?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(200));
        if let Some(f) = find_daemon() {
            return Ok(f);
        }
        // it stopped: say why (another daemon may have won the race, which is fine)
        if let Some(c) = child.as_mut()
            && let Ok(Some(status)) = c.try_wait()
        {
            if let Some(f) = find_daemon() {
                return Ok(f);
            }
            let log = locations::state_dir().join("daemon.log");
            let tail = std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .rev()
                .take(3)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n  ");
            bail!("the BackDown daemon stopped ({status}):\n  {tail}");
        }
    }
    bail!(
        "the BackDown daemon didn't answer within 30 s; see {}",
        locations::state_dir().join("daemon.log").display()
    )
}

/// The address that opens the UI: a one-time code, or (from a daemon too old to issue codes)
/// the token.
pub fn launch_url(f: &Found) -> String {
    match local_request(f.port, "POST", "/api/launch-code", &f.token) {
        Ok((200, body)) => {
            if let Some(code) = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["code"].as_str().map(String::from))
            {
                return format!("http://127.0.0.1:{}/?c={code}", f.port);
            }
            format!("http://127.0.0.1:{}/?t={}", f.port, f.token)
        }
        _ => format!("http://127.0.0.1:{}/?t={}", f.port, f.token),
    }
}

// ---------- browsers

#[derive(Debug, Clone, PartialEq)]
pub struct Browser {
    pub name: &'static str,
    pub path: PathBuf,
    /// a snap (Ubuntu's Chromium): it can only write in ~/snap/<name>
    pub snap: Option<String>,
}

const LINUX: &[(&str, &[&str])] = &[
    ("Google Chrome", &["google-chrome-stable", "google-chrome"]),
    ("Chromium", &["chromium", "chromium-browser"]),
    (
        "Microsoft Edge",
        &["microsoft-edge-stable", "microsoft-edge"],
    ),
    ("Brave", &["brave-browser", "brave"]),
];

const MACOS: &[(&str, &str)] = &[
    (
        "Google Chrome",
        "Google Chrome.app/Contents/MacOS/Google Chrome",
    ),
    ("Chromium", "Chromium.app/Contents/MacOS/Chromium"),
    (
        "Microsoft Edge",
        "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    ),
    ("Brave", "Brave Browser.app/Contents/MacOS/Brave Browser"),
];

const WINDOWS: &[(&str, &str, &str)] = &[
    (
        "Google Chrome",
        r"Google\Chrome\Application\chrome.exe",
        "chrome.exe",
    ),
    ("Chromium", r"Chromium\Application\chrome.exe", ""),
    (
        "Microsoft Edge",
        r"Microsoft\Edge\Application\msedge.exe",
        "msedge.exe",
    ),
    (
        "Brave",
        r"BraveSoftware\Brave-Browser\Application\brave.exe",
        "brave.exe",
    ),
];

/// Every Chromium-family browser here, in order of preference.
pub fn browsers() -> Vec<Browser> {
    let mut out = Vec::new();
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        for (name, rel) in MACOS {
            for base in [
                Some(PathBuf::from("/Applications")),
                home.clone().map(|h| h.join("Applications")),
            ]
            .into_iter()
            .flatten()
            {
                let p = base.join(rel);
                if p.is_file() {
                    out.push(Browser {
                        name,
                        path: p,
                        snap: None,
                    });
                    break;
                }
            }
        }
    } else if cfg!(windows) {
        let bases: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
            .iter()
            .filter_map(|v| std::env::var_os(v).map(PathBuf::from))
            .collect();
        for (name, rel, app_path) in WINDOWS {
            let found = bases
                .iter()
                .map(|b| b.join(rel))
                .find(|p| p.is_file())
                .or_else(|| registry_app_path(app_path));
            if let Some(p) = found {
                out.push(Browser {
                    name,
                    path: p,
                    snap: None,
                });
            }
        }
    } else {
        for (name, cmds) in LINUX {
            if let Some(p) = cmds.iter().find_map(|c| on_path(c)) {
                let snap = snap_name(&p);
                out.push(Browser {
                    name,
                    path: p,
                    snap,
                });
            }
        }
    }
    out
}

fn on_path(cmd: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(cmd))
        .find(|p| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(p)
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            }
            #[cfg(not(unix))]
            p.is_file()
        })
}

/// A snap's name, if `p` runs one: /snap/bin/<name>, or a wrapper script that execs it (Ubuntu's
/// /usr/bin/chromium-browser).
fn snap_name(p: &Path) -> Option<String> {
    let name_in = |s: &str| -> Option<String> {
        let rest = &s[s.find("/snap/bin/")? + "/snap/bin/".len()..];
        let n: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        (!n.is_empty()).then_some(n)
    };
    if let Some(n) = name_in(&p.to_string_lossy()) {
        return Some(n);
    }
    let meta = std::fs::metadata(p).ok()?;
    if meta.len() > 16 << 10 {
        return None;
    }
    let text = std::fs::read(p).ok()?;
    if !text.starts_with(b"#!") {
        return None;
    }
    name_in(&String::from_utf8_lossy(&text))
}

#[cfg(windows)]
fn registry_app_path(exe: &str) -> Option<PathBuf> {
    if exe.is_empty() {
        return None;
    }
    for hive in ["HKCU", "HKLM"] {
        let key = format!(r"{hive}\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exe}");
        let Ok(out) = Command::new("reg").args(["query", &key, "/ve"]).output() else {
            continue;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        // "    (Default)    REG_SZ    C:\Program Files\...\chrome.exe"
        if let Some(p) = text
            .lines()
            .find_map(|l| {
                l.split_once("REG_SZ")
                    .map(|(_, v)| v.trim().trim_matches('"').to_string())
            })
            .map(PathBuf::from)
            .filter(|p| p.is_file())
        {
            return Some(p);
        }
    }
    None
}

#[cfg(not(windows))]
fn registry_app_path(_: &str) -> Option<PathBuf> {
    None
}

/// The browser profile the app window uses: its own, so it never mixes with your browsing and
/// Chromium keeps the window's size and place in it.
pub fn profile_dir(b: &Browser) -> PathBuf {
    match &b.snap {
        // a snap can't write outside ~/snap/<name> (not even in ~/.local)
        Some(name) => PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
            .join("snap")
            .join(name)
            .join("common/backdown-app-window"),
        None => locations::state_dir().join("app-window"),
    }
}

/// The browser's command line for the app window.
pub fn app_args(url: &str, profile: &Path, first: bool) -> Vec<String> {
    let mut v = vec![
        format!("--app={url}"),
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        // a profile that only ever shows this computer's BackDown needn't phone anywhere
        "--disable-background-networking".into(),
    ];
    if cfg!(target_os = "linux") {
        // the window's class, so the desktop files it under BackDown's launcher (StartupWMClass)
        v.push("--class=BackDown".into());
    }
    if first {
        // later, Chromium restores the size and place it was left at
        v.push("--window-size=1280,860".into());
    }
    v
}

/// The process holding the app window's profile, if one does: Chromium's own lock (a symlink
/// "host-pid" on Linux and macOS; a file held open, and our record of the pid, on Windows).
fn window_process(profile: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        let target = std::fs::read_link(profile.join("SingletonLock")).ok()?;
        let pid: u32 = target.to_string_lossy().rsplit('-').next()?.parse().ok()?;
        alive(pid).then_some(pid)
    }
    #[cfg(windows)]
    {
        let lock = profile.join("lockfile");
        if !lock.exists() {
            return None;
        }
        // Chromium holds it open without sharing writes: a second writer means it's free
        if std::fs::OpenOptions::new().write(true).open(&lock).is_ok() {
            return None;
        }
        let rec = std::fs::read(locations::state_dir().join("app.json")).ok()?;
        serde_json::from_slice::<Value>(&rec).ok()?["pid"]
            .as_u64()
            .map(|p| p as u32)
    }
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        Path::new(&format!("/proc/{pid}")).exists()
    } else {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

/// Bring the window of process `pid` forward. False where the desktop has no way (Wayland).
fn focus(pid: u32) -> bool {
    let quiet = |c: &mut Command| {
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    };
    if cfg!(target_os = "macos") {
        let script = format!(
            "ObjC.import('AppKit'); var a = $.NSRunningApplication.runningApplicationWithProcessIdentifier({pid}); a && a.activateWithOptions(3) ? 'yes' : 'no'"
        );
        return Command::new("osascript")
            .args(["-l", "JavaScript", "-e", &script])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "yes");
    }
    if cfg!(windows) {
        let script = format!("(New-Object -ComObject WScript.Shell).AppActivate({pid})");
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        detach(&mut c);
        return c
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "True");
    }
    // X11 (the window's class is BackDown); nothing can do this on Wayland
    if std::env::var_os("DISPLAY").is_none() {
        return false;
    }
    let mut w = Command::new("wmctrl");
    w.args(["-x", "-a", "BackDown"]);
    quiet(&mut w);
    if w.status().is_ok_and(|s| s.success()) {
        return true;
    }
    let mut x = Command::new("xdotool");
    x.args(["search", "--class", "BackDown", "windowactivate"]);
    quiet(&mut x);
    x.status().is_ok_and(|s| s.success())
}

pub struct Args {
    /// only make sure the daemon runs (for starting at login)
    pub no_window: bool,
    /// print the browser command instead of running it
    pub print: bool,
    pub browser: Option<PathBuf>,
}

/// `backdown app`
pub fn run(a: Args) -> Result<ExitCode> {
    crate::update::tidy();
    let found = ensure_daemon()?;
    if a.no_window {
        eprintln!(
            "BackDown is running{} (http://127.0.0.1:{}/)",
            if found.system {
                " as the machine-wide service"
            } else {
                ""
            },
            found.port
        );
        return Ok(ExitCode::SUCCESS);
    }
    let browser = match a.browser {
        Some(p) => Some(Browser {
            name: "your browser",
            snap: snap_name(&p),
            path: p,
        }),
        None => browsers().into_iter().next(),
    };
    let Some(b) = browser else {
        // no app-mode browser: a tab in the default one
        let url = launch_url(&found);
        if a.print {
            println!("{url}");
        } else {
            eprintln!(
                "opening BackDown in your browser (for a window of its own, install Chrome, Chromium, Edge or Brave)"
            );
            crate::serve::open(&url);
        }
        return Ok(ExitCode::SUCCESS);
    };
    let profile = profile_dir(&b);
    let first = !profile.exists();
    if !a.print
        && let Some(pid) = window_process(&profile)
    {
        if focus(pid) {
            eprintln!("BackDown is open already: brought its window forward");
            return Ok(ExitCode::SUCCESS);
        }
        // the browser that has it opens the UI again (in a new window)
        eprintln!("BackDown is open already; opening another window");
    }
    let url = launch_url(&found);
    let args = app_args(&url, &profile, first);
    if a.print {
        println!(
            "{}",
            std::iter::once(b.path.display().to_string())
                .chain(args)
                .collect::<Vec<_>>()
                .join(" ")
        );
        return Ok(ExitCode::SUCCESS);
    }
    if b.snap.is_none() {
        locations::ensure_private_dir(&profile, false)
            .with_context(|| format!("can't create {}", profile.display()))?;
    }
    let mut c = Command::new(&b.path);
    c.args(&args).stdout(Stdio::null()).stderr(Stdio::null());
    detach(&mut c);
    let mut child = c
        .spawn()
        .with_context(|| format!("can't start {} ({})", b.name, b.path.display()))?;
    // Windows has no pid in the profile lock: remember the one that holds the window
    std::thread::sleep(Duration::from_millis(1500));
    match child.try_wait() {
        Ok(None) => {
            let rec = serde_json::json!({ "pid": child.id(), "browser": b.path.display().to_string(), "profile": profile.display().to_string() });
            let _ = crate::settings::write_private(
                &locations::state_dir().join("app.json"),
                rec.to_string().as_bytes(),
                false,
            );
        }
        // it gave up straight away (no display, a broken install): the default browser, then
        // (the code wasn't used, so it still opens the UI)
        Ok(Some(status)) if !status.success() => {
            eprintln!(
                "{} stopped ({status}); opening your default browser instead",
                b.name
            );
            crate::serve::open(&url);
            return Ok(ExitCode::SUCCESS);
        }
        // handed to the browser already running with this profile
        _ => {}
    }
    eprintln!("opened BackDown in {}", b.name);
    Ok(ExitCode::SUCCESS)
}

/// `backdown open`: the UI in the default browser, through a one-time code.
pub fn open_in_browser() -> Result<ExitCode> {
    let Some(found) = find_daemon() else {
        if let Some(why) = someone_elses_daemon("backdown open") {
            bail!(why);
        }
        bail!(
            "no BackDown daemon is running. Start one with `backdown app` (or `backdown service install`), or browse without it: `backdown serve`"
        );
    };
    let url = launch_url(&found);
    eprintln!(
        "opening BackDown (http://127.0.0.1:{}/) in your browser",
        found.port
    );
    crate::serve::open(&url);
    Ok(ExitCode::SUCCESS)
}

/// Started with no arguments in a way only a double-click does (BackDown.app, or backdown.exe
/// from Explorer with a console of its own): run as `backdown app`.
pub fn double_clicked() -> bool {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let bare =
        args.is_empty() || (args.len() == 1 && args[0].to_string_lossy().starts_with("-psn_"));
    if !bare {
        return false;
    }
    if cfg!(target_os = "macos") {
        return std::env::current_exe()
            .is_ok_and(|e| e.to_string_lossy().contains(".app/Contents/MacOS/"));
    }
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
        }
        let mut pids = [0u32; 4];
        // only this process on its console: Explorer made the console for it
        return unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) } == 1;
    }
    #[allow(unreachable_code)]
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_work_once_and_only_as_issued() {
        let c = issue_code().unwrap();
        assert_eq!(c.len(), 48);
        assert!(!redeem("0".repeat(48).as_str()));
        assert!(!redeem(&c[..47]));
        assert!(redeem(&c));
        assert!(!redeem(&c), "a code works once");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_listener_and_its_owner_by_name() {
        let tcp = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
            \x20  0: 0100007F:0277 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 1 0 100 0 0 10 0\n\
            \x20  1: 0100007F:2253 0100007F:A1B2 01 00000000:00000000 00:00000000 00000000     7        0 2 1 0 20 4 30 10 -1\n\
            \x20  2: 0100007F:2253 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 3 1 0 100 0 0 10 0\n";
        assert_eq!(
            listener_uid(tcp, 8787),
            Some(1000),
            "the listener, not a connection"
        );
        assert_eq!(listener_uid(tcp, 631), Some(0));
        assert_eq!(listener_uid(tcp, 8788), None);
        let tcp6 = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
            \x20  0: 00000000000000000000000000000000:2253 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1001        0 4 1 0 100 0 0 10 0\n";
        assert_eq!(listener_uid(tcp6, 8787), Some(1001));

        let passwd = "root:x:0:0:root:/root:/bin/bash\n\
                      daemon:x:1:1::/usr/sbin:/usr/sbin/nologin\n\
                      alex:x:1000:1000:Alex,,,:/home/alex:/bin/bash\n";
        assert_eq!(user_name(passwd, 1000).as_deref(), Some("alex"));
        assert_eq!(user_name(passwd, 0).as_deref(), Some("root"));
        assert_eq!(user_name(passwd, 4242), None);
    }

    #[test]
    fn app_window_arguments() {
        let a = app_args(
            "http://127.0.0.1:8787/?c=abc",
            Path::new("/data/app-window"),
            true,
        );
        assert_eq!(a[0], "--app=http://127.0.0.1:8787/?c=abc");
        assert_eq!(a[1], "--user-data-dir=/data/app-window");
        assert!(a.contains(&"--no-first-run".to_string()));
        assert!(a.contains(&"--window-size=1280,860".to_string()));
        let again = app_args(
            "http://127.0.0.1:8787/?c=abc",
            Path::new("/data/app-window"),
            false,
        );
        assert!(
            !again.iter().any(|x| x.starts_with("--window-size")),
            "Chromium restores it"
        );
        assert!(!a.iter().any(|x| x.contains("?t=")), "never the token");
    }

    #[cfg(unix)]
    #[test]
    fn snaps_are_recognised() {
        assert_eq!(
            snap_name(Path::new("/snap/bin/chromium")).as_deref(),
            Some("chromium")
        );
        let dir = tempfile::tempdir().unwrap();
        let wrapper = dir.path().join("chromium-browser");
        std::fs::write(&wrapper, "#!/bin/sh\nexec /snap/bin/chromium \"$@\"\n").unwrap();
        assert_eq!(snap_name(&wrapper).as_deref(), Some("chromium"));
        let plain = dir.path().join("google-chrome");
        std::fs::write(
            &plain,
            "#!/bin/bash\nexec -a \"$0\" /opt/google/chrome/chrome \"$@\"\n",
        )
        .unwrap();
        assert_eq!(snap_name(&plain), None);
        let b = Browser {
            name: "Chromium",
            path: wrapper,
            snap: Some("chromium".into()),
        };
        assert!(profile_dir(&b).ends_with("snap/chromium/common/backdown-app-window"));
    }

    #[test]
    fn chunked_bodies() {
        assert_eq!(dechunk("4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n"), "Wikipedia");
    }
}
