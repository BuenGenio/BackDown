//! Saving what was found, and whether the destination is on the disk being read.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::fmt;

use super::*;

// ---------- saving

/// The list of what `scan_and_save` saved, one JSON object a line, in the destination folder.
pub const INDEX: &str = "found.jsonl";

#[derive(Serialize)]
pub(super) struct Saved<'a> {
    #[serde(flatten)]
    pub(super) found: &'a Found,
    /// where it was saved, from the destination folder
    pub(super) file: &'a str,
    #[serde(skip_serializing_if = "is_zero")]
    pub(super) unreadable: u64,
}

pub(super) fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// Copy a found file out to `dest_dir/<group>/f<sector>.<ext>` (named after where it starts, like
/// PhotoRec, so the name is the same every time), never overwriting anything: a name that's taken
/// by something else gets -1, -2… Saving the same file again finds it already there.
pub fn save(src: &Path, f: &Found, dest_dir: &Path) -> Result<PathBuf> {
    check_destination(src, dest_dir)?;
    let input = super::super::open(src)
        .with_context(|| format!("can't open {} to read it", src.display()))?;
    Ok(save_from(&input, f, dest_dir)?.0)
}

/// Scan `src` and save everything found into `dest_dir` as it goes (see `save`), listing it in
/// `dest_dir/found.jsonl`. Files that can't be read whole are saved with zeros for the unreadable
/// parts (listed in `problems`); a destination that can't be written to stops the scan.
pub fn scan_and_save(
    src: &Path,
    opts: &Options,
    dest_dir: &Path,
    progress: &mut dyn FnMut(&Progress),
    stop: &AtomicBool,
) -> Result<Stats> {
    check_destination(src, dest_dir)?;
    fs::create_dir_all(dest_dir).with_context(|| format!("can't create {}", dest_dir.display()))?;
    let input = super::super::open(src)
        .with_context(|| format!("can't open {} to read it", src.display()))?;
    let ix_path = dest_dir.join(INDEX);
    let mut ix = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&ix_path)
        .with_context(|| format!("can't create {}", ix_path.display()))?;
    let (mut saved, mut saved_bytes, mut problems) = (0u64, 0u64, Vec::new());
    let mut stats = scan(src, opts, progress, stop, &mut |f: &Found| -> Result<()> {
        let (path, bad) = save_from(&input, f, dest_dir)?;
        let rel = path
            .strip_prefix(dest_dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let line = serde_json::to_string(&Saved {
            found: f,
            file: &rel,
            unreadable: bad,
        })?;
        writeln!(ix, "{line}").with_context(|| format!("can't write to {}", ix_path.display()))?;
        saved += 1;
        saved_bytes += f.size;
        if bad > 0 {
            note(
                &mut problems,
                format!("{rel}: {} couldn't be read, and are zeros", fmt::bytes(bad)),
            );
        }
        Ok(())
    })?;
    stats.saved = saved;
    stats.saved_bytes = saved_bytes;
    for p in problems {
        note(&mut stats.problems, p);
    }
    Ok(stats)
}

/// Saving onto the drive being rescued could overwrite the very files being looked for. (An image
/// file is safe to save next to: its blocks belong to it.)
pub(super) fn check_destination(src: &Path, dest_dir: &Path) -> Result<()> {
    let image = fs::metadata(src).is_ok_and(|m| m.is_file());
    if !image && same_device(src, dest_dir) {
        bail!(
            "{} is on the drive being rescued: saving there could overwrite the very files being \
             looked for. Choose a folder on another drive",
            dest_dir.display()
        );
    }
    Ok(())
}

/// f0001234567: the 512-byte sector it starts at, like PhotoRec.
pub(super) fn file_stem(offset: u64) -> String {
    if offset.is_multiple_of(512) {
        format!("f{:010}", offset / 512)
    } else {
        format!("f{:010}_{}", offset / 512, offset % 512)
    }
}

/// Save `f` from the open source; the path, and how many bytes couldn't be read.
pub(super) fn save_from(input: &File, f: &Found, dest_dir: &Path) -> Result<(PathBuf, u64)> {
    let dir = dest_dir.join(f.group);
    fs::create_dir_all(&dir).with_context(|| format!("can't create {}", dir.display()))?;
    let stem = file_stem(f.offset);
    for i in 0..1000 {
        let name = if i == 0 {
            format!("{stem}.{}", f.ext)
        } else {
            format!("{stem}-{i}.{}", f.ext)
        };
        let path = dir.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(out) => {
                return match copy_out(input, f, out) {
                    Ok(bad) => Ok((path, bad)),
                    Err(e) => {
                        let _ = fs::remove_file(&path);
                        Err(e).with_context(|| format!("can't write {}", path.display()))
                    }
                };
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if same_bytes(input, f, &path)? {
                    return Ok((path, 0));
                }
            }
            Err(e) => return Err(e).with_context(|| format!("can't create {}", path.display())),
        }
    }
    bail!("too many files named {stem}.* in {}", dir.display())
}

/// Copy the bytes out; what can't be read is written as zeros. How many bytes that was.
pub(super) fn copy_out(input: &File, f: &Found, mut out: File) -> io::Result<u64> {
    let mut buf = vec![0u8; BLOCK];
    let (mut done, mut bad) = (0u64, 0u64);
    while done < f.size {
        let k = (f.size - done).min(BLOCK as u64) as usize;
        let at = f.offset + done;
        match read_full(input, at, &mut buf[..k]) {
            Ok(n) if n == k => {}
            Ok(n) => {
                // the source is shorter than it was: keep what there is
                out.write_all(&buf[..n])?;
                return Ok(bad);
            }
            Err(_) => {
                for (i, piece) in buf[..k].chunks_mut(4096).enumerate() {
                    let o = at + (i * 4096) as u64;
                    if read_full(input, o, piece).is_err() {
                        piece.fill(0);
                        bad += piece.len() as u64;
                    }
                }
            }
        }
        out.write_all(&buf[..k])?;
        done += k as u64;
    }
    out.flush()?;
    Ok(bad)
}

/// Is the file at `path` already exactly this found file (saved by an earlier run)?
pub(super) fn same_bytes(input: &File, f: &Found, path: &Path) -> Result<bool> {
    if fs::metadata(path).map(|m| m.len()).ok() != Some(f.size) {
        return Ok(false);
    }
    let there = File::open(path).with_context(|| format!("can't read {}", path.display()))?;
    let (mut a, mut b) = (vec![0u8; BLOCK], vec![0u8; BLOCK]);
    let mut done = 0u64;
    while done < f.size {
        let k = (f.size - done).min(BLOCK as u64) as usize;
        let n = read_full(input, f.offset + done, &mut a[..k]).unwrap_or(0);
        let m = read_full(&there, done, &mut b[..k])
            .with_context(|| format!("can't read {}", path.display()))?;
        if n != k || m != k || a[..k] != b[..k] {
            return Ok(false);
        }
        done += k as u64;
    }
    Ok(true)
}

// ---------- same device

/// Whether `dest_dir` is on the same device as `src`: for a disk or partition, the destination's
/// filesystem lives on it (on Linux, anywhere on the same physical disk, through partitions,
/// device-mapper and RAID); for an image file, it's on the same filesystem as the image.
#[cfg(unix)]
pub fn same_device(src: &Path, dest_dir: &Path) -> bool {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let Ok(sm) = fs::metadata(src) else {
        return false;
    };
    let Some(dm) = existing(dest_dir).and_then(|p| fs::metadata(p).ok()) else {
        return false;
    };
    let ft = sm.file_type();
    if ft.is_block_device() || ft.is_char_device() {
        sm.rdev() == dm.dev() || shares_disk(src, sm.rdev(), dest_dir, dm.dev())
    } else {
        sm.dev() == dm.dev()
    }
}

/// Best effort on Windows: the same drive letter (`\\.\E:` and `E:\Recovered`). A whole disk
/// (`\\.\PhysicalDrive1`) can't be told apart from its volumes here.
#[cfg(windows)]
pub fn same_device(src: &Path, dest_dir: &Path) -> bool {
    fn letter(p: &Path) -> Option<char> {
        let s = p.to_string_lossy();
        let s = s.trim_start_matches(r"\\.\").trim_start_matches(r"\\?\");
        let mut c = s.chars();
        let l = c.next()?;
        (l.is_ascii_alphabetic() && c.next() == Some(':')).then(|| l.to_ascii_uppercase())
    }
    let dest = std::path::absolute(dest_dir).unwrap_or_else(|_| dest_dir.to_path_buf());
    matches!((letter(src), letter(&dest)), (Some(a), Some(b)) if a == b)
}

#[cfg(not(any(unix, windows)))]
pub fn same_device(_src: &Path, _dest_dir: &Path) -> bool {
    false
}

/// The nearest folder that exists at or above `p` (the destination may not be created yet).
#[cfg(unix)]
pub(super) fn existing(p: &Path) -> Option<PathBuf> {
    let abs = std::path::absolute(p).ok()?;
    abs.ancestors().find(|a| a.exists()).map(Path::to_path_buf)
}

#[cfg(target_os = "linux")]
pub(super) fn shares_disk(_src: &Path, src_dev: u64, dest: &Path, dest_dev: u64) -> bool {
    let a = linux_disks(src_dev);
    let mut b = linux_disks(dest_dev);
    if b.is_empty()
        && let Some(d) = mount_source(dest)
    {
        b = linux_disks(d); // btrfs and others report a device number of their own
    }
    a.iter().any(|x| b.contains(x))
}

/// The whole disks under a block device: itself, the disk a partition is on, and the disks under
/// a device-mapper (LVM, LUKS) or RAID device, as their sysfs paths.
#[cfg(target_os = "linux")]
pub(super) fn linux_disks(dev: u64) -> Vec<PathBuf> {
    let major = ((dev >> 32) & 0xFFFF_F000) | ((dev >> 8) & 0xFFF);
    let minor = ((dev >> 12) & 0xFFFF_FF00) | (dev & 0xFF);
    let mut out = Vec::new();
    if let Ok(p) = fs::canonicalize(format!("/sys/dev/block/{major}:{minor}")) {
        disks_under(&p, &mut out, 0);
    }
    out
}

#[cfg(target_os = "linux")]
pub(super) fn disks_under(p: &Path, out: &mut Vec<PathBuf>, depth: u32) {
    let disk = if p.join("partition").exists() {
        p.parent().unwrap_or(p)
    } else {
        p
    };
    let mut lower = false;
    if depth < 8
        && let Ok(rd) = fs::read_dir(disk.join("slaves"))
    {
        for e in rd.flatten() {
            if let Ok(q) = fs::canonicalize(e.path()) {
                lower = true;
                disks_under(&q, out, depth + 1);
            }
        }
    }
    if !lower && !out.iter().any(|x| x == disk) {
        out.push(disk.to_path_buf());
    }
}

/// The device the filesystem holding `dest` was mounted from, per /proc/self/mountinfo.
#[cfg(target_os = "linux")]
pub(super) fn mount_source(dest: &Path) -> Option<u64> {
    crate::volumes::mountinfo::block_device_of(&existing(dest)?)
}

/// macOS, best effort: the destination's volume is a slice of the same disk (`disk4s2` of
/// `/dev/rdisk4`). APFS volumes sit on a synthesized container disk, which this doesn't see through.
#[cfg(target_os = "macos")]
pub(super) fn shares_disk(src: &Path, _src_dev: u64, _dest: &Path, dest_dev: u64) -> bool {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let base = |name: &str| -> Option<String> {
        let n = name
            .strip_prefix('r')
            .unwrap_or(name)
            .strip_prefix("disk")?;
        let digits: String = n.chars().take_while(char::is_ascii_digit).collect();
        (!digits.is_empty()).then(|| format!("disk{digits}"))
    };
    let Some(a) = src.file_name().and_then(|n| base(&n.to_string_lossy())) else {
        return false;
    };
    let Ok(rd) = fs::read_dir("/dev") else {
        return false;
    };
    rd.flatten().any(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        name.starts_with("disk")
            && e.metadata()
                .is_ok_and(|m| m.file_type().is_block_device() && m.rdev() == dest_dev)
            && base(&name).as_deref() == Some(a.as_str())
    })
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub(super) fn shares_disk(_src: &Path, _src_dev: u64, _dest: &Path, _dest_dev: u64) -> bool {
    false
}
