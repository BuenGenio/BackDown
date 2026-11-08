//! macOS: a LaunchAgent in ~/Library/LaunchAgents, loaded into your login session with
//! `launchctl bootstrap gui/<uid>`. It runs as you, starts when you log in, is restarted if it
//! crashes (not when it's stopped on purpose), and runs at background priority with low-priority
//! disk access. Its output goes to ~/Library/Logs/BackDown.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

pub const LABEL: &str = "io.backdown.daemon";

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The LaunchAgent for `exe`, logging into `logs`.
pub fn plist(exe: &Path, logs: &Path) -> String {
    let exe = xml_escape(&exe.display().to_string());
    let log = xml_escape(&logs.join("daemon.log").display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- BackDown daemon for one user. Installed by `backdown service install`. -->
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>daemon</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ThrottleInterval</key>
	<integer>10</integer>
	<key>ProcessType</key>
	<string>Background</string>
	<key>LowPriorityIO</key>
	<true/>
	<key>Nice</key>
	<integer>10</integer>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#
    )
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

pub fn plist_path() -> PathBuf {
    home()
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

fn logs_dir() -> PathBuf {
    home().join("Library/Logs/BackDown")
}

/// The login session's launchd domain: `gui/<uid>`.
fn domain() -> Result<String> {
    let out = Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .context("can't run id")?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if uid.is_empty() || !uid.chars().all(|c| c.is_ascii_digit()) {
        bail!("can't tell your user id");
    }
    Ok(format!("gui/{uid}"))
}

fn launchctl(args: &[&str]) -> Result<std::process::Output> {
    Command::new("/bin/launchctl")
        .args(args)
        .output()
        .context("launchctl isn't available")
}

/// What `launchctl print gui/<uid>/<label>` says about the agent: its state and pid.
pub fn parse_print(text: &str) -> (Option<String>, Option<u32>) {
    let mut state = None;
    let mut pid = None;
    for line in text.lines() {
        // the agent's own keys are one level in; nested blocks repeat some names further in
        let top = line
            .strip_prefix('\t')
            .is_some_and(|r| !r.starts_with(char::is_whitespace))
            || line
                .strip_prefix("    ")
                .is_some_and(|r| !r.starts_with(char::is_whitespace));
        if !top {
            continue;
        }
        let l = line.trim();
        if let Some(v) = l.strip_prefix("state = ") {
            state.get_or_insert_with(|| v.trim().to_string());
        } else if let Some(v) = l.strip_prefix("pid = ") {
            pid = pid.or(v.trim().parse().ok());
        }
    }
    (state, pid)
}

fn no_system() -> Result<ExitCode> {
    bail!(
        "a machine-wide service isn't available on macOS: `backdown service install` sets BackDown up for you (it runs while you're logged in)"
    )
}

pub fn install(exe: &Path, system: bool, start: bool, print: bool) -> Result<ExitCode> {
    if system {
        return no_system();
    }
    let path = plist_path();
    let text = plist(exe, &logs_dir());
    if print {
        print!("{text}");
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    }
    std::fs::create_dir_all(logs_dir())?;
    let existed = path.exists();
    std::fs::write(&path, text).with_context(|| format!("can't write {}", path.display()))?;
    eprintln!(
        "{} {}",
        if existed { "updated" } else { "wrote" },
        path.display()
    );
    let domain = domain()?;
    let target = format!("{domain}/{LABEL}");
    if start {
        // a loaded older version is replaced (bootout fails harmlessly when nothing is loaded)
        let _ = launchctl(&["bootout", &target]);
        let out = launchctl(&["bootstrap", &domain, &path.display().to_string()])?;
        if !out.status.success() {
            bail!(
                "launchctl bootstrap failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let _ = launchctl(&["enable", &target]);
        eprintln!("backdown is running. Open it with `backdown open`.");
    } else {
        eprintln!(
            "installed but not started: launchctl bootstrap {domain} {}",
            path.display()
        );
    }
    eprintln!(
        "It starts whenever you log in. Its log is in {}.",
        logs_dir().display()
    );
    Ok(ExitCode::SUCCESS)
}

/// Start the installed agent now, if there is one (`backdown app`): kickstart it where it's
/// loaded, load it where it isn't. False when there's no agent.
pub fn start_installed() -> bool {
    let path = plist_path();
    if !path.exists() {
        return false;
    }
    let Ok(domain) = domain() else {
        return false;
    };
    launchctl(&["kickstart", &format!("{domain}/{LABEL}")]).is_ok_and(|o| o.status.success())
        || launchctl(&["bootstrap", &domain, &path.display().to_string()])
            .is_ok_and(|o| o.status.success())
}

pub fn uninstall(system: bool) -> Result<ExitCode> {
    if system {
        return no_system();
    }
    let path = plist_path();
    if !path.exists() {
        eprintln!("no service installed ({} doesn't exist)", path.display());
        return Ok(ExitCode::SUCCESS);
    }
    if let Ok(domain) = domain() {
        let _ = launchctl(&["bootout", &format!("{domain}/{LABEL}")]);
    }
    std::fs::remove_file(&path).with_context(|| format!("can't remove {}", path.display()))?;
    eprintln!(
        "removed {}. The index is kept in {}.",
        path.display(),
        crate::locations::state_dir().display()
    );
    Ok(ExitCode::SUCCESS)
}

pub fn status(system: bool) -> Result<ExitCode> {
    if system {
        return no_system();
    }
    let path = plist_path();
    println!(
        "LaunchAgent: {}",
        if path.exists() {
            path.display().to_string()
        } else {
            "not installed".into()
        }
    );
    let (state, pid) = match domain() {
        Ok(d) => launchctl(&["print", &format!("{d}/{LABEL}")])
            .ok()
            .filter(|o| o.status.success())
            .map_or((None, None), |o| {
                parse_print(&String::from_utf8_lossy(&o.stdout))
            }),
        Err(_) => (None, None),
    };
    println!(
        "  loaded: {}   state: {}{}",
        if state.is_some() { "yes" } else { "no" },
        state.as_deref().unwrap_or("-"),
        pid.map(|p| format!(" (pid {p})")).unwrap_or_default()
    );
    super::print_daemon()?;
    Ok(if state.as_deref() == Some("running") {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(3)
    })
}

// launchd is macOS-only (the code compiles everywhere via cfg!, but only runs on macOS); the test
// builds paths with Path::join, which uses backslashes on Windows, so it runs off Windows.
#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn the_launch_agent() {
        let p = plist(
            Path::new("/Users/someone/bin/back&down"),
            Path::new("/Users/someone/Library/Logs/BackDown"),
        );
        // a property list the plist reader accepts, with what launchd needs
        let v = backdown_core::volumes::macos::plist(p.as_bytes()).unwrap();
        assert_eq!(v["Label"], LABEL);
        assert_eq!(
            v["ProgramArguments"],
            serde_json::json!(["/Users/someone/bin/back&down", "daemon"])
        );
        assert_eq!(v["RunAtLoad"], true);
        assert_eq!(v["KeepAlive"]["SuccessfulExit"], false);
        assert_eq!(v["ProcessType"], "Background");
        assert_eq!(v["LowPriorityIO"], true);
        assert_eq!(
            v["StandardErrorPath"],
            "/Users/someone/Library/Logs/BackDown/daemon.log"
        );
        assert!(p.contains("back&amp;down"));
    }

    #[test]
    fn launchctl_print() {
        let text = "gui/501/io.backdown.daemon = {
	active count = 1
	path = /Users/someone/Library/LaunchAgents/io.backdown.daemon.plist
	type = LaunchAgent
	state = running

	program = /Users/someone/bin/backdown
	arguments = {
		/Users/someone/bin/backdown
		daemon
	}

	pid = 4242
	immediate reason = speculative
	endpoints = {
	}
}
";
        assert_eq!(parse_print(text), (Some("running".into()), Some(4242)));
        let stopped =
            "gui/501/io.backdown.daemon = {\n\tstate = not running\n\tlast exit code = 0\n}\n";
        assert_eq!(parse_print(stopped), (Some("not running".into()), None));
    }
}
