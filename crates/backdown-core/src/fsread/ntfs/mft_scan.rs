//! Reading an NTFS volume's master file table from start to end, record after record, the way
//! Everything and WizTree do: every file and folder in use with its names (one per hard link, each
//! in its folder), size and times, without walking a single folder. Far fewer, larger reads than
//! listing folders one by one, which matters most where listing is slow (ntfs-3g) or a disk seeks.
//!
//! What a walk would record is what this gives, record for record, the way the system the table
//! is compared against shows files (`Flavour`): which entries are folders, files, links (not
//! followed or recorded) or special files. Sizes are the unnamed data stream's size, times the
//! last-write time in $STANDARD_INFORMATION, as both Windows (for an open file) and ntfs-3g
//! report them. The filesystem's own metadata files ($MFT, $Extend…) are left out, as both do.
//!
//! Records split over extension records (through an attribute list: many names, very fragmented
//! files) are put back together. A record in use that fails its checks (torn by a write in
//! progress, damaged) fails the whole read: a table that might be missing something isn't used.

use std::collections::BTreeSet;
use std::io::{Read, Seek};
use std::time::Instant;

use anyhow::{Context, Result, bail};

use super::{
    AT_DATA, AT_REPARSE_POINT, AT_STANDARD_INFORMATION, AT_VOLUME_INFORMATION, AT_VOLUME_NAME,
    ATTR_COMPRESSED, ATTR_ENCRYPTED, FILE_HIDDEN, FILE_REPARSE, FILE_SYSTEM, FIRST_USER, ROOT,
    fixup,
};
use crate::fsread::{Fs, Volume, filetime, read_mapped, utf16le};
use crate::magic::le;

const AT_FILE_NAME: u32 = 0x30;
/// The table is read in pieces of this size.
const CHUNK: u64 = 4 << 20;
/// Name-surrogate reparse tags stand for another name: symbolic links, junctions, WSL links.
const NAME_SURROGATE: u32 = 0x2000_0000;
/// Interix special files (how ntfs-3g stores symlinks, devices, pipes and sockets by default).
const INTX_SYMLINK: u64 = u64::from_le_bytes(*b"IntxLNK\x01");
const INTX_CHAR: u64 = u64::from_le_bytes(*b"IntxCHR\x00");
const INTX_BLOCK: u64 = u64::from_le_bytes(*b"IntxBLK\x00");

/// Whose view of the files the table must match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    /// Windows: reparse points that stand for another name (symbolic links, junctions, mount
    /// points) are links; everything else is a file or folder.
    Windows,
    /// ntfs-3g on Linux: every reparse point is a link (unless a plugin handles its kind; then
    /// the table is refused, see `Table::tags`), and a system file can be an Interix symlink,
    /// device, pipe or socket (ntfs-3g's own way of storing them).
    Ntfs3g,
}

/// What an entry is to the system the table is compared against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    /// shown as a symbolic link: not followed, not recorded
    Link,
    /// a pipe, socket or device: not recorded
    Special,
}

/// One file or folder in use.
#[derive(Debug, Clone)]
pub struct Record {
    pub record: u64,
    pub seq: u16,
    pub kind: Kind,
    /// bytes of the unnamed data stream (0 for a folder)
    pub size: u64,
    /// last modified, unix seconds (0 before 1970 or unset)
    pub mtime: i64,
    /// the DOS/Windows attributes ($STANDARD_INFORMATION)
    pub attributes: u32,
    /// its reparse tag, if it's a reparse point
    pub reparse: Option<u32>,
}

impl Record {
    /// The file reference number Windows gives it (record and sequence number).
    pub fn frn(&self) -> u64 {
        self.record | (u64::from(self.seq) << 48)
    }
}

/// One name of a record in a folder.
struct Name {
    parent: u64,
    child: u32,
    start: u32,
    len: u16,
}

/// Everything in use in the table.
pub struct Table {
    /// records the table has room for, and in use
    pub records: u64,
    pub in_use: u64,
    /// the volume's serial number (its boot sector's)
    pub serial: u64,
    /// marked dirty: not shut down cleanly, or needing a check
    pub dirty: bool,
    pub label: Option<String>,
    /// the reparse tags met (ntfs-3g shows kinds it has a plugin for as files)
    pub tags: BTreeSet<u32>,
    /// names left out: in a folder that's gone or was reused, or not a folder
    pub stale_names: u64,
    files: Vec<Record>,
    /// record number → index in `files` (u32::MAX: not in use)
    at: Vec<u32>,
    /// sorted by folder, then name
    names: Vec<Name>,
    chars: Vec<u16>,
}

/// What a name's parent says: the record and sequence number it points at.
struct Pending {
    base: u64,
    base_seq: u16,
    parent: u64,
    start: u32,
    len: u16,
}

/// What one record (base or extension) holds that matters here.
#[derive(Default)]
struct Found {
    si: Option<(u64, u32)>,
    /// the unnamed data stream: its size, and where its first bytes are (resident: here;
    /// non-resident: at this byte offset of the volume; None: can't be read cheaply)
    data: Option<(u64, Head)>,
    reparse: Option<u32>,
    reparse_at: Option<u64>,
}

#[derive(Clone, Copy, Default)]
enum Head {
    #[default]
    Unknown,
    Bytes(u64),
    At(u64),
}

/// A record in use, before it's classified.
struct Raw {
    record: u64,
    seq: u16,
    dir: bool,
    found: Found,
}

/// Reads every record of the volume's table. `progress` gets (records read, records).
pub fn read<R: Read + Seek>(
    vol: &mut Volume<R>,
    flavour: Flavour,
    until: Option<Instant>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Table> {
    let Fs::Ntfs(ntfs) = &vol.fs else {
        bail!("not an NTFS volume");
    };
    let r = &mut vol.region;
    if until.is_some() {
        r.until = until;
    }
    let boot = r.read_vec(0, 512)?;
    let serial = le(&boot, 0x48, 8);
    let (rs, total, cluster) = (ntfs.rec, ntfs.records, ntfs.cluster);
    let per = (CHUNK / rs).max(1);
    let mut buf = vec![0u8; (per * rs) as usize];
    let mut raws: Vec<Raw> = Vec::new();
    let mut ext: Vec<(u64, u16, Found)> = Vec::new();
    let mut pending: Vec<Pending> = Vec::new();
    let mut chars: Vec<u16> = Vec::new();
    let (mut dirty, mut label) = (false, None);
    let mut n = 0u64;
    while n < total {
        let count = per.min(total - n);
        let bytes = &mut buf[..(count * rs) as usize];
        read_mapped(r, &ntfs.mft, n * rs, bytes)
            .with_context(|| format!("can't read the file table at record {n}"))?;
        for (i, rec) in bytes.chunks_exact_mut(rs as usize).enumerate() {
            let no = n + i as u64;
            if &rec[..4] != b"FILE" || le(rec, 22, 2) & 1 == 0 {
                continue; // never used, or not in use
            }
            fixup(rec, b"FILE", &|| format!("MFT record {no}"))?;
            let seq = le(rec, 16, 2) as u16;
            let base_ref = le(rec, 32, 8);
            let base = base_ref & 0xFFFF_FFFF_FFFF;
            let owner = if base == 0 { no } else { base };
            let owner_seq = if base == 0 {
                seq
            } else {
                (base_ref >> 48) as u16
            };
            let mut found = Found::default();
            each_attr(rec, no, |kind, x| {
                attr(
                    kind,
                    x,
                    no,
                    cluster,
                    owner,
                    owner_seq,
                    &mut found,
                    &mut pending,
                    &mut chars,
                    &mut dirty,
                    &mut label,
                )
            })?;
            if base == 0 {
                raws.push(Raw {
                    record: no,
                    seq,
                    dir: le(rec, 22, 2) & 2 != 0,
                    found,
                });
            } else {
                ext.push((base, owner_seq, found));
            }
        }
        n += count;
        progress(n, total);
    }

    // extension records: their parts belong to their base record (if it's the same file)
    let mut at = vec![u32::MAX; total as usize];
    for (i, raw) in raws.iter().enumerate() {
        at[raw.record as usize] = i as u32;
    }
    for (base, base_seq, f) in ext {
        let Some(&i) = at.get(base as usize).filter(|&&i| i != u32::MAX) else {
            continue;
        };
        let raw = &mut raws[i as usize];
        if raw.seq != base_seq {
            continue; // left over from a file that's gone
        }
        if raw.found.data.is_none() {
            raw.found.data = f.data;
        }
        if raw.found.reparse.is_none() {
            raw.found.reparse = f.reparse;
            raw.found.reparse_at = raw.found.reparse_at.or(f.reparse_at);
        }
    }

    // reparse tags kept outside the record, and the heads of possible Interix files
    let mut tags = BTreeSet::new();
    let mut files = Vec::with_capacity(raws.len());
    for raw in raws {
        let f = &raw.found;
        let (mtime, attributes) = f.si.unwrap_or((0, 0));
        let reparse = if attributes & FILE_REPARSE as u32 != 0 {
            Some(match (f.reparse, f.reparse_at) {
                (Some(t), _) => t,
                (None, Some(off)) => le(&r.read_vec(off, 4)?, 0, 4) as u32,
                (None, None) => 0,
            })
        } else {
            None
        };
        if let Some(t) = reparse {
            tags.insert(t);
        }
        let (size, head) = f.data.unwrap_or((0, Head::Bytes(0)));
        let kind = if let Some(tag) = reparse {
            match flavour {
                Flavour::Windows if tag & NAME_SURROGATE != 0 => Kind::Link,
                Flavour::Windows if raw.dir => Kind::Dir,
                Flavour::Windows => Kind::File,
                Flavour::Ntfs3g => Kind::Link,
            }
        } else if raw.dir {
            Kind::Dir
        } else if flavour == Flavour::Ntfs3g && attributes & FILE_SYSTEM as u32 != 0 {
            interix(r, attributes, size, head)?
        } else {
            Kind::File
        };
        files.push(Record {
            record: raw.record,
            seq: raw.seq,
            kind,
            size: if raw.dir { 0 } else { size },
            mtime: filetime(mtime).unwrap_or(0).max(0),
            attributes,
            reparse,
        });
    }

    // names, in the folders they're in: only folders in use, of the same sequence number
    let mut names = Vec::with_capacity(pending.len());
    let mut stale = 0u64;
    for p in pending {
        let Some(&child) = at.get(p.base as usize).filter(|&&i| i != u32::MAX) else {
            continue;
        };
        if files[child as usize].seq != p.base_seq {
            continue;
        }
        let parent = p.parent & 0xFFFF_FFFF_FFFF;
        let parent_seq = (p.parent >> 48) as u16;
        let ok = at
            .get(parent as usize)
            .filter(|&&i| i != u32::MAX)
            .map(|&i| &files[i as usize])
            .is_some_and(|d| matches!(d.kind, Kind::Dir | Kind::Link) && d.seq == parent_seq);
        if !ok || parent == p.base {
            stale += 1;
            continue;
        }
        names.push(Name {
            parent,
            child,
            start: p.start,
            len: p.len,
        });
    }
    names.sort_by(|a, b| {
        a.parent.cmp(&b.parent).then_with(|| {
            chars[a.start as usize..][..a.len as usize]
                .cmp(&chars[b.start as usize..][..b.len as usize])
        })
    });
    Ok(Table {
        records: total,
        in_use: files.len() as u64,
        serial,
        dirty,
        label,
        tags,
        stale_names: stale,
        files,
        at,
        names,
        chars,
    })
}

/// Calls `each` with every attribute of a (fixed-up) record: its type and bytes.
fn each_attr(rec: &[u8], no: u64, mut each: impl FnMut(u32, &[u8]) -> Result<()>) -> Result<()> {
    let used = (le(rec, 24, 4) as usize).min(rec.len());
    let mut a = le(rec, 20, 2) as usize;
    while a + 16 <= used {
        let kind = le(rec, a, 4) as u32;
        if kind == 0xFFFF_FFFF {
            return Ok(());
        }
        let len = le(rec, a + 4, 4) as usize;
        if len < 16 || a + len > used {
            bail!("MFT record {no} has a damaged attribute");
        }
        each(kind, &rec[a..a + len])?;
        a += len;
    }
    Ok(())
}

/// A resident attribute's value.
fn value(x: &[u8], no: u64) -> Result<&[u8]> {
    let (vlen, voff) = (le(x, 16, 4) as usize, le(x, 20, 2) as usize);
    x.get(voff..voff + vlen)
        .with_context(|| format!("MFT record {no} has a damaged attribute"))
}

/// Where a non-resident attribute's first byte is on the volume (None: a hole, or none).
fn first_lcn(x: &[u8], cluster: u64) -> Option<u64> {
    let b = x.get(le(x, 32, 2) as usize..)?;
    let h = *b.first()?;
    let (ln, on) = (usize::from(h & 0xF), usize::from(h >> 4));
    if h == 0 || ln == 0 || ln > 8 || on == 0 || on > 8 || 1 + ln + on > b.len() {
        return None;
    }
    let shift = 64 - 8 * on as u32;
    let lcn = ((le(b, 1 + ln, on) << shift) as i64) >> shift;
    (lcn >= 0).then(|| lcn as u64 * cluster)
}

#[allow(clippy::too_many_arguments)]
fn attr(
    kind: u32,
    x: &[u8],
    no: u64,
    cluster: u64,
    owner: u64,
    owner_seq: u16,
    f: &mut Found,
    pending: &mut Vec<Pending>,
    chars: &mut Vec<u16>,
    dirty: &mut bool,
    label: &mut Option<String>,
) -> Result<()> {
    let resident = x[8] == 0;
    let named = x[9] != 0;
    match kind {
        AT_STANDARD_INFORMATION if resident => {
            let v = value(x, no)?;
            f.si = Some((le(v, 8, 8), le(v, 32, 4) as u32));
        }
        AT_FILE_NAME if resident => {
            let v = value(x, no)?;
            let (len, space) = (
                usize::from(*v.get(64).unwrap_or(&0)),
                *v.get(65).unwrap_or(&2),
            );
            if space != 2
                && let Some(name) = v.get(66..66 + 2 * len)
            {
                let start = chars.len() as u32;
                chars.extend(
                    name.as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_le_bytes(*c)),
                );
                pending.push(Pending {
                    base: owner,
                    base_seq: owner_seq,
                    parent: le(v, 0, 8),
                    start,
                    len: len as u16,
                });
            }
        }
        AT_DATA if !named => {
            if resident {
                let v = value(x, no)?;
                f.data = Some((v.len() as u64, Head::Bytes(le(v, 0, 8.min(v.len())))));
            } else if x.len() >= 64 && le(x, 16, 8) == 0 {
                let flags = le(x, 12, 2) as u16;
                let head = if flags & (ATTR_COMPRESSED | ATTR_ENCRYPTED) != 0 {
                    Head::Unknown
                } else {
                    first_lcn(x, cluster).map_or(Head::Unknown, Head::At)
                };
                f.data = Some((le(x, 48, 8), head));
            }
        }
        AT_REPARSE_POINT => {
            if resident {
                f.reparse = Some(le(value(x, no)?, 0, 4) as u32);
            } else if x.len() >= 64 && le(x, 16, 8) == 0 {
                f.reparse_at = first_lcn(x, cluster);
            }
        }
        AT_VOLUME_INFORMATION if resident && no == 3 => {
            *dirty = le(value(x, no)?, 10, 2) & 1 != 0;
        }
        AT_VOLUME_NAME if resident && no == 3 => {
            *label = Some(utf16le(value(x, no)?)).filter(|l| !l.is_empty());
        }
        _ => {}
    }
    Ok(())
}

/// What ntfs-3g shows a system file as: an Interix pipe or socket (0 or 1 byte, not hidden), an
/// Interix symbolic link or device (by the magic its data starts with), or a plain file.
fn interix<R: Read + Seek>(
    r: &mut crate::fsread::Region<R>,
    attributes: u32,
    size: u64,
    head: Head,
) -> Result<Kind> {
    if attributes & FILE_HIDDEN as u32 == 0 && size <= 1 {
        return Ok(Kind::Special);
    }
    if size <= 8 || size > 8 + 2 * 4096 {
        return Ok(Kind::File);
    }
    let magic = match head {
        Head::Bytes(b) => b,
        Head::At(off) => le(&r.read_vec(off, 8)?, 0, 8),
        // compressed or encrypted: can't be Interix's
        Head::Unknown => return Ok(Kind::File),
    };
    Ok(match magic {
        INTX_SYMLINK => Kind::Link,
        INTX_CHAR | INTX_BLOCK if size == 24 => Kind::Special,
        _ => Kind::File,
    })
}

impl Table {
    /// The record with this number, if it's in use.
    pub fn get(&self, record: u64) -> Option<&Record> {
        self.at
            .get(record as usize)
            .filter(|&&i| i != u32::MAX)
            .map(|&i| &self.files[i as usize])
    }

    /// Every in-use file and folder.
    pub fn files(&self) -> &[Record] {
        &self.files
    }

    /// The entries of folder `record`, in name order, as a listing shows them (the root folder's
    /// metadata files left out).
    pub fn children(&self, record: u64) -> impl Iterator<Item = (&[u16], &Record)> {
        let from = self.names.partition_point(|n| n.parent < record);
        self.names[from..]
            .iter()
            .take_while(move |n| n.parent == record)
            .map(|n| {
                (
                    &self.chars[n.start as usize..][..n.len as usize],
                    &self.files[n.child as usize],
                )
            })
            .filter(move |(_, f)| record != ROOT || f.record >= FIRST_USER)
    }

    /// The record reached by following `path` (names, as UTF-16) down from the root folder.
    pub fn find(&self, path: &[Vec<u16>]) -> Option<&Record> {
        let mut cur = self.get(ROOT)?;
        for name in path {
            if cur.kind != Kind::Dir {
                return None;
            }
            cur = self
                .children(cur.record)
                .find(|(n, _)| n == &name.as_slice())
                .map(|(_, f)| f)?;
        }
        Some(cur)
    }

    /// A walk down from folder `top`, a folder before what's in it: the depth (0 for `top`, whose
    /// name is empty), the entry's name and its record. Links and special files come too (they
    /// aren't recorded, but they are entries). A folder reached twice (a damaged table) comes
    /// again but isn't entered again.
    pub fn walk(&self, top: u64) -> Walk<'_> {
        Walk {
            table: self,
            stack: self.get(top).map(|t| (0, &[][..], t)).into_iter().collect(),
            seen: vec![false; self.files.len()],
            enter: None,
        }
    }
}

/// See `Table::walk`.
pub struct Walk<'a> {
    table: &'a Table,
    stack: Vec<(usize, &'a [u16], &'a Record)>,
    seen: Vec<bool>,
    /// the folder just passed, entered on the next step (unless skipped)
    enter: Option<(usize, &'a Record)>,
}

impl Walk<'_> {
    /// Doesn't go into the folder just passed.
    pub fn dont_enter(&mut self) {
        self.enter = None;
    }
}

impl<'a> Iterator for Walk<'a> {
    type Item = (usize, &'a [u16], &'a Record);

    fn next(&mut self) -> Option<Self::Item> {
        if let Some((depth, dir)) = self.enter.take() {
            let i = self.table.at[dir.record as usize] as usize;
            if !std::mem::replace(&mut self.seen[i], true) {
                let kids: Vec<(&[u16], &Record)> = self.table.children(dir.record).collect();
                for (n, f) in kids.into_iter().rev() {
                    self.stack.push((depth + 1, n, f));
                }
            }
        }
        let (depth, name, rec) = self.stack.pop()?;
        if rec.kind == Kind::Dir {
            self.enter = Some((depth, rec));
        }
        Some((depth, name, rec))
    }
}
