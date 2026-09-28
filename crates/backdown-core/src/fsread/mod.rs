//! Read-only filesystem readers: the files inside a disk image, a partition in one, or a partition
//! Rescue found on a device, listed and read without mounting or restoring anything. FAT12/16/32
//! (with long names), exFAT, ext2/3/4 (extents or block maps, 64-bit, inline data) and NTFS
//! (resident, non-resident, sparse and LZNT1-compressed data; directory indexes).
//!
//! A `Volume` reads through a `Region` (a window onto any `Read + Seek` source: an image file, a
//! device) and never writes. Every number read from the disk is checked before it's used, so a
//! damaged or hostile image gives a clear error, never a panic, a huge allocation or an endless
//! loop: listings stop at a limit (at most `MAX_LIST`), a directory's own data is read up to
//! `DIR_CAP` bytes, chains, trees and paths have step and depth limits, and a file is read in at
//! most `MAX_RUNS` pieces. A caller can also set a deadline for everything but a file's own bytes.
//!
//! Entries only come from listings: `find` walks a path name by name from the root, so a name can
//! only ever pick something the filesystem's own directories list (never a path on this computer),
//! and symbolic links are shown, never followed.
//!
//! ```no_run
//! # fn main() -> anyhow::Result<()> {
//! use std::io::Read;
//! use backdown_core::fsread::Volume;
//! let img = std::fs::File::open("card.img")?;
//! let mut vol = Volume::open(img, 1 << 20, 255 << 20)?; // the partition 1 MiB in
//! for e in vol.list_path("DCIM", 100)?.entries {
//!     println!("{} {}", e.name, e.size);
//! }
//! let photo = vol.find("DCIM/IMG_0001.JPG")?;
//! let mut bytes = Vec::new();
//! vol.reader(&photo)?.read_to_end(&mut bytes)?;
//! # Ok(()) }
//! ```

use std::io::{self, Read, Seek, SeekFrom};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde::Serialize;

mod exfat;
mod ext;
mod fat;
mod ntfs;

/// Listings stop here; `Listing::truncated` says there was more.
pub const MAX_LIST: usize = 10_000;
/// The most of one directory's own data that's read (its entries, index or blocks).
const DIR_CAP: u64 = 64 << 20;
/// Paths deeper than this are refused.
const MAX_DEPTH: usize = 256;
/// A file in more pieces than this is refused rather than mapped (24 MiB of map).
const MAX_RUNS: usize = 1 << 20;
/// The most metadata read into memory in one piece (a record, a block, a list of attributes).
const META_CAP: u64 = 16 << 20;
/// Directory data is read in pieces of this size.
const CHUNK: u64 = 64 << 10;

/// A window onto a disk image or device: `len` bytes from `base`. Reads outside it fail, and so
/// do reads after its deadline, if it has one.
pub struct Region<R> {
    src: R,
    base: u64,
    len: u64,
    until: Option<Instant>,
}

impl<R: Read + Seek> Region<R> {
    pub fn new(src: R, base: u64, len: u64) -> Region<R> {
        Region {
            src,
            base,
            len,
            until: None,
        }
    }

    fn in_time(&self) -> Result<()> {
        if self.until.is_some_and(|t| Instant::now() > t) {
            bail!(
                "gave up: reading this filesystem's structures took longer than allowed (it may be damaged, or huge)"
            );
        }
        Ok(())
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Exactly `buf.len()` bytes at `off` (from the region's start).
    pub fn read_exact_at(&mut self, off: u64, buf: &mut [u8]) -> Result<()> {
        self.in_time()?;
        read_region(&mut self.src, self.base, self.len, off, buf)?;
        Ok(())
    }

    /// `n` bytes at `off`, for metadata (at most `META_CAP`).
    pub fn read_vec(&mut self, off: u64, n: u64) -> Result<Vec<u8>> {
        if n > META_CAP {
            bail!("a {n}-byte structure is implausibly large (damaged)");
        }
        let mut v = vec![0u8; n as usize];
        self.read_exact_at(off, &mut v)?;
        Ok(v)
    }

    pub fn into_inner(self) -> R {
        self.src
    }
}

fn bad(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Exactly `buf.len()` bytes at `off` within the `len` bytes from `base`.
fn read_region<S: Read + Seek>(
    src: &mut S,
    base: u64,
    len: u64,
    off: u64,
    buf: &mut [u8],
) -> io::Result<()> {
    if off
        .checked_add(buf.len() as u64)
        .is_none_or(|end| end > len)
    {
        return Err(bad(format!(
            "a read at byte {off} runs past the end of the volume ({len} bytes): the filesystem is damaged or the image is cut short"
        )));
    }
    let at = base
        .checked_add(off)
        .ok_or_else(|| bad("offset out of range".into()))?;
    src.seek(SeekFrom::Start(at))?;
    src.read_exact(buf).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the image file ends before the volume does (a partial or cut-short image)",
            )
        } else {
            e
        }
    })
}

/// What a filesystem says about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Info {
    /// "FAT32", "exFAT", "ext4", "NTFS".
    pub kind: String,
    pub label: Option<String>,
    /// The bytes it spans, by its own account.
    pub size: u64,
    /// Its allocation unit: a cluster or block, in bytes.
    pub block: u64,
    /// Worth knowing before trusting a listing ("wasn't unmounted cleanly…").
    pub notes: Vec<String>,
}

/// One file or folder in a listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    /// Bytes of content (0 for folders).
    pub size: u64,
    /// Last modified, in seconds since 1970 (FAT and exFAT clocks have no time zone; read as UTC).
    pub mtime: Option<i64>,
    /// A symbolic link's (or an NTFS junction's) target: shown, never followed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Why the contents can't be read here, when they can't ("encrypted (EFS)").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unreadable: Option<String>,
    /// Worth showing: "compressed", "sparse", "hidden", "system".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<&'static str>,
    #[serde(skip)]
    pub(crate) node: Node,
}

/// Where an entry lives, in its filesystem's own terms. Only listings make these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node {
    /// Cluster 0 as a folder: the root.
    Fat {
        cluster: u32,
        size: u32,
    },
    Exfat {
        cluster: u32,
        len: u64,
        valid: u64,
        contiguous: bool,
    },
    Ext(u32),
    Ntfs {
        record: u64,
        seq: u16,
    },
}

/// A folder's entries, in the order the filesystem keeps them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Listing {
    pub entries: Vec<Entry>,
    /// There were more than the limit.
    pub truncated: bool,
}

/// Where a file's bytes are: `len` bytes of the file from byte `at` are at `phys` in the volume,
/// or read as zeros (a hole) when `phys` is None.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Run {
    pub at: u64,
    pub len: u64,
    pub phys: Option<u64>,
}

/// Appends a run, merging it into the one before when it carries straight on.
pub(crate) fn push_run(runs: &mut Vec<Run>, r: Run) -> Result<()> {
    if r.len == 0 {
        return Ok(());
    }
    if let Some(last) = runs.last_mut()
        && last.at.checked_add(last.len) == Some(r.at)
    {
        let joins = match (last.phys, r.phys) {
            (Some(a), Some(b)) => a.checked_add(last.len) == Some(b),
            (None, None) => true,
            _ => false,
        };
        if joins {
            last.len = last.len.saturating_add(r.len);
            return Ok(());
        }
    }
    if let Some(last) = runs.last()
        && r.at < last.at.saturating_add(last.len)
    {
        bail!("the file's pieces overlap (damaged)");
    }
    if runs.len() >= MAX_RUNS {
        bail!("the file is in more than {MAX_RUNS} pieces: too fragmented to read here");
    }
    runs.push(r);
    Ok(())
}

/// Whether any two runs share bytes of the volume: a cluster chain that loops back on itself.
pub(crate) fn overlapping(runs: &[Run]) -> bool {
    let mut spans: Vec<(u64, u64)> = runs
        .iter()
        .filter_map(|r| r.phys.map(|p| (p, p.saturating_add(r.len))))
        .collect();
    spans.sort_unstable();
    spans.windows(2).any(|w| w[1].0 < w[0].1)
}

/// A cluster chain's runs as `chain` returns them, cut back to nothing if the chain comes back
/// to a cluster it already passed through.
pub(crate) fn looped(
    runs: Vec<Run>,
    at: u64,
    why: Option<String>,
) -> (Vec<Run>, u64, Option<String>) {
    if overlapping(&runs) {
        (
            Vec::new(),
            0,
            Some("its cluster chain loops back on itself".into()),
        )
    } else {
        (runs, at, why)
    }
}

/// A file's contents, mapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Data {
    /// Kept in the metadata itself (NTFS resident data, ext4 inline data).
    Bytes(Vec<u8>),
    /// Pieces of the volume; past `valid` (and in holes) the file reads as zeros.
    Runs {
        runs: Vec<Run>,
        size: u64,
        valid: u64,
    },
    /// NTFS compression: LZNT1 in units of `unit` bytes.
    Lznt1 {
        runs: Vec<Run>,
        size: u64,
        valid: u64,
        unit: u64,
    },
}

impl Data {
    fn size(&self) -> u64 {
        match self {
            Data::Bytes(b) => b.len() as u64,
            Data::Runs { size, .. } | Data::Lznt1 { size, .. } => *size,
        }
    }
}

/// Reads a directory's (or other metadata's) runs in pieces of at most `CHUNK` bytes, stopping
/// after `cap` bytes or when `each` returns false. Holes read as zeros.
pub(crate) fn each_chunk<R: Read + Seek>(
    r: &mut Region<R>,
    runs: &[Run],
    cap: u64,
    mut each: impl FnMut(&[u8]) -> Result<bool>,
) -> Result<()> {
    let mut done = 0u64;
    for run in runs {
        let mut off = 0;
        while off < run.len {
            if done >= cap {
                return Ok(());
            }
            let n = CHUNK.min(run.len - off).min(cap - done);
            let buf = match run.phys {
                Some(p) => r.read_vec(p + off, n)?,
                None => vec![0u8; n as usize],
            };
            if !each(&buf)? {
                return Ok(());
            }
            off += n;
            done += n;
        }
    }
    Ok(())
}

/// A small file's (or stream's) first `cap` bytes, whole: a link target, an attribute list.
pub(crate) fn read_all<R: Read + Seek>(r: &mut Region<R>, data: Data, cap: u64) -> Result<Vec<u8>> {
    r.in_time()?;
    let n = data.size().min(cap).min(META_CAP);
    let mut out = Vec::with_capacity(n as usize);
    FileReader::new(&mut r.src, r.base, r.len, data)
        .take(n)
        .read_to_end(&mut out)?;
    Ok(out)
}

/// Exactly `buf.len()` bytes from byte `off` of what `runs` map (holes read as zeros).
pub(crate) fn read_mapped<R: Read + Seek>(
    r: &mut Region<R>,
    runs: &[Run],
    off: u64,
    buf: &mut [u8],
) -> Result<()> {
    r.in_time()?;
    let mut done = 0;
    while done < buf.len() {
        let at = off
            .checked_add(done as u64)
            .context("offset out of range (damaged)")?;
        done += read_runs(
            &mut r.src,
            r.base,
            r.len,
            runs,
            u64::MAX,
            at,
            &mut buf[done..],
        )?;
    }
    Ok(())
}

/// A file's bytes, read on demand through its map. `Volume::reader` and `into_reader` make these.
pub struct FileReader<S> {
    src: S,
    base: u64,
    len: u64,
    data: Data,
    pos: u64,
    /// The compression unit last decompressed: its number and bytes.
    unit: Option<(u64, Vec<u8>)>,
}

impl<S: Read + Seek> FileReader<S> {
    fn new(src: S, base: u64, len: u64, data: Data) -> FileReader<S> {
        FileReader {
            src,
            base,
            len,
            data,
            pos: 0,
            unit: None,
        }
    }

    /// The file's size in bytes.
    pub fn size(&self) -> u64 {
        self.data.size()
    }
}

impl<S: Read + Seek> Read for FileReader<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let size = self.data.size();
        if self.pos >= size || buf.is_empty() {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(size - self.pos) as usize;
        let buf = &mut buf[..want];
        let FileReader {
            src,
            base,
            len,
            data,
            pos,
            unit,
        } = self;
        let n = match data {
            Data::Bytes(b) => {
                let at = *pos as usize;
                buf.copy_from_slice(&b[at..at + want]);
                want
            }
            Data::Runs { runs, valid, .. } => read_runs(src, *base, *len, runs, *valid, *pos, buf)?,
            Data::Lznt1 {
                runs,
                valid,
                unit: size_u,
                ..
            } => {
                let (u, off) = (*pos / *size_u, *pos % *size_u);
                if unit.as_ref().is_none_or(|(n, _)| *n != u) {
                    *unit = Some((u, compressed_unit(src, *base, *len, runs, u, *size_u)?));
                }
                let bytes = &unit.as_ref().expect("just filled").1;
                let n = want.min((*size_u - off) as usize);
                buf[..n].copy_from_slice(&bytes[off as usize..off as usize + n]);
                // past the initialized size, the file reads as zeros whatever is stored
                let keep = valid.saturating_sub(*pos).min(n as u64) as usize;
                buf[keep..n].fill(0);
                n
            }
        };
        *pos += n as u64;
        Ok(n)
    }
}

impl<S: Read + Seek> Seek for FileReader<S> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let size = self.data.size();
        let pos = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => size.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos = pos.ok_or_else(|| bad("seek before the start of the file".into()))?;
        Ok(self.pos)
    }
}

/// One read's worth from a run map: up to the end of the run (or hole) `pos` is in.
fn read_runs<S: Read + Seek>(
    src: &mut S,
    base: u64,
    len: u64,
    runs: &[Run],
    valid: u64,
    pos: u64,
    buf: &mut [u8],
) -> io::Result<usize> {
    if pos >= valid {
        buf.fill(0);
        return Ok(buf.len());
    }
    let want = (buf.len() as u64).min(valid - pos);
    let i = runs.partition_point(|r| r.at <= pos);
    if i > 0 {
        let r = runs[i - 1];
        let end = r.at.saturating_add(r.len);
        if pos < end {
            let n = want.min(end - pos) as usize;
            match r.phys {
                Some(p) => read_region(src, base, len, p + (pos - r.at), &mut buf[..n])?,
                None => buf[..n].fill(0),
            }
            return Ok(n);
        }
    }
    let next = runs.get(i).map_or(u64::MAX, |r| r.at);
    let n = want.min(next - pos) as usize;
    buf[..n].fill(0);
    Ok(n)
}

/// Compression unit `u` of an NTFS-compressed file: all holes is zeros, no holes is stored as is,
/// and data followed by a hole is LZNT1.
fn compressed_unit<S: Read + Seek>(
    src: &mut S,
    base: u64,
    len: u64,
    runs: &[Run],
    u: u64,
    unit: u64,
) -> io::Result<Vec<u8>> {
    let start = u * unit;
    let end = start.saturating_add(unit);
    let (mut stored, mut hole) = (Vec::new(), false);
    let first = runs.partition_point(|r| r.at.saturating_add(r.len) <= start);
    for r in &runs[first..] {
        if r.at >= end {
            break;
        }
        let a = r.at.max(start);
        let b = r.at.saturating_add(r.len).min(end);
        match r.phys {
            None => hole = true,
            Some(p) => {
                let at = stored.len();
                stored.resize(at + (b - a) as usize, 0);
                read_region(src, base, len, p + (a - r.at), &mut stored[at..])?;
            }
        }
    }
    if stored.is_empty() {
        return Ok(vec![0u8; unit as usize]);
    }
    if !hole {
        stored.resize(unit as usize, 0);
        return Ok(stored);
    }
    ntfs::lznt1(&stored, unit as usize).map_err(|e| bad(format!("{e:#}")))
}

enum Fs {
    Fat(fat::Fat),
    Exfat(exfat::Exfat),
    Ext(ext::Ext),
    Ntfs(ntfs::Ntfs),
}

/// A filesystem, open for reading.
pub struct Volume<R> {
    region: Region<R>,
    fs: Fs,
    info: Info,
}

/// The filesystem at the start of `head` (a volume's first 4 KiB), if it's one this module reads.
pub fn detect(head: &[u8]) -> Option<&'static str> {
    if ntfs::detect(head) {
        Some("NTFS")
    } else if exfat::detect(head) {
        Some("exFAT")
    } else if ext::detect(head) {
        Some("ext")
    } else if fat::detect(head) {
        Some("FAT")
    } else {
        None
    }
}

impl<R: Read + Seek> Volume<R> {
    /// Opens the filesystem in the `len` bytes of `src` from `base` (a partition's start and size,
    /// or 0 and the image's size for an image of one filesystem).
    pub fn open(src: R, base: u64, len: u64) -> Result<Volume<R>> {
        let mut region = Region::new(src, base, len);
        if len < 512 {
            bail!("too small to hold a filesystem ({len} bytes)");
        }
        let head = region
            .read_vec(0, len.min(4096))
            .context("can't read the start of the volume")?;
        let (fs, info) = match detect(&head) {
            Some("NTFS") => {
                let (f, i) = ntfs::Ntfs::open(&mut region, &head)?;
                (Fs::Ntfs(f), i)
            }
            Some("exFAT") => {
                let (f, i) = exfat::Exfat::open(&mut region, &head)?;
                (Fs::Exfat(f), i)
            }
            Some("ext") => {
                let (f, i) = ext::Ext::open(&mut region, &head)?;
                (Fs::Ext(f), i)
            }
            Some(_) => {
                let (f, i) = fat::Fat::open(&mut region, &head)?;
                (Fs::Fat(f), i)
            }
            None => bail!(
                "no filesystem BackDown can read here (it reads FAT, exFAT, ext2/3/4 and NTFS)"
            ),
        };
        Ok(Volume { region, fs, info })
    }

    pub fn info(&self) -> &Info {
        &self.info
    }

    /// From now on, reading folders and finding files fails once `until` has passed (reading a
    /// file's own bytes, once it's open, never does).
    pub fn set_deadline(&mut self, until: Instant) {
        self.region.until = Some(until);
    }

    /// The root folder.
    pub fn root(&self) -> Entry {
        let node = match &self.fs {
            Fs::Fat(_) => Node::Fat {
                cluster: 0,
                size: 0,
            },
            Fs::Exfat(x) => x.root(),
            Fs::Ext(_) => Node::Ext(ext::ROOT),
            Fs::Ntfs(_) => Node::Ntfs {
                record: ntfs::ROOT,
                seq: 0,
            },
        };
        Entry {
            name: String::new(),
            dir: true,
            size: 0,
            mtime: None,
            link: None,
            unreadable: None,
            flags: Vec::new(),
            node,
        }
    }

    /// Calls `each` for the entries of `dir` until it returns false. With `want`, entries by other
    /// names may be skipped without being looked at closely.
    fn read_dir(
        &mut self,
        dir: &Entry,
        want: Option<&str>,
        each: &mut dyn FnMut(Entry) -> bool,
    ) -> Result<()> {
        if !dir.dir {
            bail!("{} is a file, not a folder", dir.name);
        }
        let r = &mut self.region;
        match &mut self.fs {
            Fs::Fat(x) => x.read_dir(r, &dir.node, want, each),
            Fs::Exfat(x) => x.read_dir(r, &dir.node, want, each),
            Fs::Ext(x) => x.read_dir(r, &dir.node, want, each),
            Fs::Ntfs(x) => x.read_dir(r, &dir.node, want, each),
        }
    }

    /// The first `limit` (at most `MAX_LIST`) entries of a folder, without "." and "..".
    pub fn list(&mut self, dir: &Entry, limit: usize) -> Result<Listing> {
        let limit = limit.clamp(1, MAX_LIST);
        let (mut entries, mut truncated) = (Vec::new(), false);
        self.read_dir(dir, None, &mut |e| {
            if entries.len() >= limit {
                truncated = true;
                return false;
            }
            entries.push(e);
            true
        })?;
        Ok(Listing { entries, truncated })
    }

    /// The entry at `path` ("DCIM/100CANON/IMG_0001.JPG", "/" separated, from the root), found
    /// name by name through the folders' own listings. An empty path is the root.
    pub fn find(&mut self, path: &str) -> Result<Entry> {
        let mut cur = self.root();
        for (depth, seg) in path.split('/').filter(|s| !s.is_empty()).enumerate() {
            if depth >= MAX_DEPTH {
                bail!("the path is too deep");
            }
            if !cur.dir {
                bail!("no {path:?} here: {} is a file", cur.name);
            }
            let mut hit = None;
            self.read_dir(&cur, Some(seg), &mut |e| {
                if e.name == seg {
                    hit = Some(e);
                    false
                } else {
                    true
                }
            })?;
            cur = hit.with_context(|| format!("no {path:?} in this filesystem"))?;
        }
        Ok(cur)
    }

    /// The listing of the folder at `path`.
    pub fn list_path(&mut self, path: &str, limit: usize) -> Result<Listing> {
        let dir = self.find(path)?;
        if !dir.dir {
            bail!("{path} is a file, not a folder");
        }
        self.list(&dir, limit)
    }

    fn data(&mut self, file: &Entry) -> Result<Data> {
        if file.dir {
            bail!("{} is a folder", file.name);
        }
        if let Some(t) = &file.link {
            bail!("{} is a link to {t}; links aren't followed", file.name);
        }
        if let Some(why) = &file.unreadable {
            bail!("{}: {why}", file.name);
        }
        let r = &mut self.region;
        match &mut self.fs {
            Fs::Fat(x) => x.data(r, &file.node),
            Fs::Exfat(x) => x.data(r, &file.node),
            Fs::Ext(x) => x.data(r, &file.node),
            Fs::Ntfs(x) => x.data(r, &file.node),
        }
    }

    /// Reads a file's contents (an entry from a listing or `find`).
    pub fn reader(&mut self, file: &Entry) -> Result<FileReader<&mut R>> {
        let data = self.data(file)?;
        let Region { src, base, len, .. } = &mut self.region;
        Ok(FileReader::new(src, *base, *len, data))
    }

    /// Like `reader`, handing the source over to it (to stream a file after the volume is done).
    pub fn into_reader(mut self, file: &Entry) -> Result<FileReader<R>> {
        let data = self.data(file)?;
        let Region { src, base, len, .. } = self.region;
        Ok(FileReader::new(src, base, len, data))
    }
}

// ---- shared helpers

/// A name as listings give it: None for "", "." and ".."; "/" and NUL (only in damaged
/// filesystems) become U+FFFD so every listed name is one path segment.
pub(crate) fn clean_name(name: String) -> Option<String> {
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    if name.contains(['/', '\0']) {
        return Some(name.replace(['/', '\0'], "\u{FFFD}"));
    }
    Some(name)
}

/// Days from 1970-01-01 to a proleptic Gregorian date.
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

/// A FAT or exFAT date and time (no time zone).
pub(crate) fn dos_time(date: u64, time: u64) -> Option<i64> {
    let (m, d) = ((date >> 5) & 15, date & 31);
    let (h, min, s) = (time >> 11, (time >> 5) & 63, (time & 31) * 2);
    if !(1..=12).contains(&m) || d == 0 || h > 23 || min > 59 || s > 59 {
        return None;
    }
    let day = days_from_civil(1980 + (date >> 9) as i64, m as i64, d as i64);
    Some(day * 86_400 + (h * 3600 + min * 60 + s) as i64)
}

/// A Windows FILETIME (100 ns ticks since 1601) as Unix seconds.
pub(crate) fn filetime(t: u64) -> Option<i64> {
    (t != 0).then(|| (t / 10_000_000) as i64 - 11_644_473_600)
}

/// UTF-16LE, lossily.
pub(crate) fn utf16le(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests;
