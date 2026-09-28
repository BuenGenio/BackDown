//! Linux: udisks2, smartctl where it's allowed, and hwmon in sysfs.
//!
//! udisks2 reads each drive's SMART data itself (as root, every few minutes, leaving sleeping disks
//! asleep) and shares it over the system D-Bus with any local user. `GetManagedObjects` on its
//! object manager returns every drive and block device at once: `Drive` (model, serial, WWN),
//! `Drive.Ata` (SmartFailing, SmartTemperature in kelvin, SmartPowerOnSeconds, SmartNumBadSectors,
//! SmartNumAttributesFailing) and, from udisks 2.10, `NVMe.Controller` (SmartCriticalWarning,
//! SmartTemperature, SmartPowerOnHours). `SmartGetAttributes` adds the ATA attribute table
//! (reallocated, pending, uncorrectable, CRC) or the NVMe health log (spare, wear, media errors);
//! it answers unprivileged callers from udisks' cache, and BackDown never lets it ask for a
//! password (`--allow-interactive-authorization=no`). Everything goes through `busctl --json`;
//! where busctl is missing (systems without systemd), `gdbus introspect` gives the properties as
//! text, parsed here, but not the attribute tables.
//!
//! `smartctl -j -a -n standby` is richer, and is tried where it's installed; it usually needs root
//! for SATA and USB disks, and says so, and then udisks' numbers stand. hwmon (NVMe controllers,
//! and SATA disks with the `drivetemp` module) fills in temperatures nothing else gave.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value, json};

use super::{Health, Verdict};

/// A drive as udisks describes it.
#[derive(Debug, Default, Clone)]
pub struct UDrive {
    /// its D-Bus object path
    pub object: String,
    /// whole-disk device nodes on it (/dev/sda; each namespace of an NVMe drive)
    pub devices: Vec<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub wwn: Option<String>,
    /// "ata" or "nvme" when udisks has SMART for it
    pub protocol: Option<&'static str>,
    /// udisks has read its SMART data at least once
    pub updated: bool,
    /// SMART as properties (reported, temperature, hours, bad sectors, critical warning)
    pub health: Health,
    /// ATA attributes the drive says are failing, when only their number is known
    pub failing: i64,
}

const DRIVE: &str = "org.freedesktop.UDisks2.Drive";
const ATA: &str = "org.freedesktop.UDisks2.Drive.Ata";
const NVME: &str = "org.freedesktop.UDisks2.NVMe.Controller";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const PARTITION: &str = "org.freedesktop.UDisks2.Partition";

/// A property's value in busctl's JSON (`{"type": "t", "data": 5}`).
fn prop<'a>(o: &'a Value, iface: &str, name: &str) -> &'a Value {
    &o[iface][name]["data"]
}

fn text(v: &Value) -> Option<String> {
    let s = match v {
        Value::String(s) => s.clone(),
        // `ay`: bytes, NUL-terminated
        Value::Array(a) => a
            .iter()
            .filter_map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()))
            .take_while(|b| *b != 0)
            .map(char::from)
            .collect(),
        _ => return None,
    };
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Kelvin to whole °C (0 K: unknown).
fn celsius(k: f64) -> Option<i64> {
    (k > 1.0).then(|| (k - 273.15).round() as i64)
}

/// NVMe critical-warning names (udisks) to the bits of the health log.
fn warning_bits(names: &Value) -> u64 {
    names.as_array().map_or(0, |a| {
        a.iter()
            .filter_map(Value::as_str)
            .map(|n| match n {
                "spare" => 0x01,
                "temperature" => 0x02,
                "degraded" => 0x04,
                "readonly" => 0x08,
                "volatile_mem" => 0x10,
                "pmr_readonly" => 0x20,
                _ => 0,
            })
            .fold(0, |a, b| a | b)
    })
}

/// Every drive in udisks' object tree: `{object: {interface: {property: {type, data}}}}`, as
/// `busctl --json` prints GetManagedObjects' reply (and [`gdbus_objects`] rebuilds it).
pub fn parse_objects(objects: &Map<String, Value>) -> Vec<UDrive> {
    let mut drives: Vec<UDrive> = Vec::new();
    for (path, o) in objects {
        if o.get(DRIVE).is_none() {
            continue;
        }
        let mut d = UDrive {
            object: path.clone(),
            model: text(prop(o, DRIVE, "Model")),
            serial: text(prop(o, DRIVE, "Serial")),
            wwn: text(prop(o, DRIVE, "WWN")),
            ..UDrive::default()
        };
        if let (Some(v), Some(m)) = (text(prop(o, DRIVE, "Vendor")), d.model.clone())
            && !m.to_lowercase().starts_with(&v.to_lowercase())
        {
            d.model = Some(format!("{v} {m}"));
        }
        let h = &mut d.health;
        if o.get(ATA).is_some() {
            let supported = prop(o, ATA, "SmartSupported").as_bool() == Some(true);
            let enabled = prop(o, ATA, "SmartEnabled").as_bool() == Some(true);
            d.updated = prop(o, ATA, "SmartUpdated").as_u64().unwrap_or(0) > 0;
            if supported {
                d.protocol = Some("ata");
            }
            if supported && enabled && d.updated {
                let failing = prop(o, ATA, "SmartFailing").as_bool() == Some(true);
                h.reported = Some(if failing {
                    Verdict::Failing
                } else {
                    Verdict::Ok
                });
                h.reported_as = Some(if failing { "failing" } else { "passed" }.into());
                h.temperature = prop(o, ATA, "SmartTemperature").as_f64().and_then(celsius);
                h.power_on_hours = prop(o, ATA, "SmartPowerOnSeconds")
                    .as_u64()
                    .filter(|s| *s > 0)
                    .map(|s| s / 3600);
                h.bad_sectors = prop(o, ATA, "SmartNumBadSectors")
                    .as_i64()
                    .and_then(|n| u64::try_from(n).ok());
                d.failing = prop(o, ATA, "SmartNumAttributesFailing")
                    .as_i64()
                    .unwrap_or(0);
            } else if supported && !enabled {
                h.note = Some("SMART is switched off on this drive".into());
            }
        }
        if o.get(NVME).is_some() {
            d.protocol = Some("nvme");
            d.updated = prop(o, NVME, "SmartUpdated").as_u64().unwrap_or(0) > 0;
            if d.updated {
                h.critical_warning = Some(warning_bits(prop(o, NVME, "SmartCriticalWarning")));
                h.temperature = prop(o, NVME, "SmartTemperature").as_f64().and_then(celsius);
                h.power_on_hours = prop(o, NVME, "SmartPowerOnHours").as_u64();
            }
        }
        drives.push(d);
    }
    // whole-disk block devices, by the drive they belong to
    for o in objects.values() {
        if o.get(BLOCK).is_none() || o.get(PARTITION).is_some() {
            continue;
        }
        let drive = prop(o, BLOCK, "Drive").as_str().unwrap_or("/");
        let backing = prop(o, BLOCK, "CryptoBackingDevice")
            .as_str()
            .unwrap_or("/");
        if drive == "/" || backing != "/" {
            continue;
        }
        if let (Some(d), Some(dev)) = (
            drives.iter_mut().find(|d| d.object == drive),
            text(prop(o, BLOCK, "Device")),
        ) {
            d.devices.push(dev);
        }
    }
    for d in &mut drives {
        d.devices.sort();
    }
    drives.sort_by(|a, b| a.object.cmp(&b.object));
    drives
}

/// The reply of `busctl --json=short call … GetManagedObjects`.
pub fn parse_busctl(reply: &Value) -> Vec<UDrive> {
    reply["data"][0]
        .as_object()
        .map(parse_objects)
        .unwrap_or_default()
}

/// ATA attributes from `SmartGetAttributes` (busctl's JSON of `a(ysqiiixia{sv})`): id, name,
/// flags, normalised value, worst, threshold, pretty value, its unit, and extras.
pub fn fill_ata_attributes(h: &mut Health, reply: &Value) -> bool {
    let Some(table) = reply["data"][0].as_array() else {
        return false;
    };
    let mut failing = Vec::new();
    let mut get: HashMap<u64, (u64, u64)> = HashMap::new(); // id -> (pretty, unit)
    for a in table {
        let Some(f) = a.as_array() else { continue };
        let (Some(id), Some(pretty), Some(unit)) = (
            f.first().and_then(Value::as_u64),
            f.get(6).and_then(Value::as_i64),
            f.get(7).and_then(Value::as_i64),
        ) else {
            continue;
        };
        if let Ok(p) = u64::try_from(pretty) {
            get.insert(id, (p, unit.max(0) as u64));
        }
        let flags = f.get(2).and_then(Value::as_u64).unwrap_or(0);
        let value = f.get(3).and_then(Value::as_i64).unwrap_or(-1);
        let thresh = f.get(5).and_then(Value::as_i64).unwrap_or(0);
        if flags & 1 != 0 && thresh > 0 && value >= 0 && value <= thresh {
            failing.push(
                f.get(1)
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .replace('-', " "),
            );
        }
    }
    // counts are in "sectors" (3) or plain numbers (1)
    let count = |id: u64| {
        get.get(&id)
            .filter(|(_, u)| matches!(u, 1 | 3))
            .map(|(p, _)| *p)
    };
    h.reallocated = count(5);
    h.pending = count(197);
    h.uncorrectable = count(198).or_else(|| count(187));
    h.crc_errors = count(199);
    if let Some((ms, 2)) = get.get(&9) {
        h.power_on_hours = h.power_on_hours.or(Some(ms / 3_600_000));
    }
    if let Some(n) = count(12) {
        h.power_cycles = Some(n);
    }
    h.failing_attributes = failing;
    if h.reallocated.is_some() || h.pending.is_some() {
        h.bad_sectors = None; // told apart now
    }
    h.protocol = Some("ata".into());
    true
}

/// The NVMe health log from `SmartGetAttributes` (busctl's JSON of `a{sv}`).
pub fn fill_nvme_attributes(h: &mut Health, reply: &Value) -> bool {
    let Some(m) = reply["data"][0].as_object() else {
        return false;
    };
    let n = |k: &str| m.get(k).and_then(|v| v["data"].as_u64());
    h.available_spare = n("avail_spare");
    h.spare_threshold = n("spare_thresh");
    h.percentage_used = n("percent_used");
    h.media_errors = n("media_errors");
    h.unsafe_shutdowns = n("unsafe_shutdowns");
    h.power_cycles = n("power_cycles").or(h.power_cycles);
    h.protocol = Some("nvme".into());
    true
}

// ---------- gdbus, where busctl isn't installed

/// `gdbus introspect --recurse --only-properties` as text, turned into the same
/// `{object: {interface: {property: {type, data}}}}` shape busctl gives. Values that aren't
/// strings, numbers, booleans or arrays of those (dicts, tuples) come out as null.
pub fn gdbus_objects(text: &str) -> Map<String, Value> {
    let mut out = Map::new();
    let mut node: Option<String> = None;
    let mut iface: Option<String> = None;
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("node ") {
            let path = rest.trim_end_matches('{').trim().to_string();
            out.entry(path.clone()).or_insert_with(|| json!({}));
            node = Some(path);
            iface = None;
        } else if let Some(rest) = l.strip_prefix("interface ") {
            let name = rest.trim_end_matches('{').trim().to_string();
            if let Some(n) = &node {
                out[n][&name] = json!({});
            }
            iface = Some(name);
        } else if let Some(rest) = l.strip_prefix("readonly ").or(l.strip_prefix("readwrite ")) {
            let (Some(n), Some(i)) = (&node, &iface) else {
                continue;
            };
            let Some((sig, rest)) = rest.split_once(' ') else {
                continue;
            };
            let Some((name, value)) = rest.split_once(" = ") else {
                continue;
            };
            let value = value.trim().trim_end_matches(';');
            out[n][i][name.trim()] = json!({ "type": sig, "data": gvariant(value) });
        }
    }
    out
}

/// One GVariant value in its text form: 'string', b'bytes', 42, 3.5, true, [..] (typed forms
/// like `uint64 5`, `objectpath '/x'`, `@as []` included).
fn gvariant(s: &str) -> Value {
    let s = s.trim();
    let s = s
        .strip_prefix('@')
        .map_or(s, |r| r.split_once(' ').map_or(r, |(_, v)| v));
    for prefix in [
        "byte ",
        "int16 ",
        "uint16 ",
        "int32 ",
        "uint32 ",
        "int64 ",
        "uint64 ",
        "handle ",
        "objectpath ",
        "signature ",
    ] {
        if let Some(r) = s.strip_prefix(prefix) {
            return gvariant(r);
        }
    }
    if s == "true" || s == "false" {
        return Value::Bool(s == "true");
    }
    let s = s
        .strip_prefix('b')
        .filter(|r| r.starts_with(['\'', '"']))
        .unwrap_or(s);
    for q in ['\'', '"'] {
        if let Some(inner) = s.strip_prefix(q).and_then(|r| r.strip_suffix(q)) {
            return Value::String(unquote(inner));
        }
    }
    if let Some(inner) = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        return Value::Array(split_top(inner).into_iter().map(gvariant).collect());
    }
    if let Some(hex) = s.strip_prefix("0x") {
        return u64::from_str_radix(hex, 16).map_or(Value::Null, Value::from);
    }
    if let Ok(n) = s.parse::<i64>() {
        return Value::from(n);
    }
    if let Ok(n) = s.parse::<u64>() {
        return Value::from(n);
    }
    if let Ok(f) = s.parse::<f64>() {
        return Value::from(f);
    }
    Value::Null
}

fn unquote(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Split an array's inside at its top-level commas (not inside quotes or brackets).
fn split_top(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut quote, mut start) = (0i32, None::<char>, 0);
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '[' | '(' | '{' => depth += 1,
            ']' | ')' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    if !s[start..].trim().is_empty() {
        out.push(s[start..].trim());
    }
    out
}

// ---------- putting it together

/// Copy what udisks knows about a drive into its record.
pub fn apply(h: &mut Health, d: &UDrive) {
    if h.model.is_none() {
        h.model = d.model.clone();
    }
    if h.serial.is_none() {
        h.serial = d.serial.clone();
    }
    if h.wwn.is_none() {
        h.wwn = d.wwn.clone();
    }
    let u = &d.health;
    if d.updated {
        h.protocol = d.protocol.map(str::to_string);
        h.reported = u.reported;
        h.reported_as = u.reported_as.clone();
        h.temperature = u.temperature.or(h.temperature);
        h.power_on_hours = u.power_on_hours.or(h.power_on_hours);
        h.bad_sectors = u.bad_sectors;
        h.critical_warning = u.critical_warning;
        if d.failing > 0 && h.failing_attributes.is_empty() {
            h.failing_attributes = vec![format!("{} attributes", d.failing)];
        }
        h.add_source("udisks2");
    }
    if let Some(n) = &u.note {
        h.add_note(n.clone());
    }
}

/// The udisks drive a record is: by device node, else by serial or WWN.
pub fn matching<'a>(h: &Health, drives: &'a [UDrive]) -> Option<&'a UDrive> {
    drives
        .iter()
        .find(|d| d.devices.contains(&h.device))
        .or_else(|| {
            let id = super::identity(h.serial.as_deref(), None);
            if id.is_empty() {
                return None;
            }
            drives
                .iter()
                .find(|d| super::identity(d.serial.as_deref(), None) == id)
        })
}

/// A temperature from hwmon: NVMe controllers, and SATA disks with the `drivetemp` module.
pub fn hwmon_temperature(sys: &Path, name: &str) -> Option<i64> {
    let dev = sys.join("block").join(name).join("device");
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    for base in [dev.clone(), dev.join("hwmon")] {
        if let Ok(rd) = std::fs::read_dir(&base) {
            dirs.extend(rd.flatten().map(|e| e.path()).filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("hwmon"))
            }));
        }
    }
    dirs.sort();
    dirs.iter().find_map(|d| {
        let milli: i64 = std::fs::read_to_string(d.join("temp1_input"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        (milli > 0).then(|| (milli as f64 / 1000.0).round() as i64)
    })
}

/// Why a drive has no health data, and what would read it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn nothing_readable(h: &Health, udisks: bool) -> String {
    let dev = &h.device;
    if !udisks {
        format!(
            "udisks isn't running, so drive health needs administrator rights: sudo smartctl -a {dev}"
        )
    } else if h.transport.as_deref() == Some("usb") {
        format!(
            "its USB enclosure doesn't pass health data on (as administrator, sudo smartctl -d sat -a {dev} sometimes gets through)"
        )
    } else {
        format!("udisks has no health data for it: sudo smartctl -a {dev} may say more")
    }
}

#[cfg(target_os = "linux")]
fn busctl(args: &[&str]) -> Option<Value> {
    let mut all = vec![
        "--system",
        "--json=short",
        "--allow-interactive-authorization=no",
    ];
    all.extend_from_slice(args);
    let (code, out) = super::run(
        "busctl",
        &all,
        &[("LC_ALL", "C")],
        std::time::Duration::from_secs(20),
    )
    .ok()?;
    if code != Some(0) {
        return None;
    }
    serde_json::from_slice(&out).ok()
}

#[cfg(target_os = "linux")]
fn gdbus_tree(path: &str) -> Option<Map<String, Value>> {
    let (code, out) = super::run(
        "gdbus",
        &[
            "introspect",
            "--system",
            "--dest",
            "org.freedesktop.UDisks2",
            "--object-path",
            path,
            "--recurse",
            "--only-properties",
        ],
        &[("LC_ALL", "C")],
        std::time::Duration::from_secs(20),
    )
    .ok()?;
    (code == Some(0)).then(|| gdbus_objects(&String::from_utf8_lossy(&out)))
}

/// Read every drive's health (see the module docs). Returns notes about the probe itself.
#[cfg(target_os = "linux")]
pub fn read(drives: &mut [Health]) -> Vec<String> {
    const DEST: &str = "org.freedesktop.UDisks2";
    let mut notes = Vec::new();
    let have_busctl = super::which("busctl").is_some();
    let udrives: Option<Vec<UDrive>> = if have_busctl {
        busctl(&[
            "call",
            DEST,
            "/org/freedesktop/UDisks2",
            "org.freedesktop.DBus.ObjectManager",
            "GetManagedObjects",
        ])
        .map(|v| parse_busctl(&v))
    } else if super::which("gdbus").is_some() {
        gdbus_tree("/org/freedesktop/UDisks2/drives").map(|mut objs| {
            objs.extend(gdbus_tree("/org/freedesktop/UDisks2/block_devices").unwrap_or_default());
            parse_objects(&objs)
        })
    } else {
        notes.push(
            "neither busctl nor gdbus is installed, so udisks can't be asked for drive health"
                .into(),
        );
        None
    };
    if udrives.is_none() && (have_busctl || super::which("gdbus").is_some()) {
        notes.push("udisks isn't running (or didn't answer): drive health is read from smartctl and sysfs only".into());
    }
    if !have_busctl && udrives.is_some() {
        notes.push("busctl isn't installed: udisks' attribute tables can't be read, so reallocated and pending sectors are counted together".into());
    }
    let smartctl_bin = super::which("smartctl");
    for h in drives.iter_mut() {
        if let Some(d) = udrives.as_deref().and_then(|u| matching(h, u)) {
            apply(h, d);
            if have_busctl && d.updated {
                let (iface, fill): (&str, fn(&mut Health, &Value) -> bool) = match d.protocol {
                    Some("ata") => ("org.freedesktop.UDisks2.Drive.Ata", fill_ata_attributes),
                    Some("nvme") => (NVME, fill_nvme_attributes),
                    _ => ("", |_, _| false),
                };
                if !iface.is_empty() {
                    let got = busctl(&[
                        "call",
                        DEST,
                        &d.object,
                        iface,
                        "SmartGetAttributes",
                        "a{sv}",
                        "0",
                    ])
                    .is_some_and(|v| fill(h, &v));
                    if !got && d.protocol == Some("ata") {
                        h.add_note("udisks wouldn't share the attribute table without authorisation, so reallocated and pending sectors are counted together");
                    }
                }
            }
        }
        if let Some(bin) = &smartctl_bin {
            let bin = bin.display().to_string();
            if let Ok((_, out)) = super::run(
                &bin,
                &["-j", "-a", "-n", "standby", &h.device],
                &[("LC_ALL", "C")],
                std::time::Duration::from_secs(30),
            ) && let Ok(v) = serde_json::from_slice::<Value>(&out)
            {
                let had = h.has_data();
                match super::smartctl::fill(h, &v) {
                    Ok(()) => {}
                    Err(super::smartctl::Problem::Permission) if !had => h.add_note(format!(
                        "needs administrator rights: sudo smartctl -a {}",
                        h.device
                    )),
                    Err(super::smartctl::Problem::Asleep) if !had => {
                        h.add_note("it's asleep (spun down): not woken to read it")
                    }
                    Err(_) => {}
                }
            }
        }
        if h.temperature.is_none()
            && let Some(t) = hwmon_temperature(Path::new("/sys"), &h.name)
        {
            h.temperature = Some(t);
            h.add_source("sysfs");
        }
        if !h.has_data() && h.note.is_none() {
            h.note = Some(nothing_readable(h, udrives.is_some()));
        }
    }
    notes
}

#[cfg(not(target_os = "linux"))]
pub fn read(_: &mut [Health]) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn objects() -> Value {
        serde_json::from_str(include_str!("testdata/udisks-objects.json")).unwrap()
    }

    #[test]
    fn udisks_drives_from_busctl() {
        let drives = parse_busctl(&objects());
        assert_eq!(drives.len(), 3);
        let ata = drives.iter().find(|d| d.protocol == Some("ata")).unwrap();
        assert_eq!(ata.devices, ["/dev/sda"]);
        assert_eq!(ata.model.as_deref(), Some("FAKEDISK FD40EZRX-00ABCD0"));
        assert_eq!(ata.serial.as_deref(), Some("FD-WX00A1234567"));
        assert_eq!(ata.wwn.as_deref(), Some("0x50014ee00000abde"));
        assert!(ata.updated);
        assert_eq!(ata.health.reported, Some(Verdict::Ok));
        assert_eq!(ata.health.temperature, Some(38));
        assert_eq!(ata.health.power_on_hours, Some(31_202));
        assert_eq!(ata.health.bad_sectors, Some(10));
        let nvme = drives.iter().find(|d| d.protocol == Some("nvme")).unwrap();
        // two namespaces of one controller, both whole disks; partitions don't count
        assert_eq!(nvme.devices, ["/dev/nvme0n1", "/dev/nvme0n2"]);
        assert_eq!(nvme.health.critical_warning, Some(0x02));
        assert_eq!(nvme.health.temperature, Some(71));
        assert_eq!(nvme.health.power_on_hours, Some(4_210));
        let usb = drives.iter().find(|d| d.protocol.is_none()).unwrap();
        assert_eq!(usb.devices, ["/dev/sdb"]);
        assert!(!usb.updated);
    }

    #[test]
    fn attribute_tables() {
        let mut h = Health {
            bad_sectors: Some(10),
            ..Health::default()
        };
        let reply: Value =
            serde_json::from_str(include_str!("testdata/udisks-ata-attributes.json")).unwrap();
        assert!(fill_ata_attributes(&mut h, &reply));
        assert_eq!(h.reallocated, Some(8));
        assert_eq!(h.pending, Some(2));
        assert_eq!(h.uncorrectable, Some(1));
        assert_eq!(h.crc_errors, Some(14));
        assert_eq!(h.power_cycles, Some(1_417));
        assert_eq!(h.bad_sectors, None);
        assert!(h.failing_attributes.is_empty());

        let mut h = Health::default();
        let reply: Value =
            serde_json::from_str(include_str!("testdata/udisks-nvme-attributes.json")).unwrap();
        assert!(fill_nvme_attributes(&mut h, &reply));
        assert_eq!(h.available_spare, Some(100));
        assert_eq!(h.spare_threshold, Some(99));
        assert_eq!(h.percentage_used, Some(2));
        assert_eq!(h.media_errors, Some(0));
        assert_eq!(h.unsafe_shutdowns, Some(27));
        assert_eq!(h.power_cycles, Some(562));
    }

    #[test]
    fn a_failing_attribute_is_named() {
        let reply = json!({"type": "a(ysqiiixia{sv})", "data": [[
            [5, "reallocated-sector-count", 51, 5, 5, 10, 2008, 3, {}],
            [197, "current-pending-sector", 18, 100, 100, 0, 48, 3, {}],
            [194, "temperature-celsius-2", 34, 40, 52, 0, 313150, 4, {}]
        ]]});
        let mut h = Health::default();
        fill_ata_attributes(&mut h, &reply);
        assert_eq!(h.failing_attributes, ["reallocated sector count"]);
        assert_eq!(h.reallocated, Some(2008));
        assert_eq!(h.pending, Some(48));
    }

    #[test]
    fn gdbus_text_gives_the_same_drives() {
        let objs = gdbus_objects(include_str!("testdata/gdbus-introspect.txt"));
        let drives = parse_objects(&objs);
        assert_eq!(drives.len(), 2);
        let nvme = drives.iter().find(|d| d.protocol == Some("nvme")).unwrap();
        assert_eq!(nvme.serial.as_deref(), Some("S0FAKE0NV970123"));
        assert_eq!(nvme.devices, ["/dev/nvme0n1"]);
        assert_eq!(nvme.health.critical_warning, Some(0));
        assert_eq!(nvme.health.temperature, Some(33));
        assert_eq!(nvme.health.power_on_hours, Some(928));
        let ata = drives.iter().find(|d| d.protocol == Some("ata")).unwrap();
        assert_eq!(ata.devices, ["/dev/sda"]);
        assert_eq!(ata.health.reported, Some(Verdict::Failing));
        assert_eq!(ata.health.temperature, Some(47));
        assert_eq!(ata.health.bad_sectors, Some(56));
        assert_eq!(ata.failing, 1);
        assert_eq!(ata.wwn.as_deref(), Some("0x5000c500abcd0001"));
    }

    #[test]
    fn gvariant_values() {
        assert_eq!(gvariant(r"'a\'b'"), json!("a'b"));
        assert_eq!(gvariant("b'/dev/sda'"), json!("/dev/sda"));
        assert_eq!(gvariant("'Kühlschrank'"), json!("Kühlschrank"));
        // cut off mid-way: not a string, and no panic on a character boundary
        assert_eq!(gvariant("'Kühlschrankü"), Value::Null);
        assert_eq!(gvariant("b"), Value::Null);
        assert_eq!(gvariant("uint64 42"), json!(42));
        assert_eq!(gvariant("310.5"), json!(310.5));
        assert_eq!(gvariant("@as []"), json!([]));
        assert_eq!(
            gvariant("['spare', 'temperature']"),
            json!(["spare", "temperature"])
        );
        assert_eq!(
            gvariant("[b'/boot', b'/boot/efi']"),
            json!(["/boot", "/boot/efi"])
        );
        assert_eq!(gvariant("objectpath '/x'"), json!("/x"));
        assert_eq!(gvariant("{}"), Value::Null);
    }

    #[test]
    fn applying_udisks_and_what_to_say_without_it() {
        let drives = parse_busctl(&objects());
        let mut h = Health {
            device: "/dev/sda".into(),
            name: "sda".into(),
            ..Health::default()
        };
        let d = matching(&h, &drives).unwrap();
        apply(&mut h, d);
        assert_eq!(h.source, "udisks2");
        assert_eq!(h.serial.as_deref(), Some("FD-WX00A1234567"));
        assert!(h.has_data());
        // found by serial when the device node differs (it moved to another port)
        let h2 = Health {
            device: "/dev/sdf".into(),
            serial: Some("fd wx00a1234567".into()),
            ..Health::default()
        };
        assert!(matching(&h2, &drives).is_some());
        let usb = Health {
            device: "/dev/sdb".into(),
            transport: Some("usb".into()),
            ..Health::default()
        };
        assert!(nothing_readable(&usb, true).contains("USB enclosure"));
        assert!(nothing_readable(&usb, false).contains("sudo smartctl -a /dev/sdb"));
    }

    #[test]
    fn hwmon() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("block/nvme0n1/device/hwmon3");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("temp1_input"), "35850\n").unwrap();
        let s = dir.path().join("block/sda/device/hwmon/hwmon7");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join("temp1_input"), "41000\n").unwrap();
        assert_eq!(hwmon_temperature(dir.path(), "nvme0n1"), Some(36));
        assert_eq!(hwmon_temperature(dir.path(), "sda"), Some(41));
        assert_eq!(hwmon_temperature(dir.path(), "sdb"), None);
    }
}
