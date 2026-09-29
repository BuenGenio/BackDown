//! Windows: a scheduled task of your own that starts the daemon when you log on, registered
//! with PowerShell's `Register-ScheduledTask` from a task definition written here. It runs as
//! you, with your normal (not elevated) rights, only while you're logged on, at below-normal
//! priority, and is restarted if it fails. `backdown.exe` is a console program, so the task
//! starts it through `conhost.exe --headless`, which gives it a console that's never shown:
//! no window appears at logon.
//!
//! The definition goes to PowerShell in an environment variable and the task's name is fixed,
//! so nothing of yours is ever parsed as a command.

use std::path::Path;
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use serde_json::Value;

pub const NAME: &str = "BackDown";

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The task definition: at `user`'s logon, run `exe daemon` through a hidden console.
pub fn task_xml(exe: &Path, user: &str, system_root: &str) -> String {
    let conhost = format!(
        r"{}\System32\conhost.exe",
        system_root.trim_end_matches('\\')
    );
    let args = format!("--headless \"{}\" daemon", exe.display());
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>BackDown: index, verify and browse your drives. Installed by `backdown service install`.</Description>
    <URI>\{NAME}</URI>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>5</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{conhost}</Command>
      <Arguments>{args}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        user = xml_escape(user),
        conhost = xml_escape(&conhost),
        args = xml_escape(&args),
    )
}

/// DOMAIN\user of whoever runs this.
fn user() -> Result<String> {
    let name = std::env::var("USERNAME").context("USERNAME isn't set")?;
    Ok(match std::env::var("USERDOMAIN") {
        Ok(d) if !d.is_empty() => format!(r"{d}\{name}"),
        _ => name,
    })
}

fn powershell(script: &str, env: &[(&str, &str)]) -> Result<std::process::Output> {
    let exe = std::env::var_os("SystemRoot").map_or_else(
        || "powershell".to_string(),
        |r| {
            Path::new(&r)
                .join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
                .display()
                .to_string()
        },
    );
    Command::new(exe)
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .envs(env.iter().copied())
        .output()
        .context("PowerShell isn't available")
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

fn no_system() -> Result<ExitCode> {
    bail!(
        "a machine-wide service isn't available on Windows: `backdown service install` sets BackDown up for you (it runs while you're logged on)"
    )
}

pub fn install(exe: &Path, system: bool, start: bool, print: bool) -> Result<ExitCode> {
    if system {
        return no_system();
    }
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let xml = task_xml(exe, &user().unwrap_or_else(|_| "%USERNAME%".into()), &root);
    if print {
        print!("{xml}");
        return Ok(ExitCode::SUCCESS);
    }
    let out = powershell(
        "$ErrorActionPreference = 'Stop'\n\
         Register-ScheduledTask -TaskName 'BackDown' -Xml $env:BD_TASK_XML -Force | Out-Null\n\
         if ($env:BD_START -eq '1') { Start-ScheduledTask -TaskName 'BackDown' }",
        &[
            ("BD_TASK_XML", &xml),
            ("BD_START", if start { "1" } else { "0" }),
        ],
    )?;
    check(out, "registering the scheduled task")?;
    if start {
        eprintln!("backdown is running. Open it with `backdown open`.");
    } else {
        eprintln!("installed but not started: it starts the next time you log on");
    }
    eprintln!("It starts whenever you log on (Task Scheduler, task \"{NAME}\").");
    Ok(ExitCode::SUCCESS)
}

/// Start the registered task now, if there is one (`backdown app`). False when there's none.
pub fn start_installed() -> bool {
    powershell(
        "if (-not (Get-ScheduledTask -TaskName 'BackDown' -ErrorAction SilentlyContinue)) { exit 3 }\n\
         Start-ScheduledTask -TaskName 'BackDown'",
        &[],
    )
    .is_ok_and(|o| o.status.success())
}

pub fn uninstall(system: bool) -> Result<ExitCode> {
    if system {
        return no_system();
    }
    let exe = dunce::canonicalize(std::env::current_exe()?)?;
    let exe = exe.display().to_string();
    let pid = std::process::id().to_string();
    // stopping the task ends conhost; the daemon it started is ended too (never this process)
    let out = powershell(
        "$t = Get-ScheduledTask -TaskName 'BackDown' -ErrorAction SilentlyContinue\n\
         if (-not $t) { exit 3 }\n\
         Stop-ScheduledTask -TaskName 'BackDown' -ErrorAction SilentlyContinue\n\
         Get-Process -Name backdown -ErrorAction SilentlyContinue |\n\
           Where-Object { $_.Id -ne [int]$env:BD_SELF -and $_.Path -eq $env:BD_EXE } |\n\
           Stop-Process -Force -ErrorAction SilentlyContinue\n\
         Unregister-ScheduledTask -TaskName 'BackDown' -Confirm:$false -ErrorAction Stop",
        &[("BD_EXE", &exe), ("BD_SELF", &pid)],
    )?;
    if out.status.code() == Some(3) {
        eprintln!("no scheduled task \"{NAME}\" installed");
        return Ok(ExitCode::SUCCESS);
    }
    check(out, "removing the scheduled task")?;
    eprintln!(
        "removed the scheduled task \"{NAME}\". The index is kept in {}.",
        crate::locations::state_dir().display()
    );
    Ok(ExitCode::SUCCESS)
}

/// `Get-ScheduledTask`'s answer: the task's state (Ready, Running, Disabled) and last result.
pub fn parse_status(json: &str) -> Option<(String, Option<i64>)> {
    let v: Value = serde_json::from_str(json.trim()).ok()?;
    let state = match &v["State"] {
        Value::String(s) => s.clone(),
        // numeric: TASK_STATE (0 unknown, 1 disabled, 2 queued, 3 ready, 4 running)
        Value::Number(n) => match n.as_u64() {
            Some(1) => "Disabled",
            Some(2) => "Queued",
            Some(3) => "Ready",
            Some(4) => "Running",
            _ => "Unknown",
        }
        .to_string(),
        _ => return None,
    };
    Some((state, v["LastTaskResult"].as_i64()))
}

pub fn status(system: bool) -> Result<ExitCode> {
    if system {
        return no_system();
    }
    let out = powershell(
        "$t = Get-ScheduledTask -TaskName 'BackDown' -ErrorAction SilentlyContinue\n\
         if ($t) { $i = $t | Get-ScheduledTaskInfo\n\
           [pscustomobject]@{ State = [string]$t.State; LastTaskResult = $i.LastTaskResult } | ConvertTo-Json -Compress }",
        &[],
    )?;
    let parsed = parse_status(&String::from_utf8_lossy(&out.stdout));
    match &parsed {
        Some((state, last)) => println!(
            "scheduled task \"{NAME}\": {state}{}",
            last.filter(|c| *c != 0 && *c != 0x41301)
                .map(|c| format!(" (last result 0x{c:x})"))
                .unwrap_or_default()
        ),
        None => println!("scheduled task \"{NAME}\": not installed"),
    }
    super::print_daemon()?;
    Ok(if parsed.is_some_and(|(s, _)| s == "Running") {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_task_definition() {
        let x = task_xml(
            Path::new(r"C:\Users\someone\AppData\Local\Programs\BackDown\backdown.exe"),
            r"EXAMPLE\someone",
            r"C:\Windows\",
        );
        // well-formed, with what Task Scheduler needs where it expects it
        use quick_xml::events::Event;
        let mut r = quick_xml::Reader::from_str(&x);
        let mut path: Vec<String> = Vec::new();
        let mut text = String::new();
        let mut found: Vec<(String, String)> = Vec::new();
        loop {
            match r.read_event().unwrap() {
                Event::Start(e) => {
                    path.push(String::from_utf8_lossy(e.local_name().as_ref()).into_owned());
                    text.clear();
                }
                Event::Text(t) => text.push_str(&t.decode().unwrap()),
                Event::GeneralRef(g) => text.push(match g.decode().unwrap().as_ref() {
                    "quot" => '"',
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "apos" => '\'',
                    other => panic!("unexpected &{other};"),
                }),
                Event::End(_) => {
                    if !text.trim().is_empty() {
                        found.push((path.join("/"), std::mem::take(&mut text)));
                    }
                    text.clear();
                    path.pop();
                }
                Event::Eof => break,
                _ => {}
            }
        }
        let get = |p: &str| {
            found
                .iter()
                .find(|(k, _)| k == p)
                .map(|(_, v)| v.as_str())
                .unwrap_or_else(|| panic!("no {p}"))
        };
        assert_eq!(get("Task/Triggers/LogonTrigger/UserId"), r"EXAMPLE\someone");
        assert_eq!(
            get("Task/Principals/Principal/LogonType"),
            "InteractiveToken"
        );
        assert_eq!(get("Task/Principals/Principal/RunLevel"), "LeastPrivilege");
        assert_eq!(get("Task/Settings/ExecutionTimeLimit"), "PT0S");
        assert_eq!(get("Task/Settings/MultipleInstancesPolicy"), "IgnoreNew");
        assert_eq!(
            get("Task/Actions/Exec/Command"),
            r"C:\Windows\System32\conhost.exe"
        );
        assert_eq!(
            get("Task/Actions/Exec/Arguments"),
            r#"--headless "C:\Users\someone\AppData\Local\Programs\BackDown\backdown.exe" daemon"#
        );
        assert!(x.contains("&quot;C:\\Users"));
    }

    #[test]
    fn task_status() {
        assert_eq!(
            parse_status(r#"{"State":"Running","LastTaskResult":267009}"#),
            Some(("Running".into(), Some(267_009)))
        );
        assert_eq!(
            parse_status(r#"{"State":3,"LastTaskResult":0}"#),
            Some(("Ready".into(), Some(0)))
        );
        assert_eq!(parse_status(""), None);
    }
}
