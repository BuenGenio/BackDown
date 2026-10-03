//! Drive health: what each physical drive says about itself (SMART), read without root, and kept
//! as readings in the index so that changes show up as trends.
//!
//! A [`Health`] record describes one physical drive, identified by its serial number (or its WWN
//! when it has no serial), so its history follows it between ports, enclosures and machines. It
//! carries the drive's own overall verdict where it gives one, and the counters that predict
//! failure: reallocated, pending and uncorrectable sectors, cable (CRC) errors, and for NVMe the
//! critical-warning bits, wear (percentage used), available spare and media errors; plus
//! temperature, power-on hours, and where the numbers came from.
//!
//! Reading never needs root, never writes to a drive, and never wakes one that has spun down:
//! - Linux: udisks2 over D-Bus (`busctl`, else `gdbus introspect`), which caches SMART data and
//!   hands it to any local user; `smartctl -j` when it's installed and allowed to open the device;
//!   hwmon in sysfs for temperatures (NVMe, and SATA with the `drivetemp` module). See [`linux`].
//! - macOS: `diskutil info -plist` (SMART status, and NVMe's health log on recent macOS), and
//!   `smartctl -j` when installed (Homebrew's smartmontools). See [`macos`].
//! - Windows: the Storage cmdlets through PowerShell: `Get-PhysicalDisk`'s health and operational
//!   status for everyone, `Get-StorageReliabilityCounter`'s counters where Windows allows them.
//!   See [`windows`].
//!
//! Every parser is plain code over text, so all of them are tested on every platform; only the
//! part that runs the tools is platform-specific. When something can't be read, the record says
//! why and, where there is one, the command that would read it ("needs administrator rights:
//! sudo smartctl -a /dev/sdb").
//!
//! [`assess`] turns one reading into a verdict (ok, warning, failing, unknown) with reasons;
//! [`step`] compares a reading with the previous one and says what got worse (new reallocated or
//! pending sectors, rising cable errors, wear crossing a threshold, a temperature spike, the drive
//! saying it's failing). [`record`] stores a reading in the index's `health` table when it tells
//! something new (or once a day), and returns those changes.

use std::collections::BTreeMap;

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::Db;
use crate::volumes::{self, Disk, Volume};

pub mod linux;
pub mod macos;
pub mod smartctl;
pub mod windows;

/// How a drive is doing, worst last.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Nothing readable: no SMART over this USB bridge, the drive is asleep, or the system won't
    /// say without an administrator.
    #[default]
    Unknown,
    Ok,
    /// Worth watching, and worth a second copy of what's on it.
    Warning,
    /// Copy what matters off it now.
    Failing,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Unknown => "unknown",
            Verdict::Ok => "ok",
            Verdict::Warning => "warning",
            Verdict::Failing => "failing",
        }
    }

    /// For "did it get worse": unknown counts as fine (a drive that can't be read now isn't news).
    pub fn rank(self) -> u8 {
        match self {
            Verdict::Unknown | Verdict::Ok => 0,
            Verdict::Warning => 1,
            Verdict::Failing => 2,
        }
    }

    /// From its name (`as_str`); anything else is unknown.
    pub fn parse(s: &str) -> Verdict {
        match s {
            "ok" => Verdict::Ok,
            "warning" => Verdict::Warning,
            "failing" => Verdict::Failing,
            _ => Verdict::Unknown,
        }
    }
}

/// One reading of one physical drive.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Health {
    /// Stable identity: `sn:<serial>` (normalised), else `wwn:<wwn>`; empty when the drive has
    /// neither, and then nothing about it is kept over time.
    pub id: String,
    /// Where it is now: `/dev/sda`, `/dev/disk4`, `\\.\PHYSICALDRIVE1`.
    pub device: String,
    /// Short name: `sda`, `disk4`, `Disk 1`.
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wwn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotational: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// What kind of health data it gave: `ata`, `nvme` or `scsi`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// The drive's (or the operating system's) own overall verdict, where it gives one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reported: Option<Verdict>,
    /// …in its own words: PASSED, FAILED!, Verified, Healthy, Predictive Failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reported_as: Option<String>,
    /// °C.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_on_hours: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_cycles: Option<u64>,
    /// Sectors replaced by spares (ATA 5; SCSI's grown defect list).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reallocated: Option<u64>,
    /// Sectors that couldn't be read and wait to be replaced (ATA 197).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<u64>,
    /// Sectors that couldn't be read even after retries (ATA 198, else 187; SCSI's uncorrected
    /// errors; Windows' uncorrected read errors).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uncorrectable: Option<u64>,
    /// Reallocated and pending together, when only the sum is known (udisks without attributes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bad_sectors: Option<u64>,
    /// Transfer errors between the drive and the computer (ATA 199): a cable or enclosure problem.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crc_errors: Option<u64>,
    /// NVMe critical warning bits (0: none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub critical_warning: Option<u64>,
    /// NVMe (and Windows' "Wear"): how much of its rated write endurance it has used, in %.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percentage_used: Option<u64>,
    /// NVMe: spare blocks left, in %, and the level below which the drive itself warns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_spare: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spare_threshold: Option<u64>,
    /// NVMe: unrecovered data integrity errors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_errors: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unsafe_shutdowns: Option<u64>,
    /// ATA attributes the drive itself marks as failing now.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failing_attributes: Vec<String>,
    /// Where the numbers came from: udisks2, smartctl, diskutil, Windows, sysfs (joined by " + ").
    pub source: String,
    /// Why some or all of it couldn't be read, and what would read it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Mount points of the drive's volumes when it was read: which drive a folder is on.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<String>,
    /// Unix seconds.
    pub read_at: i64,
}

/// What one probe found: every physical drive, and notes about the probe itself.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Probe {
    pub drives: Vec<Health>,
    pub notes: Vec<String>,
}

/// Something the verdict rests on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    /// `ok` for something worth knowing that isn't a problem (yet).
    pub level: Verdict,
    pub code: &'static str,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Assessment {
    pub verdict: Verdict,
    pub findings: Vec<Finding>,
}

/// Something that got worse between two readings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub level: Verdict,
    pub code: String,
    pub text: String,
    /// when it was noticed (the later reading)
    pub at: i64,
    /// the same event gives the same key (for not telling anyone twice)
    pub key: String,
}

impl Health {
    /// Anything that says how the drive is (not just its temperature or age).
    pub fn has_data(&self) -> bool {
        self.reported.is_some()
            || self.critical_warning.is_some()
            || self.reallocated.is_some()
            || self.pending.is_some()
            || self.uncorrectable.is_some()
            || self.bad_sectors.is_some()
            || self.percentage_used.is_some()
            || self.available_spare.is_some()
            || self.media_errors.is_some()
    }

    /// A readable name: the model, else the device.
    pub fn label(&self) -> String {
        self.model.clone().unwrap_or_else(|| self.device.clone())
    }

    fn add_source(&mut self, s: &str) {
        if !self.source.split(" + ").any(|x| x == s) {
            if !self.source.is_empty() {
                self.source.push_str(" + ");
            }
            self.source.push_str(s);
        }
    }

    fn add_note(&mut self, n: impl Into<String>) {
        let n = n.into();
        match &mut self.note {
            Some(x) if !x.contains(&n) => {
                x.push_str("; ");
                x.push_str(&n);
            }
            Some(_) => {}
            None => self.note = Some(n),
        }
    }

    /// Everything that says how the drive is, without what changes by the hour (temperature,
    /// hours, where it's plugged in): two readings with the same signature say the same thing.
    fn signature(&self) -> String {
        format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
            self.reported,
            self.reallocated,
            self.pending,
            self.uncorrectable,
            self.bad_sectors,
            self.crc_errors,
            self.critical_warning,
            self.percentage_used,
            self.available_spare,
            self.spare_threshold,
            self.media_errors,
            self.failing_attributes,
            self.protocol,
        )
    }
}

// ---------- identity

/// Serial numbers that some USB bridges and cheap drives report for every unit: no identity.
const JUNK_SERIALS: [&str; 8] = [
    "0",
    "00000000",
    "000000000000",
    "0000000000000000",
    "0123456789ABCDEF",
    "123456789",
    "NONE",
    "DEFAULTSTRING",
];

/// The identity a drive's readings are kept under: its serial (upper case, without spaces,
/// dashes, dots or underscores, which platforms add differently), else its WWN.
pub fn identity(serial: Option<&str>, wwn: Option<&str>) -> String {
    let norm = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_uppercase())
            .collect()
    };
    if let Some(s) = serial.map(norm).filter(|s| {
        !s.is_empty() && !JUNK_SERIALS.contains(&s.as_str()) && s.chars().any(|c| c != '0')
    }) {
        return format!("sn:{s}");
    }
    match wwn.map(norm) {
        Some(w) if !w.is_empty() && w.chars().any(|c| c != '0' && c != 'X') => {
            format!("wwn:{}", w.trim_start_matches("0X"))
        }
        _ => String::new(),
    }
}

// ---------- probing

/// Drives that have no health to read: loop devices, attached images, RAM disks.
fn physical(d: &Disk) -> bool {
    d.backing_file.is_none()
        && !matches!(
            d.transport.as_deref(),
            Some("loop" | "image" | "virtual" | "virtio")
        )
        && !d.name.starts_with("zram")
        && !d.name.starts_with("ram")
}

fn mounts_of(d: &Disk) -> Vec<String> {
    fn walk(v: &Volume, out: &mut Vec<String>) {
        out.extend(v.mounts.iter().map(|m| m.path.clone()));
        for h in &v.holders {
            walk(h, out);
        }
    }
    let mut out = Vec::new();
    for v in &d.volumes {
        walk(v, &mut out);
    }
    out
}

/// A record for each physical disk the volume probe found, before any health is read. Disks that
/// are one drive (the namespaces of one NVMe SSD) become one record.
pub fn skeletons(disks: &[Disk], now: i64) -> Vec<Health> {
    let mut out: Vec<Health> = Vec::new();
    for d in disks.iter().filter(|d| physical(d)) {
        let id = identity(d.serial.as_deref(), None);
        if let Some(same) = out.iter_mut().find(|h| !id.is_empty() && h.id == id) {
            same.mounts.extend(mounts_of(d));
            continue;
        }
        out.push(Health {
            id,
            device: d.device.clone(),
            name: d.name.clone(),
            model: d.model.clone(),
            serial: d.serial.clone(),
            transport: d.transport.clone(),
            rotational: d.rotational,
            size: (d.size > 0).then_some(d.size),
            mounts: mounts_of(d),
            read_at: now,
            ..Health::default()
        });
    }
    out
}

/// Read every drive's health on this machine (unprivileged, read-only).
pub fn probe(now: i64) -> Result<Probe> {
    let report = volumes::probe()?;
    let mut drives = skeletons(&report.disks, now);
    let notes = platform(&mut drives);
    for h in &mut drives {
        if h.id.is_empty() {
            h.id = identity(h.serial.as_deref(), h.wwn.as_deref());
        }
        if h.source.is_empty() {
            h.source = "none".into();
        }
    }
    Ok(Probe { drives, notes })
}

#[cfg(target_os = "linux")]
fn platform(drives: &mut [Health]) -> Vec<String> {
    linux::read(drives)
}

#[cfg(target_os = "macos")]
fn platform(drives: &mut [Health]) -> Vec<String> {
    macos::read(drives)
}

#[cfg(windows)]
fn platform(drives: &mut [Health]) -> Vec<String> {
    windows::read(drives)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn platform(_: &mut [Health]) -> Vec<String> {
    vec![format!(
        "drive health isn't read on {} yet",
        std::env::consts::OS
    )]
}

/// Run a tool with a time limit, returning its exit code and stdout even when it fails (smartctl
/// reports through both). A dying drive can make any of them hang: they're killed after `timeout`.
pub fn run(
    program: &str,
    args: &[&str],
    env: &[(&str, &str)],
    timeout: std::time::Duration,
) -> Result<(Option<i32>, Vec<u8>)> {
    use anyhow::Context;
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut cmd = Command::new(program);
    cmd.args(args)
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("can't run {program}"))?;
    let mut out = child.stdout.take().context("no stdout")?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        buf
    });
    let until = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status.code(), reader.join().unwrap_or_default()));
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("{program} didn't answer in {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Is `program` on PATH (or an absolute path that exists)?
pub fn which(program: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(program);
    if p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    let exts: &[&str] = if cfg!(windows) { &["", ".exe"] } else { &[""] };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| exts.iter().map(move |e| dir.join(format!("{program}{e}"))))
        .find(|c| c.is_file())
}

// ---------- the verdict of one reading

/// Hotter than this (°C) is worth a word: spinning disks like it cooler than SSDs.
fn hot(h: &Health) -> i64 {
    if h.rotational == Some(true) { 55 } else { 70 }
}

const CRITICAL_BITS: [(u64, Verdict, &str); 6] = [
    (0x01, Verdict::Failing, "its spare blocks have run out"),
    (0x02, Verdict::Warning, "it has been running too hot"),
    (
        0x04,
        Verdict::Failing,
        "it says media errors have degraded its reliability",
    ),
    (
        0x08,
        Verdict::Failing,
        "it has made itself read-only to protect what's on it",
    ),
    (
        0x10,
        Verdict::Failing,
        "its power-loss protection has failed",
    ),
    (0x20, Verdict::Warning, "its persistent memory is read-only"),
];

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// What one reading says about the drive.
pub fn assess(h: &Health) -> Assessment {
    let mut f: Vec<Finding> = Vec::new();
    let mut add = |level, code, text: String| f.push(Finding { level, code, text });
    match h.reported {
        Some(Verdict::Failing) => add(
            Verdict::Failing,
            "reported",
            format!(
                "the drive's own check says it's failing{}",
                h.reported_as
                    .as_deref()
                    .map(|s| format!(" ({s})"))
                    .unwrap_or_default()
            ),
        ),
        Some(Verdict::Warning) => add(
            Verdict::Warning,
            "reported",
            format!(
                "the system reports a problem with it{}",
                h.reported_as
                    .as_deref()
                    .map(|s| format!(" ({s})"))
                    .unwrap_or_default()
            ),
        ),
        _ => {}
    }
    if !h.failing_attributes.is_empty() {
        add(
            Verdict::Failing,
            "attributes",
            format!(
                "it marks {} as failing now",
                h.failing_attributes.join(", ")
            ),
        );
    }
    if let Some(cw) = h.critical_warning.filter(|c| *c != 0) {
        for (bit, level, text) in CRITICAL_BITS {
            if cw & bit != 0 {
                add(level, "critical", text.to_string());
            }
        }
    }
    if let (Some(spare), Some(thr)) = (h.available_spare, h.spare_threshold.filter(|t| *t > 0)) {
        // a warning band above the drive's own limit: 10 points, less for drives whose limit is
        // near 100% (Apple's SSDs say 99)
        let margin = (100u64.saturating_sub(thr) / 2).min(10);
        if spare < thr {
            add(
                Verdict::Failing,
                "spare",
                format!("only {spare}% of its spare blocks are left (its own limit is {thr}%)"),
            );
        } else if spare < thr + margin {
            add(
                Verdict::Warning,
                "spare",
                format!("its spare blocks are running low: {spare}% left"),
            );
        }
    }
    if let Some(u) = h.percentage_used {
        if u >= 100 {
            add(
                Verdict::Warning,
                "wear",
                format!("it has used up its rated write endurance ({u}%)"),
            );
        } else if u >= 90 {
            add(
                Verdict::Warning,
                "wear",
                format!("it has used {u}% of its rated write endurance"),
            );
        } else if u >= 50 {
            add(
                Verdict::Ok,
                "wear",
                format!("it has used {u}% of its rated write endurance"),
            );
        }
    }
    let sectors = |n: u64, many: u64, what: &str| {
        (
            if n >= many {
                Verdict::Failing
            } else {
                Verdict::Warning
            },
            format!("{} {what}", plural(n, "sector", "sectors")),
        )
    };
    if let Some(n) = h.reallocated.filter(|n| *n > 0) {
        let (level, text) = sectors(
            n,
            100,
            "had gone bad and have been replaced by spares (reallocated)",
        );
        add(level, "reallocated", text);
    }
    if let Some(n) = h.pending.filter(|n| *n > 0) {
        let (level, text) = sectors(
            n,
            10,
            "can't be read and are waiting to be replaced (pending)",
        );
        add(level, "pending", text);
    }
    if let Some(n) = h.uncorrectable.filter(|n| *n > 0) {
        let (level, text) = sectors(n, 10, "couldn't be read even after retries (uncorrectable)");
        add(level, "uncorrectable", text);
    }
    if let Some(n) = h.bad_sectors.filter(|n| *n > 0) {
        let (level, text) = sectors(n, 100, "are bad (reallocated or waiting to be)");
        add(level, "bad-sectors", text);
    }
    if let Some(n) = h.media_errors.filter(|n| *n > 0) {
        add(
            Verdict::Warning,
            "media-errors",
            format!(
                "{} lost data it couldn't recover",
                plural(n, "read or write", "reads or writes")
            ),
        );
    }
    if let Some(t) = h.temperature.filter(|t| *t >= hot(h)) {
        add(
            Verdict::Warning,
            "temperature",
            format!("it's at {t} °C: hotter than is good for it"),
        );
    }
    if let Some(n) = h.crc_errors.filter(|n| *n > 0) {
        add(
            Verdict::Ok,
            "crc",
            format!(
                "{} between it and the computer so far (a cable or enclosure problem, if it keeps rising)",
                plural(n, "transfer error", "transfer errors")
            ),
        );
    }
    let verdict = if !h.has_data() {
        Verdict::Unknown
    } else {
        f.iter()
            .map(|x| x.level)
            .max_by_key(|l| l.rank())
            .filter(|l| l.rank() > 0)
            .unwrap_or(Verdict::Ok)
    };
    Assessment {
        verdict,
        findings: f,
    }
}

// ---------- what changed between two readings

/// Wear levels worth a word as the drive crosses them (the first only for the record).
const WEAR_STEPS: [u64; 4] = [50, 80, 90, 100];

/// What got worse from `prev` (the last reading with data, if any) to `cur`.
pub fn step(prev: Option<&Health>, cur: &Health) -> Vec<Change> {
    let at = cur.read_at;
    let mut out: Vec<Change> = Vec::new();
    let mut add = |level: Verdict, code: &str, text: String, key: String| {
        out.push(Change {
            level,
            code: code.to_string(),
            text,
            at,
            key,
        })
    };
    let before = prev.map_or(Verdict::Unknown, |p| assess(p).verdict);
    let now = assess(cur).verdict;
    if now.rank() > before.rank() {
        let why = assess(cur)
            .findings
            .into_iter()
            .filter(|f| f.level == now)
            .map(|f| f.text)
            .collect::<Vec<_>>()
            .join("; ");
        add(
            now,
            "verdict",
            match prev {
                Some(_) => format!("was {}, now {}: {why}", before.as_str(), now.as_str()),
                None => format!("{}: {why}", now.as_str()),
            },
            format!("verdict:{}", now.as_str()),
        );
    }
    let Some(p) = prev else { return out };
    let rise = |a: Option<u64>, b: Option<u64>| match (a, b) {
        (Some(a), Some(b)) if b > a => Some((b - a, b)),
        (None, Some(b)) if b > 0 => Some((b, b)),
        _ => None,
    };
    if cur.reported == Some(Verdict::Failing) && p.reported != Some(Verdict::Failing) {
        add(
            Verdict::Failing,
            "reported",
            "the drive's own check now says it's failing".into(),
            "reported:failing".into(),
        );
    }
    if let Some((d, n)) = rise(p.reallocated, cur.reallocated) {
        add(
            if n >= 100 {
                Verdict::Failing
            } else {
                Verdict::Warning
            },
            "reallocated",
            format!(
                "{} more {} gone bad and been replaced by spares (now {n})",
                d,
                if d == 1 { "sector has" } else { "sectors have" }
            ),
            format!("reallocated:{n}"),
        );
    }
    if let Some((d, n)) = rise(p.pending, cur.pending) {
        add(
            if n >= 10 {
                Verdict::Failing
            } else {
                Verdict::Warning
            },
            "pending",
            format!(
                "{} more {} be read (now {n} waiting to be replaced)",
                d,
                if d == 1 {
                    "sector can't"
                } else {
                    "sectors can't"
                }
            ),
            format!("pending:{n}"),
        );
    }
    if let Some((d, n)) = rise(p.uncorrectable, cur.uncorrectable) {
        add(
            if n >= 10 {
                Verdict::Failing
            } else {
                Verdict::Warning
            },
            "uncorrectable",
            format!(
                "{} more {} (now {n})",
                plural(d, "sector", "sectors"),
                "couldn't be read even after retries"
            ),
            format!("uncorrectable:{n}"),
        );
    }
    if let Some((d, n)) = rise(p.bad_sectors, cur.bad_sectors) {
        add(
            Verdict::Warning,
            "bad-sectors",
            format!(
                "{} more bad {} (now {n})",
                d,
                if d == 1 { "sector" } else { "sectors" }
            ),
            format!("bad-sectors:{n}"),
        );
    }
    if let Some((d, n)) = rise(p.media_errors, cur.media_errors) {
        add(
            Verdict::Warning,
            "media-errors",
            format!(
                "{} more unrecoverable media {} (now {n})",
                d,
                if d == 1 { "error" } else { "errors" }
            ),
            format!("media-errors:{n}"),
        );
    }
    if let (Some(a), Some(b)) = (p.crc_errors, cur.crc_errors)
        && b > a
    {
        add(
            Verdict::Warning,
            "crc",
            format!(
                "{} more transfer {} between the drive and the computer (now {b}): check or replace the cable or enclosure; the drive itself may be fine",
                b - a,
                if b - a == 1 { "error" } else { "errors" }
            ),
            format!("crc:{b}"),
        );
    }
    if let (Some(a), Some(b)) = (p.percentage_used, cur.percentage_used) {
        for s in WEAR_STEPS.iter().copied().filter(|s| a < *s && b >= *s) {
            add(
                if s >= 80 {
                    Verdict::Warning
                } else {
                    Verdict::Ok
                },
                "wear",
                format!("it has now used {b}% of its rated write endurance"),
                format!("wear:{s}"),
            );
        }
    }
    if let (Some(a), Some(b), Some(t)) = (
        p.available_spare,
        cur.available_spare,
        cur.spare_threshold.filter(|t| *t > 0),
    ) && a >= t
        && b < t
    {
        add(
            Verdict::Failing,
            "spare",
            format!("its spare blocks fell below its own limit ({b}% left, limit {t}%)"),
            "spare:below".into(),
        );
    }
    let new_bits = cur.critical_warning.unwrap_or(0) & !p.critical_warning.unwrap_or(0);
    for (bit, level, text) in CRITICAL_BITS {
        if new_bits & bit != 0 {
            add(
                level,
                "critical",
                text.to_string(),
                format!("critical:{bit}"),
            );
        }
    }
    if let (Some(a), Some(b)) = (p.temperature, cur.temperature)
        && b >= hot(cur)
        && b >= a + 10
    {
        add(
            Verdict::Warning,
            "temperature",
            format!("it ran hot: {b} °C (it was {a} °C)"),
            format!("temperature:{}", b / 5 * 5),
        );
    }
    out
}

/// Every change across a drive's readings (oldest first), each reading against the one before.
pub fn changes(history: &[Health]) -> Vec<Change> {
    let mut out = Vec::new();
    let mut prev: Option<&Health> = None;
    for h in history.iter().filter(|h| h.has_data()) {
        out.extend(step(prev, h));
        prev = Some(h);
    }
    out
}

/// How a drive changed from its first reading to its last, in a few words.
pub fn trend(first: &Health, last: &Health) -> Vec<String> {
    let mut out = Vec::new();
    let delta = |a: Option<u64>, b: Option<u64>| match (a, b) {
        (Some(a), Some(b)) if b != a => Some(b as i64 - a as i64),
        _ => None,
    };
    for (a, b, what) in [
        (first.reallocated, last.reallocated, "reallocated"),
        (first.pending, last.pending, "pending"),
        (first.uncorrectable, last.uncorrectable, "uncorrectable"),
        (first.bad_sectors, last.bad_sectors, "bad sectors"),
        (first.media_errors, last.media_errors, "media errors"),
        (first.crc_errors, last.crc_errors, "CRC errors"),
    ] {
        if let Some(d) = delta(a, b) {
            out.push(format!("{d:+} {what}"));
        }
    }
    if let Some(d) = delta(first.percentage_used, last.percentage_used) {
        out.push(format!("wear {d:+}%"));
    }
    if let Some(d) = delta(first.available_spare, last.available_spare) {
        out.push(format!("spare {d:+}%"));
    }
    if let Some(d) = delta(first.power_on_hours, last.power_on_hours) {
        out.push(format!("{d:+} h on"));
    }
    out
}

// ---------- readings in the index

/// Readings kept per drive: at most one a day unless something changed, so years of them.
const KEEP: i64 = 2000;
/// An unchanged drive is recorded again after this long, so the trend has points.
const REREAD_SECS: i64 = 20 * 3600;

/// What [`record`] did with a reading.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Recorded {
    pub stored: bool,
    /// the drive's first reading with data
    pub first: bool,
    pub before: Option<Verdict>,
    pub verdict: Verdict,
    pub changes: Vec<Change>,
}

fn load(data: &str) -> Option<Health> {
    serde_json::from_str(data).ok()
}

/// The drive's latest stored reading.
pub fn last(db: &Db, drive: &str) -> Result<Option<Health>> {
    let data: Option<String> = db
        .conn
        .query_row(
            "SELECT data FROM health WHERE drive = ?1 ORDER BY at DESC, id DESC LIMIT 1",
            [drive],
            |r| r.get(0),
        )
        .optional()?;
    Ok(data.as_deref().and_then(load))
}

/// Keep a reading if it says something new, and say what got worse since the last one. Readings
/// without data (asleep, unreadable) and drives without an identity aren't kept.
pub fn record(db: &Db, h: &Health) -> Result<Recorded> {
    let verdict = assess(h).verdict;
    if h.id.is_empty() || !h.has_data() {
        return Ok(Recorded {
            verdict,
            ..Recorded::default()
        });
    }
    let prev = last(db, &h.id)?;
    let changes = step(prev.as_ref(), h);
    let keep = match &prev {
        None => true,
        Some(p) => {
            p.signature() != h.signature()
                || h.read_at - p.read_at >= REREAD_SECS
                || !changes.is_empty()
                || (h.temperature.is_some_and(|t| t >= hot(h)) && h.temperature != p.temperature)
        }
    };
    if keep {
        db.conn.execute(
            "INSERT INTO health (drive, at, verdict, data) VALUES (?1, ?2, ?3, ?4)",
            params![h.id, h.read_at, verdict.as_str(), serde_json::to_string(h)?],
        )?;
        db.conn.execute(
            "DELETE FROM health WHERE drive = ?1 AND id NOT IN
               (SELECT id FROM health WHERE drive = ?1 ORDER BY at DESC, id DESC LIMIT ?2)",
            params![h.id, KEEP],
        )?;
    }
    Ok(Recorded {
        stored: keep,
        first: prev.is_none(),
        before: prev.as_ref().map(|p| assess(p).verdict),
        verdict,
        changes,
    })
}

/// A drive's readings, oldest first.
pub fn history(db: &Db, drive: &str) -> Result<Vec<Health>> {
    let mut stmt = db
        .conn
        .prepare("SELECT data FROM health WHERE drive = ?1 ORDER BY at, id")?;
    let rows = stmt.query_map([drive], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.extend(load(&r?));
    }
    Ok(out)
}

/// Every drive with readings, most recently read first, with its latest verdict.
pub fn drives(db: &Db) -> Result<Vec<(String, i64, Verdict)>> {
    let mut stmt = db.conn.prepare(
        "SELECT drive, MAX(at), (SELECT verdict FROM health h2 WHERE h2.drive = h.drive ORDER BY at DESC, id DESC LIMIT 1)
         FROM health h GROUP BY drive ORDER BY MAX(at) DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            Verdict::parse(&r.get::<_, String>(2)?),
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The health table exists (an index made before drive health was read, opened read-only).
pub fn available(db: &Db) -> bool {
    db.conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'health'",
            [],
            |_| Ok(()),
        )
        .is_ok()
}

// ---------- one drive's picture, for the CLI and the browser

/// A drive as people see it: the latest reading (live if it's connected, else the last stored),
/// what it means, what changed, and a small series for a sparkline.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub id: String,
    /// connected and read just now
    pub present: bool,
    pub health: Health,
    pub verdict: Verdict,
    pub findings: Vec<Finding>,
    /// the latest changes for the worse (newest last)
    pub changes: Vec<Change>,
    /// since the first reading
    pub trend: Vec<String>,
    pub first_at: Option<i64>,
    pub readings: usize,
    pub series: Vec<Point>,
}

/// One reading, reduced to what a sparkline shows.
#[derive(Debug, Clone, Serialize)]
pub struct Point {
    pub at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp: Option<i64>,
    /// reallocated + pending + uncorrectable (or the bad-sector sum)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bad: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wear: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crc: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<u64>,
    pub verdict: Verdict,
}

fn point(h: &Health) -> Point {
    let parts = [h.reallocated, h.pending, h.uncorrectable, h.bad_sectors];
    Point {
        at: h.read_at,
        temp: h.temperature,
        bad: parts
            .iter()
            .any(Option::is_some)
            .then(|| parts.iter().flatten().sum()),
        wear: h.percentage_used,
        crc: h.crc_errors,
        media: h.media_errors,
        verdict: assess(h).verdict,
    }
}

/// Put a drive's picture together from its stored history and, if it's connected, a live
/// reading (which counts as the latest point).
pub fn summarize(live: Option<&Health>, stored: &[Health]) -> Option<Summary> {
    let latest_stored = stored.last();
    let health = live.or(latest_stored)?.clone();
    let mut all: Vec<Health> = stored.to_vec();
    if let Some(l) = live.filter(|l| l.has_data())
        && all.last().is_none_or(|s| s.read_at < l.read_at)
    {
        all.push(l.clone());
    }
    // a live reading without data (asleep, unreadable) says less than the last one that had some
    let shown = if health.has_data() {
        health.clone()
    } else {
        latest_stored.cloned().unwrap_or_else(|| health.clone())
    };
    let a = assess(&shown);
    let mut ch = changes(&all);
    if ch.len() > 20 {
        ch.drain(..ch.len() - 20);
    }
    let with_data: Vec<&Health> = all.iter().filter(|h| h.has_data()).collect();
    let trend = match (with_data.first(), with_data.last()) {
        (Some(f), Some(l)) if with_data.len() > 1 => trend(f, l),
        _ => Vec::new(),
    };
    // at most ~120 points: the sparkline is small
    let stride = (all.len() / 120).max(1);
    let mut series: Vec<Point> = all.iter().step_by(stride).map(point).collect();
    if let Some(l) = all.last()
        && series.last().is_none_or(|p| p.at != l.read_at)
    {
        series.push(point(l));
    }
    let mut out = Summary {
        id: health.id.clone(),
        present: live.is_some(),
        health: shown,
        verdict: a.verdict,
        findings: a.findings,
        changes: ch,
        trend,
        first_at: with_data.first().map(|h| h.read_at),
        readings: stored.len(),
        series,
    };
    // where it is now, even when the numbers come from an earlier reading
    if let Some(l) = live {
        out.health.device = l.device.clone();
        out.health.name = l.name.clone();
        out.health.mounts = l.mounts.clone();
        if l.note.is_some() {
            out.health.note = l.note.clone();
        }
    }
    Some(out)
}

/// Every drive: the connected ones (from `live`) and those with stored readings that aren't
/// connected now. Failing first, then warning; connected before away.
pub fn overview(db: Option<&Db>, live: &[Health]) -> Result<Vec<Summary>> {
    let mut out = Vec::new();
    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    for l in live {
        let stored = match db {
            Some(db) if !l.id.is_empty() => history(db, &l.id)?,
            _ => Vec::new(),
        };
        if !l.id.is_empty() {
            seen.insert(l.id.clone(), ());
        }
        out.extend(summarize(Some(l), &stored));
    }
    if let Some(db) = db {
        for (id, _, _) in drives(db)? {
            if seen.contains_key(&id) {
                continue;
            }
            let stored = history(db, &id)?;
            out.extend(summarize(None, &stored));
        }
    }
    out.sort_by_key(|s| (std::cmp::Reverse(s.verdict.rank()), !s.present));
    Ok(out)
}

/// The drive a path is on, by the deepest mount point that holds it.
pub fn drive_of<'a>(path: &str, drives: &'a [Summary]) -> Option<&'a Summary> {
    let p = std::path::Path::new(path);
    drives
        .iter()
        .flat_map(|d| d.health.mounts.iter().map(move |m| (d, m)))
        .filter(|(_, m)| p.starts_with(m.as_str()))
        .max_by_key(|(_, m)| m.len())
        .map(|(d, _)| d)
}

#[cfg(test)]
mod tests;
