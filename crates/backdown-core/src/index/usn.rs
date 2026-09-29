//! NTFS's USN change journal: every change to a file or folder on the volume, as a record with the
//! file's reference number (its MFT record and sequence number, so never reused), the folder it's
//! in, its name and why it was written (created, data changed, renamed from, renamed to,
//! deleted…). Windows keeps it on the volume itself, so it survives restarts and unplugging.
//!
//! A scan notes the journal's id and next USN when it begins. The next rescan checks the journal
//! is the same (a deleted and recreated journal has a new id) and still holds everything since
//! (a full journal drops its oldest records), reads the records since, and turns each into the
//! path it names at that moment: the folder it was in, followed up to the root through the
//! folders on record (the previous scan's, with their reference numbers) and those the records
//! themselves create, move and delete. A rename gives both paths, the old and the new; a folder
//! moved in from outside the root gives its new path (and is walked whole). Records for anything
//! outside the root are left out.
//!
//! Reading needs no administrator rights on Windows 10 and later
//! (`FSCTL_READ_UNPRIVILEGED_USN_JOURNAL`, on a handle to the root folder); elevated, the classic
//! read on the volume works too. The turning of records into paths is ordinary code, tested on
//! every platform; only `sys` calls Windows.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::ChangeSet;

pub const REASON_DATA_OVERWRITE: u32 = 0x0000_0001;
pub const REASON_DATA_EXTEND: u32 = 0x0000_0002;
pub const REASON_DATA_TRUNCATION: u32 = 0x0000_0004;
pub const REASON_FILE_CREATE: u32 = 0x0000_0100;
pub const REASON_FILE_DELETE: u32 = 0x0000_0200;
pub const REASON_SECURITY_CHANGE: u32 = 0x0000_0800;
pub const REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
pub const REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;
pub const REASON_BASIC_INFO_CHANGE: u32 = 0x0000_8000;
pub const REASON_HARD_LINK_CHANGE: u32 = 0x0001_0000;
pub const REASON_REPARSE_POINT_CHANGE: u32 = 0x0010_0000;
pub const REASON_CLOSE: u32 = 0x8000_0000;
pub const ATTRIBUTE_DIRECTORY: u32 = 0x10;

/// One journal record, as far as scans care.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub usn: i64,
    /// The file's reference number: its MFT record, and in the top 16 bits its sequence number.
    pub frn: u64,
    /// The folder it's in (for "renamed from" and "deleted", the one it was in).
    pub parent: u64,
    pub name: OsString,
    pub reason: u32,
    pub attributes: u32,
}

impl Record {
    fn is_dir(&self) -> bool {
        self.attributes & ATTRIBUTE_DIRECTORY != 0
    }
}

/// What `FSCTL_READ_USN_JOURNAL` returns: the USN to read from next, then records (version 2,
/// or 3 with 128-bit ids, whose low 64 bits are NTFS's ids). None if the buffer is damaged.
pub fn parse(buf: &[u8]) -> Option<(i64, Vec<Record>)> {
    let le = |at: usize, n: usize| -> Option<u64> {
        let b = buf.get(at..at + n)?;
        Some(b.iter().rev().fold(0u64, |a, &x| (a << 8) | u64::from(x)))
    };
    let next = le(0, 8)? as i64;
    let mut out = Vec::new();
    let mut at = 8;
    while at + 8 <= buf.len() {
        let len = le(at, 4)? as usize;
        if len == 0 {
            break;
        }
        if len < 60 || at + len > buf.len() || !len.is_multiple_of(8) {
            return None;
        }
        let major = le(at + 4, 2)?;
        let (frn, parent, rest) = match major {
            2 => (le(at + 8, 8)?, le(at + 16, 8)?, at + 24),
            3 if len >= 76 => (le(at + 8, 8)?, le(at + 24, 8)?, at + 40),
            _ => return None, // version 4 (ranges) isn't asked for
        };
        let usn = le(rest, 8)? as i64;
        let reason = le(rest + 16, 4)? as u32;
        let attributes = le(rest + 28, 4)? as u32;
        let (nlen, noff) = (le(rest + 32, 2)? as usize, le(rest + 34, 2)? as usize);
        let name = buf.get(at + noff..at + noff + nlen)?;
        if noff + nlen > len || nlen % 2 != 0 {
            return None;
        }
        out.push(Record {
            usn,
            frn,
            parent,
            name: name_of(name),
            reason,
            attributes,
        });
        at += len;
    }
    Some((next, out))
}

/// A name in UTF-16LE, as the OS spells names.
#[cfg(windows)]
fn name_of(b: &[u8]) -> OsString {
    use std::os::windows::ffi::OsStringExt;
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    OsString::from_wide(&units)
}

#[cfg(not(windows))]
fn name_of(b: &[u8]) -> OsString {
    OsString::from(crate::fsread::utf16le(b))
}

/// Where a folder is, as far as the records and the index say.
#[derive(Debug, Clone)]
enum Node {
    /// in a folder, by name
    In { parent: u64, name: OsString },
    /// found where it is now (by its id, asked of the system)
    At(PathBuf),
    /// outside the root, or gone without a trace (then it was never inside: see `Paths::new`)
    Out,
}

/// Turns records into paths under a root. Folders are followed by reference number: those on
/// record, those the records create and move, and (for one not heard of) wherever the system
/// says it is now.
pub struct Paths<'a> {
    root: PathBuf,
    root_frn: u64,
    folders: HashMap<u64, Node>,
    locate: &'a mut dyn FnMut(u64) -> Option<PathBuf>,
}

impl<'a> Paths<'a> {
    /// `folders`: every folder on record under the root (the root included) with its reference
    /// number, as the previous scan saw them; they must all have one, or a record in an unknown
    /// folder couldn't be told from one outside the root. `locate`: where a folder is now, by
    /// reference number, if it's still there.
    pub fn new(
        root: &Path,
        folders: impl IntoIterator<Item = (PathBuf, u64)>,
        locate: &'a mut dyn FnMut(u64) -> Option<PathBuf>,
    ) -> Result<Paths<'a>, String> {
        let by_path: HashMap<PathBuf, u64> = folders.into_iter().collect();
        let root_frn = *by_path
            .get(root)
            .ok_or("the root folder has no id on record")?;
        let mut map = HashMap::new();
        for (p, frn) in &by_path {
            if p == root {
                continue;
            }
            let (Some(parent), Some(name)) = (p.parent(), p.file_name()) else {
                continue;
            };
            let parent = *by_path
                .get(parent)
                .ok_or_else(|| format!("{} is on record without its folder", p.display()))?;
            map.insert(
                *frn,
                Node::In {
                    parent,
                    name: name.to_os_string(),
                },
            );
        }
        Ok(Paths {
            root: root.to_path_buf(),
            root_frn,
            folders: map,
            locate,
        })
    }

    /// Where folder `frn` is: Some(path) under the root, None outside it.
    fn folder(&mut self, frn: u64) -> Option<PathBuf> {
        let mut names: Vec<OsString> = Vec::new();
        let mut at = frn;
        for _ in 0..4096 {
            if at == self.root_frn {
                let mut p = self.root.clone();
                p.extend(names.iter().rev());
                return Some(p);
            }
            match self.folders.get(&at).cloned() {
                Some(Node::In { parent, name }) => {
                    names.push(name);
                    at = parent;
                }
                Some(Node::At(p)) => {
                    let mut p = p;
                    p.extend(names.iter().rev());
                    return p.starts_with(&self.root).then_some(p);
                }
                Some(Node::Out) => return None,
                None => {
                    // not heard of: where is it now? Gone without a trace means it was never
                    // under the root (every folder there is on record, or was created since)
                    let node = match (self.locate)(at) {
                        Some(p) if p.starts_with(&self.root) => Node::At(p),
                        _ => Node::Out,
                    };
                    self.folders.insert(at, node);
                }
            }
        }
        None // a loop: can't be placed
    }

    /// The path a record names, and what it does to the folders.
    pub fn apply(&mut self, r: &Record, out: &mut ChangeSet) {
        if r.frn == self.root_frn
            && r.reason & (REASON_RENAME_OLD_NAME | REASON_RENAME_NEW_NAME | REASON_FILE_DELETE)
                != 0
        {
            out.everything("the root folder itself was renamed, moved or deleted");
            return;
        }
        let at = self.folder(r.parent).map(|p| p.join(&r.name));
        if let Some(p) = &at {
            out.path(p.clone());
        }
        if !r.is_dir() {
            return;
        }
        if r.reason & REASON_FILE_DELETE != 0 {
            self.folders.insert(r.frn, Node::Out);
        } else if r.reason & (REASON_RENAME_NEW_NAME | REASON_FILE_CREATE) != 0 {
            let node = if at.is_some() {
                Node::In {
                    parent: r.parent,
                    name: r.name.clone(),
                }
            } else {
                Node::Out
            };
            self.folders.insert(r.frn, node);
        }
    }
}

/// The change set records (in USN order) come to, for `root`: see `Paths`. The root is always
/// looked at (cheap), so a root folder that isn't the one on record any more is noticed. `names`:
/// every name a file has now, by reference number (a file changed through one hard link changed
/// under all of them, and the name it was changed through may be gone since).
pub fn changes<'r>(
    root: &Path,
    folders: impl IntoIterator<Item = (PathBuf, u64)>,
    records: impl IntoIterator<Item = &'r Record>,
    locate: &mut dyn FnMut(u64) -> Option<PathBuf>,
    names: &mut dyn FnMut(u64) -> Vec<PathBuf>,
) -> Result<ChangeSet, String> {
    let mut paths = Paths::new(root, folders, locate)?;
    let mut out = ChangeSet::new();
    out.path(root.to_path_buf());
    let mut files = std::collections::BTreeSet::new();
    for r in records {
        if r.name.is_empty() {
            // an unprivileged read leaves out names it may not show: can't be placed
            out.everything("the change log left out a name");
            break;
        }
        paths.apply(r, &mut out);
        if out.full.is_some() {
            break;
        }
        if !r.is_dir() && r.reason & !(REASON_FILE_DELETE | REASON_CLOSE) != 0 {
            files.insert(r.frn);
        }
    }
    if out.full.is_none() {
        for frn in files {
            for p in names(frn) {
                if p.starts_with(root) {
                    out.path(p);
                }
            }
        }
    }
    Ok(out)
}

// ---------- Windows

#[cfg(windows)]
pub(crate) mod sys {
    //! The Windows calls: the volume's serial number and a file's id, the journal's state, its
    //! records, and where a folder is by its id. Kept thin; the logic is above.

    use std::ffi::{OsString, c_void};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};

    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdType,
        FindClose, FindFirstFileNameW, FindNextFileNameW, GetFileInformationByHandle,
        GetFinalPathNameByHandleW, GetVolumeInformationByHandleW, GetVolumePathNameW,
        OPEN_EXISTING, OpenFileById, VOLUME_NAME_DOS,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_UNPRIVILEGED_USN_JOURNAL, FSCTL_READ_USN_JOURNAL,
        READ_USN_JOURNAL_DATA_V1, USN_JOURNAL_DATA_V0,
    };

    use super::super::ChangeSet;
    use super::super::journal::Cursor;
    use crate::{Db, paths};

    const ERROR_JOURNAL_NOT_ACTIVE: u32 = 1179;
    const ERROR_JOURNAL_DELETE_IN_PROGRESS: u32 = 1178;
    const ERROR_JOURNAL_ENTRY_DELETED: u32 = 1181;
    const ERROR_HANDLE_EOF: u32 = 38;

    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    /// A handle closed when dropped.
    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: a handle this module opened and nothing else closes
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    fn open(path: &Path, access: u32) -> Option<Handle> {
        let w = wide(path);
        // SAFETY: a NUL-terminated path; the handle is checked
        let h = unsafe {
            CreateFileW(
                w.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        (h != INVALID_HANDLE_VALUE).then_some(Handle(h))
    }

    fn info(h: &Handle) -> Option<BY_HANDLE_FILE_INFORMATION> {
        // SAFETY: an open handle and a buffer of the right type
        unsafe {
            let mut i: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            (GetFileInformationByHandle(h.0, &mut i) != 0).then_some(i)
        }
    }

    /// A file's or folder's volume serial number and file id (its full reference number).
    pub(crate) fn identity(path: &Path) -> Option<(u32, u64)> {
        let h = open(path, FILE_READ_ATTRIBUTES)?;
        let i = info(&h)?;
        Some((
            i.dwVolumeSerialNumber,
            (u64::from(i.nFileIndexHigh) << 32) | u64::from(i.nFileIndexLow),
        ))
    }

    /// How many names a file has.
    pub(crate) fn link_count(path: &Path) -> Option<u64> {
        let h = open(path, FILE_READ_ATTRIBUTES)?;
        info(&h).map(|i| u64::from(i.nNumberOfLinks))
    }

    /// Every name of a file (its hard links), as full paths.
    pub(crate) fn link_names(path: &Path) -> Vec<PathBuf> {
        let mut vol = [0u16; 1024];
        let w = wide(path);
        // SAFETY: NUL-terminated input, a buffer of the given length
        if unsafe { GetVolumePathNameW(w.as_ptr(), vol.as_mut_ptr(), vol.len() as u32) } == 0 {
            return Vec::new();
        }
        let n = vol.iter().position(|&c| c == 0).unwrap_or(0);
        let vol = PathBuf::from(OsString::from_wide(&vol[..n]));
        let mut out = Vec::new();
        let mut buf = vec![0u16; 32_768];
        let mut len = buf.len() as u32;
        // SAFETY: as above; the find handle is closed below
        let find = unsafe { FindFirstFileNameW(w.as_ptr(), 0, &mut len, buf.as_mut_ptr()) };
        if find == INVALID_HANDLE_VALUE {
            return out;
        }
        loop {
            let name = OsString::from_wide(&buf[..(len as usize).saturating_sub(1)]);
            let rel = PathBuf::from(name);
            out.push(vol.join(rel.strip_prefix("\\").unwrap_or(&rel)));
            len = buf.len() as u32;
            // SAFETY: an open find handle, a buffer of the given length
            if unsafe { FindNextFileNameW(find, &mut len, buf.as_mut_ptr()) } == 0 {
                break;
            }
        }
        // SAFETY: the find handle opened above
        unsafe {
            FindClose(find);
        }
        out
    }

    /// The volume's serial number and file system name.
    fn volume(h: &Handle) -> Option<(u32, String)> {
        let mut serial = 0u32;
        let mut fs = [0u16; 64];
        // SAFETY: an open handle, buffers of the given lengths
        let ok = unsafe {
            GetVolumeInformationByHandleW(
                h.0,
                std::ptr::null_mut(),
                0,
                &mut serial,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs.as_mut_ptr(),
                fs.len() as u32,
            )
        };
        let n = fs.iter().position(|&c| c == 0).unwrap_or(0);
        (ok != 0).then(|| (serial, String::from_utf16_lossy(&fs[..n])))
    }

    fn query(h: &Handle) -> Result<USN_JOURNAL_DATA_V0, u32> {
        // SAFETY: an open handle, an output buffer of the right type and size
        unsafe {
            let mut d: USN_JOURNAL_DATA_V0 = std::mem::zeroed();
            let mut got = 0u32;
            let ok = DeviceIoControl(
                h.0,
                FSCTL_QUERY_USN_JOURNAL,
                std::ptr::null(),
                0,
                &mut d as *mut _ as *mut c_void,
                std::mem::size_of::<USN_JOURNAL_DATA_V0>() as u32,
                &mut got,
                std::ptr::null_mut(),
            );
            if ok == 0 { Err(GetLastError()) } else { Ok(d) }
        }
    }

    pub(crate) fn cursor(root: &Path) -> Result<Option<Cursor>, String> {
        let h = open(root, FILE_READ_ATTRIBUTES).ok_or("can't open the root folder")?;
        let (serial, fs) = volume(&h).ok_or("can't read the volume's details")?;
        if fs != "NTFS" {
            return Ok(None);
        }
        match query(&h) {
            Ok(d) => Ok(Some(Cursor::Usn {
                volume: u64::from(serial),
                journal: d.UsnJournalID,
                usn: d.NextUsn,
            })),
            Err(ERROR_JOURNAL_NOT_ACTIVE) | Err(ERROR_JOURNAL_DELETE_IN_PROGRESS) => Ok(None),
            Err(e) => Err(format!("the change log can't be read (error {e})")),
        }
    }

    /// Every name a file has now, by its id (none if it's gone).
    fn names(hint: &Handle, frn: u64) -> Vec<PathBuf> {
        locate(hint, frn)
            .map(|p| link_names(&p))
            .unwrap_or_default()
    }

    /// Where a file or folder is now, by its id (one of its names).
    fn locate(hint: &Handle, frn: u64) -> Option<PathBuf> {
        let id = FILE_ID_DESCRIPTOR {
            dwSize: std::mem::size_of::<FILE_ID_DESCRIPTOR>() as u32,
            Type: FileIdType,
            Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: frn as i64 },
        };
        // SAFETY: an open volume hint, a descriptor of the right size; the handle is checked
        let h = unsafe {
            OpenFileById(
                hint.0,
                &id,
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                FILE_FLAG_BACKUP_SEMANTICS,
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return None;
        }
        let h = Handle(h);
        let mut buf = vec![0u16; 32_768];
        // SAFETY: an open handle, a buffer of the given length
        let n = unsafe {
            GetFinalPathNameByHandleW(h.0, buf.as_mut_ptr(), buf.len() as u32, VOLUME_NAME_DOS)
        } as usize;
        if n == 0 || n >= buf.len() {
            return None;
        }
        let p = PathBuf::from(OsString::from_wide(&buf[..n]));
        Some(dunce::simplified(&p).to_path_buf())
    }

    /// Reads the records from `from` up to `to`: unprivileged on a handle to the root, else the
    /// classic way on the volume (which needs administrator rights).
    fn read(
        root: &Path,
        h: &Handle,
        journal: u64,
        from: i64,
        to: i64,
    ) -> Result<Vec<super::Record>, String> {
        let volume = PathBuf::from(format!(
            r"\\.\{}",
            root.to_string_lossy().chars().take(2).collect::<String>()
        ));
        let mut out = Vec::new();
        let mut start = from;
        let mut buf = vec![0u8; 1 << 20];
        let mut elevated: Option<Handle> = None;
        while start < to {
            let req = READ_USN_JOURNAL_DATA_V1 {
                StartUsn: start,
                ReasonMask: 0xFFFF_FFFF,
                ReturnOnlyOnClose: 0,
                Timeout: 0,
                BytesToWaitFor: 0,
                UsnJournalID: journal,
                MinMajorVersion: 2,
                MaxMajorVersion: 3,
            };
            let mut got = 0u32;
            let (handle, code) = match &elevated {
                Some(v) => (v.0, FSCTL_READ_USN_JOURNAL),
                None => (h.0, FSCTL_READ_UNPRIVILEGED_USN_JOURNAL),
            };
            // SAFETY: an open handle, input and output buffers of the given sizes
            let ok = unsafe {
                DeviceIoControl(
                    handle,
                    code,
                    &req as *const _ as *const c_void,
                    std::mem::size_of::<READ_USN_JOURNAL_DATA_V1>() as u32,
                    buf.as_mut_ptr() as *mut c_void,
                    buf.len() as u32,
                    &mut got,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                // SAFETY: reads the calling thread's last error
                let e = unsafe { GetLastError() };
                if e == ERROR_HANDLE_EOF {
                    break;
                }
                if e == ERROR_JOURNAL_ENTRY_DELETED {
                    return Err("the change log was trimmed since the previous scan".into());
                }
                if elevated.is_none() {
                    // older Windows, or not allowed: the classic read, if this runs elevated
                    elevated = Some(open(&volume, GENERIC_READ).ok_or(format!(
                        "the change log can't be read without administrator rights (error {e})"
                    ))?);
                    continue;
                }
                return Err(format!("the change log can't be read (error {e})"));
            }
            let (next, records) = super::parse(&buf[..got as usize])
                .ok_or("the change log returned damaged records")?;
            let done = records.is_empty() || next <= start;
            out.extend(records.into_iter().filter(|r| r.usn < to));
            if done {
                break;
            }
            start = next;
        }
        Ok(out)
    }

    pub(crate) fn changes(
        db: &Db,
        root: &Path,
        root_id: i64,
        from: &Cursor,
        to: &Cursor,
    ) -> Result<ChangeSet, String> {
        let (
            Cursor::Usn {
                volume,
                journal,
                usn,
            },
            Cursor::Usn {
                volume: vol_now,
                journal: journal_now,
                usn: usn_now,
            },
        ) = (from, to)
        else {
            return Err("the previous scan noted another kind of change log".into());
        };
        if volume != vol_now {
            return Err("a different volume than at the previous scan".into());
        }
        if journal != journal_now {
            return Err("the change log was reset since the previous scan".into());
        }
        let h = open(root, FILE_READ_ATTRIBUTES).ok_or("can't open the root folder")?;
        let d = query(&h).map_err(|e| format!("the change log can't be read (error {e})"))?;
        if d.UsnJournalID != *journal {
            return Err("the change log was reset since the previous scan".into());
        }
        if *usn < d.LowestValidUsn || *usn < d.FirstUsn {
            return Err("the change log was trimmed since the previous scan".into());
        }
        let records = read(root, &h, *journal, *usn, *usn_now)?;
        let folders = folders(db, root_id).map_err(|e| format!("{e:#}"))?;
        let mut loc = |frn| locate(&h, frn);
        let mut all = |frn| names(&h, frn);
        super::changes(root, folders, &records, &mut loc, &mut all)
    }

    /// What changed since a little before `from` (the table's read began there), for a table
    /// read on a live volume: Windows writes the table lazily, so changes made just before may
    /// not be in it yet. `folders`: the table's folders under the root, with their ids.
    pub(crate) fn catch_up(
        root: &Path,
        journal: u64,
        from: i64,
        folders: Vec<(PathBuf, u64)>,
    ) -> Result<ChangeSet, String> {
        let h = open(root, FILE_READ_ATTRIBUTES).ok_or("can't open the root folder")?;
        let d = query(&h).map_err(|e| format!("the change log can't be read (error {e})"))?;
        if d.UsnJournalID != journal {
            return Err("the change log was reset while the file table was read".into());
        }
        // records never straddle a 4 KiB page, so a page's start is a record's start
        let start = ((from - (1 << 20)) & !0xFFF)
            .max(d.FirstUsn)
            .max(d.LowestValidUsn);
        let records = read(root, &h, journal, start, d.NextUsn)?;
        let mut loc = |frn| locate(&h, frn);
        let mut all = |frn| names(&h, frn);
        super::changes(root, folders, &records, &mut loc, &mut all)
    }

    /// Every folder on record under the root, with its id (all must have one).
    fn folders(db: &Db, root_id: i64) -> anyhow::Result<Vec<(PathBuf, u64)>> {
        let mut stmt = db
            .conn
            .prepare("SELECT path, ino FROM dirs WHERE root_id = ?1 AND current = 1")?;
        let rows = stmt.query_map([root_id], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Option<i64>>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (p, ino) = row?;
            let ino = ino.ok_or_else(|| {
                anyhow::anyhow!("folders on record have no ids (scanned by an older BackDown)")
            })?;
            out.push((paths::from_bytes(&p), ino as u64));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(
        buf: &mut Vec<u8>,
        frn: u64,
        parent: u64,
        usn: i64,
        reason: u32,
        attrs: u32,
        name: &str,
    ) {
        let name: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let len = (60 + name.len()).div_ceil(8) * 8;
        let at = buf.len();
        buf.resize(at + len, 0);
        let b = &mut buf[at..];
        b[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        b[4..6].copy_from_slice(&2u16.to_le_bytes());
        b[8..16].copy_from_slice(&frn.to_le_bytes());
        b[16..24].copy_from_slice(&parent.to_le_bytes());
        b[24..32].copy_from_slice(&usn.to_le_bytes());
        b[40..44].copy_from_slice(&reason.to_le_bytes());
        b[52..56].copy_from_slice(&attrs.to_le_bytes());
        b[56..58].copy_from_slice(&(name.len() as u16).to_le_bytes());
        b[58..60].copy_from_slice(&60u16.to_le_bytes());
        b[60..60 + name.len()].copy_from_slice(&name);
    }

    #[test]
    fn parses_version_2_records() {
        let mut buf = 5000i64.to_le_bytes().to_vec();
        rec(&mut buf, 77, 5, 4096, REASON_FILE_CREATE, 0x20, "notes.txt");
        rec(
            &mut buf,
            78,
            5,
            4200,
            REASON_RENAME_NEW_NAME,
            ATTRIBUTE_DIRECTORY,
            "Ünï",
        );
        let (next, rs) = parse(&buf).unwrap();
        assert_eq!(next, 5000);
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[0].name, OsString::from("notes.txt"));
        assert_eq!((rs[0].frn, rs[0].parent, rs[0].usn), (77, 5, 4096));
        assert!(rs[1].is_dir());
        assert_eq!(rs[1].name, OsString::from("Ünï"));
        // damage: a length running past the end
        let mut bad = buf.clone();
        bad[8] = 0xF8;
        assert!(parse(&bad).is_none());
        assert_eq!(parse(&5i64.to_le_bytes()).unwrap().1, vec![]);
    }

    #[test]
    fn records_become_paths() {
        let root = PathBuf::from("/r");
        // /r (1) with a/ (2) holding b/ (3)
        let folders = vec![
            (root.clone(), 1),
            (root.join("a"), 2),
            (root.join("a/b"), 3),
        ];
        let r = |frn, parent, reason, dir: bool, name: &str| Record {
            usn: 0,
            frn,
            parent,
            name: name.into(),
            reason,
            attributes: if dir { ATTRIBUTE_DIRECTORY } else { 0 },
        };
        let recs = vec![
            r(10, 3, REASON_DATA_EXTEND, false, "f.txt"),
            // a moves to x: both names; then something in b, now under x
            r(2, 1, REASON_RENAME_OLD_NAME, true, "a"),
            r(2, 1, REASON_RENAME_NEW_NAME, true, "x"),
            r(11, 3, REASON_FILE_CREATE, false, "g.txt"),
            // a folder made outside the root (under 99, which the system says is elsewhere)
            r(20, 99, REASON_FILE_CREATE, true, "tmp"),
            r(21, 20, REASON_FILE_CREATE, false, "t.bin"),
            // ... then moved in
            r(20, 99, REASON_RENAME_OLD_NAME, true, "tmp"),
            r(20, 1, REASON_RENAME_NEW_NAME, true, "in"),
            r(22, 20, REASON_FILE_CREATE, false, "u.bin"),
        ];
        let mut locate = |frn: u64| (frn == 99).then(|| PathBuf::from("/elsewhere"));
        // file 10 has another name, elsewhere in the root, which changed with it
        let mut names = |frn: u64| {
            if frn == 10 {
                vec![PathBuf::from("/r/x/b/f.txt"), PathBuf::from("/r/link.txt")]
            } else {
                Vec::new()
            }
        };
        let c = changes(&root, folders, &recs, &mut locate, &mut names).unwrap();
        let got: Vec<String> = c.paths.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(
            got,
            [
                "/r",
                "/r/a",
                "/r/a/b/f.txt",
                "/r/in",
                "/r/in/u.bin",
                "/r/link.txt",
                "/r/x",
                "/r/x/b/f.txt",
                "/r/x/b/g.txt"
            ]
        );
        assert!(c.full.is_none());
        // the root itself renamed: everything
        let mut locate = |_| None;
        let mut names = |_| Vec::new();
        let c = changes(
            &root,
            vec![(root.clone(), 1)],
            &[r(1, 0, REASON_RENAME_OLD_NAME, true, "r")],
            &mut locate,
            &mut names,
        )
        .unwrap();
        assert!(c.full.is_some());
        // a folder on record without its own folder can't be placed
        let mut locate = |_| None;
        assert!(
            changes(
                &root,
                vec![(root.clone(), 1), (root.join("a/b"), 3)],
                &[],
                &mut locate,
                &mut names
            )
            .is_err()
        );
    }
}
