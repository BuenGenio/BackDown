//! `smartctl -j -a`: smartmontools' JSON, the richest source where it's installed and allowed to
//! open the device (NVMe on macOS usually is; on Linux, only root or a `disk` group member).
//!
//! BackDown always passes `-n standby`, so a spun-down disk is left asleep: smartctl then says so
//! and exits with bit 1 set, and that round simply has no reading for it.

use serde_json::Value;

use super::{Health, Verdict};

/// Why smartctl gave nothing (useful).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The device node can't be opened by this user.
    Permission,
    /// The drive is spun down; it wasn't woken.
    Asleep,
    /// The drive (or its USB bridge) doesn't pass SMART on.
    Unsupported(String),
    Other(String),
}

/// The attribute's raw value as smartctl prints it: the leading number of its string form
/// (vendors pack more into the 48 raw bits; smartctl knows how to show the count), else the
/// low 32 bits of the raw number.
fn raw(attr: &Value) -> Option<u64> {
    let s = attr["raw"]["string"].as_str().unwrap_or("");
    let digits: String = s.trim().chars().take_while(char::is_ascii_digit).collect();
    digits
        .parse()
        .ok()
        .or_else(|| attr["raw"]["value"].as_u64().map(|v| v & 0xFFFF_FFFF))
}

fn messages(v: &Value) -> Vec<String> {
    v["smartctl"]["messages"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| m["string"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Fill `h` from one `smartctl -j -a` document. Returns what went wrong when it has no health data.
pub fn fill(h: &mut Health, v: &Value) -> Result<(), Problem> {
    let msgs = messages(v);
    let exit = v["smartctl"]["exit_status"].as_u64().unwrap_or(0);
    let said = |needle: &str| {
        msgs.iter().any(|m| {
            m.to_ascii_lowercase()
                .contains(&needle.to_ascii_lowercase())
        })
    };
    if said("permission denied") || said("operation not permitted") || said("access is denied") {
        return Err(Problem::Permission);
    }
    if said("standby mode") || said("sleep mode") {
        return Err(Problem::Asleep);
    }
    // identity, where the volume probe had none
    let text = |k: &str| {
        v[k].as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    if h.model.is_none() {
        h.model = text("model_name").or_else(|| text("scsi_model_name"));
    }
    if h.serial.is_none() {
        h.serial = text("serial_number");
    }
    if let (Some(naa), Some(oui), Some(id)) = (
        v["wwn"]["naa"].as_u64(),
        v["wwn"]["oui"].as_u64(),
        v["wwn"]["id"].as_u64(),
    ) {
        h.wwn = Some(format!("0x{naa:x}{oui:06x}{id:09x}"));
    }
    let protocol = v["device"]["protocol"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut any = false;
    if let Some(passed) = v["smart_status"]["passed"].as_bool() {
        h.reported = Some(if passed {
            Verdict::Ok
        } else {
            Verdict::Failing
        });
        h.reported_as = Some(if passed { "PASSED" } else { "FAILED!" }.into());
        any = true;
    }
    if let Some(t) = v["temperature"]["current"].as_i64() {
        h.temperature = Some(t);
    }
    if let Some(n) = v["power_on_time"]["hours"].as_u64() {
        h.power_on_hours = Some(n);
    }
    if let Some(n) = v["power_cycle_count"].as_u64() {
        h.power_cycles = Some(n);
    }
    match protocol.as_str() {
        "ata" => {
            h.protocol = Some("ata".into());
            let table = v["ata_smart_attributes"]["table"].as_array();
            let find =
                |id: u64| table.and_then(|t| t.iter().find(|a| a["id"].as_u64() == Some(id)));
            if let Some(a) = find(5) {
                h.reallocated = raw(a);
            }
            if let Some(a) = find(197) {
                h.pending = raw(a);
            }
            h.uncorrectable = find(198).or_else(|| find(187)).and_then(raw);
            if let Some(a) = find(199) {
                h.crc_errors = raw(a);
            }
            for a in table.into_iter().flatten() {
                // failing now, on an attribute that predicts failure (not merely "old age")
                if a["when_failed"].as_str() == Some("now")
                    && a["flags"]["prefailure"].as_bool() != Some(false)
                {
                    h.failing_attributes
                        .push(a["name"].as_str().unwrap_or("?").replace('_', " "));
                }
            }
            any |= table.is_some();
        }
        "nvme" => {
            h.protocol = Some("nvme".into());
            let log = &v["nvme_smart_health_information_log"];
            if log.is_object() {
                h.critical_warning = log["critical_warning"].as_u64();
                h.available_spare = log["available_spare"].as_u64();
                h.spare_threshold = log["available_spare_threshold"].as_u64();
                h.percentage_used = log["percentage_used"].as_u64();
                h.media_errors = log["media_errors"].as_u64();
                h.unsafe_shutdowns = log["unsafe_shutdowns"].as_u64();
                h.power_on_hours = h.power_on_hours.or(log["power_on_hours"].as_u64());
                h.power_cycles = h.power_cycles.or(log["power_cycles"].as_u64());
                h.temperature = h.temperature.or(log["temperature"].as_i64());
                any = true;
            }
        }
        "scsi" => {
            h.protocol = Some("scsi".into());
            if let Some(n) = v["scsi_grown_defect_list"].as_u64() {
                h.reallocated = Some(n);
                any = true;
            }
            let log = &v["scsi_error_counter_log"];
            let unc: Vec<u64> = ["read", "write", "verify"]
                .iter()
                .filter_map(|k| log[*k]["total_uncorrected_errors"].as_u64())
                .collect();
            if !unc.is_empty() {
                h.uncorrectable = Some(unc.iter().sum());
                any = true;
            }
        }
        _ => {}
    }
    if any {
        h.add_source("smartctl");
        return Ok(());
    }
    if exit & 1 != 0 || exit & 2 != 0 || said("unknown usb bridge") || said("unsupported") {
        let why = msgs
            .iter()
            .find(|m| !m.to_ascii_lowercase().contains("please specify"))
            .or(msgs.first())
            .cloned()
            .unwrap_or_else(|| format!("smartctl exited with {exit}"));
        return Err(Problem::Unsupported(why));
    }
    Err(Problem::Other(
        msgs.first()
            .cloned()
            .unwrap_or_else(|| "smartctl found no health data".into()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> (Health, Result<(), Problem>) {
        let v: Value = serde_json::from_str(s).unwrap();
        let mut h = Health::default();
        let r = fill(&mut h, &v);
        (h, r)
    }

    #[test]
    fn ata_disk_with_bad_sectors() {
        let (h, r) = parse(include_str!("testdata/smartctl-ata.json"));
        assert_eq!(r, Ok(()));
        assert_eq!(h.protocol.as_deref(), Some("ata"));
        assert_eq!(h.model.as_deref(), Some("FAKEDISK FD40EZRX-00ABCD0"));
        assert_eq!(h.serial.as_deref(), Some("FD-WX00A1234567"));
        // naa 5, oui 0x0014ee, id 0xabde: the 16 hex digits `smartctl -i` prints as "5 0014ee 00000abde"
        assert_eq!(h.wwn.as_deref(), Some("0x50014ee00000abde"));
        assert_eq!(h.reported, Some(Verdict::Ok));
        assert_eq!(h.temperature, Some(38));
        assert_eq!(h.power_on_hours, Some(31_202));
        assert_eq!(h.power_cycles, Some(1_417));
        assert_eq!(h.reallocated, Some(8));
        assert_eq!(h.pending, Some(2));
        assert_eq!(h.uncorrectable, Some(1));
        assert_eq!(h.crc_errors, Some(14));
        assert!(h.failing_attributes.is_empty());
        assert_eq!(h.source, "smartctl");
    }

    #[test]
    fn nvme_health_log() {
        let (h, r) = parse(include_str!("testdata/smartctl-nvme.json"));
        assert_eq!(r, Ok(()));
        assert_eq!(h.protocol.as_deref(), Some("nvme"));
        assert_eq!(h.critical_warning, Some(0));
        assert_eq!(h.available_spare, Some(100));
        assert_eq!(h.spare_threshold, Some(10));
        assert_eq!(h.percentage_used, Some(7));
        assert_eq!(h.media_errors, Some(0));
        assert_eq!(h.unsafe_shutdowns, Some(41));
        assert_eq!(h.temperature, Some(36));
        assert_eq!(h.power_on_hours, Some(5_120));
        assert_eq!(h.reported_as.as_deref(), Some("PASSED"));
    }

    #[test]
    fn failing_disk_says_so() {
        let (h, r) = parse(include_str!("testdata/smartctl-ata-failing.json"));
        assert_eq!(r, Ok(()));
        assert_eq!(h.reported, Some(Verdict::Failing));
        assert_eq!(h.failing_attributes, ["Reallocated Sector Ct"]);
        assert_eq!(h.reallocated, Some(2_008));
        assert_eq!(super::super::assess(&h).verdict, Verdict::Failing);
    }

    #[test]
    fn problems() {
        let (_, r) = parse(
            r#"{"smartctl":{"exit_status":2,"messages":[{"string":"Smartctl open device: /dev/sdb failed: Permission denied","severity":"error"}]}}"#,
        );
        assert_eq!(r, Err(Problem::Permission));
        let (_, r) = parse(
            r#"{"smartctl":{"exit_status":2,"messages":[{"string":"Device is in STANDBY mode, exit(2)","severity":"information"}]}}"#,
        );
        assert_eq!(r, Err(Problem::Asleep));
        let (_, r) = parse(
            r#"{"smartctl":{"exit_status":1,"messages":[{"string":"/dev/sdc: Unknown USB bridge [0x1234:0x5678 (0x100)]","severity":"error"},{"string":"Please specify device type with the -d option.","severity":"error"}]}}"#,
        );
        assert!(matches!(r, Err(Problem::Unsupported(m)) if m.contains("Unknown USB bridge")));
    }
}
