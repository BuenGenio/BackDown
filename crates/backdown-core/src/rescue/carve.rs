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
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::size_of;
use crate::fmt;
use crate::magic::{be, field, has, le, utf16};

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
        File::open(src).with_context(|| format!("can't open {} to read it", src.display()))?;
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

// ---------- JPEG

const JPEG_MAX: u64 = 256 * MIB;

/// JPEG: the segments up to the first scan, then the compressed data (which never holds an FF
/// byte except as FF 00, a restart marker FF D0–D7, or before the next marker), scan after scan
/// for progressive JPEGs, up to the end-of-image marker. Then what cameras put after it: the
/// larger preview listed in an MPF index, or a motion photo's video.
fn jpeg(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 4)?;
    if h.len() < 4 || !matches!(h[3], 0xC0..=0xC7 | 0xC9..=0xCF | 0xDB | 0xDD | 0xE0..=0xEF | 0xFE)
    {
        return Ok(None);
    }
    let cap = off + JPEG_MAX;
    let mut st = Stream::new(*s, off + 2);
    let (mut sof, mut dqt, mut scans) = (false, false, 0u32);
    let mut hit = Hit::new("jpg", 0);
    let mut mpf: Option<(u64, Vec<u8>)> = None;
    // where it stops making sense: worth keeping only once there's image data
    let broken = |hit: Hit, at: u64, scans: u32, why: String| -> Option<Hit> {
        (scans > 0).then(|| hit.partial(off, at, why))
    };
    let eoi = loop {
        let at = st.offset();
        if at >= cap {
            return Ok(broken(hit, at, scans, capped(JPEG_MAX)));
        }
        let b = st.peek(4)?;
        if b.len() < 2 {
            return Ok((scans > 0).then(|| hit.cut(s, off)));
        }
        if b[0] != 0xFF {
            return Ok(broken(hit, at, scans, DAMAGED.into()));
        }
        let m = b[1];
        match m {
            0xFF => {
                st.advance(1); // fill byte
                continue;
            }
            0xD9 if scans > 0 => break at + 2,
            0xD0..=0xD7 | 0x01 if scans > 0 => {
                st.advance(2);
                continue;
            }
            0xC0..=0xCF | 0xDA..=0xFE => {}
            _ => return Ok(broken(hit, at, scans, DAMAGED.into())),
        }
        if b.len() < 4 {
            return Ok((scans > 0).then(|| hit.cut(s, off)));
        }
        let len = be(b, 2, 2);
        if len < 2 {
            return Ok(broken(hit, at, scans, DAMAGED.into()));
        }
        let seg = st.peek(2 + len as usize)?;
        if seg.len() < 2 + len as usize {
            return Ok((scans > 0).then(|| hit.cut(s, off)));
        }
        let p = &seg[4..];
        match m {
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                let nc = u64::from(p.get(5).copied().unwrap_or(0));
                if p.len() < 6
                    || !matches!(p[0], 8 | 12 | 16)
                    || !(1..=4).contains(&nc)
                    || len != 8 + 3 * nc
                    || be(p, 3, 2) == 0
                {
                    return Ok(broken(hit, at, scans, DAMAGED.into()));
                }
                if !sof {
                    hit.dims = Some((be(p, 3, 2) as u32, be(p, 1, 2) as u32));
                }
                sof = true;
            }
            0xDB => dqt = true,
            0xE1 if scans == 0 && hit.camera.is_none() && p.starts_with(b"Exif\0\0") => {
                let t = &p[6..];
                if let Some(i) = tiff_walk(&Src::mem(t), 0, t.len() as u64, false)? {
                    hit.taken = i.taken;
                    hit.camera = camera(i.make, i.model);
                }
            }
            0xE2 if mpf.is_none() && p.starts_with(b"MPF\0") => {
                mpf = Some((at + 8, p[4..].to_vec()));
            }
            0xDA => {
                let ns = u64::from(p.first().copied().unwrap_or(0));
                if !sof || !dqt || !(1..=4).contains(&ns) || len != 6 + 2 * ns {
                    return Ok(broken(hit, at, scans, DAMAGED.into()));
                }
                st.advance(2 + len);
                scans += 1;
                if !skip_entropy(&mut st, cap)? {
                    let at = st.offset();
                    return Ok(Some(if at >= cap {
                        hit.partial(off, at, capped(JPEG_MAX))
                    } else {
                        hit.cut(s, off)
                    }));
                }
                continue;
            }
            _ => {}
        }
        st.advance(2 + len);
    };
    let mut end = eoi;
    if let Some((base, t)) = mpf
        && let Some(e) = mpf_end(s, base, &t)?
        && e > end
        && e - off <= JPEG_MAX
    {
        end = e;
    }
    // Google's motion photos: a short video right after the photo
    if has(&s.get(end, 8)?, 4, b"ftyp") {
        let w = iso_walk(s, end, JPEG_MAX)?;
        if w.moov.is_some() && w.mdat && !w.cut && w.open.is_none() {
            end = w.end;
            hit.note = Some("a motion photo: the photo with its short video".into());
        }
    }
    hit.size = end - off;
    Ok(Some(hit))
}

/// Skip compressed image data, leaving the stream on the next marker's FF. False when the data ran
/// out first (the end of the scanned area, or `cap`).
fn skip_entropy(st: &mut Stream, cap: u64) -> io::Result<bool> {
    loop {
        if st.offset() >= cap {
            return Ok(false);
        }
        let w = st.peek(2)?;
        if w.len() < 2 {
            let n = w.len() as u64;
            st.advance(n);
            return Ok(false);
        }
        match w[..w.len() - 1].iter().position(|&c| c == 0xFF) {
            None => {
                let n = (w.len() - 1) as u64;
                st.advance(n);
            }
            Some(i) => match w[i + 1] {
                0x00 | 0xD0..=0xD7 => st.advance((i + 2) as u64),
                0xFF => st.advance((i + 1) as u64),
                _ => {
                    st.advance(i as u64);
                    return Ok(true);
                }
            },
        }
    }
}

/// Where the images listed in a JPEG's MPF index end (the photo itself is the one at offset 0;
/// the others follow it). `base` is where the index's TIFF header is in the source.
fn mpf_end(s: &Src, base: u64, t: &[u8]) -> io::Result<Option<u64>> {
    let e = match t.get(..4) {
        Some(b"II*\0") => End(true),
        Some(b"MM\0*") => End(false),
        _ => return Ok(None),
    };
    let ifd = e.u(t, 4, 4) as usize;
    let n = e.u(t, ifd, 2) as usize;
    let mut end = None;
    for i in 0..n.min(64) {
        let en = ifd + 2 + 12 * i;
        if e.u(t, en, 2) != 0xB002 {
            continue;
        }
        let (count, at) = (e.u(t, en + 4, 4) as usize, e.u(t, en + 8, 4) as usize);
        let Some(list) = at.checked_add(count).and_then(|x| t.get(at..x)) else {
            return Ok(None);
        };
        for x in list.as_chunks::<16>().0 {
            let (size, o) = (e.u(x, 4, 4), e.u(x, 8, 4));
            if o == 0 {
                continue;
            }
            if !has(&s.get(base + o, 2)?, 0, &[0xFF, 0xD8]) {
                return Ok(None);
            }
            end = end.max(Some(base + o + size));
        }
    }
    Ok(end)
}

// ---------- TIFF and EXIF

#[derive(Clone, Copy)]
struct End(bool);

impl End {
    fn u(self, b: &[u8], at: usize, n: usize) -> u64 {
        if self.0 { le(b, at, n) } else { be(b, at, n) }
    }
}

#[derive(Debug, Default)]
struct TiffInfo {
    /// how far its structure reaches from the header
    size: u64,
    make: Option<String>,
    model: Option<String>,
    taken: Option<i64>,
    dng: bool,
    dims: Option<(u32, u32)>,
    /// image data: strips, tiles or a JPEG
    image: bool,
    /// image data whose length isn't recorded
    open: bool,
}

const MAX_IFDS: usize = 64;
const MAX_VALUES: u64 = 1 << 16;

/// Walk a TIFF structure at `base` (a TIFF or camera RAW file, or a JPEG's EXIF block): every IFD
/// it links to, and every block of data they point at, `limit` bytes at most. `strict` (for a file
/// found by its signature alone) wants the first IFD to look right: known types, tags in order.
fn tiff_walk(s: &Src, base: u64, limit: u64, strict: bool) -> io::Result<Option<TiffInfo>> {
    let h = s.get(base, 8)?;
    if h.len() < 8 {
        return Ok(None);
    }
    let e = match &h[..2] {
        b"II" => End(true),
        b"MM" => End(false),
        _ => return Ok(None),
    };
    let mut info = TiffInfo {
        size: 8,
        ..TiffInfo::default()
    };
    let (mut original, mut digitized, mut modified) = (None, None, None);
    let mut todo = vec![(e.u(&h, 4, 4), true)];
    let mut seen: Vec<u64> = Vec::new();
    while let Some((o, first)) = todo.pop() {
        if o < 8 || o >= limit || seen.contains(&o) || seen.len() >= MAX_IFDS {
            if first {
                return Ok(None);
            }
            continue;
        }
        seen.push(o);
        let nb = s.get(base + o, 2)?;
        let n = e.u(&nb, 0, 2);
        let len = 12 * n as usize + 4;
        let raw = s.get(base + o + 2, len)?;
        if nb.len() < 2 || n == 0 || n > 1000 || raw.len() < len {
            if first {
                return Ok(None);
            }
            continue;
        }
        info.size = info.size.max(o + 2 + len as u64);
        let (mut bad, mut unsorted, mut prev) = (0u64, 0u64, 0u64);
        let (mut strips, mut counts) = (Vec::new(), Vec::new());
        let (mut jpeg_at, mut jpeg_len) = (None, None);
        for en in raw[..len - 4].as_chunks::<12>().0 {
            let (tag, typ, count) = (e.u(en, 0, 2), e.u(en, 2, 2), e.u(en, 4, 4));
            let unit = match typ {
                1 | 2 | 6 | 7 => 1,
                3 | 8 => 2,
                4 | 9 | 11 | 13 => 4,
                5 | 10 | 12 => 8,
                _ => {
                    bad += 1;
                    continue;
                }
            };
            if tag < prev {
                unsorted += 1;
            }
            prev = tag;
            let bytes = count.saturating_mul(unit);
            let at = if bytes > 4 {
                let v = e.u(en, 8, 4);
                if v.saturating_add(bytes) > limit {
                    bad += 1;
                    continue;
                }
                info.size = info.size.max(v + bytes);
                Some(v)
            } else {
                None
            };
            let first_value = || -> io::Result<Option<u64>> {
                Ok(tiff_values(s, base, e, typ, count.min(1), en, at)?
                    .first()
                    .copied())
            };
            match tag {
                0x010F => info.make = tiff_text(s, base, en, at, count)?,
                0x0110 => info.model = tiff_text(s, base, en, at, count)?,
                0x0132 => modified = tiff_text(s, base, en, at, count)?,
                0x9003 => original = tiff_text(s, base, en, at, count)?,
                0x9004 => digitized = tiff_text(s, base, en, at, count)?,
                0x0100 if first && info.dims.is_none() => {
                    if let Some(w) = first_value()? {
                        info.dims = Some((w as u32, 0));
                    }
                }
                0x0101 if first => {
                    if let (Some(h), Some(d)) = (first_value()?, info.dims.as_mut()) {
                        d.1 = h as u32;
                    }
                }
                0x0111 | 0x0144 => strips = tiff_values(s, base, e, typ, count, en, at)?,
                0x0117 | 0x0145 => counts = tiff_values(s, base, e, typ, count, en, at)?,
                0x0201 => jpeg_at = first_value()?,
                0x0202 => jpeg_len = first_value()?,
                0x014A => {
                    for v in tiff_values(s, base, e, typ, count, en, at)? {
                        todo.push((v, false));
                    }
                }
                0x8769 | 0x8825 | 0xA005 => {
                    if let Some(v) = first_value()? {
                        todo.push((v, false));
                    }
                }
                0xC612 => info.dng = true,
                _ => {}
            }
        }
        if strict && first && (bad * 2 > n || unsorted * 4 > n) {
            return Ok(None);
        }
        for (i, &so) in strips.iter().enumerate() {
            match counts.get(i) {
                Some(&c) if c > 0 && so.saturating_add(c) <= limit => {
                    info.size = info.size.max(so + c);
                    info.image = true;
                }
                _ if so < limit => {
                    info.size = info.size.max(so);
                    info.image = true;
                    info.open = true;
                }
                _ => {}
            }
        }
        if let (Some(a), Some(l)) = (jpeg_at, jpeg_len)
            && l > 0
            && a.saturating_add(l) <= limit
        {
            info.size = info.size.max(a + l);
            info.image = true;
        }
        let next = e.u(&raw, len - 4, 4);
        if next != 0 {
            todo.push((next, false));
        }
    }
    if info.dims.is_some_and(|d| d.0 == 0 || d.1 == 0) {
        info.dims = None;
    }
    info.taken = original
        .or(digitized)
        .or(modified)
        .as_deref()
        .and_then(exif_time);
    Ok(Some(info))
}

/// A TIFF entry's numbers (bytes, shorts, longs).
fn tiff_values(
    s: &Src,
    base: u64,
    e: End,
    typ: u64,
    count: u64,
    en: &[u8],
    at: Option<u64>,
) -> io::Result<Vec<u64>> {
    let unit = match typ {
        1 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 13 => 4,
        _ => return Ok(Vec::new()),
    };
    let n = count.min(MAX_VALUES) as usize;
    let data: Cow<[u8]> = match at {
        None => Cow::Borrowed(&en[8..12]),
        Some(v) => s.get(base + v, n * unit)?,
    };
    Ok((0..n)
        .take_while(|i| (i + 1) * unit <= data.len())
        .map(|i| e.u(&data, i * unit, unit))
        .collect())
}

fn tiff_text(
    s: &Src,
    base: u64,
    en: &[u8],
    at: Option<u64>,
    count: u64,
) -> io::Result<Option<String>> {
    let n = count.min(256) as usize;
    let d: Cow<[u8]> = match at {
        None => Cow::Borrowed(&en[8..8 + n.min(4)]),
        Some(v) => s.get(base + v, n)?,
    };
    let t = field(&d, 0, d.len());
    Ok((!t.is_empty()).then_some(t))
}

/// "2019:08:14 10:22:33" as Unix seconds (the camera's clock, read as UTC).
fn exif_time(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        std::str::from_utf8(&b[r]).ok()?.trim().parse().ok()
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1970..=2100).contains(&y)
        || !(1..=12).contains(&mo)
        || !(1..=31).contains(&d)
        || !(0..24).contains(&h)
        || !(0..60).contains(&mi)
        || !(0..=60).contains(&se)
    {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se)
}

/// Days since 1970-01-01 of a calendar date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "Canon EOS 80D", "Apple iPhone 12": the model, with the brand in front when it isn't already.
fn camera(make: Option<String>, model: Option<String>) -> Option<String> {
    match (make, model) {
        (Some(make), Some(model)) => {
            let brand = make.split_whitespace().next().unwrap_or_default();
            if model.to_lowercase().starts_with(&brand.to_lowercase()) {
                Some(model)
            } else {
                Some(format!("{brand} {model}"))
            }
        }
        (make, model) => model.or(make),
    }
}

const RAW_MAX: u64 = GIB;
const OPEN_MAX: u64 = 4 * GIB;

/// TIFF and the camera RAW formats built on it: the furthest byte any IFD points at.
fn tiff(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 16)?;
    let magic = h.get(..4).unwrap_or_default();
    let orf = matches!(magic, b"IIRO" | b"IIRS" | b"MMOR");
    let rw2 = magic == b"IIU\0";
    let Some(t) = tiff_walk(s, off, RAW_MAX, true)? else {
        return Ok(None);
    };
    if !t.image {
        return Ok(None);
    }
    let make = t.make.as_deref().unwrap_or_default().to_ascii_uppercase();
    let ext = if has(&h, 8, b"CR\x02\0") {
        "cr2"
    } else if orf {
        "orf"
    } else if rw2 {
        "rw2"
    } else if t.dng {
        "dng"
    } else if make.starts_with("NIKON") {
        "nef"
    } else if make.starts_with("SONY") {
        "arw"
    } else if make.starts_with("PENTAX") || make.starts_with("RICOH") {
        "pef"
    } else if make.starts_with("SAMSUNG") {
        "srw"
    } else {
        "tif"
    };
    let mut hit = Hit::new(ext, t.size);
    hit.taken = t.taken;
    hit.dims = t.dims;
    hit.camera = camera(t.make, t.model);
    if t.open {
        let e = until_next(s, off + t.size, OPEN_MAX, true)?;
        return Ok(Some(hit.partial(off, e, NO_END)));
    }
    Ok(Some(hit))
}

const RAF_MAX: u64 = 512 * MIB;

/// Fujifilm RAF: its header lists where the preview JPEG and the raw data are, and how long.
fn raf(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 108)?;
    if h.len() < 108 {
        return Ok(None);
    }
    let mut end = 0;
    for at in [84, 92, 100] {
        let (o, l) = (be(&h, at, 4), be(&h, at + 4, 4));
        if o == 0 && l == 0 {
            continue;
        }
        if o < 108 {
            return Ok(None);
        }
        end = end.max(o + l);
    }
    if end == 0 || end > RAF_MAX || !has(&s.get(off + be(&h, 84, 4), 3)?, 0, &[0xFF, 0xD8, 0xFF]) {
        return Ok(None);
    }
    let mut hit = Hit::new("raf", end);
    let model = field(&h, 28, 32);
    hit.camera = (!model.is_empty()).then(|| format!("Fujifilm {model}"));
    Ok(Some(hit))
}

// ---------- PNG, GIF, BMP

const PNG_MAX: u64 = 512 * MIB;

/// PNG: chunk after chunk, each with its CRC checked, up to IEND.
fn png(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let mut st = Stream::new(*s, off + 8);
    let mut hit = Hit::new("png", 0);
    let (mut first, mut idat) = (true, false);
    let mut good = off + 8;
    loop {
        let at = st.offset();
        if at - off > PNG_MAX {
            return Ok(idat.then(|| hit.partial(off, good, capped(PNG_MAX))));
        }
        let b = st.peek(8 + 13)?;
        if b.len() < 8 {
            return Ok((!first).then(|| hit.cut(s, off)));
        }
        let len = be(b, 0, 4);
        let typ = [b[4], b[5], b[6], b[7]];
        let broken = len > 0x7FFF_FFFF
            || !typ.iter().all(u8::is_ascii_alphabetic)
            || (first && (&typ != b"IHDR" || len != 13));
        if broken {
            return Ok(idat.then(|| hit.partial(off, good, DAMAGED)));
        }
        if first {
            if b.len() < 21 {
                return Ok(None);
            }
            let d = &b[8..21];
            if !matches!(d[8], 1 | 2 | 4 | 8 | 16)
                || !matches!(d[9], 0 | 2 | 3 | 4 | 6)
                || d[10] != 0
                || d[11] != 0
                || d[12] > 1
            {
                return Ok(None);
            }
            hit.dims = Some((be(d, 0, 4) as u32, be(d, 4, 4) as u32));
        }
        let mut crc = crc32_update(0, &typ);
        st.advance(8);
        let mut left = len;
        while left > 0 {
            let w = st.peek(1)?;
            if w.is_empty() {
                return Ok((!first).then(|| hit.cut(s, off)));
            }
            let k = (w.len() as u64).min(left) as usize;
            crc = crc32_update(crc, &w[..k]);
            st.advance(k as u64);
            left -= k as u64;
        }
        let c = st.peek(4)?;
        if c.len() < 4 {
            return Ok((!first).then(|| hit.cut(s, off)));
        }
        if be(c, 0, 4) as u32 != crc {
            return Ok(idat.then(|| hit.partial(off, good, DAMAGED)));
        }
        st.advance(4);
        good = st.offset();
        first = false;
        match &typ {
            b"IDAT" => idat = true,
            b"IEND" => {
                hit.size = good - off;
                return Ok(Some(hit));
            }
            _ => {}
        }
    }
}

const GIF_MAX: u64 = 128 * MIB;

/// GIF: the header, then blocks (extensions and images, each a run of sub-blocks) up to the
/// trailer byte.
fn gif(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 13)?;
    if h.len() < 13 {
        return Ok(None);
    }
    let (w, ht) = (le(&h, 6, 2), le(&h, 8, 2));
    if w == 0 || ht == 0 {
        return Ok(None);
    }
    let flags = h[10];
    let gct = if flags & 0x80 != 0 {
        3 << ((flags & 7) + 1)
    } else {
        0
    };
    let mut hit = Hit::new("gif", 0);
    hit.dims = Some((w as u32, ht as u32));
    let mut st = Stream::new(*s, off + 13 + gct);
    let mut images = 0;
    loop {
        let at = st.offset();
        if at - off > GIF_MAX {
            return Ok((images > 0).then(|| hit.partial(off, at, capped(GIF_MAX))));
        }
        let b = st.peek(10)?;
        let Some(&c) = b.first() else {
            return Ok(Some(hit.cut(s, off)));
        };
        match c {
            0x3B => {
                hit.size = at + 1 - off;
                return Ok((images > 0).then_some(hit));
            }
            0x21 => {
                st.advance(2);
            }
            0x2C => {
                if b.len() < 10 {
                    return Ok(Some(hit.cut(s, off)));
                }
                let f = b[9];
                let lct = if f & 0x80 != 0 { 3 << ((f & 7) + 1) } else { 0 };
                st.advance(10 + lct);
                let m = st.peek(1)?;
                let Some(&bits) = m.first() else {
                    return Ok(Some(hit.cut(s, off)));
                };
                if !(1..=11).contains(&bits) {
                    return Ok((images > 0).then(|| hit.partial(off, at, DAMAGED)));
                }
                st.advance(1);
                images += 1;
            }
            _ => return Ok((images > 0).then(|| hit.partial(off, at, DAMAGED))),
        }
        // sub-blocks: a length byte and that many bytes, up to a zero length
        loop {
            let b = st.peek(1)?;
            let Some(&n) = b.first() else {
                return Ok(Some(hit.cut(s, off)));
            };
            st.advance(1 + u64::from(n));
            if n == 0 {
                break;
            }
        }
    }
}

const BMP_MAX: u64 = 512 * MIB;

/// BMP: the size in its header, checked against the image's dimensions.
fn bmp(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 64)?;
    if h.len() < 30 || le(&h, 6, 4) != 0 {
        return Ok(None);
    }
    let (size, data, dib) = (le(&h, 2, 4), le(&h, 10, 4), le(&h, 14, 4));
    let (w, ht, planes, bpp, comp) = match dib {
        12 => (
            le(&h, 18, 2) as i64,
            le(&h, 20, 2) as i64,
            le(&h, 22, 2),
            le(&h, 24, 2),
            0,
        ),
        40 | 52 | 56 | 64 | 108 | 124 if h.len() >= 34 => (
            i64::from(le(&h, 18, 4) as u32 as i32),
            i64::from(le(&h, 22, 4) as u32 as i32),
            le(&h, 26, 2),
            le(&h, 28, 2),
            le(&h, 30, 4),
        ),
        _ => return Ok(None),
    };
    if planes != 1
        || !matches!(bpp, 1 | 2 | 4 | 8 | 16 | 24 | 32 | 64)
        || comp > 13
        || !(1..=65_536).contains(&w)
        || ht == 0
        || ht.abs() > 65_536
        || data < 14 + dib
        || data > size
        || size > BMP_MAX
    {
        return Ok(None);
    }
    let raw = (w as u64 * bpp).div_ceil(32) * 4 * ht.unsigned_abs();
    if matches!(comp, 0 | 3 | 6) {
        if size < data + raw || size > data + raw + MIB {
            return Ok(None);
        }
    } else if size <= data {
        return Ok(None);
    }
    let mut hit = Hit::new("bmp", size);
    hit.dims = Some((w as u32, ht.unsigned_abs() as u32));
    Ok(Some(hit))
}

// ---------- RIFF (WebP, WAV, AVI), AIFF

const RIFF_MAX: u64 = 4 * GIB + 8;

/// RIFF: the size in its header, checked by walking the chunks inside, which have to add up to it.
/// AVIs over a gigabyte go on in "AVIX" extension RIFFs right after.
fn riff(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 64)?;
    if h.len() < 32 {
        return Ok(None);
    }
    let rf64 = has(&h, 0, b"RF64");
    let ext = match &h[8..12] {
        b"WEBP" if !rf64 => "webp",
        b"WAVE" => "wav",
        b"AVI " if !rf64 => "avi",
        _ => return Ok(None),
    };
    let first = &h[12..16];
    let first_ok = match ext {
        "webp" => matches!(first, b"VP8 " | b"VP8L" | b"VP8X"),
        "avi" => first == b"LIST",
        _ => first.iter().all(|c| c.is_ascii_graphic() || *c == b' '),
    };
    if !first_ok {
        return Ok(None);
    }
    let mut hit = Hit::new(ext, 0);
    if ext == "webp" {
        hit.dims = match first {
            b"VP8X" => Some(((le(&h, 24, 3) + 1) as u32, (le(&h, 27, 3) + 1) as u32)),
            b"VP8L" if h[20] == 0x2F => {
                let v = le(&h, 21, 4);
                Some((((v & 0x3FFF) + 1) as u32, (((v >> 14) & 0x3FFF) + 1) as u32))
            }
            b"VP8 " if has(&h, 23, &[0x9D, 0x01, 0x2A]) => Some((
                (le(&h, 26, 2) & 0x3FFF) as u32,
                (le(&h, 28, 2) & 0x3FFF) as u32,
            )),
            _ => None,
        };
    }
    if rf64 {
        // RF64: the real sizes are in a "ds64" chunk (a WAV over 4 GB)
        if !has(&h, 12, b"ds64") {
            return Ok(None);
        }
        hit.size = le(&h, 20, 8).saturating_add(8);
        return Ok((hit.size <= ISO_MAX).then_some(hit));
    }
    let declared = le(&h, 4, 4);
    if matches!(declared, 0 | 0xFFFF_FFFF) && ext != "webp" {
        // a recorder that stopped before writing the sizes (a flat battery, a pulled card)
        let e = until_next(s, off + 12, OPEN_MAX, true)?;
        return Ok(Some(hit.partial(off, e, NO_SIZES)));
    }
    let size = declared + 8;
    if !(20..=RIFF_MAX).contains(&size) {
        return Ok(None);
    }
    let walked = riff_walk(s, off + 12, off + size)?;
    let mut end = off + size;
    if walked != end && walked != end + 1 {
        if walked > s.end {
            return Ok(Some(hit.cut(s, off)));
        }
        // a recorder that never wrote the final size, or chunks that don't add up
        return Ok(Some(hit.partial(
            off,
            walked.max(off + 12).min(end),
            DAMAGED,
        )));
    }
    end = walked;
    if ext == "avi" {
        loop {
            let x = s.get(end, 12)?;
            if !(has(&x, 0, b"RIFF") && has(&x, 8, b"AVIX")) {
                break;
            }
            end += le(&x, 4, 4) + 8;
        }
    }
    hit.size = end - off;
    Ok(Some(hit))
}

/// Walk RIFF chunks from `pos` towards `end`: where the walk stops (at `end` when they add up).
fn riff_walk(s: &Src, mut pos: u64, end: u64) -> io::Result<u64> {
    let mut n = 0;
    while pos + 8 <= end && n < 100_000 {
        let b = s.get(pos, 8)?;
        if b.len() < 8 {
            return Ok(pos.max(s.end + 1)); // runs past the scanned area
        }
        if !b[..4].iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
            break;
        }
        let size = le(&b, 4, 4);
        pos += 8 + size + (size & 1);
        n += 1;
    }
    Ok(pos)
}

/// AIFF: a big-endian IFF "FORM" whose size covers the file.
fn aiff(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 16)?;
    if h.len() < 16
        || !matches!(&h[8..12], b"AIFF" | b"AIFC")
        || !matches!(&h[12..16], b"COMM" | b"FVER")
    {
        return Ok(None);
    }
    let size = be(&h, 4, 4) + 8;
    if size < 20 {
        return Ok(None);
    }
    Ok(Some(Hit::new("aif", size)))
}

// ---------- ISO base media (MP4, MOV, HEIC, M4A, 3GP, CR3)

const ISO_MAX: u64 = 64 * GIB;
/// What can be at the top level of an ISO media file.
const ISO_TOP: &[&[u8]] = &[
    b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"uuid", b"meta", b"pnot", b"PICT",
    b"pdin", b"moof", b"mfra", b"styp", b"sidx", b"ssix", b"prft", b"emsg", b"udta", b"junk",
];

#[derive(Debug, Default)]
struct IsoWalk {
    /// where the last whole box ends
    end: u64,
    moov: Option<u64>,
    mdat: bool,
    meta: bool,
    moof: bool,
    /// a box runs past the end of the scanned area
    cut: bool,
    /// where the data of a box "running to the end of the file" starts (its size is 0)
    open: Option<u64>,
    /// larger than `max`
    big: bool,
}

/// Walk top-level boxes from `off` while they're plausible: the file ends after the last one.
fn iso_walk(s: &Src, off: u64, max: u64) -> io::Result<IsoWalk> {
    let mut w = IsoWalk {
        end: off,
        ..IsoWalk::default()
    };
    let mut pos = off;
    while pos < s.end {
        let h = s.get(pos, 16)?;
        if h.len() < 8 {
            break;
        }
        let typ = &h[4..8];
        if !ISO_TOP.contains(&typ) {
            break;
        }
        // the next file, starting right after this one
        if pos > off
            && (typ == b"ftyp"
                || (w.moov.is_some() && w.mdat && matches!(typ, b"wide" | b"moov" | b"pnot")))
        {
            break;
        }
        let (mut size, mut hdr) = (be(&h, 0, 4), 8);
        if size == 1 {
            if h.len() < 16 {
                break;
            }
            size = be(&h, 8, 8);
            hdr = 16;
            if size < 16 {
                break;
            }
        } else if size != 0 && size < 8 {
            break;
        }
        match typ {
            b"moov" => {
                if w.moov.is_some() {
                    break;
                }
                w.moov = Some(pos);
            }
            b"mdat" => w.mdat = true,
            b"meta" => w.meta = true,
            b"moof" => w.moof = true,
            _ => {}
        }
        if size == 0 {
            w.open = Some(pos + hdr);
            break;
        }
        if (pos - off).saturating_add(size) > max {
            w.big = true;
            break;
        }
        pos += size;
        if pos > s.end {
            w.cut = true;
            break;
        }
        w.end = pos;
    }
    Ok(w)
}

/// The extension an ISO media file's brands say it has.
fn iso_ext(major: &[u8], compat: &[u8]) -> &'static str {
    let brand = |x: &[u8]| compat.as_chunks::<4>().0.iter().any(|c| c == x);
    match major {
        b"qt  " => "mov",
        b"crx " => "cr3",
        b"avif" | b"avis" => "avif",
        b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" => "heic",
        b"mif1" | b"msf1" if brand(b"avif") => "avif",
        b"mif1" | b"msf1" => "heic",
        b"M4A " | b"M4B " | b"M4P " => "m4a",
        b"M4V " | b"M4VH" | b"M4VP" => "m4v",
        m if m.starts_with(b"3g") => "3gp",
        _ => "mp4",
    }
}

/// ISO media: `ftyp` (whose brands say what it is), then the top-level boxes. Old QuickTime movies
/// have no `ftyp` and start straight with their boxes (`old`).
fn iso(s: &Src, off: u64, old: bool) -> io::Result<Option<Hit>> {
    let h = s.get(off, 256)?;
    if h.len() < 16 {
        return Ok(None);
    }
    let ext = if old {
        "mov"
    } else {
        let n = be(&h, 0, 4) as usize;
        if !(16..=256).contains(&n)
            || !n.is_multiple_of(4)
            || n > h.len()
            || !h[8..12]
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || *c == b' ')
        {
            return Ok(None);
        }
        iso_ext(&h[8..12], &h[16..n])
    };
    let w = iso_walk(s, off, ISO_MAX)?;
    if w.big {
        return Ok(None);
    }
    let mut hit = Hit::new(ext, w.end - off);
    if let Some(m) = w.moov {
        let b = s.get(m, 40)?;
        let hdr = if be(&b, 0, 4) == 1 { 16 } else { 8 };
        let child = b.get(hdr + 4..hdr + 8).unwrap_or_default();
        if child.len() == 4
            && !child
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || *c == b' ')
        {
            return Ok(None); // not a real movie header
        }
        hit.taken = mvhd_time(&b, hdr);
    }
    let whole = match ext {
        "heic" | "avif" => w.meta && w.mdat,
        _ => (w.moov.is_some() || w.moof) && w.mdat,
    };
    if old && !(w.mdat && w.moov.is_some()) {
        return Ok(None); // a weaker signature: only a whole movie counts
    }
    if w.cut {
        return Ok(Some(hit.cut(s, off)));
    }
    if let Some(data) = w.open {
        let e = until_next(s, data, OPEN_MAX, true)?;
        return Ok(Some(hit.partial(off, e, NO_END)));
    }
    if !whole {
        return Ok((w.mdat && !matches!(ext, "heic" | "avif"))
            .then(|| hit.partial(off, w.end, UNFINISHED)));
    }
    Ok(Some(hit))
}

/// When a movie was made, from its `mvhd` box (seconds since 1904).
fn mvhd_time(b: &[u8], hdr: usize) -> Option<i64> {
    if !has(b, hdr + 4, b"mvhd") {
        return None;
    }
    let t = if *b.get(hdr + 8)? == 1 {
        be(b, hdr + 12, 8)
    } else {
        be(b, hdr + 12, 4)
    };
    let unix = t as i64 - 2_082_844_800;
    // 1990 to 2100: a clock that was never set says 1904 or 1970
    (631_152_000..4_102_444_800).contains(&unix).then_some(unix)
}

// ---------- PDF

const PDF_MAX: u64 = GIB;
/// With no end found yet, give up this far past another file's signature.
const PDF_PAST_HEADER: u64 = 64 * MIB;
/// Every PDF object ends with `endobj`: this far without one, it's not PDF any more.
const PDF_GAP: u64 = 64 * MIB;

/// PDF: the last `%%EOF` whose `startxref` points back at a cross-reference table of this very
/// file (so another PDF's end isn't taken for this one's). One followed by more PDF (an
/// incremental update appends objects, a new table and a new `%%EOF`) isn't the end yet; one
/// followed by anything else, or by another file's signature at a sector boundary, is.
fn pdf(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 16)?;
    if h.len() < 8 || !(h[5].is_ascii_digit() && h[6] == b'.' && h[7].is_ascii_digit()) {
        return Ok(None);
    }
    let limit = off.saturating_add(PDF_MAX).min(s.end);
    let mut st = Stream::new(*s, off);
    let mut good: Option<u64> = None;
    let mut first_hdr: Option<u64> = None;
    let mut stop_at: Option<u64> = None;
    let (mut zeros, mut zeros_at) = (0u64, 0u64);
    let mut nb = s.align_up(off + 1);
    let mut p = off + 5;
    let mut last_obj = off;
    'scan: while p < limit {
        st.seek(p);
        let w = st.peek(128 << 10)?;
        let len = w.len().min((limit - p) as usize);
        if len == 0 {
            break;
        }
        let last = p + len as u64 >= limit || len < 128;
        // positions fully judged in this window
        let u = if last { len } else { len - 64 };
        let mut i = 0;
        loop {
            let q = find(&w[..(u + 4).min(len)], b"%%EOF", i).filter(|&q| q < u);
            let upto = p + q.unwrap_or(u) as u64;
            while nb < upto {
                let b = &w[(nb - p) as usize..len];
                if strong(b) {
                    match good {
                        Some(g) if nb >= g => break 'scan,
                        Some(_) => {}
                        None => {
                            first_hdr.get_or_insert(nb);
                        }
                    }
                }
                if good.is_none() {
                    let z = &b[..b.len().min(s.sector as usize)];
                    if z.iter().all(|&c| c == 0) {
                        if zeros == 0 {
                            zeros_at = nb;
                        }
                        zeros += s.sector;
                        if zeros >= MIB {
                            stop_at = Some(zeros_at);
                            break 'scan;
                        }
                    } else {
                        zeros = 0;
                    }
                }
                nb += s.sector;
            }
            let Some(q) = q else { break };
            if let Some(e) = pdf_eof(s, off, p + q as u64)? {
                good = Some(e);
                if !pdf_continues(s, e)? {
                    break 'scan;
                }
            }
            i = q + 5;
        }
        if good.is_none() && first_hdr.is_some_and(|f| p + u as u64 > f + PDF_PAST_HEADER) {
            stop_at = first_hdr;
            break;
        }
        if let Some(k) = rfind(&w[..(u + 6).min(len)], b"endobj") {
            last_obj = last_obj.max(p + k as u64 + 6);
        }
        if p + u as u64 > last_obj + PDF_GAP {
            if good.is_none() {
                stop_at = Some(s.align_up(last_obj).min(limit));
            }
            break;
        }
        p += u as u64;
        if last {
            break;
        }
    }
    let hit = Hit::new("pdf", 0);
    Ok(Some(match good {
        Some(e) => Hit::new("pdf", pdf_comments(s, e)? - off),
        None => {
            let end = stop_at.or(first_hdr).unwrap_or(limit);
            if end >= s.end {
                hit.cut(s, off)
            } else if end >= limit {
                hit.partial(off, end, capped(PDF_MAX))
            } else {
                hit.partial(off, end, NO_END)
            }
        }
    }))
}

/// Where the `%%EOF` at `at` ends (with its line ending), if the `startxref` just before it points
/// back at a cross-reference table (or stream) of the PDF starting at `off`.
fn pdf_eof(s: &Src, off: u64, at: u64) -> io::Result<Option<u64>> {
    let back = (at - off).min(128) as usize;
    let b = s.get(at - back as u64, back + 7)?;
    let pre = &b[..back];
    let Some(k) = rfind(pre, b"startxref") else {
        return Ok(None);
    };
    let num = pre[k + 9..].trim_ascii();
    if num.is_empty() || num.len() > 12 || !num.iter().all(u8::is_ascii_digit) {
        return Ok(None);
    }
    let x: u64 = std::str::from_utf8(num)
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    if x < 9 || off + x >= at {
        return Ok(None); // 0 in a linearized file's first trailer
    }
    let t = s.get(off + x, 32)?;
    if !(t.starts_with(b"xref") || is_obj(&t)) {
        return Ok(None);
    }
    let tail = b.get(back + 5..).unwrap_or_default();
    let eol = if tail.starts_with(b"\r\n") {
        2
    } else if matches!(tail.first(), Some(b'\n' | b'\r')) {
        1
    } else {
        0
    };
    Ok(Some(at + 5 + eol))
}

/// "12 0 obj"
fn is_obj(t: &[u8]) -> bool {
    let mut i = 0;
    let digits = |i: &mut usize| {
        let s = *i;
        while t.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        *i > s
    };
    let spaces = |i: &mut usize| {
        let s = *i;
        while t.get(*i).is_some_and(u8::is_ascii_whitespace) {
            *i += 1;
        }
        *i > s
    };
    digits(&mut i)
        && spaces(&mut i)
        && digits(&mut i)
        && spaces(&mut i)
        && t[i..].starts_with(b"obj")
}

/// Comment lines some writers add after the last `%%EOF` (CUPS: `%%#PDF-BANNER`…): kept, so the
/// file comes back byte for byte.
fn pdf_comments(s: &Src, mut e: u64) -> io::Result<u64> {
    let b = s.get(e, 4096)?;
    let mut i = 0;
    while b.get(i) == Some(&b'%') && !b[i..].starts_with(b"%PDF") {
        let Some(n) = b[i..].iter().position(|&c| c == b'\n') else {
            break;
        };
        if !b[i..i + n]
            .iter()
            .all(|&c| c == b'\t' || c == b'\r' || (0x20..0x7F).contains(&c))
        {
            break;
        }
        i += n + 1;
        e += n as u64 + 1;
    }
    Ok(e)
}

/// After a `%%EOF`: more of the same PDF (an update's objects, table or comment), or something else?
fn pdf_continues(s: &Src, e: u64) -> io::Result<bool> {
    let b = s.get(e, 64)?;
    let t = b.trim_ascii_start();
    Ok(match t.first() {
        Some(c) if c.is_ascii_digit() => true,
        Some(b'%') => !t.starts_with(b"%PDF"),
        _ => t.starts_with(b"xref"),
    })
}

// ---------- ZIP and what's built on it (Office, OpenDocument, EPUB)

const ZIP_MAX: u64 = 64 * GIB;
/// How far to search for the end record when the entries can't be walked.
const ZIP_SEARCH: u64 = 4 * GIB;
/// How much of a central directory is read to see what the archive is.
const ZIP_CD_READ: u64 = 16 * MIB;
/// Searching for the end record stops this far past the last ZIP record of any kind.
const ZIP_GAP: u64 = 64 * MIB;

#[derive(Debug, Default)]
struct ZipNames {
    content_types: bool,
    word: bool,
    xl: bool,
    ppt: bool,
    container: bool,
    mimetype: Option<String>,
}

impl ZipNames {
    fn add(&mut self, name: &[u8]) {
        match name {
            b"[Content_Types].xml" => self.content_types = true,
            b"META-INF/container.xml" => self.container = true,
            n if n.starts_with(b"word/") => self.word = true,
            n if n.starts_with(b"xl/") => self.xl = true,
            n if n.starts_with(b"ppt/") => self.ppt = true,
            _ => {}
        }
    }

    fn ext(&self) -> &'static str {
        match self.mimetype.as_deref().map(str::trim) {
            Some("application/vnd.oasis.opendocument.text") => return "odt",
            Some("application/vnd.oasis.opendocument.spreadsheet") => return "ods",
            Some("application/vnd.oasis.opendocument.presentation") => return "odp",
            Some("application/epub+zip") if self.container => return "epub",
            _ => {}
        }
        match (self.content_types, self.word, self.xl, self.ppt) {
            (true, true, _, _) => "docx",
            (true, _, true, _) => "xlsx",
            (true, _, _, true) => "pptx",
            _ => "zip",
        }
    }
}

/// ZIP: walk the entries by their recorded sizes, then the central directory, to the
/// end-of-central-directory record, which has to agree with where the archive starts. Entries whose
/// sizes come after their data (written as a stream) can't be walked: then the end record is
/// searched for. What the entries are called says whether it's a Word document, an OpenDocument…
fn zip(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 30)?;
    if h.len() < 30 {
        return Ok(None);
    }
    let (ver, method, csize) = (le(&h, 4, 2), le(&h, 8, 2), le(&h, 18, 4));
    let (nlen, xlen) = (le(&h, 26, 2), le(&h, 28, 2));
    if ver & 0xFF > 63 || !matches!(method, 0..=20 | 93 | 95..=99) || !(1..=1024).contains(&nlen) {
        return Ok(None);
    }
    let name = s.get(off + 30, nlen as usize)?;
    if name.len() < nlen as usize || name.iter().any(|&c| c < 0x20) {
        return Ok(None);
    }
    let mut names = ZipNames::default();
    if &name[..] == b"mimetype" && method == 0 && csize < 128 {
        let d = s.get(off + 30 + nlen + xlen, csize as usize)?;
        names.mimetype = Some(String::from_utf8_lossy(&d).into_owned());
    }
    let limit = off.saturating_add(ZIP_MAX);
    let (mut pos, mut entries, mut in_cd) = (off, 0u64, false);
    let stuck = loop {
        if pos >= s.end {
            break pos;
        }
        let b = s.get(pos, 46)?;
        if b.len() < 4 {
            break pos;
        }
        let next = match &b[..4] {
            b"PK\x03\x04" if !in_cd && b.len() >= 30 => {
                let (fl, n, x) = (le(&b, 6, 2), le(&b, 26, 2), le(&b, 28, 2));
                let mut cs = le(&b, 18, 4);
                let nm = s.get(pos + 30, (n + x) as usize)?;
                if n == 0 || nm.len() < (n + x) as usize {
                    break pos;
                }
                let nm_only = &nm[..n as usize];
                names.add(nm_only);
                let mut z64 = false;
                if cs == 0xFFFF_FFFF {
                    match zip64_csize(&nm[n as usize..]) {
                        Some(c) => (cs, z64) = (c, true),
                        None => break pos,
                    }
                }
                if fl & 8 != 0 && cs == 0 && !nm_only.ends_with(b"/") {
                    entries += 1;
                    break pos; // its size comes after the data
                }
                let mut e = pos + 30 + n + x + cs;
                if fl & 8 != 0 {
                    let d = s.get(e, 4)?;
                    e += if has(&d, 0, b"PK\x07\x08") { 4 } else { 0 } + if z64 { 20 } else { 12 };
                }
                entries += 1;
                e
            }
            b"PK\x01\x02" if b.len() >= 46 => {
                in_cd = true;
                let (n, x, c) = (le(&b, 28, 2), le(&b, 30, 2), le(&b, 32, 2));
                names.add(&s.get(pos + 46, n as usize)?);
                pos + 46 + n + x + c
            }
            b"PK\x06\x06" if b.len() >= 12 => pos + 12 + le(&b, 4, 8),
            b"PK\x06\x07" => pos + 20,
            b"PK\x05\x05" if b.len() >= 6 => pos + 6 + le(&b, 4, 2),
            b"PK\x05\x06" => {
                if let Some((end, _, _)) = zip_eocd(s, off, pos)? {
                    return Ok(Some(Hit::new(names.ext(), end - off)));
                }
                break pos;
            }
            _ => break pos,
        };
        if next > limit {
            break pos;
        }
        pos = next;
    };
    if entries == 0 && !in_cd {
        return Ok(None);
    }
    if stuck < s.end
        && let Some((end, cd, cd_size)) = zip_search(s, off, stuck, ZIP_SEARCH)?
    {
        if !in_cd {
            zip_cd_names(s, cd, cd_size, &mut names)?;
        }
        return Ok(Some(Hit::new(names.ext(), end - off)));
    }
    // no end record: cut off, or the rest is elsewhere or gone
    let hit = Hit::new(names.ext(), 0);
    let e = if stuck >= s.end {
        s.end
    } else {
        until_next(s, stuck, ZIP_GAP, true)?
    };
    Ok(Some(if e >= s.end {
        hit.cut(s, off)
    } else {
        hit.partial(off, e, DAMAGED)
    }))
}

/// The compressed size from a local header's ZIP64 extra field.
fn zip64_csize(extra: &[u8]) -> Option<u64> {
    let mut i = 0;
    while i + 4 <= extra.len() {
        let (id, n) = (le(extra, i, 2), le(extra, i + 2, 2) as usize);
        if id == 1 {
            return (n >= 16).then(|| le(extra, i + 12, 8));
        }
        i += 4 + n;
    }
    None
}

/// If the end-of-central-directory record at `at` belongs to the archive starting at `off` (its
/// central directory sits right before it, or before the ZIP64 records, and starts where it says
/// relative to `off`): where the archive ends, and where its central directory is and how big.
fn zip_eocd(s: &Src, off: u64, at: u64) -> io::Result<Option<(u64, u64, u64)>> {
    let e = s.get(at, 22)?;
    if e.len() < 22 || !matches!(le(&e, 4, 2), 0 | 0xFFFF) {
        return Ok(None);
    }
    let (mut cd_size, mut cd_off) = (le(&e, 12, 4), le(&e, 16, 4));
    let mut cd_end = at;
    if cd_size == 0xFFFF_FFFF || cd_off == 0xFFFF_FFFF || le(&e, 10, 2) == 0xFFFF {
        if at < off + 20 {
            return Ok(None);
        }
        let l = s.get(at - 20, 20)?;
        if !has(&l, 0, b"PK\x06\x07") {
            return Ok(None);
        }
        let rec = off + le(&l, 8, 8);
        let r = s.get(rec, 56)?;
        if !has(&r, 0, b"PK\x06\x06") || r.len() < 56 {
            return Ok(None);
        }
        (cd_size, cd_off, cd_end) = (le(&r, 40, 8), le(&r, 48, 8), rec);
    }
    let cd = off.saturating_add(cd_off);
    if cd.saturating_add(cd_size) != cd_end {
        return Ok(None);
    }
    if cd_size > 0 && !has(&s.get(cd, 4)?, 0, b"PK\x01\x02") {
        return Ok(None);
    }
    Ok(Some((at + 22 + le(&e, 20, 2), cd, cd_size)))
}

/// Search from `from` for an end record that belongs to the archive at `off` (giving up where the
/// ZIP records of any kind stop).
fn zip_search(s: &Src, off: u64, from: u64, max: u64) -> io::Result<Option<(u64, u64, u64)>> {
    let limit = from.saturating_add(max).min(s.end);
    let mut st = Stream::new(*s, from);
    let (mut p, mut last_sig) = (from, from);
    while p < limit && p <= last_sig + ZIP_GAP {
        st.seek(p);
        let w = st.peek(128 << 10)?;
        let len = w.len().min((limit - p) as usize);
        if len < 4 {
            break;
        }
        let mut i = 0;
        while let Some(q) = find(&w[..len], b"PK", i) {
            match w.get(q + 2..q + 4) {
                Some([5, 6]) => {
                    if let Some(r) = zip_eocd(s, off, p + q as u64)? {
                        return Ok(Some(r));
                    }
                }
                Some([3, 4] | [7, 8] | [1, 2]) => last_sig = p + q as u64,
                _ => {}
            }
            i = q + 1;
        }
        p += (len - 3) as u64;
    }
    Ok(None)
}

fn zip_cd_names(s: &Src, cd: u64, size: u64, names: &mut ZipNames) -> io::Result<()> {
    let b = s.get(cd, size.min(ZIP_CD_READ) as usize)?;
    let mut i = 0;
    while has(&b, i, b"PK\x01\x02") && i + 46 <= b.len() {
        let (n, x, c) = (
            le(&b, i + 28, 2) as usize,
            le(&b, i + 30, 2) as usize,
            le(&b, i + 32, 2) as usize,
        );
        if let Some(nm) = b.get(i + 46..i + 46 + n) {
            names.add(nm);
        }
        i += 46 + n + x + c;
    }
    Ok(())
}

// ---------- OLE2 compound files (old Office, Outlook .msg)

const CFB_MAX: u64 = GIB;
const FREESECT: u64 = 0xFFFF_FFFF;
const MAXREGSECT: u64 = 0xFFFF_FFFA;

/// Compound files: the size is where the highest sector in use (by the FAT) ends; what the root's
/// streams are called says what it is.
fn cfb(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 512)?;
    if h.len() < 512 {
        return Ok(None);
    }
    let (major, shift) = (le(&h, 26, 2), le(&h, 30, 2));
    if le(&h, 28, 2) != 0xFFFE
        || le(&h, 32, 2) != 6
        || le(&h, 56, 4) != 4096
        || !matches!((major, shift), (3, 9) | (4, 12))
    {
        return Ok(None);
    }
    let ss = 1u64 << shift;
    let per = ss / 4;
    let (nfat, first_dir, first_difat, ndifat) =
        (le(&h, 44, 4), le(&h, 48, 4), le(&h, 68, 4), le(&h, 72, 4));
    let max_sectors = CFB_MAX / ss;
    if nfat == 0 || nfat > max_sectors / per + 1 || ndifat > max_sectors / per + 1 {
        return Ok(None);
    }
    let sector = |n: u64| off + (n + 1) * ss;
    // the FAT's own sectors: the first 109 listed in the header, the rest in a chain of DIFAT sectors
    let mut fat_secs: Vec<u64> = (0..nfat.min(109))
        .map(|i| le(&h, 76 + 4 * i as usize, 4))
        .collect();
    let (mut d, mut hops) = (first_difat, 0);
    while (fat_secs.len() as u64) < nfat && d < MAXREGSECT && hops <= ndifat {
        let b = s.get(sector(d), ss as usize)?;
        if b.len() < ss as usize {
            return Ok(Some(Hit::new("ole", 0).cut(s, off)));
        }
        for i in 0..(per - 1) as usize {
            if (fat_secs.len() as u64) < nfat {
                fat_secs.push(le(&b, 4 * i, 4));
            }
        }
        d = le(&b, (ss - 4) as usize, 4);
        hops += 1;
    }
    if (fat_secs.len() as u64) < nfat {
        return Ok(None);
    }
    let entries = nfat * per;
    let mut fat: Vec<u32> = Vec::with_capacity(entries as usize);
    let mut hi = 0u64;
    for &fs in &fat_secs {
        if fs >= max_sectors {
            return Ok(None);
        }
        let b = s.get(sector(fs), ss as usize)?;
        if b.len() < ss as usize {
            return Ok(Some(Hit::new("ole", 0).cut(s, off)));
        }
        for v in b.as_chunks::<4>().0.iter().map(|c| le(c, 0, 4)) {
            if v != FREESECT {
                if v < MAXREGSECT && v >= entries {
                    return Ok(None); // points past the FAT
                }
                hi = fat.len() as u64;
            }
            fat.push(v as u32);
        }
    }
    // the FAT's first sector is marked as one
    if fat.get(fat_secs[0] as usize) != Some(&0xFFFF_FFFD) {
        return Ok(None);
    }
    let size = (hi + 2) * ss;
    if size > CFB_MAX {
        return Ok(None);
    }
    let Some(ext) = cfb_kind(s, off, ss, first_dir, &fat)? else {
        return Ok(None);
    };
    Ok(Some(Hit::new(ext, size)))
}

/// What a compound file is, from the names of the streams in its root.
fn cfb_kind(
    s: &Src,
    off: u64,
    ss: u64,
    first_dir: u64,
    fat: &[u32],
) -> io::Result<Option<&'static str>> {
    struct Entry {
        name: String,
        typ: u8,
        left: u64,
        right: u64,
        child: u64,
    }
    let mut ents: Vec<Entry> = Vec::new();
    let (mut sec, mut hops) = (first_dir, 0);
    while sec < MAXREGSECT && hops < 64 {
        let b = s.get(off + (sec + 1) * ss, ss as usize)?;
        if b.len() < ss as usize {
            break;
        }
        for e in b.as_chunks::<128>().0 {
            let nl = (le(e, 64, 2) as usize).min(64);
            ents.push(Entry {
                name: utf16(&e[..nl], false),
                typ: e[66],
                left: le(e, 68, 4),
                right: le(e, 72, 4),
                child: le(e, 76, 4),
            });
        }
        sec = fat.get(sec as usize).map_or(FREESECT, |&v| u64::from(v));
        hops += 1;
    }
    let Some(root) = ents.first().filter(|r| r.typ == 5) else {
        return Ok(None);
    };
    // the root's children: a tree of siblings
    let mut names: Vec<&str> = Vec::new();
    let mut todo = vec![root.child];
    while let Some(i) = todo.pop() {
        if i >= MAXREGSECT || names.len() > 4096 {
            continue;
        }
        let Some(e) = ents.get(i as usize) else {
            continue;
        };
        names.push(&e.name);
        todo.push(e.left);
        todo.push(e.right);
    }
    let named = |n: &str| names.contains(&n);
    Ok(Some(
        if names.iter().any(|n| n.starts_with("__substg1.0_")) {
            "msg"
        } else if named("WordDocument") {
            "doc"
        } else if named("PowerPoint Document") {
            "ppt"
        } else if named("Workbook") || named("Book") {
            "xls"
        } else {
            "ole"
        },
    ))
}

// ---------- SQLite

const SQLITE_MAX: u64 = 16 * GIB;

/// SQLite: page size × page count from the header, when the header's size is known to be current
/// (its "valid-for" counter matches the change counter).
fn sqlite(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 101)?;
    if h.len() < 101 {
        return Ok(None);
    }
    let ps = be(&h, 16, 2);
    let page = if ps == 1 { 65_536 } else { ps };
    if page < 512
        || !page.is_power_of_two()
        || !(1..=2).contains(&h[18])
        || !(1..=2).contains(&h[19])
        || h[21] != 64
        || h[22] != 32
        || h[23] != 32
        || be(&h, 44, 4) > 4
        || be(&h, 56, 4) > 3
        || !matches!(h[100], 0x0D | 0x05)
    {
        return Ok(None);
    }
    let (changes, count, valid) = (be(&h, 24, 4), be(&h, 28, 4), be(&h, 92, 4));
    if count > 0 && valid == changes {
        let size = page * count;
        return Ok((size <= SQLITE_MAX).then(|| Hit::new("sqlite", size)));
    }
    // an old writer that didn't keep the size in the header
    let e = until_next(s, off + page, OPEN_MAX, false)?;
    Ok(Some(Hit::new("sqlite", 0).partial(off, e, NO_END)))
}

// ---------- MP3

const MP3_MAX: u64 = GIB;
/// A run of MPEG audio frames without an ID3 tag has to be at least this long (about 2.6 s).
const MIN_BARE_FRAMES: u64 = 100;

#[derive(Debug, Clone, Copy)]
struct Mpa {
    len: u64,
    /// version, layer and sample rate: the same all through a stream
    key: u32,
    layer3: bool,
}

/// An MPEG audio frame header: how long the frame is.
fn mpa(h: u32) -> Option<Mpa> {
    if h >> 21 != 0x7FF {
        return None;
    }
    let (ver, layer) = ((h >> 19) & 3, (h >> 17) & 3);
    let (br, sr) = (((h >> 12) & 15) as usize, ((h >> 10) & 3) as usize);
    if ver == 1 || layer == 0 || br == 0 || br == 15 || sr == 3 || h & 3 == 2 {
        return None;
    }
    let v1 = ver == 3;
    let kbps: u64 = match (v1, layer) {
        (true, 3) => [
            0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
        ][br],
        (true, 2) => [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
        ][br],
        (true, _) => [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ][br],
        (false, 3) => [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
        ][br],
        (false, _) => [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160][br],
    };
    let rate: u64 = [
        [11_025, 12_000, 8_000],
        [0; 3],
        [22_050, 24_000, 16_000],
        [44_100, 48_000, 32_000],
    ][ver as usize][sr];
    let pad = u64::from((h >> 9) & 1);
    let len = match layer {
        3 => (12 * kbps * 1000 / rate + pad) * 4,
        1 if !v1 => 72 * kbps * 1000 / rate + pad,
        _ => 144 * kbps * 1000 / rate + pad,
    };
    Some(Mpa {
        len,
        key: h & 0xFFFE_0C00,
        layer3: layer == 1,
    })
}

#[derive(Debug, Default)]
struct Run {
    end: u64,
    frames: u64,
    cut: bool,
    big: bool,
}

/// Follow MPEG audio frames from `from` while they're valid and alike.
fn mpa_run(s: &Src, from: u64, max: u64) -> io::Result<Run> {
    let mut st = Stream::new(*s, from);
    let mut r = Run {
        end: from,
        ..Run::default()
    };
    let mut key = None;
    loop {
        let at = st.offset();
        if at - from > max {
            r.big = true;
            break;
        }
        let b = st.peek(4)?;
        if b.len() < 4 {
            break;
        }
        let Some(f) = mpa(be(b, 0, 4) as u32) else {
            break;
        };
        if key.is_some_and(|k| k != f.key) {
            break;
        }
        key = Some(f.key);
        if at + f.len > s.end {
            r.cut = true;
            break;
        }
        st.advance(f.len);
        r.frames += 1;
        r.end = at + f.len;
    }
    Ok(r)
}

/// Tags after the audio: an APEv2 tag (with its header) and an ID3v1 tag.
fn mp3_tail(s: &Src, mut end: u64) -> io::Result<u64> {
    let b = s.get(end, 32)?;
    if has(&b, 0, b"APETAGEX") && le(&b, 20, 4) & (1 << 29) != 0 {
        end += 32 + le(&b, 12, 4);
    }
    if has(&s.get(end, 3)?, 0, b"TAG") && end + 128 <= s.end {
        end += 128;
    }
    Ok(end)
}

/// An MP3 with an ID3v2 tag: the tag's size, then the audio frames until they stop.
fn id3(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 10)?;
    if h.len() < 10
        || !(2..=4).contains(&h[3])
        || h[4] == 0xFF
        || h[6..10].iter().any(|&c| c >= 0x80)
    {
        return Ok(None);
    }
    let size = h[6..10].iter().fold(0u64, |a, &c| a << 7 | u64::from(c));
    let mut pos = off + 10 + size + if h[5] & 0x10 != 0 { 10 } else { 0 };
    // some taggers leave padding outside the tag's size
    let pad = s.get(pos, 4096)?;
    pos += pad.iter().take_while(|&&c| c == 0).count() as u64;
    let r = mpa_run(s, pos, MP3_MAX)?;
    let hit = Hit::new("mp3", 0);
    if r.cut || pos >= s.end {
        return Ok(Some(hit.cut(s, off)));
    }
    if r.frames < 4 {
        return Ok(None);
    }
    if r.big {
        return Ok(Some(hit.partial(off, r.end, capped(MP3_MAX))));
    }
    Ok(Some(Hit::new("mp3", mp3_tail(s, r.end)? - off)))
}

/// MPEG audio frames with no tag in front: only a long run of them counts.
fn mpeg(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let mut at = off;
    let mut key = None;
    for _ in 0..3 {
        let b = s.get(at, 4)?;
        if b.len() < 4 {
            return Ok(None);
        }
        let Some(f) = mpa(be(&b, 0, 4) as u32).filter(|f| f.layer3) else {
            return Ok(None);
        };
        if key.is_some_and(|k| k != f.key) {
            return Ok(None);
        }
        key = Some(f.key);
        at += f.len;
    }
    let r = mpa_run(s, off, MP3_MAX)?;
    if r.frames < MIN_BARE_FRAMES {
        return Ok(None);
    }
    let hit = Hit::new("mp3", 0);
    if r.cut {
        return Ok(Some(hit.cut(s, off)));
    }
    if r.big {
        return Ok(Some(hit.partial(off, r.end, capped(MP3_MAX))));
    }
    Ok(Some(Hit::new("mp3", mp3_tail(s, r.end)? - off)))
}

// ---------- FLAC

const FLAC_MAX: u64 = 2 * GIB;

#[derive(Debug, Clone, Copy)]
struct FlacFrame {
    block: u64,
    num: u64,
    variable: bool,
    hdr: u64,
}

/// A FLAC frame header, checked with its CRC-8.
fn flac_frame(b: &[u8]) -> Option<FlacFrame> {
    if b.len() < 6 || b[0] != 0xFF || b[1] & 0xFE != 0xF8 {
        return None;
    }
    let (bs, rate, ch, ss) = (b[2] >> 4, b[2] & 15, b[3] >> 4, (b[3] >> 1) & 7);
    if bs == 0 || rate == 15 || ch > 10 || ss == 3 || b[3] & 1 != 0 {
        return None;
    }
    let first = b[4];
    let n = match first.leading_ones() {
        0 => 1,
        k @ 2..=7 => k as usize,
        _ => return None,
    };
    let mut num = if n == 1 {
        u64::from(first)
    } else {
        u64::from(first & (0x7F >> n))
    };
    for i in 1..n {
        let c = *b.get(4 + i)?;
        if c & 0xC0 != 0x80 {
            return None;
        }
        num = num << 6 | u64::from(c & 0x3F);
    }
    let mut i = 4 + n;
    let block: u64 = match bs {
        1 => 192,
        2..=5 => 576 << (bs - 2),
        6 => {
            let v = u64::from(*b.get(i)?) + 1;
            i += 1;
            v
        }
        7 => {
            let v = be(b, i, 2) + 1;
            i += 2;
            v
        }
        _ => 256 << (bs - 8),
    };
    i += match rate {
        12 => 1,
        13 | 14 => 2,
        _ => 0,
    };
    if crc8(b.get(..i)?) != *b.get(i)? {
        return None;
    }
    Some(FlacFrame {
        block,
        num,
        variable: b[1] & 1 == 1,
        hdr: (i + 1) as u64,
    })
}

/// FLAC: the metadata blocks, then the frames. Frames don't record their length: each ends where
/// its CRC-16 checks out and the next frame's header (with the next number) follows, the last one
/// where the stream's sample count is reached.
fn flac(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let mut pos = off + 4;
    let mut info: Option<Vec<u8>> = None;
    for i in 0.. {
        let b = s.get(pos, 4)?;
        if b.len() < 4 || i > 1000 {
            return Ok(None);
        }
        let (last, typ, len) = (b[0] & 0x80 != 0, b[0] & 0x7F, be(&b, 1, 3));
        if typ > 6 || (i == 0 && (typ != 0 || len != 34)) {
            return Ok(None);
        }
        if typ == 0 {
            let d = s.get(pos + 4, 34)?;
            if d.len() < 34 {
                return Ok(None);
            }
            info = Some(d.into_owned());
        }
        pos += 4 + len;
        if last {
            break;
        }
    }
    let Some(si) = info else { return Ok(None) };
    let (min_frame, max_frame) = (be(&si, 4, 3), be(&si, 7, 3));
    let total = (u64::from(si[13] & 0x0F) << 32) | be(&si, 14, 4);
    if be(&si, 10, 3) >> 4 == 0 || be(&si, 2, 2) < be(&si, 0, 2) {
        return Ok(None);
    }
    let hit = Hit::new("flac", 0);
    let Some(mut fr) = flac_frame(&s.get(pos, 32)?) else {
        return Ok(if pos >= s.end {
            Some(hit.cut(s, off))
        } else {
            None
        });
    };
    let longest = if max_frame > 0 { max_frame + 16 } else { MIB };
    let (mut cur, mut samples) = (pos, 0u64);
    let mut st = Stream::new(*s, cur);
    loop {
        if cur - off > FLAC_MAX {
            return Ok(Some(hit.partial(off, cur, capped(FLAC_MAX))));
        }
        let last = total > 0 && samples + fr.block >= total;
        let expected = if fr.variable {
            fr.num + fr.block
        } else {
            fr.num + 1
        };
        let shortest = min_frame.max(fr.hdr + 2);
        let limit = cur + longest;
        let (mut crc, mut p) = (0u16, cur);
        let (mut end, mut next, mut fallback) = (None, None, None);
        st.seek(cur);
        'bytes: while p < limit {
            let w = st.peek(1)?;
            if w.is_empty() {
                break;
            }
            let take = w.len().min((limit - p) as usize);
            for (i, &x) in w[..take].iter().enumerate() {
                crc = (crc << 8) ^ CRC16[usize::from((crc >> 8) as u8 ^ x)];
                let q = p + i as u64 + 1;
                if crc != 0 || q - cur < shortest {
                    continue;
                }
                if last {
                    end = Some(q);
                    break 'bytes;
                }
                let nb = s.get(q, 32)?;
                match flac_frame(&nb) {
                    Some(nf) if nf.variable == fr.variable && nf.num == expected => {
                        (end, next) = (Some(q), Some(nf));
                        break 'bytes;
                    }
                    // no next frame: the stream is over here, or what follows is damaged (or
                    // the CRC matched by chance, if a proper next frame turns up further on)
                    _ if fallback.is_none() => {
                        let sync = nb.len() >= 2 && nb[0] == 0xFF && nb[1] & 0xFE == 0xF8;
                        fallback = Some((q, sync));
                    }
                    _ => {}
                }
            }
            st.advance(take as u64);
            p += take as u64;
        }
        let e = match (end, fallback) {
            (Some(e), _) => e,
            (None, Some((q, false))) if total == 0 => q,
            (None, Some((q, _))) => return Ok(Some(hit.partial(off, q, DAMAGED))),
            (None, None) if p >= s.end => return Ok(Some(hit.cut(s, off))),
            (None, None) => return Ok((samples > 0).then(|| hit.partial(off, cur, DAMAGED))),
        };
        samples += fr.block;
        cur = e;
        match next {
            Some(nf) => fr = nf,
            None => return Ok(Some(Hit::new("flac", cur - off))),
        }
    }
}

// ---------- Ogg

const OGG_MAX: u64 = 4 * GIB;

/// Ogg: page after page (each with its CRC checked) until the pages stop.
fn ogg(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let mut st = Stream::new(*s, off);
    let (mut pages, mut ext, mut eos) = (0u64, "ogg", false);
    loop {
        let at = st.offset();
        if at - off > OGG_MAX {
            return Ok(Some(Hit::new(ext, 0).partial(off, at, capped(OGG_MAX))));
        }
        let b = st.peek(27 + 255)?;
        if b.len() < 27 || !has(b, 0, b"OggS") || b[4] != 0 || b[5] & 0xF8 != 0 {
            break;
        }
        let flags = b[5];
        let nseg = usize::from(b[26]);
        if b.len() < 27 + nseg {
            return Ok((pages > 0).then(|| Hit::new(ext, 0).cut(s, off)));
        }
        if pages == 0 && (flags & 2 == 0 || le(b, 18, 4) != 0) {
            return Ok(None); // the first page begins a stream
        }
        if pages > 0 && eos && flags & 2 != 0 {
            break; // a new stream after the last one ended: the next file
        }
        let len = 27
            + nseg
            + b[27..27 + nseg]
                .iter()
                .map(|&x| usize::from(x))
                .sum::<usize>();
        let page = st.peek(len)?;
        if page.len() < len {
            return Ok((pages > 0).then(|| Hit::new(ext, 0).cut(s, off)));
        }
        let crc = ogg_crc(ogg_crc(ogg_crc(0, &page[..22]), &[0; 4]), &page[26..len]);
        if crc != le(page, 22, 4) as u32 {
            if pages == 0 {
                return Ok(None);
            }
            break;
        }
        if pages == 0 {
            let p = &page[27 + nseg..len];
            ext = if p.starts_with(b"OpusHead") {
                "opus"
            } else if p.starts_with(b"\x80theora") {
                "ogv"
            } else {
                "ogg"
            };
        }
        eos = flags & 4 != 0;
        st.advance(len as u64);
        pages += 1;
    }
    if pages < 2 {
        return Ok(None);
    }
    let hit = Hit::new(ext, st.offset() - off);
    Ok(Some(if eos {
        hit
    } else {
        let e = st.offset();
        hit.partial(off, e, DAMAGED)
    }))
}

// ---------- Matroska / WebM

const MKV_MAX: u64 = 64 * GIB;
const CLUSTER: u64 = 0x1F43_B675;
const SEG_KIDS: &[u64] = &[
    0x114D_9B74,
    0x1549_A966,
    0x1654_AE6B,
    CLUSTER,
    0x1C53_BB6B,
    0x1043_A770,
    0x1254_C367,
    0x1941_A469,
    0xEC,
    0xBF,
];
const CLUSTER_KIDS: &[u64] = &[0xE7, 0x5854, 0xA7, 0xAB, 0xA3, 0xA0, 0xAF, 0xEC, 0xBF];

/// An EBML variable-length number: its value, its length, and whether it means "unknown" (all
/// ones). `id` keeps the length marker, as element IDs do.
fn vint(b: &[u8], id: bool) -> Option<(u64, usize, bool)> {
    let first = *b.first()?;
    if first == 0 {
        return None;
    }
    let n = first.leading_zeros() as usize + 1;
    if b.len() < n || (id && n > 4) {
        return None;
    }
    let mut v = if id {
        u64::from(first)
    } else {
        u64::from(first) & (0xFF >> n)
    };
    for &c in &b[1..n] {
        v = v << 8 | u64::from(c);
    }
    let unknown = !id && v == (1u64 << (7 * n)) - 1;
    Some((v, n, unknown))
}

/// Matroska and WebM: the EBML header (whose DocType says which), then the Segment, whose size
/// covers the rest. Live recordings leave that size unknown: then its elements are walked.
fn ebml(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 16)?;
    let Some((hs, n, unk)) = vint(h.get(4..).unwrap_or_default(), false) else {
        return Ok(None);
    };
    if unk || hs > 1024 {
        return Ok(None);
    }
    let body = s.get(off + 4 + n as u64, hs as usize)?;
    let mut doc: Option<&[u8]> = None;
    let mut i = 0;
    while i < body.len() {
        let Some((id, a, _)) = vint(&body[i..], true) else {
            break;
        };
        let Some((sz, b, _)) = vint(&body[i + a..], false) else {
            break;
        };
        let d = i + a + b;
        if id == 0x4282 {
            doc = body.get(d..d + sz as usize);
        }
        i = d + sz as usize;
    }
    let ext = match doc.map(|d| &d[..d.iter().rposition(|&c| c != 0).map_or(0, |i| i + 1)]) {
        Some(b"matroska") => "mkv",
        Some(b"webm") => "webm",
        _ => return Ok(None),
    };
    let seg = off + 4 + n as u64 + hs;
    let sh = s.get(seg, 16)?;
    if sh.is_empty() {
        return Ok(Some(Hit::new(ext, 0).cut(s, off)));
    }
    if !has(&sh, 0, &[0x18, 0x53, 0x80, 0x67]) {
        return Ok(None);
    }
    let Some((ss, n2, unk)) = vint(&sh[4..], false) else {
        return Ok(None);
    };
    let data = seg + 4 + n2 as u64;
    let c = s.get(data, 8)?;
    if c.is_empty() {
        return Ok(Some(Hit::new(ext, 0).cut(s, off)));
    }
    if !vint(&c, true).is_some_and(|(id, _, _)| SEG_KIDS.contains(&id)) {
        return Ok(None);
    }
    if !unk {
        return Ok((data + ss - off <= MKV_MAX).then(|| Hit::new(ext, data + ss - off)));
    }
    // a live recording: walk the segment's elements (clusters of unknown size, too)
    let mut st = Stream::new(*s, data);
    let mut in_cluster = false;
    loop {
        let at = st.offset();
        if at - off > MKV_MAX {
            return Ok(Some(Hit::new(ext, 0).partial(off, at, capped(MKV_MAX))));
        }
        let b = st.peek(16)?;
        let Some((id, il, _)) = vint(b, true) else {
            break;
        };
        let Some((sz, sl, unk)) = vint(&b[il..], false) else {
            break;
        };
        let neutral = matches!(id, 0xEC | 0xBF);
        let top = SEG_KIDS.contains(&id);
        if !(top || (in_cluster && CLUSTER_KIDS.contains(&id))) {
            break;
        }
        if top && !neutral {
            in_cluster = false;
        }
        if id == CLUSTER && unk {
            in_cluster = true;
            st.advance((il + sl) as u64);
            continue;
        }
        if unk {
            break;
        }
        st.advance((il + sl) as u64 + sz);
        if st.offset() > s.end {
            return Ok(Some(Hit::new(ext, 0).cut(s, off)));
        }
    }
    let e = st.offset();
    Ok(Some(Hit::new(ext, e - off)))
}

// ---------- RAR, 7z

const RAR_MAX: u64 = 64 * GIB;

/// RAR (versions 4 and 5): block after block, each header's CRC checked, up to the end-of-archive
/// block.
fn rar(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 8)?;
    if has(&h, 0, b"Rar!\x1a\x07\x00") {
        rar4(s, off)
    } else if has(&h, 0, b"Rar!\x1a\x07\x01\x00") {
        rar5(s, off)
    } else {
        Ok(None)
    }
}

fn rar4(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let (mut pos, mut files) = (off + 7, 0u64);
    loop {
        if pos - off > RAR_MAX {
            return Ok(None);
        }
        let b = s.get(pos, 7)?;
        if b.len() < 7 {
            break;
        }
        let (typ, flags, hs) = (b[2], le(&b, 3, 2), le(&b, 5, 2));
        if !(0x72..=0x7B).contains(&typ) || hs < 7 {
            break;
        }
        let head = s.get(pos, hs as usize)?;
        if head.len() < hs as usize {
            return Ok((files > 0).then(|| Hit::new("rar", 0).cut(s, off)));
        }
        if u64::from(crc32(&head[2..]) & 0xFFFF) != le(&b, 0, 2) {
            break;
        }
        let mut add = 0;
        if typ == 0x74 || typ == 0x7A {
            add = le(&head, 7, 4);
            if flags & 0x100 != 0 && hs >= 36 {
                add |= le(&head, 32, 4) << 32;
            }
            files += u64::from(typ == 0x74);
        } else if flags & 0x8000 != 0 && hs >= 11 {
            add = le(&head, 7, 4);
        }
        pos += hs + add;
        if typ == 0x7B {
            return Ok(Some(Hit::new("rar", pos - off)));
        }
    }
    // old versions don't always end with an end block
    Ok((files > 0).then(|| Hit::new("rar", pos - off)))
}

/// A RAR5 variable-length number.
fn vint5(b: &[u8]) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for (i, &c) in b.iter().enumerate().take(10) {
        v |= u64::from(c & 0x7F) << (7 * i);
        if c & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

fn rar5(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let (mut pos, mut blocks) = (off + 8, 0u64);
    loop {
        if pos - off > RAR_MAX {
            return Ok(None);
        }
        let b = s.get(pos, 16)?;
        if b.len() < 7 {
            break;
        }
        let Some((hs, n)) = vint5(&b[4..]).filter(|&(hs, _)| hs > 0 && hs <= 2 * MIB) else {
            break;
        };
        let head = s.get(pos + 4, n + hs as usize)?;
        if head.len() < n + hs as usize {
            return Ok((blocks > 0).then(|| Hit::new("rar", 0).cut(s, off)));
        }
        if u64::from(crc32(&head)) != le(&b, 0, 4) {
            break;
        }
        let body = &head[n..];
        let Some((typ, a)) = vint5(body) else { break };
        let Some((flags, c)) = vint5(&body[a..]) else {
            break;
        };
        let mut i = a + c;
        if flags & 1 != 0 {
            let Some((_, k)) = vint5(&body[i..]) else {
                break;
            };
            i += k;
        }
        let data = if flags & 2 != 0 {
            let Some((d, _)) = vint5(&body[i..]) else {
                break;
            };
            d
        } else {
            0
        };
        if !(1..=5).contains(&typ) {
            break;
        }
        pos += 4 + n as u64 + hs + data;
        blocks += 1;
        if typ == 5 {
            return Ok(Some(Hit::new("rar", pos - off)));
        }
    }
    Ok((blocks > 0).then(|| Hit::new("rar", 0).partial(off, pos, DAMAGED)))
}

const SEVENZ_MAX: u64 = 64 * GIB;

/// 7z: the start header (checked by its CRC) says where the archive's last header is and how long.
fn sevenz(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 32)?;
    if h.len() < 32 || h[6] != 0 || h[7] > 4 || u64::from(crc32(&h[12..32])) != le(&h, 8, 4) {
        return Ok(None);
    }
    let (next_off, next_size) = (le(&h, 12, 8), le(&h, 20, 8));
    let size = 32u64.saturating_add(next_off).saturating_add(next_size);
    if next_size == 0 || size > SEVENZ_MAX {
        return Ok(None);
    }
    let mut hit = Hit::new("7z", size);
    if next_size <= 16 * MIB && off + size <= s.end {
        let t = s.get(off + 32 + next_off, next_size as usize)?;
        if u64::from(crc32(&t)) != le(&h, 28, 4) {
            hit = hit.partial(off, off + size, DAMAGED);
        }
    }
    Ok(Some(hit))
}

// ---------- Windows Media, MPEG streams

const ASF_MAX: u64 = 64 * GIB;
const ASF_TOP: &[[u8; 16]] = &[
    // data, simple index, index, media object index
    [
        0x36, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE,
        0x6C,
    ],
    [
        0x90, 0x08, 0x00, 0x33, 0xB1, 0xE5, 0xCF, 0x11, 0x89, 0xF4, 0x00, 0xA0, 0xC9, 0x03, 0x49,
        0xCB,
    ],
    [
        0xD3, 0x29, 0xE2, 0xD6, 0xDA, 0x35, 0xD1, 0x11, 0x90, 0x34, 0x00, 0xA0, 0xC9, 0x03, 0x49,
        0xBE,
    ],
    [
        0xF8, 0x03, 0xB1, 0xFE, 0xAD, 0x12, 0x64, 0x4C, 0x84, 0x0F, 0x2A, 0x1D, 0x2F, 0x7A, 0xD4,
        0x8C,
    ],
];
const ASF_VIDEO: &[u8] = &[
    0xC0, 0xEF, 0x19, 0xBC, 0x4D, 0x5B, 0xCF, 0x11, 0xA8, 0xFD, 0x00, 0x80, 0x5F, 0x5C, 0x44, 0x2B,
];

/// Windows Media (ASF): the header object, then the data and index objects, each with its size.
fn asf(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 30)?;
    if h.len() < 30 {
        return Ok(None);
    }
    let (hs, nobj) = (le(&h, 16, 8), le(&h, 24, 4));
    if !(30..=16 * MIB).contains(&hs) || nobj == 0 || nobj > 1000 || h[28] != 1 || h[29] != 2 {
        return Ok(None);
    }
    let head = s.get(off, hs as usize)?;
    let ext = if find(&head, ASF_VIDEO, 0).is_some() {
        "wmv"
    } else {
        "wma"
    };
    let mut pos = off + hs;
    loop {
        let b = s.get(pos, 24)?;
        if b.len() < 24 || !ASF_TOP.iter().any(|g| has(&b, 0, g)) {
            break;
        }
        let size = le(&b, 16, 8);
        if size < 24 || pos + size - off > ASF_MAX {
            break;
        }
        pos += size;
    }
    Ok((pos > off + hs).then(|| Hit::new(ext, pos - off)))
}

const TS_MAX: u64 = 64 * GIB;
const MIN_TS_PACKETS: u64 = 256;

/// MPEG transport streams (camcorders' AVCHD .MTS: 192-byte packets): packets for as long as their
/// sync bytes keep coming.
fn ts(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let b = s.get(off, 8)?;
    let (pk, sync, ext) = if b.first() == Some(&0x47) {
        (188usize, 0usize, "ts")
    } else {
        (192, 4, "mts")
    };
    let mut st = Stream::new(*s, off);
    let mut n = 0u64;
    loop {
        if st.offset() - off > TS_MAX {
            break;
        }
        let w = st.peek(pk)?;
        if w.len() < pk {
            if !w.is_empty() && n >= MIN_TS_PACKETS {
                return Ok(Some(Hit::new(ext, 0).cut(s, off)));
            }
            break;
        }
        let full = w.len() / pk;
        let k = w.chunks_exact(pk).take_while(|p| p[sync] == 0x47).count();
        n += k as u64;
        st.advance((k * pk) as u64);
        if k < full {
            break;
        }
    }
    Ok((n >= MIN_TS_PACKETS).then(|| Hit::new(ext, n * pk as u64)))
}

const PS_MAX: u64 = 64 * GIB;

/// MPEG program streams (DVD .VOB, .MPG): pack headers and packets, each with its length, up to
/// the end code or until they stop.
fn ps(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let mut st = Stream::new(*s, off);
    let mut packs = 0u64;
    loop {
        if st.offset() - off > PS_MAX {
            break;
        }
        let b = st.peek(16)?;
        if b.len() < 6 || !has(b, 0, &[0, 0, 1]) {
            break;
        }
        let len = match b[3] {
            0xBA if b.len() >= 14 && b[4] >> 6 == 1 => 14 + u64::from(b[13] & 7),
            0xBA if b[4] >> 4 == 2 => 12,
            0xB9 => {
                st.advance(4);
                break;
            }
            0xBB..=0xFF => 6 + be(b, 4, 2),
            _ => break,
        };
        packs += u64::from(b[3] == 0xBA);
        st.advance(len);
    }
    let e = st.offset();
    Ok((packs >= 16).then(|| Hit::new("mpg", e - off)))
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

// ---------- saving

/// The list of what `scan_and_save` saved, one JSON object a line, in the destination folder.
pub const INDEX: &str = "found.jsonl";

#[derive(Serialize)]
struct Saved<'a> {
    #[serde(flatten)]
    found: &'a Found,
    /// where it was saved, from the destination folder
    file: &'a str,
    #[serde(skip_serializing_if = "is_zero")]
    unreadable: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// Copy a found file out to `dest_dir/<group>/f<sector>.<ext>` (named after where it starts, like
/// PhotoRec, so the name is the same every time), never overwriting anything: a name that's taken
/// by something else gets -1, -2… Saving the same file again finds it already there.
pub fn save(src: &Path, f: &Found, dest_dir: &Path) -> Result<PathBuf> {
    check_destination(src, dest_dir)?;
    let input =
        File::open(src).with_context(|| format!("can't open {} to read it", src.display()))?;
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
    let input =
        File::open(src).with_context(|| format!("can't open {} to read it", src.display()))?;
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
fn check_destination(src: &Path, dest_dir: &Path) -> Result<()> {
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
fn file_stem(offset: u64) -> String {
    if offset.is_multiple_of(512) {
        format!("f{:010}", offset / 512)
    } else {
        format!("f{:010}_{}", offset / 512, offset % 512)
    }
}

/// Save `f` from the open source; the path, and how many bytes couldn't be read.
fn save_from(input: &File, f: &Found, dest_dir: &Path) -> Result<(PathBuf, u64)> {
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
fn copy_out(input: &File, f: &Found, mut out: File) -> io::Result<u64> {
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
fn same_bytes(input: &File, f: &Found, path: &Path) -> Result<bool> {
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
fn existing(p: &Path) -> Option<PathBuf> {
    let abs = std::path::absolute(p).ok()?;
    abs.ancestors().find(|a| a.exists()).map(Path::to_path_buf)
}

#[cfg(target_os = "linux")]
fn shares_disk(_src: &Path, src_dev: u64, dest: &Path, dest_dev: u64) -> bool {
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
fn linux_disks(dev: u64) -> Vec<PathBuf> {
    let major = ((dev >> 32) & 0xFFFF_F000) | ((dev >> 8) & 0xFFF);
    let minor = ((dev >> 12) & 0xFFFF_FF00) | (dev & 0xFF);
    let mut out = Vec::new();
    if let Ok(p) = fs::canonicalize(format!("/sys/dev/block/{major}:{minor}")) {
        disks_under(&p, &mut out, 0);
    }
    out
}

#[cfg(target_os = "linux")]
fn disks_under(p: &Path, out: &mut Vec<PathBuf>, depth: u32) {
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
fn mount_source(dest: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    let p = fs::canonicalize(existing(dest)?).ok()?;
    let info = fs::read_to_string("/proc/self/mountinfo").ok()?;
    let mut best: Option<(usize, String)> = None;
    for line in info.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        let (Some(mp), Some(dash)) = (f.get(4), f.iter().position(|x| *x == "-")) else {
            continue;
        };
        let (mp, Some(source)) = (unescape(mp), f.get(dash + 2)) else {
            continue;
        };
        if p.starts_with(&mp) && best.as_ref().is_none_or(|(l, _)| mp.len() >= *l) {
            best = Some((mp.len(), unescape(source)));
        }
    }
    let (_, source) = best?;
    if !source.starts_with("/dev/") {
        return None;
    }
    fs::metadata(&source).ok().map(|m| m.rdev())
}

/// mountinfo writes spaces and the like as octal escapes (`\040`).
#[cfg(target_os = "linux")]
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && let Some(v) = b
                .get(i + 1..i + 4)
                .and_then(|d| std::str::from_utf8(d).ok())
                .and_then(|d| u8::from_str_radix(d, 8).ok())
        {
            out.push(v);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// macOS, best effort: the destination's volume is a slice of the same disk (`disk4s2` of
/// `/dev/rdisk4`). APFS volumes sit on a synthesized container disk, which this doesn't see through.
#[cfg(target_os = "macos")]
fn shares_disk(src: &Path, _src_dev: u64, _dest: &Path, dest_dev: u64) -> bool {
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
fn shares_disk(_src: &Path, _src_dev: u64, _dest: &Path, _dest_dev: u64) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes that are no file's signature, after a file: its end has to come from its structure.
    fn junk(n: usize) -> Vec<u8> {
        (0..n).map(|i| 0x20 + (i * 7 % 90) as u8).collect()
    }

    fn with_junk(mut v: Vec<u8>) -> Vec<u8> {
        v.extend(junk(3000));
        v
    }

    fn carve_with(img: &[u8], opts: &Options) -> Vec<Found> {
        let m = img.to_vec();
        let mut out = Vec::new();
        scan_medium(
            &m,
            m.len() as u64,
            opts,
            &mut |_| {},
            &AtomicBool::new(false),
            &mut |f| {
                out.push(f.clone());
                Ok(())
            },
        )
        .unwrap();
        out
    }

    fn carve(img: &[u8]) -> Vec<Found> {
        carve_with(img, &Options::default())
    }

    /// What the parser for the signature at the start of `b` makes of it.
    fn one(b: &[u8]) -> Option<Hit> {
        let s = Src::mem(b);
        parse(sniff(b)?, &s, 0).unwrap()
    }

    fn at(img: &mut Vec<u8>, off: usize, b: &[u8]) {
        if img.len() < off + b.len() {
            img.resize(off + b.len(), 0x33);
        }
        img[off..off + b.len()].copy_from_slice(b);
    }

    // ---------- JPEG

    fn seg(m: u8, p: &[u8]) -> Vec<u8> {
        let mut v = vec![0xFF, m];
        v.extend(((p.len() + 2) as u16).to_be_bytes());
        v.extend(p);
        v
    }

    fn jpeg_head(sof: u8) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        v.extend(seg(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"));
        v.extend(seg(0xDB, &[0; 65]));
        v.extend(seg(sof, &[8, 0, 6, 0, 8, 1, 1, 0x11, 0]));
        v.extend(seg(0xC4, &[0; 17]));
        v
    }

    fn sos() -> Vec<u8> {
        seg(0xDA, &[1, 1, 0, 0, 63, 0])
    }

    fn baseline() -> Vec<u8> {
        let mut v = jpeg_head(0xC0);
        v.extend(sos());
        v.extend([
            0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD3, 0x56, 0xFF, 0xFF, 0xD4, 0x78,
        ]);
        v.extend([0xFF, 0xD9]);
        v
    }

    #[test]
    fn jpeg_ends_at_its_end_marker_past_stuffing_and_restarts() {
        let j = baseline();
        let h = one(&with_junk(j.clone())).unwrap();
        assert_eq!((h.ext, h.size, h.complete), ("jpg", j.len() as u64, true));
        assert_eq!(h.dims, Some((8, 6)));
    }

    #[test]
    fn progressive_jpeg_has_several_scans() {
        let mut j = jpeg_head(0xC2);
        j.extend(sos());
        j.extend([1, 2, 3, 0xFF, 0x00, 4]);
        j.extend(seg(0xC4, &[0x10; 17]));
        j.extend(sos());
        j.extend([5, 6, 0xFF, 0xD0, 7]);
        j.extend([0xFF, 0xD9]);
        let h = one(&with_junk(j.clone())).unwrap();
        assert_eq!((h.size, h.complete), (j.len() as u64, true));
    }

    #[test]
    fn jpeg_signature_in_junk_is_nothing() {
        assert!(one(&with_junk(vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16, 1, 2, 3])).is_none());
        // no scan before the end marker
        let mut j = jpeg_head(0xC0);
        j.extend([0xFF, 0xD9]);
        assert!(one(&with_junk(j)).is_none());
        // a scan without the tables before it
        let mut j = vec![0xFF, 0xD8];
        j.extend(sos());
        j.extend([1, 2, 3, 0xFF, 0xD9]);
        assert!(one(&with_junk(j)).is_none());
        // FF D8 FF followed by a marker that can't open a JPEG
        assert!(one(&with_junk(vec![0xFF, 0xD8, 0xFF, 0x05, 0, 4, 0, 0])).is_none());
    }

    #[test]
    fn a_split_up_jpeg_is_kept_to_where_it_breaks() {
        let mut j = jpeg_head(0xC0);
        j.extend(sos());
        j.extend([1, 2, 3, 4, 5]);
        let broken_at = j.len();
        j.extend([0xFF, 0xD8, 0xFF, 0xE0]); // another photo's start, where this one's data should go on
        let h = one(&with_junk(j)).unwrap();
        assert_eq!((h.size, h.complete), (broken_at as u64, false));
        assert!(h.note.unwrap().contains("damaged"));
        // cut off by the end of what's scanned
        let mut j = jpeg_head(0xC0);
        j.extend(sos());
        j.extend([1, 2, 3, 4, 5]);
        let h = one(&j).unwrap();
        assert_eq!((h.size, h.complete), (j.len() as u64, false));
    }

    #[test]
    fn jpeg_mpf_preview_is_part_of_the_photo() {
        let preview = baseline();
        let mut rest = jpeg_head(0xC0)[2..].to_vec();
        rest.extend(sos());
        rest.extend([9, 8, 7]);
        rest.extend([0xFF, 0xD9]);
        // APP2 "MPF": a TIFF-style index of two images; offsets from the index's TIFF header
        let app2_len = 4 + 4 + 8 + 2 + 2 * 12 + 4 + 32;
        let main_len = 2 + app2_len + rest.len();
        let base = 2 + 4 + 4;
        let mut t = b"II*\0".to_vec();
        t.extend(8u32.to_le_bytes());
        t.extend(2u16.to_le_bytes());
        t.extend([0x00, 0xB0, 7, 0, 4, 0, 0, 0]);
        t.extend(b"0100");
        t.extend([0x02, 0xB0, 7, 0, 32, 0, 0, 0]);
        t.extend(38u32.to_le_bytes());
        t.extend(0u32.to_le_bytes());
        for (size, o) in [(main_len, 0), (preview.len(), main_len - base)] {
            t.extend(0u32.to_le_bytes());
            t.extend((size as u32).to_le_bytes());
            t.extend((o as u32).to_le_bytes());
            t.extend([0; 4]);
        }
        let mut p = b"MPF\0".to_vec();
        p.extend(t);
        let mut j = vec![0xFF, 0xD8];
        j.extend(seg(0xE2, &p));
        assert_eq!(j.len(), 2 + app2_len);
        j.extend(rest);
        j.extend(&preview);
        let h = one(&with_junk(j.clone())).unwrap();
        assert_eq!((h.size, h.complete), (j.len() as u64, true));
    }

    #[test]
    fn exif_dates_and_cameras() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(exif_time("2019:08:14 10:22:33"), Some(1_565_778_153));
        assert_eq!(exif_time("0000:00:00 00:00:00"), None);
        assert_eq!(exif_time("    :  :     :  :  "), None);
        assert_eq!(exif_time("2019:13:14 10:22:33"), None);
        let c = |a: Option<&str>, b: Option<&str>| camera(a.map(Into::into), b.map(Into::into));
        assert_eq!(
            c(Some("Canon"), Some("Canon EOS 80D")).unwrap(),
            "Canon EOS 80D"
        );
        assert_eq!(
            c(Some("NIKON CORPORATION"), Some("NIKON D750")).unwrap(),
            "NIKON D750"
        );
        assert_eq!(
            c(Some("Apple"), Some("iPhone 12")).unwrap(),
            "Apple iPhone 12"
        );
        assert_eq!(c(None, Some("X100")).unwrap(), "X100");
        assert_eq!(c(None, None), None);
    }

    // ---------- PNG, GIF, BMP, RIFF

    fn chunk(typ: &[u8], data: &[u8]) -> Vec<u8> {
        let mut v = (data.len() as u32).to_be_bytes().to_vec();
        v.extend(typ);
        v.extend(data);
        v.extend(crc32_update(crc32(typ), data).to_be_bytes());
        v
    }

    fn png_file() -> Vec<u8> {
        let mut v = PNG_SIG.to_vec();
        v.extend(chunk(b"IHDR", &[0, 0, 0, 3, 0, 0, 0, 2, 8, 6, 0, 0, 0]));
        v.extend(chunk(b"IDAT", &[0x78, 1, 1, 2, 0, 0xFD, 0xFF, 0, 0]));
        v.extend(chunk(b"IEND", b""));
        v
    }

    #[test]
    fn png_chunks_and_their_crcs() {
        let p = png_file();
        let h = one(&with_junk(p.clone())).unwrap();
        assert_eq!(
            (h.ext, h.size, h.complete, h.dims),
            ("png", p.len() as u64, true, Some((3, 2)))
        );
        // a wrong CRC in the header: not a PNG
        let mut bad = p.clone();
        bad[29] ^= 1;
        assert!(one(&with_junk(bad)).is_none());
        // a wrong CRC after the image data: kept up to there
        let mut bad = p.clone();
        let iend = p.len() - 12;
        bad[iend + 8] ^= 1;
        let h = one(&with_junk(bad)).unwrap();
        assert_eq!((h.size, h.complete), (iend as u64, false));
    }

    #[test]
    fn gif_bmp_and_riff() {
        let mut g = b"GIF89a\x01\0\x01\0\x80\0\0".to_vec();
        g.extend([0, 0, 0, 255, 255, 255]);
        g.extend([0x21, 0xF9, 4, 0, 0, 0, 0, 0]);
        g.extend([0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, 0x4C, 1, 0, 0x3B]);
        let h = one(&with_junk(g.clone())).unwrap();
        assert_eq!(
            (h.ext, h.size, h.dims),
            ("gif", g.len() as u64, Some((1, 1)))
        );

        let mut b = b"BM".to_vec();
        b.extend(70u32.to_le_bytes());
        b.extend([0; 4]);
        b.extend(54u32.to_le_bytes());
        b.extend(40u32.to_le_bytes());
        b.extend(2i32.to_le_bytes());
        b.extend(2i32.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(24u16.to_le_bytes());
        b.extend([0; 24]);
        b.extend([0x10; 16]);
        let h = one(&with_junk(b.clone())).unwrap();
        assert_eq!((h.ext, h.size, h.dims), ("bmp", 70, Some((2, 2))));
        // a size that doesn't fit the pixels
        b[2..6].copy_from_slice(&60u32.to_le_bytes());
        assert!(one(&with_junk(b)).is_none());

        let mut w = b"RIFF".to_vec();
        w.extend((4 + 24 + 8 + 100u32).to_le_bytes());
        w.extend(b"WAVEfmt ");
        w.extend(16u32.to_le_bytes());
        w.extend([1, 0, 1, 0, 0x44, 0xAC, 0, 0, 0x88, 0x58, 1, 0, 2, 0, 16, 0]);
        w.extend(b"data");
        w.extend(100u32.to_le_bytes());
        w.extend([0x11; 100]);
        let h = one(&with_junk(w.clone())).unwrap();
        assert_eq!((h.ext, h.size, h.complete), ("wav", w.len() as u64, true));
        // chunks that don't add up to the RIFF size
        w[40..44].copy_from_slice(&90u32.to_le_bytes());
        let h = one(&with_junk(w.clone())).unwrap();
        assert!(!h.complete);
        // a recording whose sizes were never written: up to the next file
        w[4..8].copy_from_slice(&0u32.to_le_bytes());
        w[40..44].copy_from_slice(&0u32.to_le_bytes());
        w.resize(1024, 0x11);
        w.extend(png_file());
        w.extend(junk(600));
        let h = one(&w).unwrap();
        assert_eq!((h.ext, h.size, h.complete), ("wav", 1024, false));

        let mut p = b"RIFF".to_vec();
        p.extend((4 + 8 + 10u32).to_le_bytes());
        p.extend(b"WEBPVP8L");
        p.extend(10u32.to_le_bytes());
        // signature, then width-1 and height-1 in 14 bits each
        let dims: u32 = 99 | (49 << 14);
        p.push(0x2F);
        p.extend(dims.to_le_bytes());
        p.extend([0; 5]);
        let h = one(&with_junk(p.clone())).unwrap();
        assert_eq!(
            (h.ext, h.size, h.dims),
            ("webp", p.len() as u64, Some((100, 50)))
        );
    }

    // ---------- ISO media

    fn bx(t: &[u8], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend(t);
        v.extend(body);
        v
    }

    fn ftyp(brand: &[u8]) -> Vec<u8> {
        let mut b = brand.to_vec();
        b.extend([0, 0, 0, 0]);
        b.extend(brand);
        bx(b"ftyp", &b)
    }

    fn moov() -> Vec<u8> {
        let mut mvhd = vec![0u8; 100];
        mvhd[4..8].copy_from_slice(&(1_600_000_000u32 + 2_082_844_800).to_be_bytes());
        bx(b"moov", &bx(b"mvhd", &mvhd))
    }

    #[test]
    fn iso_brands() {
        assert_eq!(iso_ext(b"qt  ", b""), "mov");
        assert_eq!(iso_ext(b"heic", b"mif1heic"), "heic");
        assert_eq!(iso_ext(b"mif1", b"mif1avif"), "avif");
        assert_eq!(iso_ext(b"M4A ", b"M4A isom"), "m4a");
        assert_eq!(iso_ext(b"3gp5", b""), "3gp");
        assert_eq!(iso_ext(b"crx ", b""), "cr3");
        assert_eq!(iso_ext(b"isom", b"isomavc1"), "mp4");
    }

    #[test]
    fn iso_boxes_large_sizes_and_open_ends() {
        let mut v = ftyp(b"isom");
        v.extend(moov());
        // a 64-bit size
        let data = [0x42u8; 1000];
        v.extend(1u32.to_be_bytes());
        v.extend(b"mdat");
        v.extend((16 + data.len() as u64).to_be_bytes());
        v.extend(data);
        let h = one(&with_junk(v.clone())).unwrap();
        assert_eq!((h.ext, h.size, h.complete), ("mp4", v.len() as u64, true));
        assert!(h.taken.is_some());

        // size 0: "to the end of the file", which a carver doesn't know: up to the next file
        let mut v = ftyp(b"qt  ");
        v.extend(moov());
        v.extend(0u32.to_be_bytes());
        v.extend(b"mdat");
        v.resize(4096, 0x42);
        v.extend(png_file());
        v.extend(junk(1000));
        let h = one(&v).unwrap();
        assert_eq!((h.ext, h.size, h.complete), ("mov", 4096, false));

        // a recording that was never finished: media data but no index
        let mut v = ftyp(b"isom");
        v.extend(bx(b"mdat", &[1; 500]));
        let h = one(&with_junk(v.clone())).unwrap();
        assert_eq!((h.size, h.complete), (v.len() as u64, false));

        // an ftyp alone isn't a file
        assert!(one(&with_junk(ftyp(b"isom"))).is_none());

        // HEIC: meta and mdat, no moov
        let mut v = ftyp(b"heic");
        v.extend(bx(b"meta", &[0; 40]));
        v.extend(bx(b"mdat", &[7; 300]));
        let h = one(&with_junk(v.clone())).unwrap();
        assert_eq!((h.ext, h.size, h.complete), ("heic", v.len() as u64, true));
    }

    #[test]
    fn old_quicktime_movies_back_to_back() {
        let mut m = bx(b"wide", b"");
        m.extend(bx(b"mdat", &[5; 700]));
        m.extend(moov());
        let mut img = m.clone();
        let second = img.len().div_ceil(512) * 512;
        img.resize(second, 0x33);
        img.extend(&m);
        img.extend(junk(2000));
        let f = carve(&img);
        let got: Vec<(u64, u64, &str)> = f.iter().map(|f| (f.offset, f.size, f.ext)).collect();
        assert_eq!(
            got,
            [
                (0, m.len() as u64, "mov"),
                (second as u64, m.len() as u64, "mov")
            ]
        );
        // two exactly a sector long, one right after the other with no gap: still two
        let mut m = bx(b"wide", b"");
        m.extend(bx(b"mdat", &[6; 1024 - 8 - 8 - 116]));
        m.extend(moov());
        assert_eq!(m.len(), 1024);
        let mut img = m.clone();
        img.extend(&m);
        img.extend(junk(2000));
        let got: Vec<(u64, u64)> = carve(&img).iter().map(|f| (f.offset, f.size)).collect();
        assert_eq!(got, [(0, 1024), (1024, 1024)]);
    }

    // ---------- PDF

    fn pdf_file(extra_eof_at_start: bool) -> Vec<u8> {
        let mut v = b"%PDF-1.7\n".to_vec();
        if extra_eof_at_start {
            // a linearized file's first trailer points at 0
            v.extend(b"trailer\n<< >>\nstartxref\n0\n%%EOF\n");
        }
        let o1 = v.len();
        v.extend(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let x = v.len();
        v.extend(format!("xref\n0 2\n0000000000 65535 f \n{o1:010} 00000 n \ntrailer\n<< /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\r\n").as_bytes());
        v
    }

    #[test]
    fn pdf_ends_at_its_own_last_eof() {
        let p = pdf_file(true);
        let h = one(&with_junk(p.clone())).unwrap();
        assert_eq!((h.ext, h.size, h.complete), ("pdf", p.len() as u64, true));

        // another PDF right after (its startxref counts from its own start)
        let mut img = p.clone();
        img.resize(img.len().div_ceil(512) * 512, b' ');
        let second = img.len();
        img.extend(pdf_file(false));
        img.extend(junk(1000));
        let f = carve(&img);
        let got: Vec<(u64, u64)> = f.iter().map(|f| (f.offset, f.size)).collect();
        assert_eq!(
            got,
            [
                (0, p.len() as u64),
                (second as u64, img.len() as u64 - 1000 - second as u64)
            ]
        );
    }

    #[test]
    fn a_pdf_without_its_end_stops_at_the_next_file() {
        let mut v = b"%PDF-1.4\n1 0 obj\n<< >>\nendobj\n".to_vec();
        v.resize(2048, b' ');
        let next = v.len();
        v.extend(png_file());
        v.extend(junk(2000));
        let h = one(&v).unwrap();
        assert_eq!((h.size, h.complete), (next as u64, false));
        // slack after the end that happens to look like an update: still ends at the last good
        // %%EOF, found once the next file shows up
        let p = pdf_file(false);
        let mut img = p.clone();
        img.extend(b"12 junk that looks like an update");
        img.resize(img.len().div_ceil(512) * 512, b'7');
        let next = img.len();
        img.extend(png_file());
        img.extend(junk(600));
        let got: Vec<(u64, u64)> = carve(&img).iter().map(|f| (f.offset, f.size)).collect();
        assert_eq!(
            got,
            [(0, p.len() as u64), (next as u64, png_file().len() as u64)]
        );
        // a startxref pointing at something that isn't a table doesn't end it
        let mut v = b"%PDF-1.4\nhello\nstartxref\n9\n%%EOF\n".to_vec();
        v.extend(junk(100));
        let h = one(&v).unwrap();
        assert!(!h.complete);
    }

    // ---------- ZIP

    fn local(name: &str, data: &[u8], streamed: bool) -> Vec<u8> {
        let mut v = b"PK\x03\x04".to_vec();
        v.extend(20u16.to_le_bytes());
        v.extend((if streamed { 8u16 } else { 0 }).to_le_bytes());
        v.extend([0; 6]);
        let (crc, size) = if streamed {
            (0, 0)
        } else {
            (crc32(data), data.len() as u32)
        };
        v.extend(crc.to_le_bytes());
        v.extend(size.to_le_bytes());
        v.extend(size.to_le_bytes());
        v.extend((name.len() as u16).to_le_bytes());
        v.extend(0u16.to_le_bytes());
        v.extend(name.as_bytes());
        v.extend(data);
        if streamed {
            v.extend(b"PK\x07\x08");
            v.extend(crc32(data).to_le_bytes());
            v.extend((data.len() as u32).to_le_bytes());
            v.extend((data.len() as u32).to_le_bytes());
        }
        v
    }

    fn central(name: &str, data: &[u8], at: usize) -> Vec<u8> {
        let mut v = b"PK\x01\x02".to_vec();
        v.extend([20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        v.extend(crc32(data).to_le_bytes());
        v.extend((data.len() as u32).to_le_bytes());
        v.extend((data.len() as u32).to_le_bytes());
        v.extend((name.len() as u16).to_le_bytes());
        v.extend([0; 12]);
        v.extend((at as u32).to_le_bytes());
        v.extend(name.as_bytes());
        v
    }

    fn zip_file(entries: &[(&str, &[u8])], streamed: bool, zip64: bool) -> Vec<u8> {
        let mut v = Vec::new();
        let mut cd = Vec::new();
        for (n, d) in entries {
            cd.extend(central(n, d, v.len()));
            v.extend(local(n, d, streamed));
        }
        let cd_at = v.len();
        v.extend(&cd);
        if zip64 {
            let rec = v.len();
            v.extend(b"PK\x06\x06");
            v.extend(44u64.to_le_bytes());
            v.extend([45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            v.extend((entries.len() as u64).to_le_bytes());
            v.extend((entries.len() as u64).to_le_bytes());
            v.extend((cd.len() as u64).to_le_bytes());
            v.extend((cd_at as u64).to_le_bytes());
            v.extend(b"PK\x06\x07");
            v.extend(0u32.to_le_bytes());
            v.extend((rec as u64).to_le_bytes());
            v.extend(1u32.to_le_bytes());
            v.extend(b"PK\x05\x06");
            v.extend([0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF]);
            v.extend([0xFF; 8]);
            v.extend(0u16.to_le_bytes());
        } else {
            v.extend(b"PK\x05\x06");
            v.extend([0; 4]);
            v.extend((entries.len() as u16).to_le_bytes());
            v.extend((entries.len() as u16).to_le_bytes());
            v.extend((cd.len() as u32).to_le_bytes());
            v.extend((cd_at as u32).to_le_bytes());
            v.extend(5u16.to_le_bytes());
            v.extend(b"hello");
        }
        v
    }

    #[test]
    fn zip_end_record_has_to_agree_with_the_start() {
        let z = zip_file(&[("a.txt", b"alpha"), ("b.txt", b"beta")], false, false);
        let h = one(&with_junk(z.clone())).unwrap();
        assert_eq!((h.ext, h.size, h.complete), ("zip", z.len() as u64, true));

        // written as a stream (sizes after the data): the end record is searched for
        let z = zip_file(
            &[
                ("[Content_Types].xml", b"<Types/>"),
                ("xl/workbook.xml", b"<w/>"),
            ],
            true,
            false,
        );
        let h = one(&with_junk(z.clone())).unwrap();
        assert_eq!((h.ext, h.size, h.complete), ("xlsx", z.len() as u64, true));

        // ZIP64 end records
        let z = zip_file(
            &[("[Content_Types].xml", b"<Types/>"), ("ppt/p.xml", b"<p/>")],
            false,
            true,
        );
        let h = one(&with_junk(z.clone())).unwrap();
        assert_eq!((h.ext, h.size, h.complete), ("pptx", z.len() as u64, true));

        // an OpenDocument says what it is in its first entry
        let z = zip_file(
            &[
                ("mimetype", b"application/vnd.oasis.opendocument.text"),
                ("content.xml", b"<x/>"),
            ],
            false,
            false,
        );
        assert_eq!(one(&with_junk(z)).unwrap().ext, "odt");

        // a ZIP inside a ZIP (stored): the inner end record doesn't end the outer one
        let inner = zip_file(&[("in.txt", b"inside")], false, false);
        let outer = zip_file(&[("inner.zip", &inner), ("c.txt", b"gamma")], true, false);
        let h = one(&with_junk(outer.clone())).unwrap();
        assert_eq!(h.size, outer.len() as u64);

        // no end record at all: kept up to what follows
        let mut z = local("a.txt", b"alpha", false);
        z.resize(1024, 0x33);
        z.extend(png_file());
        z.extend(junk(1000));
        let h = one(&z).unwrap();
        assert_eq!((h.size, h.complete), (1024, false));
    }

    // ---------- compound files, SQLite

    fn cfb_file(stream: &str) -> Vec<u8> {
        let free = 0xFFFF_FFFFu32;
        let mut h = vec![0u8; 512];
        h[..8].copy_from_slice(CFB_SIG);
        h[24..34].copy_from_slice(&[0x3E, 0, 3, 0, 0xFE, 0xFF, 9, 0, 6, 0]);
        h[44..48].copy_from_slice(&1u32.to_le_bytes());
        h[48..52].copy_from_slice(&1u32.to_le_bytes());
        h[56..60].copy_from_slice(&4096u32.to_le_bytes());
        h[60..64].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
        h[68..72].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
        for i in 0..109 {
            let v = if i == 0 { 0 } else { free };
            h[76 + 4 * i..80 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        let mut fat = vec![0xFFFF_FFFDu32, 0xFFFF_FFFE];
        fat.extend(3..=9u32);
        fat.push(0xFFFF_FFFE);
        fat.resize(128, free);
        let entry = |name: &str, typ: u8, child: u32, start: u32, size: u32| {
            let mut e = vec![0u8; 128];
            let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            e[..units.len()].copy_from_slice(&units);
            let nl = if name.is_empty() { 0 } else { units.len() + 2 };
            e[64..66].copy_from_slice(&(nl as u16).to_le_bytes());
            e[66] = typ;
            e[68..72].copy_from_slice(&free.to_le_bytes());
            e[72..76].copy_from_slice(&free.to_le_bytes());
            e[76..80].copy_from_slice(&child.to_le_bytes());
            e[116..120].copy_from_slice(&start.to_le_bytes());
            e[120..124].copy_from_slice(&size.to_le_bytes());
            e
        };
        let mut v = h;
        v.extend(fat.iter().flat_map(|x| x.to_le_bytes()));
        v.extend(entry("Root Entry", 5, 1, 0xFFFF_FFFE, 0));
        v.extend(entry(stream, 2, free, 2, 4096));
        v.extend(entry("", 0, free, 0, 0));
        v.extend(entry("", 0, free, 0, 0));
        v.extend(vec![0x44; 4096]);
        v
    }

    #[test]
    fn compound_files_are_named_by_their_streams() {
        for (stream, ext) in [
            ("WordDocument", "doc"),
            ("Workbook", "xls"),
            ("PowerPoint Document", "ppt"),
            ("__substg1.0_0037001F", "msg"),
            ("Something", "ole"),
        ] {
            let c = cfb_file(stream);
            let h = one(&with_junk(c.clone())).unwrap();
            assert_eq!(
                (h.ext, h.size, h.complete),
                (ext, c.len() as u64, true),
                "{stream}"
            );
        }
        // a FAT pointing past itself: not a compound file
        let mut c = cfb_file("WordDocument");
        c[512 + 8..512 + 12].copy_from_slice(&5000u32.to_le_bytes());
        assert!(one(&with_junk(c)).is_none());
    }

    #[test]
    fn sqlite_size_from_its_header() {
        let mut d = vec![0u8; 2 * 65_536];
        d[..16].copy_from_slice(b"SQLite format 3\0");
        d[16..18].copy_from_slice(&1u16.to_be_bytes()); // 65536
        d[18..24].copy_from_slice(&[1, 1, 0, 64, 32, 32]);
        d[24..28].copy_from_slice(&3u32.to_be_bytes());
        d[28..32].copy_from_slice(&2u32.to_be_bytes());
        d[92..96].copy_from_slice(&3u32.to_be_bytes());
        d[100] = 0x0D;
        let h = one(&with_junk(d.clone())).unwrap();
        assert_eq!((h.ext, h.size, h.complete), ("sqlite", 131_072, true));
        // a size that isn't current: up to the next file
        d[92..96].copy_from_slice(&2u32.to_be_bytes());
        let mut img = d.clone();
        img.extend(png_file());
        img.extend(junk(600));
        let h = one(&img).unwrap();
        assert_eq!((h.size, h.complete), (131_072, false));
    }

    // ---------- audio

    #[test]
    fn mpeg_audio_frame_lengths() {
        assert_eq!(mpa(0xFFFB_9000).unwrap().len, 417);
        assert_eq!(mpa(0xFFFB_9200).unwrap().len, 418);
        assert_eq!(mpa(0xFFF3_8000).unwrap().len, 208); // MPEG-2 layer III, 64 kbit/s, 22.05 kHz
        assert_eq!(mpa(0xFFFF_1000).unwrap().len, 32); // MPEG-1 layer I, 32 kbit/s, 44.1 kHz
        assert!(mpa(0xFFFB_F000).is_none()); // bad bitrate
        assert!(mpa(0xFFFB_9C00).is_none()); // reserved sample rate
        assert!(mpa(0xFFE9_9000).is_none()); // reserved version
    }

    fn frames(n: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..n {
            v.extend([0xFF, 0xFB, 0x90, 0x00]);
            v.extend((0..413).map(|k| ((i * 31 + k * 7) % 251) as u8));
        }
        v
    }

    #[test]
    fn mp3_with_and_without_a_tag() {
        // a long enough run of bare frames
        let f = frames(150);
        let h = one(&with_junk(f.clone())).unwrap();
        assert_eq!((h.ext, h.size), ("mp3", f.len() as u64));
        // a short run isn't enough without a tag
        assert!(one(&with_junk(frames(50))).is_none());
        // with an ID3v2 tag, a few frames are
        let mut t = b"ID3\x04\0\0\0\0\0\x0A".to_vec();
        t.extend([0; 10]);
        t.extend(frames(5));
        t.extend(b"TAG");
        t.extend([b' '; 125]);
        let h = one(&with_junk(t.clone())).unwrap();
        assert_eq!((h.ext, h.size, h.complete), ("mp3", t.len() as u64, true));
    }

    fn flac_file(n: usize) -> Vec<u8> {
        let mut v = b"fLaC".to_vec();
        v.extend([0x80, 0, 0, 34]);
        let mut si = vec![0u8; 34];
        si[0..2].copy_from_slice(&4096u16.to_be_bytes());
        si[2..4].copy_from_slice(&4096u16.to_be_bytes());
        si[7..10].copy_from_slice(&708u32.to_be_bytes()[1..]); // the longest frame
        let packed: u64 = (44_100 << 44) | (1 << 41) | (15 << 36) | (n as u64 * 4096);
        si[10..18].copy_from_slice(&packed.to_be_bytes());
        v.extend(si);
        for i in 0..n {
            let mut f = vec![0xFF, 0xF8, 0xC9, 0x18, i as u8];
            f.push(crc8(&f));
            f.extend((0..700).map(|k| ((i * 13 + k * 5) % 239) as u8));
            let mut crc = 0u16;
            for &x in &f {
                crc = (crc << 8) ^ CRC16[usize::from((crc >> 8) as u8 ^ x)];
            }
            f.extend(crc.to_be_bytes());
            v.extend(f);
        }
        v
    }

    #[test]
    fn flac_frames_end_where_the_samples_do() {
        let f = flac_file(6);
        let h = one(&with_junk(f.clone())).unwrap();
        assert_eq!((h.ext, h.size, h.complete), ("flac", f.len() as u64, true));
        // damaged in the middle: kept up to the frame that breaks
        let mut d = f.clone();
        let third = 8 + 34 + 2 * 708;
        d[third + 4] ^= 0x40;
        d[third + 100] ^= 0x40;
        let h = one(&with_junk(d)).unwrap();
        assert!(!h.complete);
        assert_eq!(h.size, third as u64);
    }

    fn ogg_page(flags: u8, seq: u32, body: &[u8]) -> Vec<u8> {
        let mut p = b"OggS\0".to_vec();
        p.push(flags);
        p.extend(0u64.to_le_bytes());
        p.extend(0x1234u32.to_le_bytes());
        p.extend(seq.to_le_bytes());
        p.extend([0; 4]);
        let mut table = vec![255u8; body.len() / 255];
        table.push((body.len() % 255) as u8);
        p.push(table.len() as u8);
        p.extend(table);
        p.extend(body);
        let crc = ogg_crc(0, &p);
        p[22..26].copy_from_slice(&crc.to_le_bytes());
        p
    }

    #[test]
    fn ogg_pages_with_their_crcs() {
        let mut o = ogg_page(2, 0, b"\x01vorbis\0\0\0\0\x02\x44\xAC\0\0");
        o.extend(ogg_page(0, 1, &[3; 600]));
        o.extend(ogg_page(4, 2, &[4; 100]));
        let h = one(&with_junk(o.clone())).unwrap();
        assert_eq!((h.ext, h.size, h.complete), ("ogg", o.len() as u64, true));
        let mut o = ogg_page(2, 0, b"OpusHead\x01\x02");
        o.extend(ogg_page(4, 1, &[1; 10]));
        assert_eq!(one(&with_junk(o)).unwrap().ext, "opus");
        // a page with a wrong CRC ends the file (without its end-of-stream page: incomplete)
        let mut o = ogg_page(2, 0, b"\x01vorbis\0\0");
        let good = {
            o.extend(ogg_page(0, 1, &[3; 60]));
            o.len()
        };
        let mut bad = ogg_page(4, 2, &[4; 60]);
        bad[30] ^= 1;
        o.extend(bad);
        let h = one(&with_junk(o)).unwrap();
        assert_eq!((h.size, h.complete), (good as u64, false));
    }

    // ---------- video, archives, raw

    fn ebml_file(unknown: bool) -> Vec<u8> {
        let mut v = vec![0x1A, 0x45, 0xDF, 0xA3, 0x8B, 0x42, 0x82, 0x88];
        v.extend(b"matroska");
        let mut kids = vec![0x15, 0x49, 0xA9, 0x66, 0x84, 1, 2, 3, 4];
        kids.extend([0x1F, 0x43, 0xB6, 0x75]);
        if unknown {
            kids.push(0xFF); // a cluster of unknown size: its children follow
            kids.extend([0xE7, 0x81, 0x00, 0xA3, 0x85, 0x81, 0, 0, 0x80, 9]);
            kids.extend([0xA3, 0x83, 0x81, 0, 1]);
        } else {
            kids.push(0x90);
            kids.extend([0x55; 16]);
        }
        v.extend([0x18, 0x53, 0x80, 0x67]);
        if unknown {
            v.extend([0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        } else {
            v.push(0x01);
            v.extend(&(kids.len() as u64).to_be_bytes()[1..]);
        }
        v.extend(kids);
        v
    }

    #[test]
    fn matroska_known_and_unknown_sizes() {
        let m = ebml_file(false);
        let h = one(&with_junk(m.clone())).unwrap();
        assert_eq!((h.ext, h.size, h.complete), ("mkv", m.len() as u64, true));
        let m = ebml_file(true);
        let h = one(&with_junk(m.clone())).unwrap();
        assert_eq!((h.ext, h.size), ("mkv", m.len() as u64));
    }

    #[test]
    fn rar_and_7z() {
        let block = |body: &[u8]| {
            let mut head = vec![body.len() as u8];
            head.extend(body);
            let mut v = crc32(&head).to_le_bytes().to_vec();
            v.extend(head);
            v
        };
        let mut r = b"Rar!\x1a\x07\x01\x00".to_vec();
        r.extend(block(&[1, 0, 0]));
        r.extend(block(&[5, 0, 0]));
        let h = one(&with_junk(r.clone())).unwrap();
        assert_eq!((h.ext, h.size, h.complete), ("rar", r.len() as u64, true));

        let old = |typ: u8, flags: u16, rest: &[u8]| {
            let mut b = vec![typ];
            b.extend(flags.to_le_bytes());
            b.extend(((5 + rest.len()) as u16 + 2).to_le_bytes());
            b.extend(rest);
            let mut v = ((crc32(&b) & 0xFFFF) as u16).to_le_bytes().to_vec();
            v.extend(b);
            v
        };
        let mut r = b"Rar!\x1a\x07\x00".to_vec();
        r.extend(old(0x73, 0, &[0; 6]));
        r.extend(old(0x7B, 0x4000, &[]));
        let h = one(&with_junk(r.clone())).unwrap();
        assert_eq!((h.ext, h.size), ("rar", r.len() as u64));

        let next = [1u8, 4, 6, 0, 1, 9, 0x20, 0, 0, 0];
        let mut start = 100u64.to_le_bytes().to_vec();
        start.extend((next.len() as u64).to_le_bytes());
        start.extend(crc32(&next).to_le_bytes());
        let mut z = SEVENZ_SIG.to_vec();
        z.extend([0, 4]);
        z.extend(crc32(&start).to_le_bytes());
        z.extend(start);
        z.extend([0x77; 100]);
        z.extend(next);
        let h = one(&with_junk(z.clone())).unwrap();
        assert_eq!((h.ext, h.size, h.complete), ("7z", z.len() as u64, true));
    }

    fn tiff_file(make: Option<&str>) -> Vec<u8> {
        let e = |tag: u16, typ: u16, count: u32, value: u32| {
            let mut v = tag.to_le_bytes().to_vec();
            v.extend(typ.to_le_bytes());
            v.extend(count.to_le_bytes());
            v.extend(value.to_le_bytes());
            v
        };
        let n: u16 = if make.is_some() { 5 } else { 4 };
        let mut t = b"II*\0\x08\0\0\0".to_vec();
        t.extend(n.to_le_bytes());
        t.extend(e(0x100, 3, 1, 4));
        t.extend(e(0x101, 3, 1, 4));
        if let Some(m) = make {
            t.extend(e(0x10F, 2, m.len() as u32 + 1, 200));
        }
        t.extend(e(0x111, 4, 1, 300));
        t.extend(e(0x117, 4, 1, 48));
        t.extend(0u32.to_le_bytes());
        t.resize(200, 0);
        if let Some(m) = make {
            t.extend(m.as_bytes());
        }
        t.resize(300, 0);
        t.extend([0x80; 48]);
        t
    }

    #[test]
    fn tiff_and_camera_raw() {
        let t = tiff_file(None);
        let h = one(&with_junk(t.clone())).unwrap();
        assert_eq!((h.ext, h.size, h.dims), ("tif", 348, Some((4, 4))));
        let t = tiff_file(Some("NIKON CORPORATION"));
        let h = one(&with_junk(t)).unwrap();
        assert_eq!((h.ext, h.size), ("nef", 348));
        assert_eq!(h.camera.as_deref(), Some("NIKON CORPORATION"));
        // a TIFF header over junk isn't a file
        assert!(one(&with_junk(b"II*\0\x08\0\0\0\xFF\xFF".to_vec())).is_none());
    }

    #[test]
    fn camcorder_streams() {
        let mut m = Vec::new();
        for i in 0..300u32 {
            m.extend(i.to_be_bytes());
            m.push(0x47);
            m.extend(if i == 0 { [0x40, 0x00] } else { [0x01, 0x00] });
            m.extend([0x10; 185]);
        }
        let h = one(&with_junk(m.clone())).unwrap();
        assert_eq!((h.ext, h.size), ("mts", m.len() as u64));

        let mut p = Vec::new();
        for _ in 0..20 {
            p.extend([0, 0, 1, 0xBA, 0x44, 0, 4, 0, 4, 1, 1, 0x89, 0xC3, 0xF8]);
            p.extend([0, 0, 1, 0xE0, 0, 10]);
            p.extend([0x42; 10]);
        }
        p.extend([0, 0, 1, 0xB9]);
        let h = one(&with_junk(p.clone())).unwrap();
        assert_eq!((h.ext, h.size), ("mpg", p.len() as u64));
    }

    // ---------- the scan

    #[test]
    fn files_are_looked_for_at_sector_boundaries_only() {
        let mut img = junk(40_000);
        at(&mut img, 4096 * 2, &png_file());
        at(&mut img, 4096 * 4 + 512, &png_file());
        at(&mut img, 4096 * 6 + 3, &png_file());
        let offs = |sector| {
            carve_with(
                &img,
                &Options {
                    sector,
                    ..Options::default()
                },
            )
            .iter()
            .map(|f| f.offset)
            .collect::<Vec<_>>()
        };
        assert_eq!(offs(512), [8192, 16_896]);
        assert_eq!(offs(4096), [8192]);
        assert_eq!(offs(1), [8192, 16_896, 24_579]);
    }

    #[test]
    fn choosing_types() {
        let want =
            |k: &[&str]| Want::new(Some(&k.iter().map(|x| x.to_string()).collect::<Vec<_>>()));
        let w = want(&["Photos"]).unwrap();
        assert!(w.reports("jpg") && w.reports("cr3") && !w.reports("mp4"));
        assert!(w.on[Fmt::Iso as usize] && !w.on[Fmt::Pdf as usize]);
        let w = want(&[".JPEG", "video"]).unwrap();
        assert!(w.reports("jpg") && w.reports("mov") && !w.reports("png"));
        assert!(want(&["raw"]).unwrap().reports("nef"));
        assert!(want(&["office"]).unwrap().reports("doc"));
        assert!(want(&["nope"]).is_err());
        assert!(want(&[]).is_err());
        assert!(Want::new(None).unwrap().reports("anything"));
        for t in TYPES {
            assert!(
                Fmt::ALL.iter().any(|f| f.exts().contains(&t.0)),
                "{} has no parser",
                t.0
            );
        }
        for f in Fmt::ALL {
            for e in f.exts() {
                assert!(TYPES.iter().any(|t| t.0 == *e), "{e} isn't described");
            }
        }
    }

    /// A source with a bad area: reads touching it fail.
    struct Flaky {
        data: Vec<u8>,
        bad: std::ops::Range<u64>,
    }

    impl Medium for Flaky {
        fn pread(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
            if off < self.bad.end && off + buf.len() as u64 > self.bad.start {
                return Err(io::Error::other("bad sector"));
            }
            self.data.pread(off, buf)
        }
    }

    #[test]
    fn unreadable_areas_are_skipped_not_fatal() {
        let mut data = junk(1 << 20);
        at(&mut data, 700_416, &png_file());
        let m = Flaky {
            data,
            bad: 200_000..200_512,
        };
        let mut found = Vec::new();
        let stats = scan_medium(
            &m,
            1 << 20,
            &Options::default(),
            &mut |_| {},
            &AtomicBool::new(false),
            &mut |f| {
                found.push(f.offset);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(found, [700_416]);
        assert_eq!(stats.unreadable, 64 << 10);
    }

    #[test]
    fn file_names_follow_the_sector() {
        assert_eq!(file_stem(0), "f0000000000");
        assert_eq!(file_stem(1_234_567 * 512), "f0001234567");
        assert_eq!(file_stem(1024 + 3), "f0000000002_3");
    }

    #[test]
    fn find_and_crcs() {
        assert_eq!(find(b"abcabc", b"bc", 0), Some(1));
        assert_eq!(find(b"abcabc", b"bc", 2), Some(4));
        assert_eq!(find(b"abcabc", b"cd", 0), None);
        assert_eq!(find(b"ab", b"abc", 0), None);
        assert_eq!(rfind(b"abcabc", b"bc"), Some(4));
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc8(b"123456789"), 0xF4);
        assert_eq!(ogg_crc(0, b"123456789"), 0x89A1_897F);
    }
}
