//! macOS: `diskutil info -plist` for every physical disk, and smartctl where it's installed.
//!
//! diskutil gives each disk's SMART status ("Verified", "Failing", or "Not Supported", which is
//! what most USB enclosures get), and on recent macOS an NVMe drive's health log as well, under
//! `SMARTDeviceSpecificKeysMayVaryNotGuaranteed` (spare, wear, media errors, power-on hours, the
//! temperature in kelvin; 128-bit counters split into `_0` and `_1` halves). As the key says,
//! Apple doesn't promise those, so each is used only when present. smartmontools (Homebrew or
//! MacPorts) reads NVMe drives without root and adds the rest; a LaunchAgent's PATH doesn't
//! include Homebrew's folders, so they're looked in too.

use serde_json::Value;

use super::{Health, Verdict};

/// Fill `h` from one `diskutil info -plist` document (as JSON: see `volumes::macos::plist`).
pub fn from_info(h: &mut Health, info: &Value) {
    let status = info["SMARTStatus"].as_str().unwrap_or("").trim();
    let lower = status.to_ascii_lowercase();
    if lower == "verified" {
        h.reported = Some(Verdict::Ok);
        h.reported_as = Some(status.to_string());
    } else if lower.contains("fail") {
        h.reported = Some(Verdict::Failing);
        h.reported_as = Some(status.to_string());
    } else if lower.contains("not supported") {
        h.add_note(if h.transport.as_deref() == Some("usb") {
            "macOS gets no health data from it (usual for USB enclosures)"
        } else {
            "macOS gets no health data from it"
        });
    }
    if h.model.is_none() {
        h.model = info["MediaName"]
            .as_str()
            .map(|m| m.trim().trim_end_matches(" Media").to_string())
            .filter(|m| !m.is_empty());
    }
    let keys = &info["SMARTDeviceSpecificKeysMayVaryNotGuaranteed"];
    let n = |k: &str| keys[k].as_u64();
    if keys.as_object().is_some_and(|k| !k.is_empty()) {
        h.protocol = Some("nvme".into());
        h.available_spare = n("AVAILABLE_SPARE");
        h.spare_threshold = n("AVAILABLE_SPARE_THRESHOLD");
        h.percentage_used = n("PERCENTAGE_USED");
        h.media_errors = n("MEDIA_ERRORS_0");
        h.unsafe_shutdowns = n("UNSAFE_SHUTDOWNS_0");
        h.power_on_hours = n("POWER_ON_HOURS_0").or(h.power_on_hours);
        h.power_cycles = n("POWER_CYCLES_0").or(h.power_cycles);
        h.critical_warning = n("CRITICAL_WARNING");
        // the NVMe composite temperature is in kelvin; some versions give °C
        h.temperature = n("TEMPERATURE")
            .filter(|t| *t > 0)
            .map(|t| {
                if t > 200 {
                    (t as f64 - 273.15).round() as i64
                } else {
                    t as i64
                }
            })
            .or(h.temperature);
    }
    if h.reported.is_some() || h.protocol.is_some() {
        h.add_source("diskutil");
    }
}

/// smartctl, wherever it was installed (a LaunchAgent's PATH has only the system's folders).
#[cfg(target_os = "macos")]
fn smartctl_path() -> Option<String> {
    super::which("smartctl")
        .or_else(|| {
            [
                "/opt/homebrew/bin/smartctl",
                "/usr/local/bin/smartctl",
                "/usr/local/sbin/smartctl",
                "/opt/local/sbin/smartctl",
            ]
            .iter()
            .map(std::path::PathBuf::from)
            .find(|p| p.is_file())
        })
        .map(|p| p.display().to_string())
}

#[cfg(target_os = "macos")]
pub fn read(drives: &mut [Health]) -> Vec<String> {
    use std::time::Duration;
    let mut notes = Vec::new();
    let smartctl = smartctl_path();
    for h in drives.iter_mut() {
        let id = h.device.trim_start_matches("/dev/").to_string();
        match super::run(
            "/usr/sbin/diskutil",
            &["info", "-plist", &id],
            &[],
            Duration::from_secs(30),
        ) {
            Ok((Some(0), out)) => {
                if let Ok(v) = crate::volumes::macos::plist(&out) {
                    from_info(h, &v);
                }
            }
            _ => h.add_note("diskutil didn't answer for it"),
        }
        if let Some(bin) = &smartctl
            && let Ok((_, out)) = super::run(
                bin,
                &["-j", "-a", "-n", "standby", &id],
                &[],
                Duration::from_secs(30),
            )
            && let Ok(v) = serde_json::from_slice::<Value>(&out)
        {
            let had = h.has_data();
            match super::smartctl::fill(h, &v) {
                Err(super::smartctl::Problem::Permission) if !had => {
                    h.add_note(format!("needs administrator rights: sudo smartctl -a {id}"))
                }
                _ => {}
            }
        }
    }
    if smartctl.is_none() && drives.iter().any(|h| !h.has_data()) {
        notes.push(
            "smartmontools isn't installed: `brew install smartmontools` lets BackDown read more drives (NVMe fully; USB where the enclosure allows)"
                .into(),
        );
    }
    notes
}

#[cfg(not(target_os = "macos"))]
pub fn read(_: &mut [Health]) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::volumes::macos::plist;

    #[test]
    fn internal_nvme_with_its_health_log() {
        let v = plist(include_bytes!("testdata/diskutil-info-nvme.plist")).unwrap();
        let mut h = Health::default();
        from_info(&mut h, &v);
        assert_eq!(h.reported, Some(Verdict::Ok));
        assert_eq!(h.reported_as.as_deref(), Some("Verified"));
        assert_eq!(h.model.as_deref(), Some("FAKEAPPLE SSD AF0500"));
        assert_eq!(h.protocol.as_deref(), Some("nvme"));
        assert_eq!(h.available_spare, Some(100));
        assert_eq!(h.spare_threshold, Some(99));
        assert_eq!(h.percentage_used, Some(3));
        assert_eq!(h.media_errors, Some(0));
        assert_eq!(h.power_on_hours, Some(2_311));
        assert_eq!(h.power_cycles, Some(402));
        assert_eq!(h.unsafe_shutdowns, Some(12));
        assert_eq!(h.critical_warning, Some(0));
        assert_eq!(h.temperature, Some(33));
        assert_eq!(h.source, "diskutil");
        assert_eq!(super::super::assess(&h).verdict, Verdict::Ok);
    }

    #[test]
    fn usb_disk_without_smart_and_a_failing_one() {
        // the volume prober's own sample: a USB disk macOS gets nothing from
        let v = plist(include_bytes!(
            "../volumes/testdata/diskutil-info-disk4.plist"
        ))
        .unwrap();
        let mut h = Health {
            transport: Some("usb".into()),
            ..Health::default()
        };
        from_info(&mut h, &v);
        assert_eq!(h.reported, None);
        assert!(h.note.as_deref().unwrap().contains("USB"));
        assert!(!h.has_data());
        let v = plist(
            br#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
            <key>DeviceIdentifier</key><string>disk2</string><key>MediaName</key><string>FAKEDISK HD2000 Media</string>
            <key>SMARTStatus</key><string>Failing</string></dict></plist>"#,
        )
        .unwrap();
        let mut h = Health::default();
        from_info(&mut h, &v);
        assert_eq!(h.reported, Some(Verdict::Failing));
        assert_eq!(h.model.as_deref(), Some("FAKEDISK HD2000"));
        assert_eq!(super::super::assess(&h).verdict, Verdict::Failing);
    }
}
