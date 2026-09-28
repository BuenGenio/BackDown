//! `backdown service install|uninstall|status`: run the daemon under the OS service manager.
//!
//! macOS: a LaunchAgent (service/launchd.rs); Windows: a scheduled task at logon
//! (service/task.rs); both run as you, while you're logged in. Linux (systemd), below: a user
//! service runs as you, with no extra privileges. The system
//! service runs as the `backdown` user with CAP_DAC_READ_SEARCH (read any file, write none) and a
//! sandbox that makes the whole filesystem read-only to it except /var/lib/backdown, and cuts it off
//! from the network except localhost: the kernel enforces "never writes to a source" and "no network".

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

use crate::locations;

mod launchd;
mod task;

const NAME: &str = "backdown.service";

pub fn user_unit(exe: &Path) -> String {
    format!(
        "# BackDown daemon for one user. Installed by `backdown service install`.
[Unit]
Description=BackDown: index, verify and browse your drives
Documentation=https://backdown.io

[Service]
Type=notify
NotifyAccess=main
ExecStart={exe} daemon
Restart=on-failure
RestartSec=5
WatchdogSec=120
Nice=10
IOSchedulingClass=idle
NoNewPrivileges=yes
LockPersonality=yes
RestrictRealtime=yes
SystemCallArchitectures=native

[Install]
WantedBy=default.target
",
        exe = exe.display()
    )
}

pub fn system_unit(exe: &Path) -> String {
    format!(
        "# BackDown daemon for this machine. Installed by `backdown service install --system`.
# Members of the `backdown` group can use it (and so can read anything it indexes).
[Unit]
Description=BackDown: index, verify and browse this machine's drives
Documentation=https://backdown.io
After=local-fs.target

[Service]
Type=notify
NotifyAccess=main
ExecStart={exe} daemon --system
User=backdown
Group=backdown
UMask=0027
StateDirectory=backdown
StateDirectoryMode=0750
RuntimeDirectory=backdown
RuntimeDirectoryMode=0750
Restart=on-failure
RestartSec=5
WatchdogSec=120
Nice=10
IOSchedulingClass=idle

# read every file, write none: the one privilege it has
AmbientCapabilities=CAP_DAC_READ_SEARCH
CapabilityBoundingSet=CAP_DAC_READ_SEARCH
NoNewPrivileges=yes
# the whole filesystem is read-only to it, except its own state in /var/lib/backdown
ProtectSystem=strict
ProtectHome=read-only
PrivateTmp=yes
# no hardware devices, no other processes, no leftover IPC (drive health comes from udisks over
# D-Bus, which needs neither)
PrivateDevices=yes
ProtectProc=invisible
ProcSubset=pid
RemoveIPC=yes
# no network beyond this machine: the web UI and the CLI socket are local only
IPAddressDeny=any
IPAddressAllow=localhost
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectKernelLogs=yes
ProtectControlGroups=yes
ProtectClock=yes
ProtectHostname=yes
RestrictNamespaces=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
LockPersonality=yes
MemoryDenyWriteExecute=yes
SystemCallArchitectures=native
SystemCallFilter=@system-service
SystemCallFilter=~@privileged @resources

[Install]
WantedBy=multi-user.target
",
        exe = exe.display()
    )
}

fn user_unit_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("systemd/user")
        .join(NAME)
}

const SYSTEM_UNIT_PATH: &str = "/etc/systemd/system/backdown.service";

fn exe() -> Result<PathBuf> {
    Ok(dunce::canonicalize(std::env::current_exe()?)?)
}

fn systemctl(system: bool, args: &[&str]) -> Result<std::process::Output> {
    let mut c = Command::new("systemctl");
    if !system {
        c.arg("--user");
    }
    c.args(args)
        .output()
        .context("systemctl isn't available (is this a systemd system?)")
}

fn check(out: std::process::Output, what: &str) -> Result<()> {
    if !out.status.success() {
        bail!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

pub fn install(system: bool, start: bool, print: bool) -> Result<ExitCode> {
    let exe = exe()?;
    if !print && (exe.starts_with("/tmp") || exe.starts_with(std::env::temp_dir())) {
        eprintln!(
            "note: {} is in a temporary folder; the service will break when it's cleaned up. Move backdown somewhere permanent (e.g. ~/.local/bin or /usr/local/bin) and install again.",
            exe.display()
        );
    }
    if cfg!(target_os = "macos") {
        return launchd::install(&exe, system, start, print);
    }
    if cfg!(windows) {
        return task::install(&exe, system, start, print);
    }
    let unit = if system {
        system_unit(&exe)
    } else {
        user_unit(&exe)
    };
    if print {
        print!("{unit}");
        return Ok(ExitCode::SUCCESS);
    }
    if !cfg!(target_os = "linux") {
        bail!(
            "`backdown service` supports Linux (systemd), macOS and Windows; elsewhere, start `backdown daemon` from your login items"
        );
    }
    let path = if system {
        PathBuf::from(SYSTEM_UNIT_PATH)
    } else {
        user_unit_path()
    };
    if system {
        if exe.starts_with("/home") {
            eprintln!(
                "note: the system service will run {} from a home folder; /usr/local/bin is the usual place.",
                exe.display()
            );
        }
        ensure_system_user()?;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| {
            if system {
                format!("can't write {} (run with sudo)", dir.display())
            } else {
                format!("can't create {}", dir.display())
            }
        })?;
    }
    let existed = path.exists();
    std::fs::write(&path, unit).with_context(|| {
        if system {
            format!("can't write {} (run with sudo)", path.display())
        } else {
            format!("can't write {}", path.display())
        }
    })?;
    eprintln!(
        "{} {}",
        if existed { "updated" } else { "wrote" },
        path.display()
    );
    check(
        systemctl(system, &["daemon-reload"])?,
        "systemctl daemon-reload",
    )?;
    if start {
        check(
            systemctl(system, &["enable", "--now", NAME])?,
            "starting the service",
        )?;
        if existed {
            check(
                systemctl(system, &["restart", NAME])?,
                "restarting the service",
            )?;
        }
        eprintln!(
            "backdown is running{}. Open it with `backdown open`.",
            if system { " as a system service" } else { "" }
        );
    } else {
        eprintln!(
            "installed but not started: systemctl{} enable --now {NAME}",
            if system { "" } else { " --user" }
        );
    }
    if system {
        eprintln!(
            "Give people access with `sudo usermod -aG backdown <user>` (they need to log in again).\n\
             The index lives in {}.",
            locations::SYSTEM_STATE
        );
    } else {
        eprintln!(
            "To keep it running while you're logged out: loginctl enable-linger {}",
            std::env::var("USER").unwrap_or_else(|_| "$USER".into())
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn ensure_system_user() -> Result<()> {
    let exists = Command::new("getent")
        .args(["passwd", "backdown"])
        .output()
        .is_ok_and(|o| o.status.success());
    if exists {
        return Ok(());
    }
    let shell = ["/usr/sbin/nologin", "/sbin/nologin", "/bin/false"]
        .into_iter()
        .find(|s| Path::new(s).exists())
        .unwrap_or("/bin/false");
    let out = Command::new("useradd")
        .args([
            "--system",
            "--user-group",
            "--home-dir",
            locations::SYSTEM_STATE,
            "--no-create-home",
            "--shell",
            shell,
            "backdown",
        ])
        .output()
        .context("useradd isn't available")?;
    check(out, "creating the backdown system user (run with sudo)")?;
    eprintln!("created system user and group `backdown`");
    Ok(())
}

pub fn uninstall(system: bool) -> Result<ExitCode> {
    if cfg!(target_os = "macos") {
        return launchd::uninstall(system);
    }
    if cfg!(windows) {
        return task::uninstall(system);
    }
    if !cfg!(target_os = "linux") {
        bail!("`backdown service` supports Linux (systemd), macOS and Windows");
    }
    let path = if system {
        PathBuf::from(SYSTEM_UNIT_PATH)
    } else {
        user_unit_path()
    };
    if !path.exists() {
        eprintln!(
            "no {} service installed ({} doesn't exist)",
            if system { "system" } else { "user" },
            path.display()
        );
        return Ok(ExitCode::SUCCESS);
    }
    let _ = systemctl(system, &["disable", "--now", NAME]);
    std::fs::remove_file(&path).with_context(|| {
        format!(
            "can't remove {}{}",
            path.display(),
            if system { " (run with sudo)" } else { "" }
        )
    })?;
    let _ = systemctl(system, &["daemon-reload"]);
    let data = if system {
        PathBuf::from(locations::SYSTEM_STATE)
    } else {
        locations::state_dir()
    };
    eprintln!(
        "removed {}. The index is kept in {}.",
        path.display(),
        data.display()
    );
    Ok(ExitCode::SUCCESS)
}

pub fn status(system: bool) -> Result<ExitCode> {
    if cfg!(target_os = "macos") {
        return launchd::status(system);
    }
    if cfg!(windows) {
        return task::status(system);
    }
    if !cfg!(target_os = "linux") {
        bail!("`backdown service` supports Linux (systemd), macOS and Windows");
    }
    let path = if system {
        PathBuf::from(SYSTEM_UNIT_PATH)
    } else {
        user_unit_path()
    };
    let state = |args: &[&str]| {
        systemctl(system, args)
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let active = state(&["is-active", NAME]);
    println!(
        "{} service: {}",
        if system { "system" } else { "user" },
        if path.exists() {
            path.display().to_string()
        } else {
            "not installed".into()
        }
    );
    println!(
        "  enabled: {}   active: {}",
        state(&["is-enabled", NAME]),
        active
    );
    print_daemon()?;
    Ok(if active == "active" {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(3)
    })
}

/// What the running daemon (if one answers) is doing.
fn print_daemon() -> Result<()> {
    match crate::client::Client::connect(None) {
        Some(mut c) => {
            let s = c.request(&serde_json::json!({ "cmd": "status" }))?;
            let d = &s["daemon"];
            println!(
                "  daemon {} (pid {}) · index {}",
                d["version"].as_str().unwrap_or("?"),
                d["pid"],
                d["db"].as_str().unwrap_or("?")
            );
            match d["current"].as_object() {
                Some(j) => println!(
                    "  now: {} {} {}",
                    j["kind"].as_str().unwrap_or(""),
                    j["path"].as_str().unwrap_or(""),
                    j["progress"].as_str().unwrap_or("")
                ),
                None => println!("  now: idle"),
            }
        }
        None if cfg!(windows) => {
            println!(
                "  (the terminal can't ask the daemon what it's doing on Windows yet: `backdown open` shows it)"
            )
        }
        None => println!(
            "  no daemon answering on {}",
            locations::candidate_sockets()
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(" or ")
        ),
    }
    Ok(())
}
