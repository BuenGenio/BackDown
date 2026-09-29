//! Scans from an NTFS volume's master file table (`fsread::mft_scan`): every file and folder of
//! the volume read in order from the device, instead of folder by folder, then written as a walk
//! would write them. Used for first scans and whenever the change log can't be used.
//!
//! The device has to be readable, and has to be shown to hold what's mounted at the root:
//!
//! - **Linux**: only for NTFS mounted by ntfs-3g (FUSE, where walking is slowest; the kernel's
//!   drivers walk fast). The mount is found in mountinfo, its device opened read-only (it needs
//!   read permission: Rescue's one-line command gives it), and the table must hold the root folder
//!   under the same record number ntfs-3g gives it as inode, with the same names in it as the
//!   mount lists. ntfs-3g writes every change straight through to the device, so the table is
//!   current. Entries are what ntfs-3g shows: reparse points and Interix files are links or special
//!   files; if ntfs-3g has a plugin for a kind of reparse point the drive uses, the table isn't
//!   used (a plugin shows those as files, and how is up to it).
//! - **Windows**: `\\.\X:` (administrator rights). The volume's serial and the root folder's file
//!   id must match. Windows writes the table lazily, so what changed while (and just before) it
//!   was read is taken from the USN journal afterwards (`catch_up`).
//!
//! Anything that doesn't match means a walk instead.

use std::collections::BTreeMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::journal::Cursor;
use super::{ChangeSet, Item, Options};
use crate::fsread::mft_scan::{self, Flavour, Kind, Record};

/// How long reading a table may take before it's given up (a walk then).
#[allow(dead_code)]
const LIMIT: Duration = Duration::from_secs(30 * 60);

/// A volume's table, ready to be written as a scan of a root on it.
pub struct Table {
    /// files and folders in use in the table
    pub records: u64,
    /// how it went, for people
    pub note: String,
    /// what changed while the table was being read, from the change log (Windows)
    pub catch_up: Option<ChangeSet>,
    mft: mft_scan::Table,
    /// the root folder's record
    top: u64,
    flavour: Flavour,
    /// Linux: the mount's device number, for every row; Windows: the volume serial, for folders
    dev: Option<i64>,
    /// folders under the root with another filesystem mounted on them: recorded empty, with the
    /// identity of what's mounted there, as a walk that stays on one filesystem does
    mounts: BTreeMap<PathBuf, (Option<i64>, Option<i64>)>,
}

/// Reads the table of the NTFS volume holding `root`, or says why it can't be used. `cursor`:
/// where the change log was when the scan began (to catch up from, on Windows).
pub(crate) fn read(root: &Path, opts: &Options, cursor: Option<&Cursor>) -> Result<Table, String> {
    if !opts.one_file_system {
        return Err("scans that go into other filesystems are always walked".into());
    }
    #[cfg(target_os = "linux")]
    {
        let _ = cursor;
        linux::read(root)
    }
    #[cfg(windows)]
    {
        windows::read(root, cursor)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (root, cursor);
        Err("the file table is read on Linux (ntfs-3g) and Windows".into())
    }
}

impl Table {
    /// The table as walk items, from the root down.
    pub(crate) fn items(&self, root: &Path) -> Items<'_> {
        Items {
            t: self,
            walk: self.mft.walk(self.top),
            paths: Vec::new(),
            root: root.to_path_buf(),
        }
    }

    fn ids(&self, rec: &Record) -> (Option<i64>, Option<i64>) {
        match (self.flavour, rec.kind) {
            // as stat on ntfs-3g gives them: the mount's device, the record number as inode
            (Flavour::Ntfs3g, _) => (self.dev, Some(rec.record as i64)),
            // as Windows walks give them: folders have the volume serial and file id, files none
            (Flavour::Windows, Kind::Dir) => (self.dev, Some(rec.frn() as i64)),
            (Flavour::Windows, _) => (None, None),
        }
    }
}

/// See `Table::items`.
pub(crate) struct Items<'a> {
    t: &'a Table,
    walk: mft_scan::Walk<'a>,
    /// the path of the folder open at each depth
    paths: Vec<PathBuf>,
    root: PathBuf,
}

impl Iterator for Items<'_> {
    type Item = Item;

    fn next(&mut self) -> Option<Item> {
        loop {
            let (depth, name, rec) = self.walk.next()?;
            let path = if depth == 0 {
                self.root.clone()
            } else {
                self.paths.truncate(depth);
                let Some(parent) = self.paths.get(depth - 1) else {
                    // can't be: folders that aren't entered have nothing below them
                    return Some(Item::Error(format!(
                        "{}: lost its place in the file table",
                        String::from_utf16_lossy(name)
                    )));
                };
                match name_of(name) {
                    Some(n) => parent.join(n),
                    None => {
                        if rec.kind == Kind::Dir {
                            self.walk.dont_enter();
                        }
                        if matches!(rec.kind, Kind::Dir | Kind::File) {
                            return Some(Item::Error(format!(
                                "{}: a name that can't be shown here ({})",
                                parent.display(),
                                String::from_utf16_lossy(name)
                            )));
                        }
                        continue;
                    }
                }
            };
            match rec.kind {
                Kind::Dir => {
                    self.paths.truncate(depth);
                    if let Some(&(dev, ino)) = self.t.mounts.get(&path) {
                        self.walk.dont_enter();
                        return Some(Item::Dir {
                            path,
                            depth,
                            dev,
                            ino,
                        });
                    }
                    self.paths.push(path.clone());
                    let (dev, ino) = self.t.ids(rec);
                    return Some(Item::Dir {
                        path,
                        depth,
                        dev,
                        ino,
                    });
                }
                Kind::File => {
                    let (dev, ino) = self.t.ids(rec);
                    return Some(Item::File {
                        path,
                        depth,
                        size: rec.size,
                        mtime: rec.mtime,
                        dev,
                        ino,
                        links: 1,
                    });
                }
                Kind::Link | Kind::Special => continue,
            }
        }
    }
}

/// A name from the table as this system spells it: None if it can't (unpaired UTF-16
/// surrogates on Linux, which ntfs-3g leaves out of listings too; a slash or NUL).
#[cfg(windows)]
fn name_of(units: &[u16]) -> Option<std::ffi::OsString> {
    use std::os::windows::ffi::OsStringExt;
    (!units.is_empty() && !units.contains(&0) && !units.contains(&u16::from(b'\\')))
        .then(|| std::ffi::OsString::from_wide(units))
}

#[cfg(not(windows))]
fn name_of(units: &[u16]) -> Option<std::ffi::OsString> {
    let s = String::from_utf16(units).ok()?;
    (!s.is_empty() && !s.contains(['/', '\0'])).then(|| s.into())
}

/// A path's names below the volume's top, as the table spells them.
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
fn names_of(rel: &Path) -> Option<Vec<Vec<u16>>> {
    rel.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(n) => Some(n),
            _ => None,
        })
        .map(|n| {
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStrExt;
                Some(n.encode_wide().collect())
            }
            #[cfg(not(windows))]
            {
                n.to_str().map(|s| s.encode_utf16().collect())
            }
        })
        .collect()
}

/// A root folder whose listing (as the system shows it) matches what the table has in it: the
/// table is of the volume that's mounted there.
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
fn same_listing(root: &Path, mft: &mft_scan::Table, top: u64) -> Result<(), String> {
    let mut shown: Vec<std::ffi::OsString> = std::fs::read_dir(root)
        .map_err(|e| format!("can't list {}: {e}", root.display()))?
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();
    let mut table: Vec<std::ffi::OsString> =
        mft.children(top).filter_map(|(n, _)| name_of(n)).collect();
    shown.sort();
    table.sort();
    if shown != table {
        return Err(
            "the drive's device doesn't list the same files as its mount (hidden files, a different drive?)"
                .into(),
        );
    }
    Ok(())
}

/// Reads through a device that only allows whole, aligned sectors (a Windows volume handle):
/// every read and seek works, served from aligned reads of up to 1 MiB.
pub struct Aligned<R> {
    inner: R,
    sector: u64,
    pos: u64,
    buf: Vec<u8>,
    buf_at: u64,
}

impl<R: Read + Seek> Aligned<R> {
    pub fn new(inner: R, sector: u64) -> Aligned<R> {
        Aligned {
            inner,
            sector: sector.max(1),
            pos: 0,
            buf: Vec::new(),
            buf_at: 0,
        }
    }
}

impl<R: Read + Seek> Read for Aligned<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let s = self.sector;
        if self.pos.is_multiple_of(s) && (out.len() as u64).is_multiple_of(s) {
            self.inner.seek(SeekFrom::Start(self.pos))?;
            let n = self.inner.read(out)?;
            self.pos += n as u64;
            return Ok(n);
        }
        let held = self.buf_at..self.buf_at + self.buf.len() as u64;
        if !held.contains(&self.pos) {
            let start = self.pos / s * s;
            let end = (self.pos + out.len() as u64).div_ceil(s) * s;
            let len = (end - start).min(1 << 20).max(s) as usize;
            self.buf.resize(len, 0);
            self.inner.seek(SeekFrom::Start(start))?;
            let mut got = 0;
            while got < len {
                match self.inner.read(&mut self.buf[got..])? {
                    0 => break,
                    n => got += n,
                }
                if !(got as u64).is_multiple_of(s) {
                    break; // a short read that isn't whole sectors: the end of the device
                }
            }
            self.buf.truncate(got);
            self.buf_at = start;
            if (self.pos - start) as usize >= got {
                return Ok(0); // past the end
            }
        }
        let from = (self.pos - self.buf_at) as usize;
        let n = out.len().min(self.buf.len() - from);
        out[..n].copy_from_slice(&self.buf[from..from + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for Aligned<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.pos = match to {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(d) => self
                .pos
                .checked_add_signed(d)
                .ok_or(io::ErrorKind::InvalidInput)?,
            SeekFrom::End(d) => {
                let end = self.inner.seek(SeekFrom::End(0))?;
                end.checked_add_signed(d)
                    .ok_or(io::ErrorKind::InvalidInput)?
            }
        };
        Ok(self.pos)
    }
}

// ---------- Linux: ntfs-3g mounts

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeMap;
    use std::fs::File;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    use super::{LIMIT, Table, names_of, same_listing};
    use crate::fsread::{self, mft_scan};

    /// One line of mountinfo.
    #[derive(Debug)]
    pub(super) struct Mount {
        pub dev: (u64, u64),
        /// the folder of the filesystem that's mounted (usually its top, "/")
        pub root: PathBuf,
        pub point: PathBuf,
        pub fstype: String,
        pub source: String,
    }

    /// mountinfo's escapes: `\040` for a space and the like.
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

    pub(super) fn mountinfo(text: &str) -> Vec<Mount> {
        text.lines()
            .filter_map(|line| {
                // id parent major:minor root mount-point options [optional…] - fstype source super
                let f: Vec<&str> = line.split(' ').collect();
                let sep = 6 + f.get(6..)?.iter().position(|x| *x == "-")?;
                let (maj, min) = f.get(2)?.split_once(':')?;
                Some(Mount {
                    dev: (maj.parse().ok()?, min.parse().ok()?),
                    root: PathBuf::from(unescape(f.get(3)?)),
                    point: PathBuf::from(unescape(f.get(4)?)),
                    fstype: f.get(sep + 1)?.to_string(),
                    source: unescape(f.get(sep + 2)?),
                })
            })
            .collect()
    }

    /// glibc's split of a device number.
    pub(super) fn split(dev: u64) -> (u64, u64) {
        (
            ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff),
            (dev & 0xff) | ((dev >> 12) & !0xff),
        )
    }

    /// Whether ntfs-3g has a plugin for reparse points of this kind (then it shows them its way).
    fn plugin(tag: u32) -> bool {
        let mut dirs: Vec<PathBuf> = [
            "/usr/lib/ntfs-3g",
            "/usr/lib64/ntfs-3g",
            "/usr/local/lib/ntfs-3g",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        if let Ok(rd) = std::fs::read_dir("/usr/lib") {
            dirs.extend(rd.flatten().map(|e| e.path().join("ntfs-3g")));
        }
        let names = [
            format!("ntfs-plugin-{tag:08x}.so"),
            format!("ntfs-plugin-{:08x}.so", tag & 0xffff_0fff),
        ];
        dirs.iter()
            .any(|d| names.iter().any(|n| d.join(n).exists()))
    }

    pub(super) fn read(root: &Path) -> Result<Table, String> {
        let meta = std::fs::metadata(root).map_err(|e| e.to_string())?;
        let text = std::fs::read_to_string("/proc/self/mountinfo")
            .map_err(|_| "can't read the system's list of mounts")?;
        let mounts = mountinfo(&text);
        let dev = split(meta.dev());
        let m = mounts
            .iter()
            .filter(|m| m.dev == dev && root.starts_with(&m.point))
            .max_by_key(|m| m.point.components().count())
            .ok_or("can't find where the drive is mounted")?;
        match m.fstype.as_str() {
            "fuseblk" | "fuse" => {}
            "ntfs3" | "ntfs" => {
                return Err(
                    "the file table is read for ntfs-3g mounts; this one is the kernel's (which walks fast)"
                        .into(),
                );
            }
            _ => return Err("not an NTFS drive".into()),
        }
        let source = PathBuf::from(&m.source);
        let access = crate::rescue::access(&source);
        if !access.readable {
            return Err(match access.fix {
                Some(fix) => format!(
                    "the file table needs read access to {} (to allow it: {fix})",
                    source.display()
                ),
                None => format!(
                    "{} can't be read directly{}",
                    source.display(),
                    access.why.map(|w| format!(": {w}")).unwrap_or_default()
                ),
            });
        }
        let f = File::open(&source).map_err(|e| format!("{}: {e}", source.display()))?;
        let size = crate::rescue::size_of(&f).map_err(|e| e.to_string())?;
        let mut vol = fsread::Volume::open(f, 0, size).map_err(|_| "not an NTFS drive")?;
        if vol.info().kind != "NTFS" {
            return Err("not an NTFS drive".into());
        }
        let mft = mft_scan::read(
            &mut vol,
            mft_scan::Flavour::Ntfs3g,
            Some(Instant::now() + LIMIT),
            &mut |_, _| {},
        )
        .map_err(|e| format!("the file table can't be read: {e:#}"))?;
        if let Some(t) = mft.tags.iter().find(|t| plugin(**t)) {
            return Err(format!(
                "ntfs-3g has a plugin for some of this drive's files (reparse tag {t:08x})"
            ));
        }
        // the root in the table: reached by name from the mount's own top, with the record
        // number ntfs-3g gives it as inode, and listing the same names
        let rel = root.strip_prefix(&m.point).map_err(|e| e.to_string())?;
        let names = names_of(&m.root.join(rel)).ok_or("a name that isn't UTF-8")?;
        let top = mft
            .find(&names)
            .ok_or("the drive's device doesn't hold the folder being scanned")?;
        if top.record != meta.ino() {
            return Err("the drive's device isn't the one mounted there".into());
        }
        let top = top.record;
        same_listing(root, &mft, top)?;
        // other filesystems mounted inside the root: a walk records those folders empty
        let mut nested = BTreeMap::new();
        for other in mounts
            .iter()
            .filter(|o| o.point != m.point && o.point.starts_with(root))
        {
            let id = std::fs::symlink_metadata(&other.point)
                .map(|m| (Some(m.dev() as i64), Some(m.ino() as i64)))
                .unwrap_or((None, None));
            nested.insert(other.point.clone(), id);
        }
        let records = mft.in_use;
        Ok(Table {
            records,
            note: format!(
                "read the file table: {} records{}",
                crate::fmt::count(records),
                if mft.dirty {
                    " (the drive wasn't shut down cleanly)"
                } else {
                    ""
                }
            ),
            catch_up: None,
            mft,
            top,
            flavour: mft_scan::Flavour::Ntfs3g,
            dev: Some(meta.dev() as i64),
            mounts: nested,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn mountinfo_lines() {
            let text = "36 25 0:57 / /media/u/Old\\040Drive rw,nosuid,nodev,relatime shared:112 - fuseblk /dev/sdb1 rw,user_id=0,group_id=0,allow_other,blksize=4096\n\
                        37 25 8:2 /photos /mnt/p rw,relatime - ext4 /dev/sda2 rw\n\
                        garbage\n";
            let m = mountinfo(text);
            assert_eq!(m.len(), 2);
            assert_eq!(m[0].point, PathBuf::from("/media/u/Old Drive"));
            assert_eq!((m[0].dev, m[0].fstype.as_str()), ((0, 57), "fuseblk"));
            assert_eq!(m[0].source, "/dev/sdb1");
            assert_eq!(m[1].root, PathBuf::from("/photos"));
            // glibc's makedev(8, 2) and makedev(259, 70000)
            assert_eq!(split(0x802), (8, 2));
            let big = (259u64 & 0xfff) << 8 | (70000 & 0xff) | ((70000 & !0xff) << 12);
            assert_eq!(split(big), (259, 70000));
        }
    }
}

// ---------- Windows: \\.\X:, as administrator

#[cfg(windows)]
mod windows {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    use windows_sys::Win32::Storage::FileSystem::{
        GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    };

    use super::{Aligned, LIMIT, Table, names_of, same_listing};
    use crate::fsread::{self, mft_scan};
    use crate::index::journal::Cursor;
    use crate::index::usn;

    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    /// The volume holding `root`: its path ("E:\") and its device (\\.\E: or \\?\Volume{…}).
    fn volume(root: &Path) -> Option<(PathBuf, PathBuf)> {
        let mut buf = [0u16; 1024];
        let w = wide(root);
        // SAFETY: a NUL-terminated input and a buffer of the given length
        if unsafe { GetVolumePathNameW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } == 0 {
            return None;
        }
        let n = buf.iter().position(|&c| c == 0)?;
        let path = PathBuf::from(OsString::from_wide(&buf[..n]));
        let s = path.to_string_lossy().to_string();
        if s.len() == 3 && s.ends_with(":\\") {
            return Some((path, PathBuf::from(format!(r"\\.\{}", &s[..2]))));
        }
        let mut name = [0u16; 128];
        let w = wide(&path);
        // SAFETY: as above
        if unsafe {
            GetVolumeNameForVolumeMountPointW(w.as_ptr(), name.as_mut_ptr(), name.len() as u32)
        } == 0
        {
            return None;
        }
        let n = name.iter().position(|&c| c == 0)?;
        let dev = String::from_utf16_lossy(&name[..n]);
        Some((path, PathBuf::from(dev.trim_end_matches('\\'))))
    }

    pub(super) fn read(root: &Path, cursor: Option<&Cursor>) -> Result<Table, String> {
        let (serial, frn) = usn::sys::identity(root).ok_or("can't read the root folder's id")?;
        let (vol_path, device) = volume(root).ok_or("can't tell which volume holds the root")?;
        let f = std::fs::File::open(&device).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                "reading the file table needs administrator rights".to_string()
            } else {
                format!("{}: {e}", device.display())
            }
        })?;
        let mut src = Aligned::new(f, 4096);
        // a volume handle has no size: the boot sector says how big the volume is
        let mut boot = [0u8; 512];
        std::io::Read::read_exact(&mut src, &mut boot).map_err(|e| e.to_string())?;
        if &boot[3..11] != b"NTFS    " {
            return Err("not an NTFS volume".into());
        }
        let bps = u64::from(u16::from_le_bytes([boot[11], boot[12]]));
        let sectors = u64::from_le_bytes(boot[40..48].try_into().expect("8 bytes"));
        let len = sectors.saturating_add(1).saturating_mul(bps);
        let mut vol = fsread::Volume::open(src, 0, len).map_err(|e| format!("{e:#}"))?;
        let mft = mft_scan::read(
            &mut vol,
            mft_scan::Flavour::Windows,
            Some(Instant::now() + LIMIT),
            &mut |_, _| {},
        )
        .map_err(|e| format!("the file table can't be read: {e:#}"))?;
        if mft.serial as u32 != serial {
            return Err("the volume's device isn't the one holding the root".into());
        }
        if mft.dirty {
            return Err("the volume is marked as needing a check (chkdsk)".into());
        }
        let rel = root.strip_prefix(&vol_path).map_err(|e| e.to_string())?;
        let names = names_of(rel).ok_or("a name that can't be read")?;
        let top = mft
            .find(&names)
            .ok_or("the volume's table doesn't hold the folder being scanned")?;
        if top.frn() != frn {
            return Err("the volume's table doesn't match the root folder".into());
        }
        let top = top.record;
        same_listing(root, &mft, top)?;
        // Windows writes the table lazily: catch up from the change log, from a little before
        // this scan began
        let catch_up = match cursor {
            Some(Cursor::Usn {
                volume,
                journal,
                usn,
            }) if *volume == u64::from(serial) => {
                let mut folders = Vec::new();
                let mut paths: Vec<PathBuf> = Vec::new();
                for (depth, name, rec) in mft.walk(top) {
                    if rec.kind != mft_scan::Kind::Dir {
                        continue;
                    }
                    paths.truncate(depth);
                    let p = if depth == 0 {
                        root.to_path_buf()
                    } else {
                        match (paths.get(depth - 1), super::name_of(name)) {
                            (Some(parent), Some(n)) => parent.join(n),
                            _ => continue,
                        }
                    };
                    paths.push(p.clone());
                    folders.push((p, rec.frn()));
                }
                Some(usn::sys::catch_up(root, *journal, *usn, folders)?)
            }
            _ => None,
        };
        let records = mft.in_use;
        Ok(Table {
            records,
            note: format!(
                "read the file table: {} records",
                crate::fmt::count(records)
            ),
            catch_up,
            mft,
            top,
            flavour: mft_scan::Flavour::Windows,
            dev: Some(i64::from(serial)),
            mounts: BTreeMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor as Mem;

    /// Only whole, aligned sectors, like a Windows volume handle.
    struct Strict(Mem<Vec<u8>>);

    impl Read for Strict {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let pos = self.0.position();
            assert!(
                pos.is_multiple_of(512) && buf.len().is_multiple_of(512),
                "unaligned read"
            );
            self.0.read(buf)
        }
    }

    impl Seek for Strict {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.0.seek(to)
        }
    }

    #[test]
    fn aligned_reads_serve_any_read() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i * 7) as u8).collect();
        let mut a = Aligned::new(Strict(Mem::new(data.clone())), 512);
        for (at, len) in [
            (0, 10),
            (3, 700),
            (511, 2),
            (1024, 1024),
            (9_990, 10),
            (5000, 3000),
        ] {
            let mut buf = vec![0u8; len];
            a.seek(SeekFrom::Start(at)).unwrap();
            a.read_exact(&mut buf).unwrap();
            assert_eq!(buf, data[at as usize..at as usize + len], "at {at}");
        }
        a.seek(SeekFrom::Start(9_999)).unwrap();
        let mut buf = [0u8; 4];
        assert_eq!(a.read(&mut buf).unwrap(), 1);
    }
}
