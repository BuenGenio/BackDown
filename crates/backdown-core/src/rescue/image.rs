//! Imaging a failing drive or memory card: copying it into an image file, the good data first and
//! the hard parts later, never writing to it, and resumable at any point (like GNU ddrescue).
//!
//! Why the order matters: a failing drive tends to get worse the longer it runs, and grinding on a
//! bad spot wears it further while the healthy areas wait. So the copy goes in phases, each one
//! only touching what the earlier ones couldn't get:
//!
//! 1. **Copying**, in large blocks. When a read fails, its block is marked *non-trimmed* and the
//!    copy jumps ahead (further after each failure in a row, and after reads that are merely very
//!    slow), leaving what it jumped over for later. Pass 1 goes forwards; pass 2 goes backwards
//!    over what was skipped, reaching each bad area from its far side; pass 3 sweeps up whatever
//!    is left, without skipping.
//! 2. **Trimming**: each failed block is read sector by sector from both of its ends inwards, up to
//!    the first bad sector on each side. What lies between is left *non-scraped*.
//! 3. **Scraping**: every non-scraped sector is read on its own; the ones that fail are *bad*.
//! 4. **Retrying** the bad sectors, `retries` times, in alternating directions.
//!
//! The map records the state of every byte in GNU ddrescue's mapfile format, so ddrescueview can
//! show it and ddrescue can carry on a job started here (or the other way round). It's replaced
//! atomically every few seconds and at every pass, always after the image data it vouches for has
//! reached the disk, so a crash or a pulled cable costs a few seconds of work at most. Running again
//! with the same image and map resumes: finished areas are never read again.
//!
//! The image has the source's size and is created sparse; whatever couldn't be read stays zeros.

use std::collections::VecDeque;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use super::size_of;
use crate::fmt::bytes;

/// Pass 1 and 2 jump this far past the first failed or slow read, doubling for each one in a row.
const SKIP_START: u64 = 64 * 1024;
/// ... up to 1% of the source, and never more than this.
const SKIP_MAX: u64 = 1 << 30;
/// The map is saved at least this often (and at every pass).
const SAVE_EVERY: Duration = Duration::from_secs(5);
/// Progress is reported at most this often (and at every pass).
const REPORT_EVERY: Duration = Duration::from_millis(250);
/// The read rate is measured over about this long.
const RATE_WINDOW: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    /// Bytes per read while copying (default 1 MiB). Bigger is faster on the healthy parts; each
    /// failed read leaves a whole block to trim later.
    pub block: usize,
    /// The unit bad areas are narrowed down to: the drive's sector size, 512 (the default) or 4096.
    pub sector: usize,
    /// How many times to go back over the bad sectors at the end (default 1).
    pub retries: u32,
    /// Copy what pass 1 skipped backwards (pass 2), reaching each bad area from its far end
    /// (default on).
    pub reverse: bool,
    /// In passes 1 and 2, treat a read taking longer than this many milliseconds like a failed
    /// one: keep its data but skip ahead, and come back later (0: never; default 1000). A drive
    /// struggling with a weak area slows down long before it fails.
    pub slow_ms: u64,
    /// Let the image (and its map) go on the same drive as the source, outside it: another
    /// partition of that drive, or next to a source that's a file. Off by default: the drive being
    /// rescued is usually the failing one, and every write to it risks what's still readable.
    /// Writing *into* the source is never allowed.
    pub allow_same_drive: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            block: 1 << 20,
            sector: 512,
            retries: 1,
            reverse: true,
            slow_ms: 1000,
            allow_same_drive: false,
        }
    }
}

impl Options {
    fn check(&self) -> Result<()> {
        if !self.sector.is_power_of_two() || !(512..=65536).contains(&self.sector) {
            bail!(
                "the sector size must be a power of two from 512 bytes to 64 KiB (not {})",
                self.sector
            );
        }
        if self.block < self.sector
            || !self.block.is_multiple_of(self.sector)
            || self.block > 64 << 20
        {
            bail!(
                "the block size must be a multiple of the sector size ({}), up to 64 MiB (not {})",
                self.sector,
                self.block
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    /// "copying", "trimming", "scraping" or "retrying".
    pub pass: &'static str,
    /// Which pass of it: copying 1 (forwards, skipping), 2 (backwards, skipping), 3 (the rest);
    /// retrying 1 to `retries`; otherwise 1.
    pub pass_no: u32,
    /// Where it's reading.
    pub pos: u64,
    pub size: u64,
    /// Bytes in the image.
    pub rescued: u64,
    /// Bytes in bad sectors (read on their own, and failed).
    pub bad: u64,
    /// Bytes still to try: not tried, not trimmed and not scraped.
    pub todo: u64,
    /// Runs of bad sectors.
    pub bad_areas: u64,
    /// Bytes per second over the last few seconds.
    pub read_rate: f64,
    /// Failed reads in this run.
    pub errors: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub size: u64,
    pub rescued: u64,
    pub bad: u64,
    pub todo: u64,
    pub bad_areas: u64,
    /// Every byte is in the image.
    pub complete: bool,
    /// Stopped on request before the end: run again (same image and map) to carry on. When
    /// neither this nor `complete`, every pass has run and what's left is bad sectors.
    pub stopped: bool,
    /// Failed reads in this run.
    pub errors: u64,
}

/// What's known about an area of the source (GNU ddrescue's block statuses).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Status {
    /// `?`: not read yet.
    NonTried,
    /// `*`: a large read in it failed; not trimmed yet.
    NonTrimmed,
    /// `/`: trimmed; its sectors haven't been read one by one yet.
    NonScraped,
    /// `-`: bad sector(s): read on their own, and failed.
    Bad,
    /// `+`: read, and in the image.
    Finished,
}

impl Status {
    pub fn as_char(self) -> char {
        match self {
            Status::NonTried => '?',
            Status::NonTrimmed => '*',
            Status::NonScraped => '/',
            Status::Bad => '-',
            Status::Finished => '+',
        }
    }

    pub fn from_char(c: char) -> Option<Status> {
        Some(match c {
            '?' => Status::NonTried,
            '*' => Status::NonTrimmed,
            '/' => Status::NonScraped,
            '-' => Status::Bad,
            '+' => Status::Finished,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub pos: u64,
    pub size: u64,
    pub status: Status,
}

impl Block {
    pub fn end(&self) -> u64 {
        self.pos + self.size
    }
}

/// The state of every byte of a source, as a GNU ddrescue mapfile holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Map {
    /// Where the run was (ddrescue's `current_pos`).
    pub pos: u64,
    /// What it was doing (ddrescue's `current_status`): `?` copying, `*` trimming, `/` scraping,
    /// `-` retrying, `+` finished; ddrescue also writes `F` (filling) and `G` (generating a map).
    pub status: char,
    /// Which pass of that (ddrescue's `current_pass`; maps from before ddrescue 1.21 have none: 1).
    pub pass: u32,
    /// Sorted, contiguous and coalesced (no two neighbours with the same status), from 0 to the
    /// source's size.
    pub blocks: Vec<Block>,
}

/// Bytes by status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    /// `+`
    pub rescued: u64,
    /// `-`
    pub bad: u64,
    /// `?` + `*` + `/`
    pub todo: u64,
    pub non_tried: u64,
    pub non_trimmed: u64,
    pub non_scraped: u64,
    /// Runs of bad sectors.
    pub bad_areas: u64,
}

impl Map {
    /// A map of `size` bytes, none of them tried.
    pub fn new(size: u64) -> Map {
        Map {
            pos: 0,
            status: '?',
            pass: 1,
            blocks: if size > 0 {
                vec![Block {
                    pos: 0,
                    size,
                    status: Status::NonTried,
                }]
            } else {
                Vec::new()
            },
        }
    }

    /// How much it covers.
    pub fn size(&self) -> u64 {
        self.blocks.last().map_or(0, Block::end)
    }

    pub fn totals(&self) -> Totals {
        let mut t = Totals::default();
        for b in &self.blocks {
            match b.status {
                Status::NonTried => t.non_tried += b.size,
                Status::NonTrimmed => t.non_trimmed += b.size,
                Status::NonScraped => t.non_scraped += b.size,
                Status::Bad => {
                    t.bad += b.size;
                    t.bad_areas += 1;
                }
                Status::Finished => t.rescued += b.size,
            }
        }
        t.todo = t.non_tried + t.non_trimmed + t.non_scraped;
        t
    }

    /// Whether any area has this status.
    pub fn has(&self, status: Status) -> bool {
        self.blocks.iter().any(|b| b.status == status)
    }

    /// The status of the byte at `pos`.
    pub fn status_at(&self, pos: u64) -> Option<Status> {
        self.blocks.get(self.index(pos)).map(|b| b.status)
    }

    /// Read a mapfile's text: `#` comments (and blank lines) anywhere; first the status line
    /// `current_pos current_status [current_pass]`, then one `pos size status` line per block.
    /// Numbers are C-style: `0x` hexadecimal (what ddrescue writes), a leading 0 octal, or decimal.
    /// Gaps between blocks count as not tried; blocks are sorted and neighbours with the same
    /// status merged.
    pub fn parse(text: &str) -> Result<Map> {
        let mut head: Option<(u64, char, u32)> = None;
        let mut blocks = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let bad_line = || anyhow!("line {}, {line:?}, isn't a mapfile line", n + 1);
            let t: Vec<&str> = line.split_whitespace().collect();
            if head.is_none() {
                let (pos, status, pass) = match t.as_slice() {
                    [pos, status] => (pos, status, None),
                    [pos, status, pass] => (pos, status, Some(pass)),
                    _ => return Err(bad_line().context("expected the status line first")),
                };
                let status = one_char(status)
                    .filter(|c| "?*/-FG+".contains(*c))
                    .ok_or_else(bad_line)?;
                let pass = match pass {
                    Some(p) => number(p)
                        .and_then(|p| u32::try_from(p).ok())
                        .ok_or_else(bad_line)?,
                    None => 1,
                };
                head = Some((number(pos).ok_or_else(bad_line)?, status, pass));
            } else {
                let [pos, size, status] = t.as_slice() else {
                    return Err(bad_line());
                };
                let pos = number(pos).ok_or_else(bad_line)?;
                let size = number(size).ok_or_else(bad_line)?;
                let status = one_char(status)
                    .and_then(Status::from_char)
                    .ok_or_else(bad_line)?;
                if pos.checked_add(size).is_none() {
                    return Err(bad_line());
                }
                if size > 0 {
                    blocks.push(Block { pos, size, status });
                }
            }
        }
        let (pos, status, pass) =
            head.context("it has no status line: it isn't a ddrescue mapfile")?;
        blocks.sort_by_key(|b| b.pos);
        let mut out: Vec<Block> = Vec::with_capacity(blocks.len());
        for b in blocks {
            let end = out.last().map_or(0, Block::end);
            if b.pos < end {
                bail!("its blocks overlap at 0x{:X}", b.pos);
            }
            if b.pos > end {
                push_merged(
                    &mut out,
                    Block {
                        pos: end,
                        size: b.pos - end,
                        status: Status::NonTried,
                    },
                );
            }
            push_merged(&mut out, b);
        }
        Ok(Map {
            pos,
            status,
            pass,
            blocks: out,
        })
    }

    /// Make the map cover exactly `size` bytes, or say why it can't be this source's map. An empty
    /// map is simply all not tried; a map running past the end is cut there only if what's past it
    /// was never tried (ddrescue maps of a source whose size it didn't know end that way).
    pub fn fit(&mut self, size: u64) -> Result<()> {
        let end = self.size();
        if self.blocks.is_empty() {
            self.blocks = Map::new(size).blocks;
        } else if end < size {
            bail!(
                "it covers {end} bytes, and the source is {size}: is it the same drive? For another \
                 drive, use a new image and a new map"
            );
        } else if end > size {
            if self
                .blocks
                .iter()
                .any(|b| b.end() > size && b.status != Status::NonTried)
            {
                bail!(
                    "it has data beyond the source's end ({end} bytes, and the source is {size}): \
                     is it the same drive? For another drive, use a new image and a new map"
                );
            }
            self.blocks.retain(|b| b.pos < size);
            match self.blocks.last_mut() {
                Some(last) => last.size = size - last.pos,
                None => self.blocks = Map::new(size).blocks,
            }
        }
        self.pos = self.pos.min(size);
        Ok(())
    }

    /// Give `[pos, end)` a status, keeping the blocks contiguous and coalesced.
    pub fn set(&mut self, pos: u64, end: u64, status: Status) {
        let end = end.min(self.size());
        if pos >= end {
            return;
        }
        let (i, j) = (self.index(pos), self.index(end - 1));
        let (first, last) = (self.blocks[i], self.blocks[j]);
        let before = (first.pos < pos).then_some(Block {
            pos: first.pos,
            size: pos - first.pos,
            status: first.status,
        });
        let after = (last.end() > end).then_some(Block {
            pos: end,
            size: last.end() - end,
            status: last.status,
        });
        let middle = Block {
            pos,
            size: end - pos,
            status,
        };
        let n = 1 + usize::from(before.is_some()) + usize::from(after.is_some());
        self.blocks
            .splice(i..=j, [before, Some(middle), after].into_iter().flatten());
        // merge what changed with its neighbours, from the right so indices stay valid
        let lo = i.saturating_sub(1);
        let mut k = (i + n).min(self.blocks.len() - 1);
        while k > lo {
            if self.blocks[k - 1].status == self.blocks[k].status {
                self.blocks[k - 1].size += self.blocks[k].size;
                self.blocks.remove(k);
            }
            k -= 1;
        }
    }

    /// The first area with `status` at or after `pos`, as `[start, end)` (clipped to start at `pos`).
    pub fn next(&self, pos: u64, status: Status) -> Option<(u64, u64)> {
        let i = self.index(pos);
        self.blocks
            .get(i..)?
            .iter()
            .find(|b| b.status == status)
            .map(|b| (b.pos.max(pos), b.end()))
    }

    /// The last area with `status` before `pos`, as `[start, end)` (clipped to end at `pos`).
    pub fn prev(&self, pos: u64, status: Status) -> Option<(u64, u64)> {
        let i = self.blocks.partition_point(|b| b.pos < pos);
        self.blocks[..i]
            .iter()
            .rev()
            .find(|b| b.status == status)
            .map(|b| (b.pos, b.end().min(pos)))
    }

    /// The index of the block holding `pos` (the number of blocks when it's past the end).
    fn index(&self, pos: u64) -> usize {
        self.blocks.partition_point(|b| b.end() <= pos)
    }
}

/// Append `b`, merging it into the last block when that has the same status.
fn push_merged(blocks: &mut Vec<Block>, b: Block) {
    match blocks.last_mut() {
        Some(last) if last.status == b.status && last.end() == b.pos => last.size += b.size,
        _ => blocks.push(b),
    }
}

/// The mapfile's text, the way GNU ddrescue writes it.
impl fmt::Display for Map {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "# Mapfile. Created by BackDown {}",
            env!("CARGO_PKG_VERSION")
        )?;
        let way = if self.pass.is_multiple_of(2) {
            "backwards"
        } else {
            "forwards"
        };
        match self.status {
            '?' => writeln!(
                f,
                "# Copying non-tried blocks... Pass {} ({way})",
                self.pass
            )?,
            '*' => writeln!(f, "# Trimming failed blocks... (forwards)")?,
            '/' => writeln!(f, "# Scraping failed blocks... (forwards)")?,
            '-' => writeln!(f, "# Retrying bad sectors... Retry {} ({way})", self.pass)?,
            '+' => writeln!(f, "# Finished")?,
            _ => {}
        }
        writeln!(f, "# current_pos  current_status  current_pass")?;
        writeln!(
            f,
            "0x{:08X}     {}               {}",
            self.pos, self.status, self.pass
        )?;
        writeln!(f, "#      pos        size  status")?;
        for b in &self.blocks {
            writeln!(
                f,
                "0x{:08X}  0x{:08X}  {}",
                b.pos,
                b.size,
                b.status.as_char()
            )?;
        }
        Ok(())
    }
}

fn one_char(s: &str) -> Option<char> {
    let mut c = s.chars();
    c.next().filter(|_| c.next().is_none())
}

/// A C-style unsigned number (what ddrescue's `strtoll(…, 0)` reads): `0x` hex, leading-0 octal,
/// or decimal.
fn number(s: &str) -> Option<u64> {
    if s.starts_with('+') || s.starts_with('-') {
        return None;
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else if s.len() > 1 && s.starts_with('0') {
        u64::from_str_radix(&s[1..], 8).ok()
    } else {
        s.parse().ok()
    }
}

/// Read a mapfile (GNU ddrescue's format).
pub fn read_map(path: &Path) -> Result<Map> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("can't read the map {}", path.display()))?;
    Map::parse(&text).with_context(|| format!("the map {} can't be used", path.display()))
}

/// Replace the map file atomically: written beside it, flushed, renamed over it.
fn write_map(path: &Path, map: &Map) -> Result<()> {
    let dir = folder_of(path);
    let name = path
        .file_name()
        .with_context(|| format!("{} isn't a file name for the map", path.display()))?;
    let tmp = dir.join(format!(".{}.backdown-partial", name.to_string_lossy()));
    let result = (|| -> io::Result<()> {
        let mut f = File::create(&tmp)?;
        f.write_all(map.to_string().as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)?;
        sync_folder(dir);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.with_context(|| format!("can't save the map {}", path.display()))
}

/// Make a rename durable (the folder's entry), where the system allows it.
fn sync_folder(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn folder_of(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// What `image_from` reads: a drive, a card, a file or (in tests) a simulated failing drive.
pub trait Source {
    /// Its size in bytes.
    fn size(&self) -> u64;
    /// Read at `off` into `buf`, like `pread`: it may return fewer bytes than asked, and 0 only at
    /// the end.
    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize>;
}

/// A drive, partition, card or file, opened read-only.
pub struct FileSource {
    file: File,
    size: u64,
}

impl FileSource {
    pub fn open(path: &Path) -> Result<FileSource> {
        // read-only: nothing done through this handle can change the source
        let file = File::open(path).map_err(|e| {
            let hint = if e.kind() == io::ErrorKind::PermissionDenied {
                " (reading a whole drive or card needs administrator rights)"
            } else {
                ""
            };
            anyhow!("can't open {} for reading: {e}{hint}", path.display())
        })?;
        if file.metadata().is_ok_and(|m| m.is_dir()) {
            bail!(
                "{} is a folder: choose a drive, a partition or an image file",
                path.display()
            );
        }
        let size =
            size_of(&file).with_context(|| format!("can't tell how big {} is", path.display()))?;
        Ok(FileSource { file, size })
    }
}

impl Source for FileSource {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        pread(&self.file, buf, off)
    }
}

#[cfg(unix)]
fn pread(f: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(f, buf, off)
}

#[cfg(windows)]
fn pread(f: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(f, buf, off)
}

#[cfg(unix)]
fn pwrite(f: &File, buf: &[u8], off: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(f, buf, off)
}

#[cfg(windows)]
fn pwrite(f: &File, mut buf: &[u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match f.seek_write(buf, off) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                buf = &buf[n..];
                off += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Read `buf.len()` bytes at `off`, carrying on after short reads and retrying interrupted ones.
/// Returns how many bytes were read, and the error that stopped it short, if one did (the source
/// ending before `buf` is full counts: its size said there was more).
fn read_full(src: &dyn Source, off: u64, buf: &mut [u8]) -> (usize, Option<io::Error>) {
    let mut got = 0;
    while got < buf.len() {
        match src.read_at(off + got as u64, &mut buf[got..]) {
            Ok(0) => {
                let e = io::Error::new(io::ErrorKind::UnexpectedEof, "the source ended early");
                return (got, Some(e));
            }
            Ok(n) => got += n.min(buf.len() - got),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return (got, Some(e)),
        }
    }
    (got, None)
}

/// Errors that mean the source itself went away (unplugged, the card pulled, the drive dead), not
/// a bad area: carrying on would only mark the rest of it bad.
fn gone(e: &io::Error) -> bool {
    #[cfg(target_os = "linux")]
    const CODES: &[i32] = &[
        6,   /* ENXIO */
        9,   /* EBADF */
        19,  /* ENODEV */
        123, /* ENOMEDIUM */
    ];
    #[cfg(all(unix, not(target_os = "linux")))]
    const CODES: &[i32] = &[6 /* ENXIO */, 9 /* EBADF */, 19 /* ENODEV */];
    #[cfg(windows)]
    const CODES: &[i32] = &[
        21,   /* ERROR_NOT_READY */
        55,   /* ERROR_DEV_NOT_EXIST */
        1167, /* ERROR_DEVICE_NOT_CONNECTED */
    ];
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
    ) || e.raw_os_error().is_some_and(|c| CODES.contains(&c))
}

/// Image the drive, partition, card or file at `src` into `dest`, keeping the map at `map`.
/// Running it again with the same three resumes where it stopped.
///
/// Before starting it refuses what could hurt the source or mix up two jobs (see `check_places`
/// and `image_from`). `progress` is called at most about 4 times a second and at every pass;
/// `stop` is checked between reads, and stopping saves the map and returns a `Summary` with
/// `stopped` set, not an error.
pub fn image(
    src: &Path,
    dest: &Path,
    map: &Path,
    opts: &Options,
    progress: &mut dyn FnMut(&Progress),
    stop: &AtomicBool,
) -> Result<Summary> {
    opts.check()?;
    check_places(src, dest, map, opts.allow_same_drive)?;
    let source = FileSource::open(src)?;
    image_from(&source, dest, map, opts, progress, stop)
}

/// `image`, reading from any `Source`. The checks that need the source's path (`check_places`)
/// are `image`'s; the rest are here: the image and map mustn't be each other, the map must fit the
/// source, an image without its map is never overwritten, and there must be room for the image.
pub fn image_from(
    src: &dyn Source,
    dest: &Path,
    map_path: &Path,
    opts: &Options,
    progress: &mut dyn FnMut(&Progress),
    stop: &AtomicBool,
) -> Result<Summary> {
    opts.check()?;
    let size = src.size();
    if size == 0 {
        bail!("the source reads as empty (0 bytes): is the card in, or the drive switched on?");
    }
    if same_file(dest, map_path) {
        bail!("the image and its map need two different files");
    }
    let had_image = exists(dest, "image")?;
    let had_map = exists(map_path, "map")?;
    let map = if had_map {
        let mut m = read_map(map_path)?;
        m.fit(size)
            .with_context(|| format!("the map {} doesn't fit the source", map_path.display()))?;
        m
    } else {
        Map::new(size)
    };
    let rescued = map.totals().rescued;
    if had_image && !had_map {
        bail!(
            "{} is already there, without a map: BackDown won't overwrite it. Choose another \
             name, or put its map back to carry on with it",
            dest.display()
        );
    }
    if had_map && !had_image && rescued > 0 {
        bail!(
            "the map {} says {} were rescued already, but their image {} isn't there. If it was \
             moved, put it back; to start over, delete the map",
            map_path.display(),
            bytes(rescued),
            dest.display()
        );
    }
    check_room(dest, size - rescued)?;
    for p in [dest, map_path] {
        let dir = folder_of(p);
        fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    }
    // The map first: if something stops us between the two, a map with nothing rescued and no
    // image simply starts over, while an image without a map would be refused.
    if !had_map {
        write_map(map_path, &map)?;
    }
    let out = match open_image(dest, size, had_image) {
        Ok(f) => f,
        Err(e) => {
            if !had_map {
                let _ = fs::remove_file(map_path);
            }
            return Err(e);
        }
    };
    let now = Instant::now();
    let mut run = Run {
        src,
        out: &out,
        map,
        map_path,
        progress,
        stop,
        buf: vec![0; opts.block],
        block: opts.block as u64,
        sector: opts.sector as u64,
        retries: opts.retries,
        reverse: opts.reverse,
        slow: (opts.slow_ms > 0).then(|| Duration::from_millis(opts.slow_ms)),
        skip_max: (size / 100 / opts.sector as u64 * opts.sector as u64)
            .clamp(SKIP_START, SKIP_MAX),
        name: "copying",
        pass_no: 1,
        errors: 0,
        read_bytes: 0,
        samples: VecDeque::new(),
        last_save: now,
        last_report: now,
    };
    let stopped = match run.run() {
        Ok(stopped) => stopped,
        Err(e) => {
            // keep what was rescued; if even this fails, the map on disk is still an older, true one
            let _ = run.save();
            return Err(e);
        }
    };
    if stopped {
        run.save()?;
    }
    let t = run.map.totals();
    Ok(Summary {
        size,
        rescued: t.rescued,
        bad: t.bad,
        todo: t.todo,
        bad_areas: t.bad_areas,
        complete: t.rescued == size,
        stopped,
        errors: run.errors,
    })
}

fn exists(path: &Path, what: &str) -> Result<bool> {
    match fs::metadata(path) {
        Ok(m) if m.is_file() => Ok(true),
        Ok(_) => bail!(
            "{} is already there and isn't a file: choose a file name for the {what}",
            path.display()
        ),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("can't look at {}", path.display())),
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b || matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

/// Whether the destination's drive has room for `need` more bytes of image.
fn check_room(dest: &Path, need: u64) -> Result<()> {
    let Some(free) = crate::apply::free_space(dest) else {
        return Ok(()); // can't tell: writing will say when it's full
    };
    if free < need {
        bail!(
            "not enough room for the image: it can take up to {} more (the whole source, less \
             what's rescued already), and there's {} free where it goes. Areas that can't be read \
             take no room, since the image is a sparse file, but a mostly healthy drive needs \
             nearly all of it: choose a place with more room",
            bytes(need),
            bytes(free)
        );
    }
    Ok(())
}

/// Create the image at the source's size (sparse where the filesystem can), or open the one being
/// resumed, never truncating it.
fn open_image(dest: &Path, size: u64, existing: bool) -> Result<File> {
    if existing {
        let f = OpenOptions::new()
            .write(true)
            .open(dest)
            .with_context(|| format!("can't open the image {} to carry on", dest.display()))?;
        let len = f
            .metadata()
            .with_context(|| format!("can't look at {}", dest.display()))?
            .len();
        if len > size {
            bail!(
                "the image {} is bigger than the source ({len} bytes, and the source is {size}): \
                 is it the image of another drive?",
                dest.display()
            );
        }
        // ddrescue's images end where their last read did: make it whole (zeros; nothing is cut)
        if len < size {
            f.set_len(size)
                .with_context(|| format!("can't extend the image {}", dest.display()))?;
        }
        return Ok(f);
    }
    let f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
        .with_context(|| format!("can't create the image {}", dest.display()))?;
    if let Err(e) = f.set_len(size) {
        drop(f);
        let _ = fs::remove_file(dest); // our own empty file
        let fat = if size >= 1 << 32 {
            " (a drive formatted FAT32 can't hold files of 4 GB or more: use one formatted exFAT, \
             NTFS, APFS or ext4)"
        } else {
            ""
        };
        bail!(
            "can't make the image {} {} big: {e}{fat}",
            dest.display(),
            bytes(size)
        );
    }
    Ok(f)
}

/// The result of one read.
struct Got {
    /// Bytes read (and written to the image) from the start of the area.
    good: u64,
    failed: bool,
    /// It worked but took longer than `slow_ms`.
    slow: bool,
}

struct Run<'a> {
    src: &'a dyn Source,
    out: &'a File,
    map: Map,
    map_path: &'a Path,
    progress: &'a mut dyn FnMut(&Progress),
    stop: &'a AtomicBool,
    buf: Vec<u8>,
    block: u64,
    sector: u64,
    retries: u32,
    reverse: bool,
    slow: Option<Duration>,
    skip_max: u64,
    name: &'static str,
    pass_no: u32,
    errors: u64,
    read_bytes: u64,
    /// (when, `read_bytes` then), for the recent read rate
    samples: VecDeque<(Instant, u64)>,
    last_save: Instant,
    last_report: Instant,
}

impl Run<'_> {
    /// All the phases, carrying on where the map says an earlier run was. True when stopped.
    fn run(&mut self) -> Result<bool> {
        let size = self.map.size();
        // an interrupted copy or retry carries on in the same pass, from the same place (ddrescue
        // does the same with its maps); the other phases pick up whatever is left of them
        let (at_pass, at_pos) = if self.map.status == '?' {
            (self.map.pass.clamp(1, 3), self.map.pos)
        } else {
            (1, 0)
        };
        let (retry_pass, retry_pos) = if self.map.status == '-' {
            (self.map.pass.max(1), Some(self.map.pos))
        } else {
            (1, None)
        };
        if at_pass == 1 && self.map.has(Status::NonTried) {
            self.phase('?', 1, "copying", at_pos)?;
            if self.copy_forwards(at_pos, true)? {
                return Ok(true);
            }
        }
        if self.reverse && at_pass <= 2 && self.map.has(Status::NonTried) {
            let from = if at_pass == 2 { at_pos } else { size };
            self.phase('?', 2, "copying", from)?;
            if self.copy_backwards(from)? {
                return Ok(true);
            }
        }
        if self.map.has(Status::NonTried) {
            let from = if at_pass == 3 { at_pos } else { 0 };
            self.phase('?', 3, "copying", from)?;
            if self.copy_forwards(from, false)? {
                return Ok(true);
            }
            if from > 0 && self.copy_forwards(0, false)? {
                return Ok(true);
            }
        }
        if self.map.has(Status::NonTrimmed) {
            self.phase('*', 1, "trimming", 0)?;
            if self.trim()? {
                return Ok(true);
            }
        }
        if self.map.has(Status::NonScraped) {
            self.phase('/', 1, "scraping", 0)?;
            if self.scrape()? {
                return Ok(true);
            }
        }
        for pass in retry_pass..=self.retries {
            if !self.map.has(Status::Bad) {
                break;
            }
            let from = match retry_pos {
                Some(pos) if pass == retry_pass => pos,
                _ if pass % 2 == 1 => 0,
                _ => size,
            };
            self.phase('-', pass, "retrying", from)?;
            if self.retry(pass, from)? {
                return Ok(true);
            }
        }
        self.map.status = '+';
        self.map.pass = 1;
        self.save()?;
        Ok(false)
    }

    /// Pass 1 (`skipping`) and pass 3: copy what's not tried yet, forwards from `from`, in blocks
    /// that never cross a block boundary of the map. When skipping, a failed or slow read makes it
    /// jump ahead, further each time in a row, so the healthy areas come first; what's jumped over
    /// stays not tried, for the later passes.
    fn copy_forwards(&mut self, from: u64, skipping: bool) -> Result<bool> {
        let mut pos = from;
        let mut skip = 0;
        while let Some((start, end)) = self.map.next(pos, Status::NonTried) {
            if self.tick()? {
                return Ok(true);
            }
            let end = end.min((start / self.block + 1) * self.block);
            self.map.pos = start;
            let got = self.read(start, end)?;
            if got.failed {
                self.map.set(start + got.good, end, Status::NonTrimmed);
            }
            pos = end;
            if skipping && (got.failed || got.slow) {
                skip = if skip == 0 {
                    SKIP_START
                } else {
                    (skip * 2).min(self.skip_max)
                };
                pos = end.saturating_add(skip);
            } else {
                skip = 0;
            }
            self.map.pos = pos.min(self.map.size());
        }
        Ok(false)
    }

    /// Pass 2: the same backwards, from `from` down, over what pass 1 skipped: each bad area is
    /// reached from its far side, so trimming has less left to do.
    fn copy_backwards(&mut self, from: u64) -> Result<bool> {
        let mut pos = from;
        let mut skip = 0;
        while let Some((lo, end)) = self.map.prev(pos, Status::NonTried) {
            if self.tick()? {
                return Ok(true);
            }
            let start = lo.max((end - 1) / self.block * self.block);
            self.map.pos = end;
            let got = self.read(start, end)?;
            if got.failed {
                self.map.set(start + got.good, end, Status::NonTrimmed);
            }
            pos = start;
            if got.failed || got.slow {
                skip = if skip == 0 {
                    SKIP_START
                } else {
                    (skip * 2).min(self.skip_max)
                };
                pos = start.saturating_sub(skip);
            } else {
                skip = 0;
            }
            self.map.pos = pos;
        }
        Ok(false)
    }

    /// Trimming: each failed block, sector by sector from its leading edge forwards to the first
    /// bad sector, then from its trailing edge backwards to the first bad sector on that side. The
    /// bad sectors found are bad; what's between them is left for scraping.
    fn trim(&mut self) -> Result<bool> {
        let mut pos = 0;
        while let Some((lo, hi)) = self.map.next(pos, Status::NonTrimmed) {
            let mut p = lo;
            let mut lead = None; // the end of the first bad sector from the front
            while p < hi {
                if self.tick()? {
                    return Ok(true);
                }
                self.map.pos = p;
                let q = hi.min(self.sector_after(p));
                let got = self.read(p, q)?;
                if got.failed {
                    self.map.set(p + got.good, q, Status::Bad);
                    lead = Some(q);
                    break;
                }
                p = q;
            }
            if let Some(lead) = lead {
                let mut t = hi;
                let mut middle_end = lead;
                while t > lead {
                    if self.tick()? {
                        return Ok(true);
                    }
                    let q = lead.max((t - 1) / self.sector * self.sector);
                    self.map.pos = q;
                    let got = self.read(q, t)?;
                    if got.failed {
                        self.map.set(q + got.good, t, Status::Bad);
                        middle_end = q;
                        break;
                    }
                    t = q;
                }
                self.map.set(lead, middle_end, Status::NonScraped);
            }
            pos = hi;
        }
        Ok(false)
    }

    /// Scraping: every sector not scraped yet, on its own.
    fn scrape(&mut self) -> Result<bool> {
        let mut pos = 0;
        while let Some((lo, hi)) = self.map.next(pos, Status::NonScraped) {
            if self.tick()? {
                return Ok(true);
            }
            let q = hi.min(self.sector_after(lo));
            self.map.pos = lo;
            let got = self.read(lo, q)?;
            if got.failed {
                self.map.set(lo + got.good, q, Status::Bad);
            }
            pos = q;
        }
        Ok(false)
    }

    /// Retrying: every bad sector once more, forwards in odd passes and backwards in even ones (a
    /// drive's retries sometimes depend on where the head comes from).
    fn retry(&mut self, pass: u32, from: u64) -> Result<bool> {
        let forwards = pass % 2 == 1;
        let mut pos = from;
        loop {
            let (lo, hi) = if forwards {
                let Some((lo, hi)) = self.map.next(pos, Status::Bad) else {
                    break;
                };
                (lo, hi.min(self.sector_after(lo)))
            } else {
                let Some((lo, hi)) = self.map.prev(pos, Status::Bad) else {
                    break;
                };
                (lo.max((hi - 1) / self.sector * self.sector), hi)
            };
            if self.tick()? {
                return Ok(true);
            }
            self.map.pos = lo;
            // a sector that reads now goes into the image; one that doesn't stays bad
            self.read(lo, hi)?;
            pos = if forwards { hi } else { lo };
            self.map.pos = pos;
        }
        Ok(false)
    }

    /// The end of the sector holding `pos`.
    fn sector_after(&self, pos: u64) -> u64 {
        (pos / self.sector + 1) * self.sector
    }

    /// Read `[pos, end)` (at most a block): what's read goes into the image and is marked
    /// finished. Marking what failed is up to the caller, which knows what it means in its phase.
    fn read(&mut self, pos: u64, end: u64) -> Result<Got> {
        let buf = &mut self.buf[..(end - pos) as usize];
        let started = Instant::now();
        let (n, err) = read_full(self.src, pos, buf);
        let took = started.elapsed();
        if n > 0 {
            pwrite(self.out, &buf[..n], pos).context("can't write to the image")?;
            self.map.set(pos, pos + n as u64, Status::Finished);
            self.read_bytes += n as u64;
        }
        match err {
            Some(e) if gone(&e) => Err(anyhow!(
                "the source stopped answering ({e}): it was unplugged, or it has failed completely. \
                 What was rescued so far is kept, with its map: run again once it's back to carry on"
            )),
            Some(_) => {
                self.errors += 1;
                Ok(Got {
                    good: n as u64,
                    failed: true,
                    slow: false,
                })
            }
            None => Ok(Got {
                good: n as u64,
                failed: false,
                slow: self.slow.is_some_and(|s| took > s),
            }),
        }
    }

    /// Between reads: true when asked to stop; otherwise report and save when it's time.
    fn tick(&mut self) -> Result<bool> {
        if self.stop.load(Ordering::Relaxed) {
            return Ok(true);
        }
        let now = Instant::now();
        if now.duration_since(self.last_report) >= REPORT_EVERY {
            self.report();
        }
        if now.duration_since(self.last_save) >= SAVE_EVERY {
            self.save()?;
        }
        Ok(false)
    }

    /// A new pass: recorded in the map, saved, and reported.
    fn phase(&mut self, status: char, pass: u32, name: &'static str, pos: u64) -> Result<()> {
        self.map.status = status;
        self.map.pass = pass;
        self.map.pos = pos;
        self.name = name;
        self.pass_no = pass;
        self.save()?;
        self.report();
        Ok(())
    }

    /// Save the map, after flushing the image: the map on disk must never say finished for data
    /// that isn't safely in the image yet.
    fn save(&mut self) -> Result<()> {
        self.out
            .sync_data()
            .context("can't flush the image to its disk")?;
        write_map(self.map_path, &self.map)?;
        self.last_save = Instant::now();
        Ok(())
    }

    fn report(&mut self) {
        let now = Instant::now();
        self.samples.push_back((now, self.read_bytes));
        while self.samples.len() > 2 && now.duration_since(self.samples[0].0) > RATE_WINDOW {
            self.samples.pop_front();
        }
        let read_rate = match (self.samples.front(), self.samples.back()) {
            (Some(&(t0, b0)), Some(&(t1, b1))) if t1 > t0 => {
                (b1 - b0) as f64 / t1.duration_since(t0).as_secs_f64()
            }
            _ => 0.0,
        };
        let t = self.map.totals();
        (self.progress)(&Progress {
            pass: self.name,
            pass_no: self.pass_no,
            pos: self.map.pos,
            size: self.map.size(),
            rescued: t.rescued,
            bad: t.bad,
            todo: t.todo,
            bad_areas: t.bad_areas,
            read_rate,
            errors: self.errors,
        });
        self.last_report = now;
    }
}

/// Refuse to put the image or its map anywhere that could harm the source:
///
/// - never the source itself (the same file, or a link to it);
/// - never *inside* a source device: on its filesystem, on a partition of the source disk, or on
///   something stacked on it (LVM, encryption, RAID). That would overwrite the very data being
///   rescued, and change it while it's being read;
/// - not on the same physical drive at all, unless `allow_same_drive`: on another partition of the
///   source's disk, or on the filesystem holding a source that's a file. That can't overwrite the
///   source, but the drive being rescued is usually the failing one, and every write makes it work
///   harder and can cost what's still readable (ddrescue's manual says the same).
///
/// How "the same drive" is told, on Unix: a device node's `st_rdev` is the device it *is*, while a
/// file's or folder's `st_dev` is the device its filesystem lives on. So a destination folder whose
/// `st_dev` is the source device's `st_rdev` is on the source; for a source that's a file, the
/// drive at risk is the one holding it, its own `st_dev`, and the same comparison applies. But a
/// partition has a number of its own (sdb1 isn't sdb), and so do LVM, dm-crypt and md devices; on
/// Linux both sides are therefore traced down through sysfs (`/sys/dev/block/MAJ:MIN`: a
/// partition's parent is its disk, `slaves/` is what a stacked device sits on) to every device
/// underneath. Filesystems whose `st_dev` isn't a real device (btrfs, network and memory
/// filesystems) are traced through the device /proc/self/mountinfo says they're mounted from.
/// Elsewhere only the exact device compares: on macOS a partition's number says nothing about its
/// disk, and on Windows a `\\.\PhysicalDriveN` says nothing about drive letters; there the caller
/// should also check with `volumes::probe` which disk the destination's volume is on.
pub fn check_places(src: &Path, dest: &Path, map: &Path, allow_same_drive: bool) -> Result<()> {
    check_place(src, dest, "image", allow_same_drive)?;
    check_place(src, map, "map", allow_same_drive)
}

/// How a destination relates to the source's drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Near {
    /// On the source's own device (or on something that lies inside it).
    Inside,
    /// On the same physical drive, outside the source.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    SameDrive,
    Apart,
}

#[cfg(unix)]
fn check_place(src: &Path, out: &Path, what: &str, allow_same_drive: bool) -> Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let s = fs::metadata(src).with_context(|| format!("can't find {}", src.display()))?;
    if let Ok(o) = fs::metadata(out)
        && o.dev() == s.dev()
        && o.ino() == s.ino()
    {
        bail!(
            "the {what} can't be {} itself: choose a file on another drive",
            src.display()
        );
    }
    let dir = existing_folder(out);
    let d = fs::metadata(&dir)
        .with_context(|| format!("can't look at {}", dir.display()))?
        .dev();
    let kind = s.file_type();
    let device = kind.is_block_device() || kind.is_char_device();
    let near = if device {
        near(s.rdev(), None, d, &dir)
    } else {
        near(s.dev(), Some(src), d, &dir)
    };
    refuse(src, &dir, what, device, near, allow_same_drive)
}

#[cfg(windows)]
fn check_place(src: &Path, out: &Path, what: &str, allow_same_drive: bool) -> Result<()> {
    if same_file(src, out) {
        bail!(
            "the {what} can't be {} itself: choose a file on another drive",
            src.display()
        );
    }
    let dir = existing_folder(out);
    // a volume opened as a device (\\.\E:) is the drive letter it's mounted at; a file is on its own
    let device = volume_device(src);
    let src_volume = device.clone().or_else(|| {
        fs::metadata(src)
            .is_ok_and(|m| m.is_file())
            .then(|| volume_of(src))
            .flatten()
    });
    let near = match (src_volume, volume_of(&dir)) {
        (Some(a), Some(b)) if a == b => Near::Inside,
        _ => Near::Apart,
    };
    refuse(src, &dir, what, device.is_some(), near, allow_same_drive)
}

fn refuse(
    src: &Path,
    dir: &Path,
    what: &str,
    device: bool,
    near: Near,
    allow_same_drive: bool,
) -> Result<()> {
    match near {
        Near::Inside if device => bail!(
            "{} is on {} itself: writing the {what} there would overwrite what's being rescued. \
             Choose a folder on another drive",
            dir.display(),
            src.display()
        ),
        Near::Inside | Near::SameDrive if !allow_same_drive => bail!(
            "{} is on the same drive as {}: if that drive is failing, writing the {what} to it \
             risks what's still readable. Choose a folder on another drive",
            dir.display(),
            src.display()
        ),
        _ => Ok(()),
    }
}

/// The folder `out` would go in, or its nearest existing ancestor.
fn existing_folder(out: &Path) -> PathBuf {
    let mut p = folder_of(out);
    while !p.is_dir() {
        match p.parent() {
            Some(q) if !q.as_os_str().is_empty() => p = q,
            _ => return PathBuf::from("."),
        }
    }
    p.to_path_buf()
}

/// `src_dev` is the source device, or for a source that's a file (`src_file`), the device of its
/// filesystem; `dest_dev` is the destination folder's.
#[cfg(unix)]
fn near(src_dev: u64, src_file: Option<&Path>, dest_dev: u64, dest_dir: &Path) -> Near {
    if src_dev == dest_dev {
        return Near::Inside;
    }
    #[cfg(target_os = "linux")]
    {
        // a filesystem's number that isn't a block device's: by what it's mounted from
        let stack = |dev: u64, path: Option<&Path>| {
            let s = linux::stack(dev);
            match path {
                Some(p) if s.is_empty() => linux::mounted_from(p).map_or(s, linux::stack),
                _ => s,
            }
        };
        let src = stack(src_dev, src_file);
        let dest = stack(dest_dev, Some(dest_dir));
        let (s, d) = (linux::dev_name(src_dev), linux::dev_name(dest_dev));
        if dest.contains(&s) || src.contains(&d) {
            return Near::Inside;
        }
        if !src.is_disjoint(&dest) {
            return Near::SameDrive;
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (src_file, dest_dir);
    Near::Apart
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A device number as sysfs names it, `major:minor` (glibc's encoding of `dev_t`).
    pub fn dev_name(dev: u64) -> String {
        let major = ((dev >> 32) & 0xffff_f000) | ((dev >> 8) & 0x0000_0fff);
        let minor = ((dev >> 12) & 0xffff_ff00) | (dev & 0x0000_00ff);
        format!("{major}:{minor}")
    }

    /// `dev` and every block device beneath it, as `major:minor`: the disk of a partition, and
    /// what a device-mapper or md device is built on, all the way down. Empty when the kernel lists
    /// no such block device (btrfs's and network or memory filesystems' device numbers).
    pub fn stack(dev: u64) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        if let Ok(dir) = fs::canonicalize(Path::new("/sys/dev/block").join(dev_name(dev))) {
            walk(&dir, &mut seen, 0);
        }
        seen
    }

    fn walk(dir: &Path, seen: &mut BTreeSet<String>, depth: u32) {
        let Ok(dev) = fs::read_to_string(dir.join("dev")) else {
            return;
        };
        if depth > 16 || !seen.insert(dev.trim().to_string()) {
            return;
        }
        if dir.join("partition").exists()
            && let Some(disk) = dir.parent()
        {
            walk(disk, seen, depth + 1);
        }
        for e in fs::read_dir(dir.join("slaves"))
            .into_iter()
            .flatten()
            .flatten()
        {
            if let Ok(below) = fs::canonicalize(e.path()) {
                walk(&below, seen, depth + 1);
            }
        }
    }

    /// The device number of the block device the filesystem holding `path` is mounted from (the
    /// deepest mount point containing it, by /proc/self/mountinfo).
    pub fn mounted_from(path: &Path) -> Option<u64> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let dir = fs::canonicalize(path).ok()?;
        let info = fs::read_to_string("/proc/self/mountinfo").ok()?;
        let mut best: Option<(usize, PathBuf)> = None;
        for line in info.lines() {
            // id parent major:minor root mount-point options [optional…] - fstype source super-options
            let Some((left, right)) = line.split_once(" - ") else {
                continue;
            };
            let (Some(point), Some(source)) = (left.split(' ').nth(4), right.split(' ').nth(1))
            else {
                continue;
            };
            let point = PathBuf::from(unescape(point));
            if dir.starts_with(&point) {
                let depth = point.components().count();
                // later mounts on the same point hide earlier ones
                if best.as_ref().is_none_or(|(d, _)| depth >= *d) {
                    best = Some((depth, PathBuf::from(unescape(source))));
                }
            }
        }
        let meta = fs::metadata(best?.1).ok()?;
        meta.file_type().is_block_device().then(|| meta.rdev())
    }

    /// mountinfo's `\040`-style escapes.
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

        #[test]
        fn device_numbers_as_sysfs_names_them() {
            let make = |major: u64, minor: u64| {
                ((major & 0xffff_f000) << 32)
                    | ((major & 0xfff) << 8)
                    | ((minor & 0xffff_ff00) << 12)
                    | (minor & 0xff)
            };
            assert_eq!(dev_name(make(8, 17)), "8:17");
            assert_eq!(dev_name(make(259, 3)), "259:3");
            assert_eq!(dev_name(make(253, 300)), "253:300");
            assert_eq!(dev_name(make(4096, 1 << 20)), "4096:1048576");
        }

        #[test]
        fn mount_points_unescape() {
            assert_eq!(unescape(r"/media/an\040example"), "/media/an example");
            assert_eq!(unescape(r"/plain"), "/plain");
            assert_eq!(unescape(r"/tab\011end\"), "/tab\tend\\");
        }
    }
}

/// On Windows: the drive letter (`E:`) of a volume opened as a device (`\\.\E:`).
#[cfg(windows)]
fn volume_device(p: &Path) -> Option<String> {
    use std::path::{Component, Prefix};
    match p.components().next()? {
        Component::Prefix(c) => match c.kind() {
            Prefix::DeviceNS(n) => {
                let n = n.to_string_lossy();
                (n.len() == 2 && n.ends_with(':')).then(|| n.to_ascii_uppercase())
            }
            _ => None,
        },
        _ => None,
    }
}

/// On Windows: the volume a path is on, as its drive letter (`C:`) or network share.
#[cfg(windows)]
fn volume_of(p: &Path) -> Option<String> {
    use std::path::{Component, Prefix};
    let c = fs::canonicalize(p).ok()?;
    match c.components().next()? {
        Component::Prefix(pc) => match pc.kind() {
            Prefix::VerbatimDisk(d) | Prefix::Disk(d) => {
                Some(format!("{}:", (d as char).to_ascii_uppercase()))
            }
            Prefix::VerbatimUNC(server, share) | Prefix::UNC(server, share) => Some(
                format!(
                    r"\\{}\{}",
                    server.to_string_lossy(),
                    share.to_string_lossy()
                )
                .to_lowercase(),
            ),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn blocks(m: &Map) -> Vec<(u64, u64, char)> {
        m.blocks
            .iter()
            .map(|b| (b.pos, b.size, b.status.as_char()))
            .collect()
    }

    #[test]
    fn setting_areas_splits_and_merges() {
        let mut m = Map::new(100);
        m.set(10, 20, Status::Finished);
        assert_eq!(blocks(&m), [(0, 10, '?'), (10, 10, '+'), (20, 80, '?')]);
        m.set(0, 10, Status::Finished);
        assert_eq!(blocks(&m), [(0, 20, '+'), (20, 80, '?')]);
        m.set(30, 40, Status::Bad);
        m.set(50, 60, Status::Bad);
        m.set(35, 55, Status::NonScraped);
        assert_eq!(
            blocks(&m),
            [
                (0, 20, '+'),
                (20, 10, '?'),
                (30, 5, '-'),
                (35, 20, '/'),
                (55, 5, '-'),
                (60, 40, '?')
            ]
        );
        m.set(0, 1000, Status::Finished);
        assert_eq!(blocks(&m), [(0, 100, '+')]);
        m.set(99, 100, Status::Bad);
        m.set(0, 1, Status::Bad);
        assert_eq!(blocks(&m), [(0, 1, '-'), (1, 98, '+'), (99, 1, '-')]);
        m.set(5, 5, Status::NonTried); // empty: nothing changes
        assert_eq!(m.totals().bad_areas, 2);
        assert_eq!(m.status_at(99), Some(Status::Bad));
        assert_eq!(m.status_at(100), None);
    }

    #[test]
    fn setting_areas_matches_a_byte_by_byte_model() {
        let all = [
            Status::NonTried,
            Status::NonTrimmed,
            Status::NonScraped,
            Status::Bad,
            Status::Finished,
        ];
        let size = 300u64;
        let mut model = vec![Status::NonTried; size as usize];
        let mut m = Map::new(size);
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let mut rand = |n: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % n
        };
        for _ in 0..5000 {
            let a = rand(size + 10);
            let b = a + rand(40);
            let s = all[rand(5) as usize];
            m.set(a, b, s);
            for st in &mut model[(a.min(size) as usize)..(b.min(size) as usize)] {
                *st = s;
            }
            let mut end = 0;
            for (i, blk) in m.blocks.iter().enumerate() {
                assert_eq!(blk.pos, end);
                assert!(blk.size > 0);
                assert!(i == 0 || m.blocks[i - 1].status != blk.status, "not merged");
                assert!(
                    model[blk.pos as usize..blk.end() as usize]
                        .iter()
                        .all(|&st| st == blk.status)
                );
                end = blk.end();
            }
            assert_eq!(end, size);
        }
    }

    #[test]
    fn finding_areas_both_ways() {
        let mut m = Map::new(100);
        m.set(10, 20, Status::Finished);
        m.set(40, 50, Status::Finished);
        assert_eq!(m.next(0, Status::NonTried), Some((0, 10)));
        assert_eq!(m.next(5, Status::NonTried), Some((5, 10)));
        assert_eq!(m.next(10, Status::NonTried), Some((20, 40)));
        assert_eq!(m.next(95, Status::NonTried), Some((95, 100)));
        assert_eq!(m.next(100, Status::NonTried), None);
        assert_eq!(m.next(0, Status::Bad), None);
        assert_eq!(m.prev(100, Status::NonTried), Some((50, 100)));
        assert_eq!(m.prev(45, Status::NonTried), Some((20, 40)));
        assert_eq!(m.prev(30, Status::NonTried), Some((20, 30)));
        assert_eq!(m.prev(20, Status::NonTried), Some((0, 10)));
        assert_eq!(m.prev(0, Status::NonTried), None);
    }

    #[test]
    fn numbers_read_like_c() {
        assert_eq!(number("0x00117200"), Some(0x117200));
        assert_eq!(number("0X1f"), Some(31));
        assert_eq!(number("1024"), Some(1024));
        assert_eq!(number("0"), Some(0));
        assert_eq!(number("010"), Some(8));
        assert_eq!(number("-1"), None);
        assert_eq!(number("0x"), None);
        assert_eq!(number("+5"), None);
    }

    #[test]
    fn maps_with_gaps_or_an_unknown_end_are_made_whole() {
        let mut m = Map::parse("0x0 ? 1\n0x200 0x200 +\n0x800 0x7FFFFFFFFFFFF800 ?\n").unwrap();
        assert_eq!(
            blocks(&m),
            [
                (0, 0x200, '?'),
                (0x200, 0x200, '+'),
                (0x400, 0x7FFF_FFFF_FFFF_FC00, '?')
            ]
        );
        m.fit(0x1000).unwrap();
        assert_eq!(
            blocks(&m),
            [(0, 0x200, '?'), (0x200, 0x200, '+'), (0x400, 0xC00, '?')]
        );
        // too short, or finished data past the end: another drive's map
        assert!(Map::parse("0 ?\n0 0x800 +\n").unwrap().fit(0x1000).is_err());
        assert!(
            Map::parse("0 ?\n0 0x2000 +\n")
                .unwrap()
                .fit(0x1000)
                .is_err()
        );
        // no blocks at all: nothing known yet
        let mut empty = Map::parse("# nothing\n0x0 ? 1\n").unwrap();
        empty.fit(0x1000).unwrap();
        assert_eq!(blocks(&empty), [(0, 0x1000, '?')]);
    }

    #[test]
    fn broken_maps_are_refused() {
        assert!(Map::parse("").is_err());
        assert!(Map::parse("0x0 0x200 +\n").is_err()); // no status line
        assert!(Map::parse("0x0 X\n").is_err());
        assert!(Map::parse("0x0 ?\n0x0 0x200 x\n").is_err());
        assert!(Map::parse("0x0 ?\n0x0 0x200\n").is_err());
        assert!(Map::parse("0x0 ?\n0x0 0x400 +\n0x200 0x400 -\n").is_err()); // overlap
        assert!(Map::parse("0x0 ?\n0xFFFFFFFFFFFFFFFF 0x2 +\n").is_err());
    }

    /// A source that reads short (a few bytes at a time) and is interrupted now and then.
    struct Choppy {
        data: Vec<u8>,
        calls: Cell<u32>,
    }

    impl Source for Choppy {
        fn size(&self) -> u64 {
            self.data.len() as u64
        }
        fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
            self.calls.set(self.calls.get() + 1);
            if self.calls.get().is_multiple_of(3) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let off = off as usize;
            let n = buf.len().min(7).min(self.data.len().saturating_sub(off));
            buf[..n].copy_from_slice(&self.data[off..off + n]);
            Ok(n)
        }
    }

    #[test]
    fn short_and_interrupted_reads_carry_on() {
        let src = Choppy {
            data: (0..100u8).collect(),
            calls: Cell::new(0),
        };
        let mut buf = [0u8; 50];
        let (n, err) = read_full(&src, 10, &mut buf);
        assert_eq!(n, 50);
        assert!(err.is_none());
        assert_eq!(buf[..], src.data[10..60]);
        // asking past the end: what's there, and an error for the rest
        let (n, err) = read_full(&src, 80, &mut buf);
        assert_eq!(n, 20);
        assert_eq!(err.unwrap().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn options_are_checked() {
        assert!(Options::default().check().is_ok());
        let bad = |o: Options| o.check().is_err();
        assert!(bad(Options {
            sector: 1000,
            ..Default::default()
        }));
        assert!(bad(Options {
            sector: 256,
            ..Default::default()
        }));
        assert!(bad(Options {
            block: 1000,
            ..Default::default()
        }));
        assert!(bad(Options {
            block: 128 << 20,
            ..Default::default()
        }));
        assert!(
            Options {
                sector: 4096,
                block: 4096,
                ..Default::default()
            }
            .check()
            .is_ok()
        );
    }
}
