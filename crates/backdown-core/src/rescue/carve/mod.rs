//! Carving: finding files by what's in them, whatever the filesystem says (like PhotoRec).
//!
//! A formatted memory card, or a drive whose filesystem is gone, still holds most of its files'
//! bytes: formatting only rewrites the tables that say where they are. Files are almost always
//! written from the start of a cluster, so the scan reads the source front to back in large chunks
//! and looks, at every sector boundary, for the first bytes of a type it knows. Where one matches,
//! that type's own structure says where the file ends: JPEG segments up to the end-of-image
//! marker, PNG chunks up to IEND, MP4 boxes, a ZIP's central directory, the last `%%EOF` of a PDF
//! that points back at its own cross-reference table. The structure is also what tells a real file
//! from a chance match in random bytes, so a file is only reported when it checks out.
//!
//! It knows photos (JPEG, HEIC, PNG, GIF, WebP, BMP, TIFF and camera RAW), videos (MP4, MOV, 3GP,
//! AVI, MKV/WebM, Windows Media, AVCHD, MPEG), audio (MP3, M4A, WAV, FLAC, Ogg, AIFF), documents
//! (PDF, Word, Excel and PowerPoint old and new, OpenDocument, EPUB, Outlook .msg), archives (ZIP,
//! RAR, 7z) and SQLite databases: `TYPES` lists them.
//!
//! After a file is found the scan carries on after its end, as PhotoRec does: a photo's embedded
//! thumbnail, or the pictures inside a Word document, aren't reported as files of their own. When
//! the end couldn't be found (the file runs off the end of the scanned area, or stops making sense
//! part-way because it was fragmented or overwritten) it's reported as incomplete with what could
//! be read, and the scan doesn't skip over it.
//!
//! What it can't do: put back together a file that was written in pieces (fragmented), rare on a
//! camera card but common for large files on a well-used computer drive; find types it doesn't
//! know; give back names, folders or dates, which lived in the filesystem. Photos keep the date and
//! camera from their EXIF data, videos their recording date.
//!
//! The source is only ever read. Memory stays bounded (one chunk, plus a small buffer while a file
//! is followed) whatever its size.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::size_of;
use crate::fmt;
use crate::magic::{be, has};

mod archives;
mod documents;
mod media;
mod photos;
mod save;

use archives::*;
use documents::*;
use media::*;
use photos::*;
pub use save::*;

// ---------- what it finds

pub const PHOTOS: &str = "photos";
pub const VIDEOS: &str = "videos";
pub const AUDIO: &str = "audio";
pub const DOCUMENTS: &str = "documents";
pub const ARCHIVES: &str = "archives";
pub const OTHER: &str = "other";
pub const GROUPS: [&str; 6] = [PHOTOS, VIDEOS, AUDIO, DOCUMENTS, ARCHIVES, OTHER];

/// Every type the carver finds: extension, what it is for people, and its group.
pub const TYPES: &[(&str, &str, &str)] = &[
    ("jpg", "JPEG photo", PHOTOS),
    ("heic", "HEIC photo", PHOTOS),
    ("png", "PNG image", PHOTOS),
    ("gif", "GIF image", PHOTOS),
    ("webp", "WebP image", PHOTOS),
    ("bmp", "BMP image", PHOTOS),
    ("avif", "AVIF image", PHOTOS),
    ("tif", "TIFF image", PHOTOS),
    ("cr2", "Canon RAW photo", PHOTOS),
    ("cr3", "Canon RAW photo", PHOTOS),
    ("nef", "Nikon RAW photo", PHOTOS),
    ("arw", "Sony RAW photo", PHOTOS),
    ("dng", "DNG RAW photo", PHOTOS),
    ("orf", "Olympus RAW photo", PHOTOS),
    ("rw2", "Panasonic RAW photo", PHOTOS),
    ("pef", "Pentax RAW photo", PHOTOS),
    ("srw", "Samsung RAW photo", PHOTOS),
    ("raf", "Fujifilm RAW photo", PHOTOS),
    ("mp4", "MP4 video", VIDEOS),
    ("mov", "QuickTime video", VIDEOS),
    ("m4v", "M4V video", VIDEOS),
    ("3gp", "3GP phone video", VIDEOS),
    ("avi", "AVI video", VIDEOS),
    ("mkv", "Matroska video", VIDEOS),
    ("webm", "WebM video", VIDEOS),
    ("wmv", "Windows Media video", VIDEOS),
    ("mts", "AVCHD video", VIDEOS),
    ("ts", "MPEG transport stream video", VIDEOS),
    ("mpg", "MPEG video", VIDEOS),
    ("ogv", "Ogg video", VIDEOS),
    ("mp3", "MP3 audio", AUDIO),
    ("m4a", "M4A audio", AUDIO),
    ("wav", "WAV audio", AUDIO),
    ("flac", "FLAC audio", AUDIO),
    ("ogg", "Ogg Vorbis audio", AUDIO),
    ("opus", "Opus audio", AUDIO),
    ("aif", "AIFF audio", AUDIO),
    ("wma", "Windows Media audio", AUDIO),
    ("pdf", "PDF document", DOCUMENTS),
    ("docx", "Word document", DOCUMENTS),
    ("xlsx", "Excel spreadsheet", DOCUMENTS),
    ("pptx", "PowerPoint presentation", DOCUMENTS),
    ("doc", "Word document (old format)", DOCUMENTS),
    ("xls", "Excel spreadsheet (old format)", DOCUMENTS),
    ("ppt", "PowerPoint presentation (old format)", DOCUMENTS),
    ("odt", "OpenDocument text", DOCUMENTS),
    ("ods", "OpenDocument spreadsheet", DOCUMENTS),
    ("odp", "OpenDocument presentation", DOCUMENTS),
    ("epub", "EPUB e-book", DOCUMENTS),
    ("msg", "Outlook email", DOCUMENTS),
    ("zip", "ZIP archive", ARCHIVES),
    ("rar", "RAR archive", ARCHIVES),
    ("7z", "7-Zip archive", ARCHIVES),
    ("sqlite", "SQLite database", OTHER),
    ("ole", "Office compound file", OTHER),
];

/// Other names people use for a type, or for a set of them.
const ALIASES: &[(&str, &[&str])] = &[
    ("jpeg", &["jpg"]),
    ("tiff", &["tif"]),
    ("aiff", &["aif"]),
    ("m2ts", &["mts"]),
    ("mpeg", &["mpg"]),
    (
        "raw",
        &[
            "cr2", "cr3", "nef", "arw", "dng", "orf", "rw2", "pef", "srw", "raf",
        ],
    ),
    (
        "office",
        &[
            "docx", "xlsx", "pptx", "doc", "xls", "ppt", "odt", "ods", "odp",
        ],
    ),
];

fn describe(ext: &str) -> (&'static str, &'static str) {
    TYPES
        .iter()
        .find(|t| t.0 == ext)
        .map_or(("File", OTHER), |t| (t.1, t.2))
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Files start at multiples of this from `start`: 512, a sector (4096 on some drives).
    pub sector: u64,
    /// None: every type. Else extensions ("jpg", "pdf") and group names ("photos", "documents").
    pub kinds: Option<Vec<String>>,
    /// Where to start, e.g. a partition's offset.
    pub start: u64,
    /// Where to stop (exclusive); None: the end of the source. Nothing at or past it is read, so a
    /// file that runs past it is reported as incomplete.
    pub end: Option<u64>,
    /// Smaller files aren't reported.
    pub min_size: u64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            sector: 512,
            kinds: None,
            start: 0,
            end: None,
            min_size: 0,
        }
    }
}

/// A file found in the source.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Found {
    /// where it starts in the source, in bytes
    pub offset: u64,
    pub size: u64,
    pub ext: &'static str,
    /// for people: "JPEG photo", "Word document"
    pub kind: &'static str,
    /// photos, videos, audio, documents, archives or other
    pub group: &'static str,
    /// its end was found (else it was cut off or damaged, and `note` says which)
    pub complete: bool,
    /// when the photo was taken or the video recorded (Unix seconds, from EXIF or the file's own
    /// header; cameras write local time, so it's that clock read as UTC)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub taken: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Groups {
    pub photos: u64,
    pub videos: u64,
    pub audio: u64,
    pub documents: u64,
    pub archives: u64,
    pub other: u64,
}

impl Groups {
    fn add(&mut self, group: &str) {
        match group {
            PHOTOS => self.photos += 1,
            VIDEOS => self.videos += 1,
            AUDIO => self.audio += 1,
            DOCUMENTS => self.documents += 1,
            ARCHIVES => self.archives += 1,
            _ => self.other += 1,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Progress {
    /// bytes of the range looked through so far, and in all
    pub done: u64,
    pub size: u64,
    /// where the scan is in the source
    pub pos: u64,
    pub found: u64,
    pub found_bytes: u64,
    pub by_group: Groups,
    pub secs: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stats {
    /// bytes of the range looked through
    pub scanned: u64,
    pub found: u64,
    pub found_bytes: u64,
    /// found but cut off or damaged
    pub incomplete: u64,
    pub by_group: Groups,
    /// written out by `scan_and_save`
    pub saved: u64,
    pub saved_bytes: u64,
    /// bytes of the source that couldn't be read (looked at as zeros)
    pub unreadable: u64,
    /// stopped when asked, before the end
    pub stopped: bool,
    pub secs: f64,
    pub problems: Vec<String>,
}

impl Stats {
    fn add(&mut self, f: &Found) {
        self.found += 1;
        self.found_bytes += f.size;
        self.by_group.add(f.group);
        if !f.complete {
            self.incomplete += 1;
        }
    }
}

// ---------- scanning

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
/// How much is read at a time: large enough that reading is sequential and fast.
const CHUNK: usize = 8 << 20;
/// Read past each chunk, so a signature near its end can be looked at whole.
const TAIL: usize = 4096;
/// A followed file is read in blocks growing up to this.
const BLOCK: usize = 1 << 20;
const FIRST_BLOCK: usize = 64 << 10;
/// When a read fails, the chunk is read again in pieces this big, and the ones that fail are zeros.
const RETRY_PIECE: usize = 64 << 10;
/// This much unreadable in a row means the source is failing badly or gone.
const MAX_BAD_RUN: u64 = 64 * MIB;
const MAX_SECTOR: u64 = MIB;
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
const MAX_PROBLEMS: usize = 20;

const CUT: &str = "cut off: it runs past the end of what was scanned";
const DAMAGED: &str =
    "damaged, overwritten or split up part-way: kept up to where it stops making sense";
const NO_END: &str = "its end isn't recorded: kept up to where the next file starts";
const UNFINISHED: &str =
    "a recording that was never finished (its index is missing): it may need repairing to play";
const NO_SIZES: &str = "a recording that was never finished (its sizes weren't written): kept up \
                        to where the next file starts";

fn capped(max: u64) -> String {
    format!(
        "its end wasn't found within {}: kept that much",
        fmt::bytes(max)
    )
}

fn note(problems: &mut Vec<String>, p: String) {
    if problems.len() < MAX_PROBLEMS {
        problems.push(p);
    }
}

/// Look through `src` for files, calling `found` for each (an error from it stops the scan and is
/// returned). `stop` is checked between chunks (and while a long file is followed): stopping
/// returns what was found so far, with `stopped` set. `progress` is called at most 4 times a
/// second.
pub fn scan(
    src: &Path,
    opts: &Options,
    progress: &mut dyn FnMut(&Progress),
    stop: &AtomicBool,
    found: &mut dyn FnMut(&Found) -> Result<()>,
) -> Result<Stats> {
    let file =
        super::open(src).with_context(|| format!("can't open {} to read it", src.display()))?;
    let total =
        size_of(&file).with_context(|| format!("can't tell how big {} is", src.display()))?;
    scan_medium(&file, total, opts, progress, stop, found)
}

fn scan_medium(
    m: &dyn Medium,
    total: u64,
    opts: &Options,
    progress: &mut dyn FnMut(&Progress),
    stop: &AtomicBool,
    found: &mut dyn FnMut(&Found) -> Result<()>,
) -> Result<Stats> {
    if opts.sector == 0 || opts.sector > MAX_SECTOR {
        bail!(
            "the sector size has to be between 1 byte and 1 MiB, not {}",
            opts.sector
        );
    }
    let want = Want::new(opts.kinds.as_deref())?;
    let end = opts.end.map_or(total, |e| e.min(total));
    let start = opts.start.min(end);
    let sector = opts.sector;
    let t0 = Instant::now();
    let mut last = t0;
    let mut stats = Stats::default();
    let mut buf = vec![0u8; CHUNK + TAIL];
    let mut bad_run = 0u64;
    let mut pos = start;
    'chunks: while pos < end {
        if stop.load(Ordering::Relaxed) {
            stats.stopped = true;
            break;
        }
        let len = (end - pos).min((CHUNK + TAIL) as u64) as usize;
        let n = fill(m, pos, &mut buf[..len], &mut stats.unreadable, &mut bad_run)?;
        if n == 0 {
            break; // the source ended early
        }
        let chunk = &buf[..n];
        let limit = (pos + CHUNK as u64).min(pos + n as u64).min(end);
        let src = Src {
            m,
            end,
            base: start,
            sector,
            stop,
            chunk,
            chunk_at: pos,
        };
        let mut at = pos;
        while at < limit {
            let i = (at - pos) as usize;
            let fmt = sniff(&chunk[i..]).filter(|f| want.on[*f as usize]);
            if let Some(fmt) = fmt {
                let hit = match parse(fmt, &src, at) {
                    Ok(h) => h,
                    Err(_) if stop.load(Ordering::Relaxed) => {
                        stats.stopped = true;
                        pos = at;
                        break 'chunks;
                    }
                    Err(e) => {
                        note(
                            &mut stats.problems,
                            format!("couldn't read what looked like a file at byte {at}: {e}"),
                        );
                        None
                    }
                };
                if let Some(mut hit) = hit.filter(|h| h.size > 0) {
                    if at + hit.size > end {
                        hit = hit.cut(&src, at);
                    }
                    let (size, skip) = (hit.size, hit.complete);
                    if want.reports(hit.ext) && size >= opts.min_size {
                        let f = hit.found(at);
                        stats.add(&f);
                        found(&f)?;
                    }
                    // what's inside it (thumbnails, pictures in a document) isn't a file of its own
                    let next = if skip {
                        src.align_up(at + size)
                    } else {
                        at + sector
                    };
                    if stop.load(Ordering::Relaxed) {
                        stats.stopped = true;
                        pos = next;
                        break 'chunks;
                    }
                    at = next;
                    continue;
                }
            }
            at += sector;
        }
        pos = at;
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            progress(&progress_of(&stats, start, end, pos, t0));
        }
    }
    stats.scanned = pos.min(end) - start;
    stats.secs = t0.elapsed().as_secs_f64();
    progress(&progress_of(&stats, start, end, pos, t0));
    Ok(stats)
}

fn progress_of(stats: &Stats, start: u64, end: u64, pos: u64, t0: Instant) -> Progress {
    Progress {
        done: pos.min(end) - start,
        size: end - start,
        pos: pos.min(end),
        found: stats.found,
        found_bytes: stats.found_bytes,
        by_group: stats.by_group.clone(),
        secs: t0.elapsed().as_secs_f64(),
    }
}

/// Read `buf.len()` bytes at `off` (fewer only where the source ends). Parts that can't be read
/// are zeros, counted in `unreadable`: a card with a few bad sectors still gets looked through,
/// and only a long run of them stops the scan.
fn fill(
    m: &dyn Medium,
    off: u64,
    buf: &mut [u8],
    unreadable: &mut u64,
    bad_run: &mut u64,
) -> Result<usize> {
    if let Ok(n) = read_full(m, off, buf) {
        *bad_run = 0;
        return Ok(n);
    }
    let mut done = 0;
    while done < buf.len() {
        let k = (buf.len() - done).min(RETRY_PIECE);
        match read_full(m, off + done as u64, &mut buf[done..done + k]) {
            Ok(n) => {
                done += n;
                *bad_run = 0;
                if n < k {
                    break;
                }
            }
            Err(_) => {
                buf[done..done + k].fill(0);
                done += k;
                *unreadable += k as u64;
                *bad_run += k as u64;
                if *bad_run >= MAX_BAD_RUN {
                    bail!(
                        "{} in a row can't be read (up to byte {}): the source is failing or was \
                         disconnected. Make an image of it first, which works around bad areas, \
                         and look for files in the image",
                        fmt::bytes(*bad_run),
                        off + done as u64
                    );
                }
            }
        }
    }
    Ok(done)
}

// ---------- reading

/// Where the bytes come from: the source (and in tests, memory).
trait Medium {
    fn pread(&self, off: u64, buf: &mut [u8]) -> io::Result<usize>;
}

impl Medium for File {
    #[cfg(unix)]
    fn pread(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(self, buf, off)
    }

    /// Raw disks and volumes on Windows only read whole sectors at sector offsets: read around.
    #[cfg(windows)]
    fn pread(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        use std::os::windows::fs::FileExt;
        const A: u64 = 4096;
        let lo = off / A * A;
        let hi = (off + buf.len() as u64).div_ceil(A) * A;
        if lo == off && hi == off + buf.len() as u64 {
            return self.seek_read(buf, off);
        }
        let mut tmp = vec![0u8; (hi - lo) as usize];
        let n = self.seek_read(&mut tmp, lo)?;
        let skip = (off - lo) as usize;
        if n <= skip {
            return Ok(0);
        }
        let k = (n - skip).min(buf.len());
        buf[..k].copy_from_slice(&tmp[skip..skip + k]);
        Ok(k)
    }

    #[cfg(not(any(unix, windows)))]
    fn pread(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        use std::io::{Read, Seek, SeekFrom};
        let mut r = self;
        r.seek(SeekFrom::Start(off))?;
        r.read(buf)
    }
}

#[cfg(test)]
impl Medium for Vec<u8> {
    fn pread(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let Some(rest) = usize::try_from(off).ok().and_then(|o| self.get(o..)) else {
            return Ok(0);
        };
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        Ok(n)
    }
}

/// For a source that's all in memory already (see `Src::mem`).
struct Nothing;

impl Medium for Nothing {
    fn pread(&self, _: u64, _: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }
}

static NOTHING: Nothing = Nothing;
static NEVER: AtomicBool = AtomicBool::new(false);

/// Fill `buf` from `off`; fewer bytes only where the source ends.
fn read_full(m: &dyn Medium, off: u64, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match m.pread(off + n as u64, &mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// What a parser reads from: the source up to the end of the scanned area, served from the chunk
/// in memory where it can be.
#[derive(Clone, Copy)]
struct Src<'a> {
    m: &'a dyn Medium,
    /// nothing at or past this is read
    end: u64,
    /// sector boundaries count from here
    base: u64,
    sector: u64,
    stop: &'a AtomicBool,
    chunk: &'a [u8],
    chunk_at: u64,
}

impl<'a> Src<'a> {
    /// Bytes already in memory (an EXIF block, a test's image).
    fn mem(b: &'a [u8]) -> Src<'a> {
        Src {
            m: &NOTHING,
            end: b.len() as u64,
            base: 0,
            sector: 512,
            stop: &NEVER,
            chunk: b,
            chunk_at: 0,
        }
    }

    fn cached(&self, off: u64, len: usize) -> Option<&'a [u8]> {
        let i = usize::try_from(off.checked_sub(self.chunk_at)?).ok()?;
        self.chunk.get(i..i.checked_add(len)?)
    }

    /// Up to `len` bytes at `off` (fewer where the scanned area ends).
    fn get(&self, off: u64, len: usize) -> io::Result<Cow<'a, [u8]>> {
        if off >= self.end {
            return Ok(Cow::Borrowed(&[]));
        }
        let len = (len as u64).min(self.end - off) as usize;
        if let Some(b) = self.cached(off, len) {
            return Ok(Cow::Borrowed(b));
        }
        let mut v = vec![0u8; len];
        let n = read_full(self.m, off, &mut v)?;
        v.truncate(n);
        Ok(Cow::Owned(v))
    }

    fn read_into(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        if off >= self.end {
            return Ok(0);
        }
        let len = (buf.len() as u64).min(self.end - off) as usize;
        if let Some(b) = self.cached(off, len) {
            buf[..len].copy_from_slice(b);
            return Ok(len);
        }
        read_full(self.m, off, &mut buf[..len])
    }

    /// The first sector boundary at or after `x`.
    fn align_up(&self, x: u64) -> u64 {
        let x = x.max(self.base);
        self.base + (x - self.base).div_ceil(self.sector) * self.sector
    }
}

/// Reading a file front to back from the source, in blocks.
struct Stream<'a> {
    src: Src<'a>,
    buf: Vec<u8>,
    /// where buf[0] is in the source
    at: u64,
    /// the next byte to look at
    pos: u64,
    block: usize,
}

impl<'a> Stream<'a> {
    fn new(src: Src<'a>, off: u64) -> Self {
        Stream {
            src,
            buf: Vec::new(),
            at: off,
            pos: off,
            block: FIRST_BLOCK,
        }
    }

    fn offset(&self) -> u64 {
        self.pos
    }

    fn seek(&mut self, off: u64) {
        self.pos = off;
    }

    fn advance(&mut self, n: u64) {
        self.pos = self.pos.saturating_add(n);
    }

    /// The bytes from the current position on that are in memory: at least `min` of them, unless
    /// the scanned area ends first.
    fn peek(&mut self, min: usize) -> io::Result<&[u8]> {
        let buf_end = self.at + self.buf.len() as u64;
        if self.pos < self.at || self.pos > buf_end {
            self.buf.clear();
            self.at = self.pos;
        }
        let have = (self.at + self.buf.len() as u64 - self.pos) as usize;
        if have < min && self.at + (self.buf.len() as u64) < self.src.end {
            if self.src.stop.load(Ordering::Relaxed) {
                return Err(io::Error::other("stopped"));
            }
            let used = (self.pos - self.at) as usize;
            self.buf.drain(..used);
            self.at = self.pos;
            let kept = self.buf.len();
            let target = min.max(self.block);
            self.block = (self.block * 2).min(BLOCK);
            self.buf.resize(target, 0);
            let n = self
                .src
                .read_into(self.at + kept as u64, &mut self.buf[kept..])?;
            self.buf.truncate(kept + n);
        }
        let i = ((self.pos - self.at) as usize).min(self.buf.len());
        Ok(&self.buf[i..])
    }
}

/// Where `needle` first starts in `h` at or after `from`.
fn find(h: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let first = *needle.first()?;
    let mut i = from;
    while i + needle.len() <= h.len() {
        let j = i + h[i..=h.len() - needle.len()]
            .iter()
            .position(|&c| c == first)?;
        if h[j..].starts_with(needle) {
            return Some(j);
        }
        i = j + 1;
    }
    None
}

/// Where `needle` last starts in `h`.
fn rfind(h: &[u8], needle: &[u8]) -> Option<usize> {
    let first = *needle.first()?;
    let mut end = h.len().checked_sub(needle.len())? + 1;
    while end > 0 {
        let j = h[..end].iter().rposition(|&c| c == first)?;
        if h[j..].starts_with(needle) {
            return Some(j);
        }
        end = j;
    }
    None
}

// ---------- recognising a start

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fmt {
    Jpeg,
    Png,
    Gif,
    Bmp,
    Riff,
    Iso,
    QtOld,
    Pdf,
    Zip,
    Cfb,
    Sqlite,
    Id3,
    Mpa,
    Flac,
    Ogg,
    Ebml,
    Rar,
    SevenZ,
    Tiff,
    Raf,
    Aiff,
    Asf,
    Ts,
    Ps,
}

const FMTS: usize = 24;

impl Fmt {
    const ALL: [Fmt; FMTS] = [
        Fmt::Jpeg,
        Fmt::Png,
        Fmt::Gif,
        Fmt::Bmp,
        Fmt::Riff,
        Fmt::Iso,
        Fmt::QtOld,
        Fmt::Pdf,
        Fmt::Zip,
        Fmt::Cfb,
        Fmt::Sqlite,
        Fmt::Id3,
        Fmt::Mpa,
        Fmt::Flac,
        Fmt::Ogg,
        Fmt::Ebml,
        Fmt::Rar,
        Fmt::SevenZ,
        Fmt::Tiff,
        Fmt::Raf,
        Fmt::Aiff,
        Fmt::Asf,
        Fmt::Ts,
        Fmt::Ps,
    ];

    /// What this format's parser can report.
    fn exts(self) -> &'static [&'static str] {
        match self {
            Fmt::Jpeg => &["jpg"],
            Fmt::Png => &["png"],
            Fmt::Gif => &["gif"],
            Fmt::Bmp => &["bmp"],
            Fmt::Riff => &["webp", "wav", "avi"],
            Fmt::Iso => &["mp4", "mov", "m4v", "3gp", "m4a", "heic", "avif", "cr3"],
            Fmt::QtOld => &["mov"],
            Fmt::Pdf => &["pdf"],
            Fmt::Zip => &["zip", "docx", "xlsx", "pptx", "odt", "ods", "odp", "epub"],
            Fmt::Cfb => &["doc", "xls", "ppt", "msg", "ole"],
            Fmt::Sqlite => &["sqlite"],
            Fmt::Id3 | Fmt::Mpa => &["mp3"],
            Fmt::Flac => &["flac"],
            Fmt::Ogg => &["ogg", "opus", "ogv"],
            Fmt::Ebml => &["mkv", "webm"],
            Fmt::Rar => &["rar"],
            Fmt::SevenZ => &["7z"],
            Fmt::Tiff => &[
                "tif", "cr2", "nef", "arw", "dng", "orf", "rw2", "pef", "srw",
            ],
            Fmt::Raf => &["raf"],
            Fmt::Aiff => &["aif"],
            Fmt::Asf => &["wmv", "wma"],
            Fmt::Ts => &["mts", "ts"],
            Fmt::Ps => &["mpg"],
        }
    }
}

/// Which parsers run, and which of what they find is reported.
struct Want {
    exts: Option<HashSet<&'static str>>,
    on: [bool; FMTS],
}

impl Want {
    fn new(kinds: Option<&[String]>) -> Result<Want> {
        let Some(kinds) = kinds else {
            return Ok(Want {
                exts: None,
                on: [true; FMTS],
            });
        };
        let mut set: HashSet<&'static str> = HashSet::new();
        for k in kinds {
            let k = k.trim().trim_start_matches('.').to_ascii_lowercase();
            if k.is_empty() {
                continue;
            }
            let group = match k.as_str() {
                "images" | "pictures" => Some(PHOTOS),
                "music" => Some(AUDIO),
                g => GROUPS
                    .iter()
                    .find(|x| **x == g || x.strip_suffix('s') == Some(g))
                    .copied(),
            };
            if let Some(g) = group {
                set.extend(TYPES.iter().filter(|t| t.2 == g).map(|t| t.0));
            } else if let Some(t) = TYPES.iter().find(|t| t.0 == k) {
                set.insert(t.0);
            } else if let Some((_, exts)) = ALIASES.iter().find(|a| a.0 == k) {
                set.extend(exts.iter().copied());
            } else {
                bail!(
                    "unknown file type {k:?}: use extensions like jpg or pdf, or photos, videos, \
                     audio, documents, archives or other"
                );
            }
        }
        if set.is_empty() {
            bail!("no file types chosen to look for");
        }
        let on = Fmt::ALL.map(|f| f.exts().iter().any(|e| set.contains(e)));
        Ok(Want {
            exts: Some(set),
            on,
        })
    }

    fn reports(&self, ext: &str) -> bool {
        self.exts.as_ref().is_none_or(|s| s.contains(ext))
    }
}

const PNG_SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
const CFB_SIG: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const EBML_SIG: &[u8] = &[0x1A, 0x45, 0xDF, 0xA3];
const SEVENZ_SIG: &[u8] = &[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C];
const ASF_HEADER: &[u8] = &[
    0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C,
];
/// What an old QuickTime movie (from before `ftyp`) can start with.
const QT_FIRST: &[&[u8]] = &[b"moov", b"mdat", b"wide", b"free", b"skip", b"pnot"];

/// The format whose signature is at the start of `b`, if any (not yet checked beyond that).
fn sniff(b: &[u8]) -> Option<Fmt> {
    if b.len() < 16 {
        return None;
    }
    let ts_pat0 = |b: &[u8], at: usize| {
        b.get(at + 1).is_some_and(|x| x & 0xDF == 0x40) && b.get(at + 2) == Some(&0)
    };
    Some(match b[0] {
        0xFF if b[1] == 0xD8 && b[2] == 0xFF => Fmt::Jpeg,
        0xFF if b[1] & 0xE0 == 0xE0 => Fmt::Mpa,
        0x89 if has(b, 0, PNG_SIG) => Fmt::Png,
        b'G' if has(b, 0, b"GIF87a") || has(b, 0, b"GIF89a") => Fmt::Gif,
        b'B' if has(b, 0, b"BM") => Fmt::Bmp,
        b'R' if has(b, 0, b"RIFF") || has(b, 0, b"RF64") => Fmt::Riff,
        b'R' if has(b, 0, b"Rar!\x1a\x07") => Fmt::Rar,
        b'%' if has(b, 0, b"%PDF-") => Fmt::Pdf,
        b'P' if has(b, 0, b"PK\x03\x04") => Fmt::Zip,
        0xD0 if has(b, 0, CFB_SIG) => Fmt::Cfb,
        b'S' if has(b, 0, b"SQLite format 3\0") => Fmt::Sqlite,
        b'I' if has(b, 0, b"ID3") => Fmt::Id3,
        b'I' if has(b, 0, b"II*\0")
            || has(b, 0, b"IIRO")
            || has(b, 0, b"IIRS")
            || has(b, 0, b"IIU\0") =>
        {
            Fmt::Tiff
        }
        b'M' if has(b, 0, b"MM\0*") || has(b, 0, b"MMOR") => Fmt::Tiff,
        b'f' if has(b, 0, b"fLaC") => Fmt::Flac,
        b'O' if has(b, 0, b"OggS") => Fmt::Ogg,
        0x1A if has(b, 0, EBML_SIG) => Fmt::Ebml,
        b'7' if has(b, 0, SEVENZ_SIG) => Fmt::SevenZ,
        b'F' if has(b, 0, b"FORM") => Fmt::Aiff,
        b'F' if has(b, 0, b"FUJIFILMCCD-RAW ") => Fmt::Raf,
        0x30 if has(b, 0, ASF_HEADER) => Fmt::Asf,
        // a transport stream starts with its program table (PID 0)
        0x47 if ts_pat0(b, 0) && b.get(188) == Some(&0x47) && b.get(376) == Some(&0x47) => Fmt::Ts,
        0 if has(b, 0, &[0, 0, 1, 0xBA]) && (b[4] >> 6 == 1 || b[4] >> 4 == 2) => Fmt::Ps,
        _ => {
            if has(b, 4, b"ftyp") {
                Fmt::Iso
            } else if b[4] == 0x47
                && ts_pat0(b, 4)
                && b.get(196) == Some(&0x47)
                && b.get(388) == Some(&0x47)
            {
                Fmt::Ts
            } else if be(b, 0, 4) >= 8 && QT_FIRST.iter().any(|t| has(b, 4, t)) {
                Fmt::QtOld
            } else {
                return None;
            }
        }
    })
}

/// A signature strong enough to say a new file starts here without parsing it: where a file whose
/// end isn't recorded is cut.
fn strong(b: &[u8]) -> bool {
    match sniff(b) {
        Some(Fmt::Jpeg) => matches!(b[3], 0xDB | 0xE0..=0xEF),
        Some(Fmt::Iso) => (16..=256).contains(&be(b, 0, 4)),
        Some(Fmt::Id3) => (2..=4).contains(&b[3]) && b[6..10].iter().all(|&c| c < 0x80),
        Some(Fmt::Mpa | Fmt::Bmp | Fmt::QtOld | Fmt::Ts | Fmt::Ps | Fmt::Tiff) | None => false,
        Some(_) => true,
    }
}

/// Where the next file starts, looking at sector boundaries from `from` for strong signatures
/// (and, where `zeros` says it marks the end, a megabyte of zeros), at most `max` bytes on: the end
/// of a file whose own structure doesn't say.
fn until_next(s: &Src, from: u64, max: u64, zeros: bool) -> io::Result<u64> {
    let limit = from.saturating_add(max).min(s.end);
    let mut at = s.align_up(from);
    let mut st = Stream::new(*s, at);
    let (mut run, mut run_at) = (0u64, 0u64);
    while at < limit {
        st.seek(at);
        let b = st.peek(64.max(s.sector as usize))?;
        if b.is_empty() {
            break;
        }
        if strong(b) {
            return Ok(at);
        }
        if zeros {
            let z = &b[..b.len().min(s.sector as usize)];
            if z.iter().all(|&c| c == 0) {
                if run == 0 {
                    run_at = at;
                }
                run += s.sector;
                if run >= MIB {
                    return Ok(run_at);
                }
            } else {
                run = 0;
            }
        }
        at += s.sector;
    }
    Ok(limit)
}

// ---------- following a file to its end

/// A file a parser found at some offset.
#[derive(Debug, Default)]
struct Hit {
    ext: &'static str,
    size: u64,
    complete: bool,
    taken: Option<i64>,
    camera: Option<String>,
    dims: Option<(u32, u32)>,
    note: Option<String>,
}

impl Hit {
    fn new(ext: &'static str, size: u64) -> Hit {
        Hit {
            ext,
            size,
            complete: true,
            ..Hit::default()
        }
    }

    /// Cut off by the end of the scanned area: everything from `off` on is kept.
    fn cut(mut self, s: &Src, off: u64) -> Hit {
        self.size = s.end.saturating_sub(off);
        self.complete = false;
        self.note = Some(CUT.into());
        self
    }

    /// Incomplete: kept from `off` up to `end`, for the reason given.
    fn partial(mut self, off: u64, end: u64, why: impl Into<String>) -> Hit {
        self.size = end.saturating_sub(off);
        self.complete = false;
        self.note = Some(why.into());
        self
    }

    fn found(self, offset: u64) -> Found {
        let (kind, group) = describe(self.ext);
        Found {
            offset,
            size: self.size,
            ext: self.ext,
            kind,
            group,
            complete: self.complete,
            taken: self.taken,
            camera: self.camera,
            width: self.dims.map(|d| d.0),
            height: self.dims.map(|d| d.1),
            note: self.note,
        }
    }
}

fn parse(fmt: Fmt, s: &Src, off: u64) -> io::Result<Option<Hit>> {
    match fmt {
        Fmt::Jpeg => jpeg(s, off),
        Fmt::Png => png(s, off),
        Fmt::Gif => gif(s, off),
        Fmt::Bmp => bmp(s, off),
        Fmt::Riff => riff(s, off),
        Fmt::Iso => iso(s, off, false),
        Fmt::QtOld => iso(s, off, true),
        Fmt::Pdf => pdf(s, off),
        Fmt::Zip => zip(s, off),
        Fmt::Cfb => cfb(s, off),
        Fmt::Sqlite => sqlite(s, off),
        Fmt::Id3 => id3(s, off),
        Fmt::Mpa => mpeg(s, off),
        Fmt::Flac => flac(s, off),
        Fmt::Ogg => ogg(s, off),
        Fmt::Ebml => ebml(s, off),
        Fmt::Rar => rar(s, off),
        Fmt::SevenZ => sevenz(s, off),
        Fmt::Tiff => tiff(s, off),
        Fmt::Raf => raf(s, off),
        Fmt::Aiff => aiff(s, off),
        Fmt::Asf => asf(s, off),
        Fmt::Ts => ts(s, off),
        Fmt::Ps => ps(s, off),
    }
}

// ---------- checksums

const fn crc32_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

const fn ogg_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u32) << 24;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04C1_1DB7
            } else {
                c << 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

const fn crc16_table() -> [u16; 256] {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u16) << 8;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000 != 0 {
                (c << 1) ^ 0x8005
            } else {
                c << 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static CRC32: [u32; 256] = crc32_table();
static OGG: [u32; 256] = ogg_table();
static CRC16: [u16; 256] = crc16_table();

/// CRC-32 (zip, PNG), continuing from `crc`.
fn crc32_update(crc: u32, b: &[u8]) -> u32 {
    let mut c = !crc;
    for &x in b {
        c = CRC32[((c ^ u32::from(x)) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

fn crc32(b: &[u8]) -> u32 {
    crc32_update(0, b)
}

fn ogg_crc(mut c: u32, b: &[u8]) -> u32 {
    for &x in b {
        c = (c << 8) ^ OGG[usize::from((c >> 24) as u8 ^ x)];
    }
    c
}

fn crc8(b: &[u8]) -> u8 {
    let mut c = 0u8;
    for &x in b {
        c ^= x;
        for _ in 0..8 {
            c = if c & 0x80 != 0 {
                (c << 1) ^ 0x07
            } else {
                c << 1
            };
        }
    }
    c
}

#[cfg(test)]
mod tests;
