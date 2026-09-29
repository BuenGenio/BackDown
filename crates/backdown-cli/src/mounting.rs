//! Mounting read-only, the safe way, as the person at the screen, never as root and never
//! read-write: a partition the volume probe calls "mountable" (a known filesystem, not mounted),
//! and a disk image whose filesystem BackDown's own reader (`fsread`) can't browse (XFS, btrfs,
//! HFS+, APFS…), attached read-only and its partitions mounted read-only.
//!
//! - Linux: udisks2, through `udisksctl` (`mount -b DEV -o ro`, `loop-setup -r -f IMAGE`), always
//!   with `--no-user-interaction`: polkit normally lets the active session mount removable drives
//!   and set up loop devices without a password; when it wants one instead, BackDown says so and
//!   gives the command to run in a terminal (where udisksctl asks for it itself).
//! - macOS: `diskutil mount readOnly DEV`; images with `hdiutil attach -readonly -nobrowse`.
//! - Windows: images with `Mount-DiskImage -Access ReadOnly` (ISO, VHD, VHDX). A volume can't be
//!   mounted read-only on Windows in general, so that isn't offered.
//!
//! What BackDown mounted is kept in `mounts.json` in its state folder, and only that is ever
//! unmounted or detached by it. Commands are built and their output read by plain functions
//! (tested on every platform); only running them is platform-specific. Nothing goes through a
//! shell.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A command to run directly (never through a shell).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
}

impl Cmd {
    fn new(program: &str, args: &[&str]) -> Cmd {
        Cmd {
            program: program.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        }
    }

    /// As it would be typed into a terminal, for "run this yourself".
    pub fn typed(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .map(quote)
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn run(&self) -> Result<Out> {
        let o = Command::new(&self.program)
            .args(&self.args)
            .stdin(std::process::Stdio::null())
            .output()
            .with_context(|| format!("can't run {}", self.program))?;
        Ok(Out {
            ok: o.status.success(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        })
    }
}

/// Quoted for a POSIX shell when it needs to be.
fn quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-:=,+@".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// What a command printed, and whether it succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Out {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// What a mount, unmount, attach or detach came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Mounted there (or attached as that device).
    Mounted(String),
    /// Unmounted or detached (or there was nothing to undo).
    Done,
    /// It was mounted already, there.
    Already(String),
    /// The system wants an administrator's password first.
    NeedsAuth,
    /// In use: something has files open on it.
    Busy,
    /// No such device.
    Missing,
    /// Nothing on it that can be mounted.
    NotMountable,
    Failed(String),
}

/// A refusal to show people: what happened, and when there is one, the command to run in a
/// terminal instead.
#[derive(Debug)]
pub struct Refused {
    pub message: String,
    pub command: Option<String>,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        if let Some(c) = &self.command {
            write!(f, "\n  {c}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Refused {}

#[cfg_attr(windows, allow(dead_code))]
fn refused(message: impl Into<String>, command: Option<&Cmd>) -> anyhow::Error {
    anyhow::Error::new(Refused {
        message: message.into(),
        command: command.map(Cmd::typed),
    })
}

// ---------- Linux: udisks2

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub mod udisks {
    use super::{Cmd, Out, Outcome};

    fn with(interactive: bool, mut c: Cmd) -> Cmd {
        if !interactive {
            c.args.push("--no-user-interaction".into());
        }
        c
    }

    /// Mounts a filesystem read-only (`-o ro`: never anything else).
    pub fn mount(dev: &str, interactive: bool) -> Cmd {
        with(
            interactive,
            Cmd::new("udisksctl", &["mount", "-b", dev, "-o", "ro"]),
        )
    }

    pub fn unmount(dev: &str, interactive: bool) -> Cmd {
        with(interactive, Cmd::new("udisksctl", &["unmount", "-b", dev]))
    }

    /// Attaches an image as a read-only loop device (its partitions show up as their own).
    pub fn loop_setup(image: &str, interactive: bool) -> Cmd {
        with(
            interactive,
            Cmd::new("udisksctl", &["loop-setup", "-r", "-f", image]),
        )
    }

    pub fn loop_delete(dev: &str, interactive: bool) -> Cmd {
        with(
            interactive,
            Cmd::new("udisksctl", &["loop-delete", "-b", dev]),
        )
    }

    /// What udisksctl's mount, unmount, loop-setup or loop-delete came to.
    pub fn parse(out: &Out) -> Outcome {
        let so = out.stdout.trim();
        if out.ok {
            if let Some(rest) = so.strip_prefix("Mounted ")
                && let Some((_, at)) = rest.split_once(" at ")
            {
                return Outcome::Mounted(at.trim_end_matches('.').to_string());
            }
            if let Some(rest) = so.strip_prefix("Mapped file ")
                && let Some((_, dev)) = rest.rsplit_once(" as ")
            {
                return Outcome::Mounted(dev.trim_end_matches('.').to_string());
            }
            return Outcome::Done;
        }
        let e = out.stderr.trim();
        if e.contains("NotAuthorized") || e.contains("Not authorized") {
            Outcome::NeedsAuth
        } else if e.contains("AlreadyMounted") {
            // "... is already mounted at `/media/me/CARD'."
            let at = e
                .split_once(" at `")
                .map(|(_, r)| r.trim_end_matches('.').trim_end_matches('\''))
                .unwrap_or("");
            Outcome::Already(at.to_string())
        } else if e.contains("NotMounted") || e.contains("is not mounted") {
            Outcome::Done
        } else if e.contains("DeviceBusy") || e.contains("target is busy") {
            Outcome::Busy
        } else if e.contains("Error looking up object") {
            Outcome::Missing
        } else if e.contains("not a mountable filesystem")
            || e.contains("No usable")
            || e.contains("wrong fs type")
            || e.contains("unknown filesystem type")
        {
            Outcome::NotMountable
        } else {
            Outcome::Failed(
                e.lines()
                    .last()
                    .unwrap_or("udisksctl failed")
                    .rsplit(": ")
                    .next()
                    .unwrap_or(e)
                    .to_string(),
            )
        }
    }
}

// ---------- macOS: diskutil, hdiutil

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub mod mac {
    use super::{Cmd, Out, Outcome};

    pub fn mount(dev: &str) -> Cmd {
        Cmd::new("diskutil", &["mount", "readOnly", dev])
    }

    pub fn unmount(dev: &str) -> Cmd {
        Cmd::new("diskutil", &["unmount", dev])
    }

    pub fn info(dev: &str) -> Cmd {
        Cmd::new("diskutil", &["info", dev])
    }

    pub fn attach(image: &str) -> Cmd {
        Cmd::new("hdiutil", &["attach", "-readonly", "-nobrowse", image])
    }

    pub fn detach(dev: &str) -> Cmd {
        Cmd::new("hdiutil", &["detach", dev])
    }

    /// diskutil mount / unmount: "Volume NAME on disk4s1 mounted", "... unmounted".
    pub fn parse(out: &Out) -> Outcome {
        let text = format!("{}\n{}", out.stdout, out.stderr);
        let t = text.trim();
        if out.ok && (t.ends_with(" mounted") || t.contains(" mounted\n")) {
            return Outcome::Mounted(String::new()); // where: from `diskutil info`
        }
        if out.ok {
            return Outcome::Done;
        }
        if t.contains("Could not find disk") {
            Outcome::Missing
        } else if t.contains("dissented") || t.contains("in use") || t.contains("busy") {
            Outcome::Busy
        } else if t.contains("already mounted") {
            Outcome::Already(String::new())
        } else if t.contains("not authorized") || t.contains("Not privileged") {
            Outcome::NeedsAuth
        } else {
            Outcome::Failed(t.lines().last().unwrap_or("diskutil failed").to_string())
        }
    }

    /// `diskutil info`: the "Mount Point:" line.
    pub fn mount_point(info: &str) -> Option<String> {
        info.lines()
            .find_map(|l| l.trim().strip_prefix("Mount Point:"))
            .map(|p| p.trim().to_string())
            .filter(|p| p.starts_with('/'))
    }

    /// `hdiutil attach`: each device it made, and where it's mounted (tab-separated columns).
    pub fn attached(out: &str) -> Vec<(String, Option<String>)> {
        out.lines()
            .filter_map(|l| {
                let cols: Vec<&str> = l.split('\t').map(str::trim).collect();
                let dev = cols.first().filter(|d| d.starts_with("/dev/"))?;
                let at = cols
                    .get(2)
                    .filter(|m| m.starts_with('/'))
                    .map(|m| m.to_string());
                Some((dev.to_string(), at))
            })
            .collect()
    }
}

// ---------- Windows: PowerShell's storage cmdlets

#[cfg_attr(not(windows), allow(dead_code))]
pub mod win {
    use super::Cmd;

    /// A PowerShell single-quoted string (a quote is doubled; nothing else is special).
    pub fn ps_quote(s: &str) -> String {
        format!("'{}'", s.replace('\'', "''"))
    }

    fn ps(script: &str) -> Cmd {
        Cmd::new(
            "powershell.exe",
            &["-NoProfile", "-NonInteractive", "-Command", script],
        )
    }

    /// Attaches an ISO, VHD or VHDX read-only and prints its volumes' drive letters.
    pub fn attach(image: &str) -> Cmd {
        ps(&format!(
            "Mount-DiskImage -ImagePath {} -Access ReadOnly -PassThru | Get-Volume | ForEach-Object {{ $_.DriveLetter }}",
            ps_quote(image)
        ))
    }

    pub fn detach(image: &str) -> Cmd {
        ps(&format!(
            "Dismount-DiskImage -ImagePath {} | Out-Null",
            ps_quote(image)
        ))
    }

    /// The drive letters PowerShell printed, as roots ("E:\").
    pub fn letters(out: &str) -> Vec<String> {
        out.lines()
            .map(str::trim)
            .filter(|l| l.len() == 1 && l.chars().all(|c| c.is_ascii_alphabetic()))
            .map(|l| format!("{}:\\", l.to_ascii_uppercase()))
            .collect()
    }
}

// ---------- what BackDown mounted

/// Something BackDown mounted or attached, so it can undo it, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mounted {
    /// The partition mounted, or the image attached.
    pub source: String,
    /// For an image: the device it's attached as (to detach).
    pub attached: Option<String>,
    /// What's mounted where: (device, mount point).
    pub mounts: Vec<(String, String)>,
    pub at: i64,
}

static RECORD: Mutex<()> = Mutex::new(());

fn record_path(state: &Path) -> PathBuf {
    state.join("mounts.json")
}

/// What BackDown has mounted, as recorded.
pub fn recorded(state: &Path) -> Vec<Mounted> {
    std::fs::read(record_path(state))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn change(state: &Path, f: impl FnOnce(&mut Vec<Mounted>)) -> Result<()> {
    let _g = RECORD.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = recorded(state);
    f(&mut all);
    std::fs::create_dir_all(state)?;
    let tmp = record_path(state).with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&all)?)?;
    std::fs::rename(&tmp, record_path(state))?;
    Ok(())
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Mounts the partition `dev` read-only and records it. `dev` must be what the volume probe calls
/// mountable (the caller checks).
pub fn mount(state: &Path, dev: &str) -> Result<Value> {
    let point = platform::mount(dev)?;
    change(state, |all| {
        all.retain(|m| m.source != dev);
        all.push(Mounted {
            source: dev.into(),
            attached: None,
            mounts: vec![(dev.into(), point.clone())],
            at: now(),
        });
    })?;
    Ok(json!({ "ok": true, "device": dev, "mounted": point }))
}

/// Unmounts what BackDown mounted from `dev` (a partition, or an image it attached, which is then
/// detached); anything else is refused.
pub fn unmount(state: &Path, target: &str) -> Result<Value> {
    let Some(m) = recorded(state).into_iter().find(|m| m.source == target) else {
        bail!(
            "BackDown didn't mount {target}, so it leaves it alone (unmount it the way it was mounted)"
        );
    };
    match &m.attached {
        Some(dev) => platform::detach(&m.source, dev, &m.mounts)?,
        None => {
            for (dev, _) in &m.mounts {
                platform::unmount(dev)?;
            }
        }
    }
    change(state, |all| all.retain(|x| x.source != target))?;
    Ok(json!({ "ok": true, "unmounted": target }))
}

/// Attaches the disk image `image` read-only and mounts what's on it read-only; records it.
pub fn attach(state: &Path, image: &Path) -> Result<Value> {
    let image =
        dunce::canonicalize(image).with_context(|| format!("can't find {}", image.display()))?;
    let src = image.display().to_string();
    if let Some(m) = recorded(state).into_iter().find(|m| m.source == src) {
        return Ok(
            json!({ "ok": true, "already": true, "attached": m.attached, "mounts": m.mounts }),
        );
    }
    let (dev, mounts) = platform::attach(&image)?;
    change(state, |all| {
        all.push(Mounted {
            source: src.clone(),
            attached: Some(dev.clone()),
            mounts: mounts.clone(),
            at: now(),
        });
    })?;
    Ok(json!({ "ok": true, "attached": dev, "mounts": mounts }))
}

/// The record, without what's no longer mounted (unplugged, or unmounted elsewhere).
pub fn current(state: &Path) -> Vec<Mounted> {
    let all = recorded(state);
    let live: Vec<Mounted> = all
        .iter()
        .filter(|m| m.attached.is_some() || m.mounts.iter().any(|(_, p)| platform::is_mount(p)))
        .cloned()
        .collect();
    if live.len() != all.len() {
        let keep = live.clone();
        let _ = change(state, move |a| *a = keep);
    }
    live
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    fn run(c: &Cmd) -> Result<Outcome> {
        Ok(udisks::parse(&c.run()?))
    }

    fn auth(what: &str, c: &Cmd) -> anyhow::Error {
        refused(
            format!(
                "Your system asks for an administrator's password to {what}. Run this in a terminal (it asks for the password itself), then look again:"
            ),
            Some(c),
        )
    }

    pub fn mount(dev: &str) -> Result<String> {
        let c = udisks::mount(dev, false);
        match run(&c)? {
            Outcome::Mounted(p) => {
                if writable(&p) {
                    // never leave a source mounted writable
                    let _ = run(&udisks::unmount(dev, false));
                    bail!("{dev} didn't mount read-only, so it was unmounted again");
                }
                Ok(p)
            }
            Outcome::Already(p) => bail!("{dev} is already mounted at {p}"),
            Outcome::NeedsAuth => Err(auth("mount this drive", &udisks::mount(dev, true))),
            Outcome::Missing => bail!("{dev} isn't there any more: is it still plugged in?"),
            Outcome::NotMountable => {
                bail!("there's no filesystem on {dev} this computer can mount")
            }
            Outcome::Busy => bail!("{dev} is busy"),
            Outcome::Done => bail!("udisksctl didn't say where it mounted {dev}"),
            Outcome::Failed(e) => bail!("{dev} couldn't be mounted: {e}"),
        }
    }

    pub fn unmount(dev: &str) -> Result<()> {
        match run(&udisks::unmount(dev, false))? {
            Outcome::Done | Outcome::Missing => Ok(()),
            Outcome::NeedsAuth => Err(auth("unmount this drive", &udisks::unmount(dev, true))),
            Outcome::Busy => bail!(
                "{dev} is in use: close anything showing files from it (a file manager, a terminal in it), then try again"
            ),
            o => bail!("{dev} couldn't be unmounted: {o:?}"),
        }
    }

    pub fn attach(image: &Path) -> Result<(String, Vec<(String, String)>)> {
        let img = image.display().to_string();
        let lc = udisks::loop_setup(&img, false);
        let dev = match run(&lc)? {
            Outcome::Mounted(d) if d.starts_with("/dev/") => d,
            Outcome::NeedsAuth => {
                return Err(auth("attach a disk image", &udisks::loop_setup(&img, true)));
            }
            o => bail!("{img} couldn't be attached: {o:?}"),
        };
        let name = dev.trim_start_matches("/dev/").to_string();
        // its partitions show up a moment after
        let mut parts = Vec::new();
        for _ in 0..15 {
            parts = partitions(&name);
            if !parts.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        if parts.is_empty() {
            parts.push(dev.clone());
        }
        let mut mounts = Vec::new();
        for p in &parts {
            // the desktop may have mounted it on its own by now (read-only: the device is)
            match run(&udisks::mount(p, false))? {
                Outcome::Mounted(at) | Outcome::Already(at) if !at.is_empty() => {
                    mounts.push((p.clone(), at))
                }
                _ => {}
            }
        }
        if mounts.is_empty() {
            let _ = detach(&img, &dev, &[]);
            bail!(
                "nothing on {img} could be mounted (this computer can't read its filesystems either)"
            );
        }
        Ok((dev, mounts))
    }

    pub fn detach(_image: &str, dev: &str, _mounts: &[(String, String)]) -> Result<()> {
        // everything mounted from it, however it got mounted
        for (d, _) in mounted_from(dev) {
            unmount(&d)?;
        }
        match run(&udisks::loop_delete(dev, false))? {
            Outcome::Done | Outcome::Missing => Ok(()),
            Outcome::NeedsAuth => Err(auth(
                "detach this disk image",
                &udisks::loop_delete(dev, true),
            )),
            o => bail!("{dev} couldn't be detached: {o:?}"),
        }
    }

    /// A loop device's partitions, from sysfs.
    fn partitions(name: &str) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(format!("/sys/class/block/{name}"))
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| n.starts_with(name) && n.len() > name.len())
                    .map(|n| format!("/dev/{n}"))
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    /// (device, mount point) for every mount, from mountinfo.
    fn mounts() -> Vec<(String, String, bool)> {
        std::fs::read_to_string("/proc/self/mountinfo")
            .map(|t| super::mountinfo(&t))
            .unwrap_or_default()
    }

    fn mounted_from(dev: &str) -> Vec<(String, String)> {
        mounts()
            .into_iter()
            .filter(|(d, _, _)| d == dev || d.strip_prefix(dev).is_some_and(|r| r.starts_with('p')))
            .map(|(d, p, _)| (d, p))
            .collect()
    }

    /// Whether something is mounted writable at `point`.
    fn writable(point: &str) -> bool {
        mounts().iter().any(|(_, p, ro)| p == point && !*ro)
    }

    pub fn is_mount(point: &str) -> bool {
        mounts().iter().any(|(_, p, _)| p == point)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    fn point_of(dev: &str) -> Option<String> {
        let out = mac::info(dev).run().ok()?;
        mac::mount_point(&out.stdout)
    }

    pub fn mount(dev: &str) -> Result<String> {
        let c = mac::mount(dev);
        match mac::parse(&c.run()?) {
            Outcome::Mounted(_) => point_of(dev).context("diskutil didn't say where it mounted it"),
            Outcome::NeedsAuth => Err(refused(
                "macOS wants an administrator to mount this drive. Run this in a terminal:",
                Some(&c),
            )),
            Outcome::Missing => bail!("{dev} isn't there any more: is it still plugged in?"),
            o => bail!("{dev} couldn't be mounted: {o:?}"),
        }
    }

    pub fn unmount(dev: &str) -> Result<()> {
        match mac::parse(&mac::unmount(dev).run()?) {
            Outcome::Done | Outcome::Mounted(_) | Outcome::Missing => Ok(()),
            Outcome::Busy => {
                bail!("{dev} is in use: close anything showing files from it, then try again")
            }
            o => bail!("{dev} couldn't be unmounted: {o:?}"),
        }
    }

    pub fn attach(image: &Path) -> Result<(String, Vec<(String, String)>)> {
        let out = mac::attach(&image.display().to_string()).run()?;
        let devs = mac::attached(&out.stdout);
        let Some((disk, _)) = devs.first().cloned() else {
            bail!(
                "{} couldn't be attached: {}",
                image.display(),
                out.stderr.trim()
            );
        };
        let mounts: Vec<(String, String)> = devs
            .into_iter()
            .filter_map(|(d, m)| m.map(|m| (d, m)))
            .collect();
        if mounts.is_empty() {
            let _ = mac::detach(&disk).run();
            bail!("nothing on {} could be mounted", image.display());
        }
        Ok((disk, mounts))
    }

    pub fn detach(_image: &str, dev: &str, _mounts: &[(String, String)]) -> Result<()> {
        let out = mac::detach(dev).run()?;
        if !out.ok && !out.stderr.contains("No such file") {
            bail!("{dev} couldn't be detached: {}", out.stderr.trim());
        }
        Ok(())
    }

    pub fn is_mount(point: &str) -> bool {
        Path::new(point).is_dir()
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    pub fn mount(_dev: &str) -> Result<String> {
        bail!(
            "Windows can't mount a drive read-only in general, so BackDown doesn't offer it: it reads Windows drives where they are, without changing them"
        )
    }

    pub fn unmount(_dev: &str) -> Result<()> {
        bail!("BackDown doesn't mount drives on Windows")
    }

    pub fn attach(image: &Path) -> Result<(String, Vec<(String, String)>)> {
        let img = image.display().to_string();
        let out = win::attach(&img).run()?;
        if !out.ok {
            bail!(
                "{img} couldn't be attached (Windows attaches ISO, VHD and VHDX images): {}",
                out.stderr.trim()
            );
        }
        let letters = win::letters(&out.stdout);
        if letters.is_empty() {
            let _ = win::detach(&img).run();
            bail!("nothing on {img} could be mounted");
        }
        Ok((img, letters.into_iter().map(|l| (l.clone(), l)).collect()))
    }

    pub fn detach(image: &str, _dev: &str, _mounts: &[(String, String)]) -> Result<()> {
        let out = win::detach(image).run()?;
        if !out.ok {
            bail!("{image} couldn't be detached: {}", out.stderr.trim());
        }
        Ok(())
    }

    pub fn is_mount(point: &str) -> bool {
        Path::new(point).is_dir()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod platform {
    use super::*;

    pub fn mount(_dev: &str) -> Result<String> {
        bail!("mounting isn't supported on this system")
    }
    pub fn unmount(_dev: &str) -> Result<()> {
        bail!("mounting isn't supported on this system")
    }
    pub fn attach(_image: &Path) -> Result<(String, Vec<(String, String)>)> {
        bail!("attaching images isn't supported on this system")
    }
    pub fn detach(_image: &str, _dev: &str, _mounts: &[(String, String)]) -> Result<()> {
        bail!("attaching images isn't supported on this system")
    }
    pub fn is_mount(_point: &str) -> bool {
        false
    }
}

/// Linux's mountinfo: (source device, mount point, read-only) for each mount.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn mountinfo(text: &str) -> Vec<(String, String, bool)> {
    text.lines()
        .filter_map(|l| {
            let (left, right) = l.split_once(" - ")?;
            let f: Vec<&str> = left.split(' ').collect();
            let point = unescape(f.get(4)?);
            let opts = f.get(5)?;
            let dev = unescape(right.split(' ').nth(1)?);
            let ro = opts.split(',').any(|o| o == "ro");
            Some((dev, point, ro))
        })
        .collect()
}

/// mountinfo's octal escapes (`\040` for a space).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(ok: bool, stdout: &str, stderr: &str) -> Out {
        Out {
            ok,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn udisksctl_commands_are_read_only() {
        assert_eq!(
            udisks::mount("/dev/sdb1", false).typed(),
            "udisksctl mount -b /dev/sdb1 -o ro --no-user-interaction"
        );
        assert_eq!(
            udisks::mount("/dev/sdb1", true).typed(),
            "udisksctl mount -b /dev/sdb1 -o ro"
        );
        assert_eq!(
            udisks::loop_setup("/media/me/old disk.img", false).args,
            [
                "loop-setup",
                "-r",
                "-f",
                "/media/me/old disk.img",
                "--no-user-interaction"
            ]
        );
        assert_eq!(
            udisks::loop_setup("/media/me/old disk.img", true).typed(),
            "udisksctl loop-setup -r -f '/media/me/old disk.img'"
        );
        assert_eq!(
            udisks::unmount("/dev/loop7p1", false).typed(),
            "udisksctl unmount -b /dev/loop7p1 --no-user-interaction"
        );
        assert_eq!(
            udisks::loop_delete("/dev/loop7", false).typed(),
            "udisksctl loop-delete -b /dev/loop7 --no-user-interaction"
        );
        assert_eq!(quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn udisksctl_output() {
        use Outcome::*;
        let p = |ok, so: &str, se: &str| udisks::parse(&out(ok, so, se));
        assert_eq!(
            p(true, "Mounted /dev/sdb1 at /media/me/CARD\n", ""),
            Mounted("/media/me/CARD".into())
        );
        // older versions end with a full stop
        assert_eq!(
            p(true, "Mounted /dev/sdb1 at /media/me/My Photos.\n", ""),
            Mounted("/media/me/My Photos".into())
        );
        assert_eq!(
            p(true, "Mapped file /media/me/old.img as /dev/loop13.\n", ""),
            Mounted("/dev/loop13".into())
        );
        assert_eq!(p(true, "Unmounted /dev/sdb1.\n", ""), Done);
        assert_eq!(
            p(
                false,
                "",
                "Error mounting /dev/sdb1: GDBus.Error:org.freedesktop.UDisks2.Error.NotAuthorizedCanObtain: Not authorized to perform operation\n"
            ),
            NeedsAuth
        );
        assert_eq!(
            p(
                false,
                "",
                "Error mounting /dev/loop13p1: GDBus.Error:org.freedesktop.UDisks2.Error.AlreadyMounted: Device /dev/loop13p1 is already mounted at `/media/me/DISK'.\n"
            ),
            Already("/media/me/DISK".into())
        );
        assert_eq!(
            p(
                false,
                "",
                "Error unmounting /dev/sdb1: GDBus.Error:org.freedesktop.UDisks2.Error.DeviceBusy: Error unmounting /dev/sdb1: target is busy\n"
            ),
            Busy
        );
        assert_eq!(
            p(
                false,
                "",
                "Error unmounting /dev/sdb1: GDBus.Error:org.freedesktop.UDisks2.Error.NotMounted: Device `/dev/sdb1' is not mounted\n"
            ),
            Done
        );
        assert_eq!(
            p(false, "", "Error looking up object for device /dev/sdz9\n"),
            Missing
        );
        assert_eq!(
            p(
                false,
                "",
                "Object /org/freedesktop/UDisks2/block_devices/loop13 is not a mountable filesystem.\n"
            ),
            NotMountable
        );
        assert_eq!(
            p(
                false,
                "",
                "Error mounting /dev/sdb1: GDBus.Error:org.freedesktop.UDisks2.Error.Failed: Error mounting system-managed device /dev/sdb1: Can't open blockdev\n"
            ),
            Failed("Can't open blockdev".into())
        );
    }

    #[test]
    fn diskutil_and_hdiutil() {
        use Outcome::*;
        assert_eq!(
            mac::mount("/dev/disk4s1").typed(),
            "diskutil mount readOnly /dev/disk4s1"
        );
        assert_eq!(
            mac::attach("/Users/me/old disk.dmg").args,
            ["attach", "-readonly", "-nobrowse", "/Users/me/old disk.dmg"]
        );
        assert_eq!(
            mac::detach("/dev/disk5").typed(),
            "hdiutil detach /dev/disk5"
        );
        let p = |ok, so: &str| mac::parse(&out(ok, so, ""));
        assert_eq!(
            p(true, "Volume CARD on disk4s1 mounted\n"),
            Mounted(String::new())
        );
        assert_eq!(p(true, "Volume CARD on disk4s1 unmounted\n"), Done);
        assert_eq!(
            p(
                false,
                "Volume CARD on disk4s1 failed to unmount: dissented by PID 412 (/usr/sbin/mds)\n"
            ),
            Busy
        );
        assert_eq!(p(false, "Could not find disk: /dev/disk9\n"), Missing);
        let info = "   Device Identifier:         disk4s1\n   Mount Point:               /Volumes/My Card\n   Read-Only Volume:          Yes\n";
        assert_eq!(mac::mount_point(info).as_deref(), Some("/Volumes/My Card"));
        assert_eq!(
            mac::mount_point("   Mount Point:               Not applicable (no file system)\n"),
            None
        );
        let attach = "/dev/disk5          \tGUID_partition_scheme          \t\n/dev/disk5s1        \tEFI                            \t\n/dev/disk5s2        \tApple_HFS                      \t/Volumes/Old Mac\n";
        assert_eq!(
            mac::attached(attach),
            vec![
                ("/dev/disk5".into(), None),
                ("/dev/disk5s1".into(), None),
                ("/dev/disk5s2".into(), Some("/Volumes/Old Mac".into())),
            ]
        );
    }

    #[test]
    fn powershell_disk_images() {
        let c = win::attach(r"D:\Backups\it's old.vhdx");
        assert_eq!(c.program, "powershell.exe");
        assert_eq!(
            c.args[3],
            r"Mount-DiskImage -ImagePath 'D:\Backups\it''s old.vhdx' -Access ReadOnly -PassThru | Get-Volume | ForEach-Object { $_.DriveLetter }"
        );
        assert_eq!(
            win::detach(r"D:\a.iso").args[3],
            r"Dismount-DiskImage -ImagePath 'D:\a.iso' | Out-Null"
        );
        assert_eq!(win::letters("E\r\n\r\nf\r\n"), ["E:\\", "F:\\"]);
        assert!(win::letters("").is_empty());
    }

    #[test]
    fn mountinfo_lines() {
        let t = "36 35 98:0 /mnt1 /mnt/parent\\040dir rw,noatime master:1 - ext3 /dev/root rw,errors=continue\n\
                 90 29 8:17 / /media/me/My\\040Card ro,nosuid,nodev shared:5 - vfat /dev/sdb1 ro,uid=1000\n";
        assert_eq!(
            mountinfo(t),
            vec![
                ("/dev/root".into(), "/mnt/parent dir".into(), false),
                ("/dev/sdb1".into(), "/media/me/My Card".into(), true),
            ]
        );
    }

    #[test]
    fn only_what_backdown_mounted_is_undone() {
        let tmp = tempfile::tempdir().unwrap();
        let err = unmount(tmp.path(), "/dev/sdz1").unwrap_err().to_string();
        assert!(err.contains("didn't mount"), "{err}");
        change(tmp.path(), |a| {
            a.push(Mounted {
                source: "/dev/sdz1".into(),
                attached: None,
                mounts: vec![("/dev/sdz1".into(), "/nonexistent/backdown-test".into())],
                at: 1,
            })
        })
        .unwrap();
        assert_eq!(recorded(tmp.path()).len(), 1);
        // no longer mounted: dropped from the record
        assert!(current(tmp.path()).is_empty());
        assert!(recorded(tmp.path()).is_empty());
    }
}
