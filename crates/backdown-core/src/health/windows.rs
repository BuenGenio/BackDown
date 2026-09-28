//! Windows: the Storage module, through one PowerShell run.
//!
//! `Get-PhysicalDisk` gives every user each disk's HealthStatus (Healthy, Warning, Unhealthy) and
//! OperationalStatus (OK, Degraded, Predictive Failure, …), which Windows derives from SMART and
//! the storage stack. `Get-StorageReliabilityCounter` adds temperature, wear, power-on hours and
//! uncorrected read and write errors, but only for an administrator on most systems; without one
//! it fails or comes back empty, and the record says so. Windows has no reallocated, pending or
//! CRC counters to offer: those need smartctl as an administrator.
//!
//! As for the volume probe, the script turns enums into strings itself (so Windows PowerShell 5.1
//! and PowerShell 7 give the same JSON), and the parser still accepts their numeric forms.

use serde_json::Value;

use super::{Health, Verdict};

/// The disks' health in one go, as compact JSON on stdout.
pub const SCRIPT: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$disks = @(Get-PhysicalDisk | ForEach-Object {
  $d = $_
  $r = $null; $err = $null
  try { $r = $d | Get-StorageReliabilityCounter -ErrorAction Stop } catch { $err = $_.Exception.Message }
  $c = $null
  if ($r) { $c = [pscustomobject]@{
    Temperature = $r.Temperature; TemperatureMax = $r.TemperatureMax; Wear = $r.Wear; PowerOnHours = $r.PowerOnHours
    ReadErrorsTotal = $r.ReadErrorsTotal; ReadErrorsUncorrected = $r.ReadErrorsUncorrected
    WriteErrorsUncorrected = $r.WriteErrorsUncorrected; StartStopCycleCount = $r.StartStopCycleCount } }
  [pscustomobject]@{
    DeviceId = [string]$d.DeviceId; FriendlyName = $d.FriendlyName; Model = $d.Model; SerialNumber = $d.SerialNumber
    BusType = [string]$d.BusType; MediaType = [string]$d.MediaType; Size = $d.Size
    HealthStatus = [string]$d.HealthStatus; OperationalStatus = @($d.OperationalStatus | ForEach-Object { [string]$_ })
    Counters = $c; CounterError = $err } })
[pscustomobject]@{ Admin = $admin; Disks = $disks } | ConvertTo-Json -Depth 5 -Compress
"#;

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// A lone object where a list was meant (Windows PowerShell unwraps one-element arrays).
fn items(v: &Value) -> Vec<&Value> {
    match v {
        Value::Array(a) => a.iter().collect(),
        Value::Null => Vec::new(),
        other => vec![other],
    }
}

/// HealthStatus as a verdict: names, or the numbers of MSFT_PhysicalDisk.HealthStatus.
fn health_status(s: &str) -> Option<Verdict> {
    match s {
        "Healthy" | "0" => Some(Verdict::Ok),
        "Warning" | "1" => Some(Verdict::Warning),
        "Unhealthy" | "2" => Some(Verdict::Failing),
        _ => None,
    }
}

/// The worst of OperationalStatus, as a verdict and in Windows' words.
fn operational(list: &[String]) -> Option<(Verdict, String)> {
    list.iter()
        .filter_map(|s| {
            let v = match s.as_str() {
                "Predictive Failure"
                | "5"
                | "Failed Media"
                | "53266"
                | "Error"
                | "6"
                | "Non-Recoverable Error"
                | "7"
                | "IO Error"
                | "53269" => Verdict::Failing,
                "Degraded" | "3" | "Stressed" | "4" | "Transient Error" | "53271" => {
                    Verdict::Warning
                }
                _ => return None,
            };
            let name = match s.as_str() {
                "5" => "Predictive Failure",
                "53266" => "Failed Media",
                "6" => "Error",
                "7" => "Non-Recoverable Error",
                "53269" => "IO Error",
                "3" => "Degraded",
                "4" => "Stressed",
                "53271" => "Transient Error",
                other => other,
            };
            Some((v, name.to_string()))
        })
        .max_by_key(|(v, _)| v.rank())
}

/// Fill the records from the script's output; returns notes about the probe itself.
pub fn from_json(v: &Value, drives: &mut [Health]) -> Vec<String> {
    let admin = v["Admin"] == true;
    let mut counters_hidden = false;
    for d in items(&v["Disks"]) {
        let n = text(&d["DeviceId"]);
        let serial = text(&d["SerialNumber"]);
        let id = super::identity(Some(&serial), None);
        let Some(h) = drives.iter_mut().find(|h| {
            h.device
                .eq_ignore_ascii_case(&format!(r"\\.\PHYSICALDRIVE{n}"))
                || (!id.is_empty() && super::identity(h.serial.as_deref(), None) == id)
        }) else {
            continue;
        };
        if h.serial.is_none() && !serial.is_empty() {
            h.serial = Some(serial.clone());
        }
        if h.model.is_none() {
            h.model = Some(text(&d["FriendlyName"])).filter(|s| !s.is_empty());
        }
        let status = text(&d["HealthStatus"]);
        let ops: Vec<String> = items(&d["OperationalStatus"])
            .into_iter()
            .map(text)
            .collect();
        let reported = health_status(&status);
        let op = operational(&ops);
        let (verdict, words) = match (reported, op) {
            (r, Some((v, w))) if r.is_none_or(|r| v.rank() > r.rank()) => (Some(v), w),
            (r, _) => (r, status.clone()),
        };
        if verdict.is_some() {
            h.reported = verdict;
            h.reported_as = Some(words);
            h.add_source("Windows");
        }
        let nvme = text(&d["BusType"]).eq_ignore_ascii_case("NVMe") || text(&d["BusType"]) == "17";
        if nvme {
            h.protocol = Some("nvme".into());
        }
        let ssd = nvme
            || matches!(text(&d["MediaType"]).as_str(), "SSD" | "4" | "SCM" | "5")
            || h.rotational == Some(false);
        let c = &d["Counters"];
        let num = |k: &str| c[k].as_u64();
        let got = c.is_object()
            && [
                "Temperature",
                "Wear",
                "PowerOnHours",
                "ReadErrorsUncorrected",
            ]
            .iter()
            .any(|k| !c[*k].is_null());
        if got {
            h.temperature = num("Temperature").filter(|t| *t > 0).map(|t| t as i64);
            h.power_on_hours = num("PowerOnHours").or(h.power_on_hours);
            if ssd {
                h.percentage_used = num("Wear");
            }
            let unc = [num("ReadErrorsUncorrected"), num("WriteErrorsUncorrected")];
            if unc.iter().any(Option::is_some) {
                let total = unc.iter().flatten().sum();
                if nvme {
                    h.media_errors = Some(total);
                } else {
                    h.uncorrectable = Some(total);
                }
            }
            h.add_source("Windows");
        } else if !admin {
            counters_hidden = true;
            h.add_note(
                "Windows shares this drive's error counters only with an administrator (an administrator terminal's `backdown health` reads them once)",
            );
        }
    }
    let mut notes = Vec::new();
    if counters_hidden {
        notes.push("Windows reports each drive's overall health to everyone, its counters (temperature, wear, errors) only to an administrator".into());
    }
    notes
}

#[cfg(windows)]
pub fn read(drives: &mut [Health]) -> Vec<String> {
    use base64::Engine;
    let utf16: Vec<u8> = SCRIPT.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    let powershell = std::env::var_os("SystemRoot")
        .map(|r| {
            std::path::Path::new(&r)
                .join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
                .display()
                .to_string()
        })
        .unwrap_or_else(|| "powershell".into());
    match super::run(
        &powershell,
        &["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded],
        &[],
        std::time::Duration::from_secs(90),
    ) {
        Ok((_, out)) => {
            let out = out.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&out);
            match serde_json::from_slice::<Value>(out) {
                Ok(v) => from_json(&v, drives),
                Err(_) => vec!["PowerShell's storage cmdlets gave nothing readable".into()],
            }
        }
        Err(e) => vec![format!("can't ask Windows about drive health: {e:#}")],
    }
}

#[cfg(not(windows))]
pub fn read(_: &mut [Health]) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drives() -> Vec<Health> {
        (0..4)
            .map(|n| Health {
                device: format!(r"\\.\PHYSICALDRIVE{n}"),
                name: format!("Disk {n}"),
                ..Health::default()
            })
            .collect()
    }

    #[test]
    fn as_an_administrator() {
        let v: Value =
            serde_json::from_str(include_str!("testdata/windows-health-admin.json")).unwrap();
        let mut d = drives();
        let notes = from_json(&v, &mut d);
        assert!(notes.is_empty());
        let nvme = &d[0];
        assert_eq!(nvme.reported, Some(Verdict::Ok));
        assert_eq!(nvme.reported_as.as_deref(), Some("Healthy"));
        assert_eq!(nvme.protocol.as_deref(), Some("nvme"));
        assert_eq!(nvme.temperature, Some(41));
        assert_eq!(nvme.percentage_used, Some(4));
        assert_eq!(nvme.media_errors, Some(0));
        assert_eq!(nvme.power_on_hours, Some(8_812));
        assert_eq!(nvme.serial.as_deref(), Some("FNV0_0000_0000_0001."));
        let hdd = &d[1];
        assert_eq!(hdd.reported, Some(Verdict::Failing));
        assert_eq!(hdd.reported_as.as_deref(), Some("Predictive Failure"));
        assert_eq!(hdd.percentage_used, None); // a spinning disk's "wear" means nothing
        assert_eq!(hdd.uncorrectable, Some(3));
        assert_eq!(super::super::assess(hdd).verdict, Verdict::Failing);
        // numeric enums (Windows PowerShell 5.1 without the casts) mean the same
        let usb = &d[2];
        assert_eq!(usb.reported, Some(Verdict::Warning));
        assert_eq!(usb.reported_as.as_deref(), Some("Degraded"));
        assert_eq!(d[3].reported, None);
    }

    #[test]
    fn without_an_administrator() {
        let v: Value =
            serde_json::from_str(include_str!("testdata/windows-health-user.json")).unwrap();
        let mut d = drives();
        let notes = from_json(&v, &mut d);
        assert_eq!(notes.len(), 1);
        assert_eq!(d[0].reported, Some(Verdict::Ok));
        assert_eq!(d[0].temperature, None);
        assert!(d[0].note.as_deref().unwrap().contains("administrator"));
        // one disk: PowerShell 5.1 gives a lone object, not a list
        assert_eq!(d[1].reported, None);
    }
}
