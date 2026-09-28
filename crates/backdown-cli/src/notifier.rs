//! Desktop notifications, and whether to send them.
//!
//! One pop-up per piece of news (backdown_core::alerts decides which news, and how many may
//! reach the desktop), through what each desktop already has: `notify-send` on Linux (else the
//! freedesktop Notifications interface through `gdbus`), `osascript`'s `display notification` on
//! macOS, and a toast from Windows PowerShell on Windows. Nothing is ever put through a shell:
//! the texts go as arguments (Linux, macOS; the body escaped for notification servers that read
//! markup) or in environment variables that the toast script turns into XML text nodes (Windows).
//!
//! The settings live beside the index's UI token in the daemon's state folder
//! (`notifications.json`): desktop pop-ups on or off, and which kinds of news are kept at all.
//! The machine-wide service never pops anything up: it runs as its own user, on nobody's desktop.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const SETTINGS_FILE: &str = "notifications.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// pop-ups on the desktop (news is kept in the list either way)
    pub desktop: bool,
    /// a drive's health got worse
    pub health: bool,
    /// a mirror no longer matches its source
    pub drift: bool,
    /// a scheduled job failed
    pub jobs: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            desktop: true,
            health: true,
            drift: true,
            jobs: true,
        }
    }
}

impl Settings {
    pub fn wants(&self, kind: &str) -> bool {
        match kind {
            "health" => self.health,
            "drift" => self.drift,
            "job" => self.jobs,
            _ => true,
        }
    }
}

pub fn load(path: &Path) -> Settings {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, s: &Settings) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s)?)
        .with_context(|| format!("can't write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// What would show a notification here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Text for notification servers that read a little HTML in the body.
fn markup_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A GVariant string literal, for `gdbus call`.
fn gv_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// Shows a toast with the title and body from BD_TITLE and BD_BODY, as PowerShell's own app (a
/// console program has no app id of its own to show toasts as).
pub const TOAST: &str = r#"
$ErrorActionPreference = 'Stop'
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
[Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] | Out-Null
$xml = New-Object Windows.Data.Xml.Dom.XmlDocument
$xml.LoadXml('<toast><visual><binding template="ToastGeneric"><text></text><text></text></binding></visual></toast>')
$t = $xml.GetElementsByTagName('text')
$t.Item(0).AppendChild($xml.CreateTextNode($env:BD_TITLE)) | Out-Null
$t.Item(1).AppendChild($xml.CreateTextNode($env:BD_BODY)) | Out-Null
$toast = New-Object Windows.UI.Notifications.ToastNotification $xml
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe').Show($toast)
"#;

/// The command that shows one notification on `os`, given which programs are installed.
pub fn command(
    os: &str,
    title: &str,
    body: &str,
    urgent: bool,
    have: &dyn Fn(&str) -> bool,
) -> Option<Cmd> {
    let s = |x: &str| x.to_string();
    match os {
        "macos" => Some(Cmd {
            program: s("/usr/bin/osascript"),
            args: vec![
                s("-e"),
                s("on run argv"),
                s("-e"),
                s(
                    "display notification (item 2 of argv) with title \"BackDown\" subtitle (item 1 of argv)",
                ),
                s("-e"),
                s("end run"),
                s(title),
                s(body),
            ],
            env: Vec::new(),
        }),
        "windows" => {
            use base64::Engine;
            let utf16: Vec<u8> = TOAST.encode_utf16().flat_map(u16::to_le_bytes).collect();
            Some(Cmd {
                program: s("powershell"),
                args: vec![
                    s("-NoProfile"),
                    s("-NonInteractive"),
                    s("-WindowStyle"),
                    s("Hidden"),
                    s("-EncodedCommand"),
                    base64::engine::general_purpose::STANDARD.encode(utf16),
                ],
                env: vec![(s("BD_TITLE"), s(title)), (s("BD_BODY"), s(body))],
            })
        }
        _ if have("notify-send") => Some(Cmd {
            program: s("notify-send"),
            args: vec![
                s("--app-name=BackDown"),
                format!("--urgency={}", if urgent { "critical" } else { "normal" }),
                s("--icon=drive-harddisk"),
                s("--category=device"),
                s("--"),
                s(title),
                markup_escape(body),
            ],
            env: Vec::new(),
        }),
        _ if have("gdbus") => Some(Cmd {
            program: s("gdbus"),
            args: vec![
                s("call"),
                s("--session"),
                s("--dest=org.freedesktop.Notifications"),
                s("--object-path=/org/freedesktop/Notifications"),
                s("--method=org.freedesktop.Notifications.Notify"),
                s("'BackDown'"),
                s("0"),
                s("'drive-harddisk'"),
                gv_quote(title),
                gv_quote(&markup_escape(body)),
                s("[]"),
                format!("{{'urgency': <byte {}>}}", if urgent { 2 } else { 1 }),
                s("-1"),
            ],
            env: Vec::new(),
        }),
        _ => None,
    }
}

/// Show one notification on this desktop. Waits a few seconds at most.
pub fn send(title: &str, body: &str, urgent: bool) -> Result<()> {
    let os = std::env::consts::OS;
    let have = |p: &str| backdown_core::health::which(p).is_some();
    let Some(mut c) = command(os, title, body, urgent, &have) else {
        anyhow::bail!("no way to show notifications here (notify-send isn't installed)");
    };
    if os == "windows"
        && let Some(root) = std::env::var_os("SystemRoot")
    {
        c.program = PathBuf::from(root)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
            .display()
            .to_string();
    }
    let args: Vec<&str> = c.args.iter().map(String::as_str).collect();
    let env: Vec<(&str, &str)> = c
        .env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let (code, _) = backdown_core::health::run(&c.program, &args, &env, Duration::from_secs(15))?;
    if code != Some(0) {
        anyhow::bail!("{} couldn't show the notification", c.program);
    }
    Ok(())
}

/// Whether this process could show a notification at all.
pub fn available() -> bool {
    let have = |p: &str| backdown_core::health::which(p).is_some();
    command(std::env::consts::OS, "", "", false, &have).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_notify_send_and_gdbus() {
        let c = command(
            "linux",
            "Old drive is failing",
            "12 <sectors> & more",
            true,
            &|p| p == "notify-send",
        )
        .unwrap();
        assert_eq!(c.program, "notify-send");
        assert_eq!(
            c.args,
            [
                "--app-name=BackDown",
                "--urgency=critical",
                "--icon=drive-harddisk",
                "--category=device",
                "--",
                "Old drive is failing",
                "12 &lt;sectors&gt; &amp; more"
            ]
        );
        let c = command("linux", "It's \\ odd", "body", false, &|p| p == "gdbus").unwrap();
        assert_eq!(c.program, "gdbus");
        assert_eq!(c.args[8], r"'It\'s \\ odd'");
        assert_eq!(c.args[11], "{'urgency': <byte 1>}");
        assert!(command("linux", "t", "b", false, &|_| false).is_none());
    }

    #[test]
    fn macos_passes_texts_as_arguments() {
        let c = command("macos", "\"quoted\" title", "a body", false, &|_| false).unwrap();
        assert_eq!(c.program, "/usr/bin/osascript");
        assert_eq!(c.args[c.args.len() - 2..], ["\"quoted\" title", "a body"]);
        assert!(
            c.args
                .iter()
                .all(|a| !a.contains("quoted") || a == "\"quoted\" title")
        );
    }

    #[test]
    fn windows_texts_go_in_the_environment() {
        let c = command("windows", "<toast/>", "body & more", true, &|_| false).unwrap();
        assert_eq!(
            c.env,
            [
                ("BD_TITLE".to_string(), "<toast/>".to_string()),
                ("BD_BODY".to_string(), "body & more".to_string())
            ]
        );
        assert!(c.args.contains(&"-EncodedCommand".to_string()));
        use base64::Engine;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(c.args.last().unwrap())
            .unwrap();
        let script = String::from_utf16(
            &raw.chunks(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(script.contains("CreateTextNode($env:BD_TITLE)"));
    }

    #[test]
    fn settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(SETTINGS_FILE);
        assert_eq!(load(&p), Settings::default());
        let s = Settings {
            desktop: false,
            jobs: false,
            ..Settings::default()
        };
        save(&p, &s).unwrap();
        assert_eq!(load(&p), s);
        assert!(!load(&p).wants("job") && load(&p).wants("health"));
        // a partial or older file keeps the defaults for what it doesn't say
        std::fs::write(&p, r#"{"desktop": false}"#).unwrap();
        assert_eq!(
            load(&p),
            Settings {
                desktop: false,
                ..Settings::default()
            }
        );
    }
}
