//! Read-only looks inside container files, for the browser's viewers: spreadsheets, slide decks,
//! mail (.eml and mbox), archives (zip and tar, plain or compressed) and disk images.
//!
//! `serve` resolves an index id to a path and hands it here; nothing in this module picks files by
//! itself, writes anything or runs anything. Every call reads a bounded amount: headers, directories
//! and tables, one page of rows or messages, one entry of at most `ENTRY_CAP` bytes. Walks through
//! compressed streams and big mailboxes also stop on a time budget and say where to carry on, so a
//! multi-gigabyte file can't pin a request thread.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use backdown_core::fmt;
use backdown_core::magic::{be, field, gpt_type, has, le, mbr_type, probe_fs, read_at, utf16};
use quick_xml::Reader as Xml;
use quick_xml::events::{BytesRef, BytesStart, Event};
use serde_json::{Value, json};

/// Largest entry or attachment served out of a container in one piece.
const ENTRY_CAP: u64 = 64 << 20;
/// Archive (and ISO directory) entries listed per request.
const MAX_ENTRIES: usize = 10_000;
/// Spreadsheet page: at most this many rows and columns.
const SHEET_ROWS: u64 = 500;
const MAX_COLS: usize = 100;
/// Largest XML part read whole (a slide, an OpenDocument content.xml, a DMG's plist).
const XML_CAP: u64 = 32 << 20;
const MAX_SLIDES: usize = 500;
/// Largest message parsed whole, and the most body text sent back.
const MAIL_CAP: u64 = 64 << 20;
const TEXT_CAP: usize = 2 << 20;
/// xz dictionaries beyond this are refused rather than allocated.
const XZ_MEM_KB: u32 = 256 << 10;
/// Time budgets: listing an archive, finding one entry by a sequential scan, a page of mbox.
const LIST_BUDGET: Duration = Duration::from_secs(6);
/// A details-pane preview only wants a glimpse.
const QUICK_BUDGET: Duration = Duration::from_millis(1500);
const STOPPED: &str = "stopped early (time limit); there may be more";
const SCAN_BUDGET: Duration = Duration::from_secs(30);
const MBOX_BUDGET: Duration = Duration::from_secs(3);

/// Bytes to stream back: one archive entry, file in a disk image, or mail attachment.
pub struct Part {
    /// The entry's own name (last path component), for its Content-Type and a download name.
    pub name: String,
    pub len: Option<u64>,
    pub body: Box<dyn Read + Send>,
}

fn part(name: &str, bytes: Vec<u8>) -> Part {
    Part {
        name: leaf(name).to_string(),
        len: Some(bytes.len() as u64),
        body: Box::new(io::Cursor::new(bytes)),
    }
}

fn leaf(name: &str) -> &str {
    name.trim_end_matches('/')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
}

// ---- small helpers

/// A GUID as Microsoft and GPT store it (first three fields little-endian).
fn guid(b: &[u8]) -> String {
    let tail: String = b
        .get(10..16)
        .unwrap_or_default()
        .iter()
        .map(|x| format!("{x:02X}"))
        .collect();
    format!(
        "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{tail}",
        le(b, 0, 4),
        le(b, 4, 2),
        le(b, 6, 2),
        b.get(8).copied().unwrap_or(0),
        b.get(9).copied().unwrap_or(0)
    )
}

/// Days from 1970-01-01 to a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

fn clip(s: &str, max: usize) -> Cow<'_, str> {
    if s.len() > max {
        Cow::Owned(format!("{}…", &s[..s.floor_char_boundary(max)]))
    } else {
        Cow::Borrowed(s)
    }
}

/// Fails reads once a time budget is spent, so a walk through a compressed stream stops even in
/// the middle of one huge entry.
struct Deadline<R> {
    inner: R,
    until: Instant,
}

impl<R: Read> Read for Deadline<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if Instant::now() > self.until {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "time budget spent"));
        }
        self.inner.read(buf)
    }
}

/// Reads as much of `buf` as the stream has.
fn fill(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

// ---- XML helpers (quick-xml hands entity references over separately)

fn xml(bytes: &[u8]) -> Xml<&[u8]> {
    let mut r = Xml::from_reader(bytes);
    r.config_mut().check_end_names = false;
    r
}

/// An attribute's value by its local name, whatever its prefix. (OOXML and OpenDocument parts
/// are UTF-8.)
fn attr(e: &BytesStart, local: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == local)
        .map(|a| decode_entities(&String::from_utf8_lossy(&a.value)).into_owned())
}

/// A prefixed attribute only (`r:id` rather than a plain `id`).
fn attr_ns(e: &BytesStart, local: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.prefix().is_some() && a.key.local_name().as_ref() == local)
        .map(|a| decode_entities(&String::from_utf8_lossy(&a.value)).into_owned())
}

fn entity(r: &BytesRef) -> Option<char> {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return Some(c);
    }
    match r.decode().ok()?.as_ref() {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => None,
    }
}

// ---- spreadsheets (calamine: xlsx/xlsb stream cell by cell, xls/ods load the one sheet)

const XLSX_CAP: u64 = 256 << 20;
const XLS_CAP: u64 = 64 << 20;
/// Parts calamine holds in memory whole (unpacked sizes): shared strings, styles, and an ODS
/// file's content.xml (every sheet).
const HELD_PARTS: [(&str, u64); 6] = [
    ("xl/sharedStrings.xml", 128 << 20),
    ("xl/sharedStrings.bin", 128 << 20),
    ("xl/styles.xml", 32 << 20),
    ("xl/styles.bin", 32 << 20),
    ("xl/workbook.xml", 32 << 20),
    ("content.xml", 64 << 20),
];

/// One page of a sheet: rows `[lo, hi)`, the first `MAX_COLS` columns.
struct Page {
    lo: u64,
    hi: u64,
    rows: Vec<Vec<Value>>,
    width: usize,
    more: bool,
    cut: bool,
    early: bool,
    seen: u64,
    until: Instant,
}

impl Page {
    /// Places one cell; false once the page is full (or the time budget spent).
    fn put(&mut self, (r, c): (u32, u32), v: &calamine::Data) -> bool {
        self.seen += 1;
        if self.seen.is_multiple_of(4096) && Instant::now() > self.until {
            self.more = true;
            self.early = true;
            return false;
        }
        let (r, c) = (u64::from(r), c as usize);
        if r < self.lo {
            return true;
        }
        if r >= self.hi {
            self.more = true;
            return false;
        }
        if matches!(v, calamine::Data::Empty) {
            return true;
        }
        if c >= MAX_COLS {
            self.cut = true;
            return true;
        }
        let i = (r - self.lo) as usize;
        if self.rows.len() <= i {
            self.rows.resize(i + 1, Vec::new());
        }
        let row = &mut self.rows[i];
        if row.len() <= c {
            row.resize(c + 1, Value::Null);
        }
        row[c] = cell_json(v);
        self.width = self.width.max(c + 1);
        true
    }
}

fn cell_json(d: &calamine::Data) -> Value {
    use calamine::Data;
    match d {
        Data::Empty => Value::Null,
        Data::String(s) | Data::DateTimeIso(s) | Data::DurationIso(s) => json!(clip(s, 4000)),
        Data::Float(f) => json!(f),
        Data::Int(i) => json!(i),
        Data::Bool(b) => json!(b),
        Data::Error(e) => json!(e.to_string()),
        Data::DateTime(dt) => {
            let (y, mo, d, h, mi, s, _) = dt.to_ymd_hms_milli();
            let v = dt.as_f64();
            json!(if dt.is_duration() {
                let secs = (v * 86_400.0).round() as i64;
                format!("{}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
            } else if v < 1.0 {
                format!("{h:02}:{mi:02}:{s:02}")
            } else if (h, mi, s) == (0, 0, 0) {
                format!("{y:04}-{mo:02}-{d:02}")
            } else {
                format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
            })
        }
    }
}

/// Sheet names and one page of a sheet's rows (`offset` counts from the sheet's first row, 0-based).
pub fn sheet(path: &Path, want: Option<&str>, offset: u64, limit: u64) -> Result<Value> {
    use calamine::{Reader, SheetType, SheetVisible, Sheets, open_workbook};
    let size = std::fs::metadata(path)?.len();
    let magic = read_at(&File::open(path)?, 0, 4)?;
    let too_big = |cap: u64| -> Result<()> {
        if size > cap {
            bail!(
                "too large to open here ({}; the limit for this format is {})",
                fmt::bytes(size),
                fmt::bytes(cap)
            );
        }
        Ok(())
    };
    let mut book: Sheets<BufReader<File>> = if magic == [0xD0, 0xCF, 0x11, 0xE0] {
        too_big(XLS_CAP)?;
        Sheets::Xls(
            open_workbook(path).map_err(|e| anyhow::anyhow!("can't read this workbook: {e}"))?,
        )
    } else if magic.starts_with(b"PK") {
        too_big(XLSX_CAP)?;
        // look at the unpacked sizes first, so a zip bomb is refused rather than inflated
        let z = Zip::open(path)?;
        for (part, cap) in HELD_PARTS {
            if let Some(e) = z.get(part)
                && e.size > cap
            {
                bail!(
                    "too large to open here ({part} unpacks to {})",
                    fmt::bytes(e.size)
                );
            }
        }
        if let Ok(b) = open_workbook(path) {
            Sheets::Xlsx(b)
        } else if let Ok(b) = open_workbook(path) {
            Sheets::Xlsb(b)
        } else {
            Sheets::Ods(
                open_workbook(path)
                    .map_err(|e| anyhow::anyhow!("not a spreadsheet BackDown can read: {e}"))?,
            )
        }
    } else {
        bail!("not a spreadsheet BackDown can read");
    };
    let sheets: Vec<(String, bool)> = book
        .sheets_metadata()
        .iter()
        .filter(|s| s.typ == SheetType::WorkSheet)
        .map(|s| (s.name.clone(), s.visible != SheetVisible::Visible))
        .collect();
    let name = want
        .filter(|w| sheets.iter().any(|(n, _)| n == w))
        .map(str::to_string)
        .or_else(|| {
            sheets
                .iter()
                .find(|(_, hidden)| !hidden)
                .or(sheets.first())
                .map(|(n, _)| n.clone())
        })
        .context("this workbook has no worksheets")?;
    let limit = limit.clamp(1, SHEET_ROWS);
    let mut page = Page {
        lo: offset,
        hi: offset.saturating_add(limit),
        rows: Vec::new(),
        width: 0,
        more: false,
        cut: false,
        early: false,
        seen: 0,
        until: Instant::now() + LIST_BUDGET,
    };
    let mut dims = None;
    macro_rules! stream {
        ($cells:expr) => {{
            let mut cells = $cells;
            let d = cells.dimensions();
            if d.end != (0, 0) {
                dims = Some((u64::from(d.end.0) + 1, u64::from(d.end.1) + 1));
            }
            while let Some(c) = cells.next_cell()? {
                if !page.put(
                    c.get_position(),
                    &calamine::Data::from(c.get_value().clone()),
                ) {
                    break;
                }
            }
            true
        }};
    }
    let streamed = match &mut book {
        Sheets::Xlsx(x) => stream!(x.worksheet_cells_reader(&name)?),
        Sheets::Xlsb(x) => stream!(x.worksheet_cells_reader(&name)?),
        _ => false,
    };
    if !streamed {
        let range = book.worksheet_range(&name)?;
        if let (Some(s), Some(e)) = (range.start(), range.end()) {
            dims = Some((u64::from(e.0) + 1, u64::from(e.1) + 1));
            let last_col = e.1.min(MAX_COLS as u32 - 1);
            for r in page.lo.max(u64::from(s.0))..page.hi.min(u64::from(e.0) + 1) {
                for c in s.1..=last_col {
                    if let Some(v) = range.get_value((r as u32, c)) {
                        page.put((r as u32, c), v);
                    }
                }
            }
            page.more = u64::from(e.0) + 1 > page.hi;
            page.cut = e.1 as usize >= MAX_COLS;
        }
    }
    let width = dims
        .map_or(0, |(_, cols)| (cols as usize).min(MAX_COLS))
        .max(page.width);
    Ok(json!({
        "sheets": sheets.iter().map(|(n, h)| json!({ "name": n, "hidden": h })).collect::<Vec<_>>(),
        "sheet": name,
        "offset": offset,
        "rows": page.rows,
        "width": width,
        "total_rows": dims.map(|d| d.0),
        "total_cols": dims.map(|d| d.1),
        "more": page.more,
        "cut": page.cut,
        "early": page.early,
    }))
}

// ---- zip: the central directory, read record by record (never all at once)

struct ZipEntry {
    name: String,
    dir: bool,
    size: u64,
    csize: u64,
    method: u64,
    flags: u64,
    /// the local header's absolute position
    header: u64,
    mtime: Option<i64>,
}

struct ZipDir {
    entries: u64,
    start: u64,
    size: u64,
    /// bytes in front of the archive proper (a self-extractor's stub), added to every offset
    shift: u64,
}

/// The end-of-central-directory record (zip64 aware), or None if this isn't a zip.
fn zip_dir(f: &File, len: u64) -> Result<Option<ZipDir>> {
    if len < 22 {
        return Ok(None);
    }
    let tail_len = len.min(22 + 0xFFFF);
    let base = len - tail_len;
    let tail = read_at(f, base, tail_len as usize)?;
    let Some(i) = (0..=tail.len().saturating_sub(22)).rev().find(|&i| {
        has(&tail, i, b"PK\x05\x06") && i + 22 + le(&tail, i + 20, 2) as usize <= tail.len()
    }) else {
        return Ok(None);
    };
    let eocd = base + i as u64;
    let (mut entries, mut size, mut offset, mut end) = (
        le(&tail, i + 10, 2),
        le(&tail, i + 12, 4),
        le(&tail, i + 16, 4),
        eocd,
    );
    if eocd >= 20 {
        let loc = read_at(f, eocd - 20, 20)?;
        if has(&loc, 0, b"PK\x06\x07") {
            // the zip64 record sits right before its locator, unless it carries extra data
            for pos in [(eocd - 20).saturating_sub(56), le(&loc, 8, 8)] {
                let rec = read_at(f, pos, 56)?;
                if has(&rec, 0, b"PK\x06\x06") {
                    (entries, size, offset, end) =
                        (le(&rec, 32, 8), le(&rec, 40, 8), le(&rec, 48, 8), pos);
                    break;
                }
            }
        }
    }
    let Some(start) = end.checked_sub(size) else {
        return Ok(None);
    };
    Ok(Some(ZipDir {
        entries,
        start,
        size,
        shift: start.saturating_sub(offset),
    }))
}

fn dos_time(date: u64, time: u64) -> Option<i64> {
    let (m, d) = ((date >> 5) & 15, date & 31);
    if m == 0 || d == 0 {
        return None;
    }
    let day = days_from_civil(1980 + (date >> 9) as i64, m as i64, d as i64);
    Some(day * 86_400 + ((time >> 11) * 3600 + ((time >> 5) & 63) * 60 + (time & 31) * 2) as i64)
}

/// Names from zips made without the UTF-8 flag are in the DOS code page.
fn cp437(b: &[u8]) -> String {
    const HIGH: &str = concat!(
        "ÇüéâäàåçêëèïîìÄÅ",
        "ÉæÆôöòûùÿÖÜ¢£¥₧ƒ",
        "áíóúñÑªº¿⌐¬½¼¡«»",
        "░▒▓│┤╡╢╖╕╣║╗╝╜╛┐",
        "└┴┬├─┼╞╟╚╔╩╦╠═╬╧",
        "╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀",
        "αßΓπΣσµτΦΘΩδ∞φε∩",
        "≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}",
    );
    b.iter()
        .map(|&c| {
            if c < 128 {
                c as char
            } else {
                HIGH.chars().nth(c as usize - 128).unwrap_or('?')
            }
        })
        .collect()
}

/// Calls `each` for every central-directory record in order; stops when it returns false or the
/// budget is spent. Returns whether the whole directory was walked.
fn zip_walk(
    f: &File,
    d: &ZipDir,
    until: Instant,
    mut each: impl FnMut(ZipEntry) -> bool,
) -> Result<bool> {
    let mut r = f;
    r.seek(SeekFrom::Start(d.start))?;
    let mut r = BufReader::with_capacity(1 << 16, r.take(d.size));
    let mut fixed = [0u8; 46];
    let mut var = Vec::new();
    let mut n = 0u64;
    loop {
        n += 1;
        if n.is_multiple_of(4096) && Instant::now() > until {
            return Ok(false);
        }
        match r.read_exact(&mut fixed) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(true),
            Err(e) => return Err(e.into()),
        }
        if !has(&fixed, 0, b"PK\x01\x02") {
            return Ok(true);
        }
        let (nl, xl, cl) = (
            le(&fixed, 28, 2) as usize,
            le(&fixed, 30, 2) as usize,
            le(&fixed, 32, 2) as usize,
        );
        var.resize(nl + xl + cl, 0);
        r.read_exact(&mut var)?;
        let flags = le(&fixed, 8, 2);
        let mut e = ZipEntry {
            name: String::new(),
            dir: false,
            size: le(&fixed, 24, 4),
            csize: le(&fixed, 20, 4),
            method: le(&fixed, 10, 2),
            flags,
            header: le(&fixed, 42, 4),
            mtime: dos_time(le(&fixed, 14, 2), le(&fixed, 12, 2)),
        };
        let mut unicode = None;
        let mut x = &var[nl..nl + xl];
        while x.len() >= 4 {
            let (id, len) = (le(x, 0, 2), le(x, 2, 2) as usize);
            let body = &x[4..(4 + len).min(x.len())];
            match id {
                // zip64: only the fields whose 32-bit slot is saturated, in this order
                0x0001 => {
                    let mut o = 0;
                    for v in [&mut e.size, &mut e.csize, &mut e.header] {
                        if *v == 0xFFFF_FFFF {
                            *v = le(body, o, 8);
                            o += 8;
                        }
                    }
                }
                // extended timestamp (Info-ZIP): a Unix mtime
                0x5455 if body.first().is_some_and(|b| b & 1 != 0) && body.len() >= 5 => {
                    e.mtime = Some(i64::from(le(body, 1, 4) as u32 as i32));
                }
                // NTFS times (7-Zip, Windows): FILETIME in tag 1
                0x000a if le(body, 4, 2) == 1 && body.len() >= 16 => {
                    let ft = le(body, 8, 8) as i64;
                    e.mtime = Some(ft / 10_000_000 - 11_644_473_600);
                }
                // Info-ZIP Unicode path
                0x7075 if body.first() == Some(&1) && body.len() > 5 => {
                    unicode = std::str::from_utf8(&body[5..]).ok().map(str::to_string);
                }
                _ => {}
            }
            x = &x[(4 + len).min(x.len())..];
        }
        let raw = &var[..nl];
        e.name = unicode.unwrap_or_else(|| {
            if flags & 0x800 != 0 {
                String::from_utf8_lossy(raw).into_owned()
            } else {
                std::str::from_utf8(raw).map_or_else(|_| cp437(raw), str::to_string)
            }
        });
        e.dir = e.name.ends_with('/') || e.name.ends_with('\\');
        e.header += d.shift;
        if !each(e) {
            return Ok(false);
        }
    }
}

fn zip_method(m: u64) -> &'static str {
    match m {
        1 => "Shrink",
        6 => "Implode",
        14 => "LZMA",
        98 => "PPMd",
        99 => "AES encryption",
        _ => "unknown",
    }
}

/// The decompressed bytes of one entry (at most its declared size).
fn zip_open(f: File, e: &ZipEntry) -> Result<Box<dyn Read + Send>> {
    if e.flags & 1 != 0 || e.method == 99 {
        bail!("{} is encrypted", e.name);
    }
    let lh = read_at(&f, e.header, 30)?;
    if !has(&lh, 0, b"PK\x03\x04") {
        bail!("damaged archive: {} has no local header", e.name);
    }
    let mut f = f;
    f.seek(SeekFrom::Start(
        e.header + 30 + le(&lh, 26, 2) + le(&lh, 28, 2),
    ))?;
    let raw = BufReader::new(f.take(e.csize));
    let r: Box<dyn Read + Send> = match e.method {
        0 => Box::new(raw),
        8 => Box::new(flate2::bufread::DeflateDecoder::new(raw)),
        9 => Box::new(deflate64::Deflate64Decoder::with_buffer(raw)),
        12 => Box::new(bzip2::read::BzDecoder::new(raw)),
        93 => Box::new(Zstd::new(raw)?),
        95 => Box::new(lzma_rust2::XzReader::new_mem_limit(raw, true, XZ_MEM_KB)),
        m => bail!(
            "{} is compressed with a method BackDown can't read ({}, #{m})",
            e.name,
            zip_method(m)
        ),
    };
    Ok(Box::new(r.take(e.size)))
}

/// A zip-based document's parts by name (for slide decks).
struct Zip {
    file: File,
    entries: HashMap<String, ZipEntry>,
}

impl Zip {
    fn open(path: &Path) -> Result<Zip> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        let d = zip_dir(&file, len)?.context("not a zip-based document")?;
        let mut entries = HashMap::new();
        zip_walk(&file, &d, Instant::now() + LIST_BUDGET, |e| {
            entries.insert(e.name.clone(), e);
            entries.len() < 100_000
        })?;
        Ok(Zip { file, entries })
    }

    /// Part names are case-insensitive in OOXML; try the exact name first.
    fn get(&self, name: &str) -> Option<&ZipEntry> {
        self.entries.get(name).or_else(|| {
            self.entries
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v)
        })
    }

    fn read(&self, name: &str, cap: u64) -> Result<Option<Vec<u8>>> {
        let Some(e) = self.get(name) else {
            return Ok(None);
        };
        if e.size > cap {
            bail!("{name} is too large ({})", fmt::bytes(e.size));
        }
        let mut buf = Vec::with_capacity(e.size as usize);
        zip_open(self.file.try_clone()?, e)?.read_to_end(&mut buf)?;
        Ok(Some(buf))
    }
}

// ---- compressed streams and tar

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Codec {
    Plain,
    Gzip,
    Bzip2,
    Xz,
    Zstd,
}

impl Codec {
    pub(crate) fn sniff(head: &[u8]) -> Codec {
        if head.starts_with(&[0x1F, 0x8B]) {
            Codec::Gzip
        } else if head.starts_with(b"BZh") {
            Codec::Bzip2
        } else if head.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0]) {
            Codec::Xz
        } else if head.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
            Codec::Zstd
        } else {
            Codec::Plain
        }
    }

    fn ext(self) -> &'static str {
        match self {
            Codec::Plain => "",
            Codec::Gzip => "gz",
            Codec::Bzip2 => "bz2",
            Codec::Xz => "xz",
            Codec::Zstd => "zst",
        }
    }

    pub(crate) fn open(self, f: File) -> Result<Box<dyn Read + Send>> {
        let r = BufReader::with_capacity(1 << 16, f);
        Ok(match self {
            Codec::Plain => Box::new(r),
            Codec::Gzip => Box::new(flate2::bufread::MultiGzDecoder::new(r)),
            Codec::Bzip2 => Box::new(bzip2::read::MultiBzDecoder::new(r)),
            Codec::Xz => Box::new(lzma_rust2::XzReader::new_mem_limit(r, true, XZ_MEM_KB)),
            Codec::Zstd => Box::new(Zstd::new(r)?),
        })
    }
}

/// Zstandard files may hold several frames back to back; the streaming decoder reads one, so this
/// starts a new one at each frame boundary.
struct Zstd<R: BufRead> {
    dec: Option<ruzstd::decoding::StreamingDecoder<R, ruzstd::decoding::FrameDecoder>>,
}

impl<R: BufRead> Zstd<R> {
    fn new(r: R) -> Result<Self> {
        let dec = ruzstd::decoding::StreamingDecoder::new(r)
            .map_err(|e| anyhow::anyhow!("not a Zstandard stream: {e}"))?;
        Ok(Zstd { dec: Some(dec) })
    }
}

impl<R: BufRead> Read for Zstd<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let Some(d) = self.dec.as_mut() else {
                return Ok(0);
            };
            let n = d.read(buf)?;
            if n > 0 || buf.is_empty() {
                return Ok(n);
            }
            let Some(d) = self.dec.take() else {
                return Ok(0);
            };
            let mut src = d.into_inner();
            if src.fill_buf()?.is_empty() {
                return Ok(0);
            }
            self.dec = Some(
                ruzstd::decoding::StreamingDecoder::new(src)
                    .map_err(|e| io::Error::other(e.to_string()))?,
            );
        }
    }
}

/// A tar header block: its checksum adds up (an all-zero block doesn't count).
fn is_tar(b: &[u8]) -> bool {
    let Some(h) = b.get(..512) else {
        return false;
    };
    let digits: String = h[148..156]
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as char)
        .collect();
    let Ok(want) = u32::from_str_radix(digits.trim(), 8) else {
        return false;
    };
    let sum: u32 = h
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            if (148..156).contains(&i) {
                32
            } else {
                u32::from(c)
            }
        })
        .sum();
    want == sum && sum != 256
}

struct TarWalk {
    entries: Vec<Value>,
    count: u64,
    bytes: u64,
    complete: bool,
    note: Option<String>,
}

fn tar_walk<R: Read>(entries: tar::Entries<'_, R>, limit: usize, until: Instant) -> TarWalk {
    let mut w = TarWalk {
        entries: Vec::new(),
        count: 0,
        bytes: 0,
        complete: false,
        note: None,
    };
    for e in entries {
        if Instant::now() > until {
            w.note = Some(STOPPED.into());
            return w;
        }
        let e = match e {
            Ok(e) => e,
            Err(err) => {
                w.note = Some(if err.kind() == io::ErrorKind::TimedOut {
                    STOPPED.into()
                } else {
                    format!("stopped at a damaged or truncated part: {err}")
                });
                return w;
            }
        };
        let t = e.header().entry_type();
        if t.is_pax_global_extensions() || t.is_pax_local_extensions() {
            continue;
        }
        w.count += 1;
        if t.is_file() || t.is_contiguous() || t.is_gnu_sparse() {
            w.bytes += e.size();
        }
        if w.entries.len() < limit {
            let link = if t.is_symlink() || t.is_hard_link() {
                e.link_name_bytes()
                    .map(|l| String::from_utf8_lossy(&l).into_owned())
            } else {
                None
            };
            w.entries.push(json!({
                "name": String::from_utf8_lossy(&e.path_bytes()),
                "size": if t.is_dir() || link.is_some() { 0 } else { e.size() },
                "mtime": e.header().mtime().ok(),
                "dir": t.is_dir(),
                "link": link,
            }));
        }
    }
    w.complete = true;
    w
}

/// The name a lone compressed file had: the gzip header's, else ours minus the extension.
fn single_name(path: &Path, codec: Codec, head: &[u8]) -> String {
    if codec == Codec::Gzip && head.get(3).is_some_and(|f| f & 8 != 0) {
        let mut i = 10;
        if head[3] & 4 != 0 {
            i += 2 + le(head, 10, 2) as usize;
        }
        if let Some(rest) = head.get(i..)
            && let Some(end) = rest.iter().position(|&c| c == 0)
        {
            let n = String::from_utf8_lossy(&rest[..end]).into_owned();
            let n = leaf(&n).to_string();
            if !n.is_empty() {
                return n;
            }
        }
    }
    let own = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = own
        .rsplit_once('.')
        .filter(|(_, x)| {
            matches!(
                x.to_ascii_lowercase().as_str(),
                "gz" | "gzip" | "z" | "bz2" | "bz" | "xz" | "zst" | "zstd"
            )
        })
        .map_or(own.as_str(), |(s, _)| s);
    if stem.is_empty() {
        "data".into()
    } else {
        stem.to_string()
    }
}

/// An archive's entries: zip, tar, tar in gzip/bzip2/xz/zstd, or a single compressed file.
/// `quick`: a short time budget, for a preview.
pub fn archive(path: &Path, limit: usize, quick: bool) -> Result<Value> {
    let limit = limit.clamp(1, MAX_ENTRIES);
    let until = Instant::now() + if quick { QUICK_BUDGET } else { LIST_BUDGET };
    let f = File::open(path)?;
    let len = f.metadata()?.len();
    let head = read_at(&f, 0, 4096)?;
    let codec = Codec::sniff(&head);
    if codec == Codec::Plain {
        if is_tar(&head) {
            let mut ar = tar::Archive::new(File::open(path)?);
            let w = tar_walk(ar.entries_with_seek()?, limit, until);
            return Ok(tar_json("tar", w));
        }
        if let Some(d) = zip_dir(&f, len)? {
            let (mut entries, mut count, mut bytes) = (Vec::new(), 0u64, 0u64);
            let complete = zip_walk(&f, &d, until, |e| {
                count += 1;
                bytes += e.size;
                if entries.len() < limit {
                    entries.push(json!({
                        "name": e.name, "size": e.size, "csize": e.csize, "mtime": e.mtime,
                        "dir": e.dir, "encrypted": e.flags & 1 != 0 || e.method == 99,
                    }));
                }
                true
            })?;
            return Ok(json!({
                "format": "zip",
                "entries": entries,
                "total": d.entries.max(count),
                "total_size": bytes,
                "complete": complete,
                "note": (!complete).then_some(STOPPED),
            }));
        }
        bail!("not an archive BackDown can read");
    }
    // its own handle: a try_clone would share the file position with `f`
    let mut r = codec.open(File::open(path)?)?;
    let mut first = vec![0u8; 512];
    let n = fill(&mut r, &mut first).context("can't decompress this file")?;
    first.truncate(n);
    if is_tar(&first) {
        let stream = Deadline {
            inner: io::Cursor::new(first).chain(r),
            until,
        };
        let mut ar = tar::Archive::new(stream);
        let w = tar_walk(ar.entries()?, limit, until);
        return Ok(tar_json(&format!("tar.{}", codec.ext()), w));
    }
    // one compressed file: its name, and its size if cheap (gzip says it) or countable in time
    let name = single_name(path, codec, &head);
    let size = if codec == Codec::Gzip && len >= 18 {
        Some(le(&read_at(&f, len - 4, 4)?, 0, 4))
    } else {
        let mut count = n as u64;
        let mut buf = vec![0u8; 1 << 16];
        loop {
            if Instant::now() > until {
                break None;
            }
            match r.read(&mut buf) {
                Ok(0) => break Some(count),
                Ok(k) => count += k as u64,
                Err(_) => break None,
            }
        }
    };
    let mtime = (codec == Codec::Gzip)
        .then(|| le(&head, 4, 4))
        .filter(|&t| t > 0);
    Ok(json!({
        "format": codec.ext(),
        "entries": [{ "name": name, "size": size, "mtime": mtime, "dir": false }],
        "total": 1,
        "total_size": size.unwrap_or(0),
        "complete": true,
        "note": if codec == Codec::Gzip && len > u64::from(u32::MAX) {
            Some("gzip records sizes modulo 4 GiB; this one is larger")
        } else if size.is_none() {
            Some("uncompressed size unknown (too large to count here)")
        } else {
            None
        },
    }))
}

fn tar_json(format: &str, w: TarWalk) -> Value {
    json!({
        "format": format,
        "entries": w.entries,
        "total": w.complete.then_some(w.count),
        "counted": w.count,
        "total_size": w.bytes,
        "complete": w.complete,
        "note": w.note,
    })
}

/// One entry's bytes, chosen by its name exactly as the listing gave it (never a filesystem path).
/// At most `max` bytes (and never more than `ENTRY_CAP`); without `max`, an entry larger than the
/// cap is refused rather than cut short.
pub fn entry(path: &Path, name: &str, max: Option<u64>) -> Result<Part> {
    let f = File::open(path)?;
    let len = f.metadata()?.len();
    let head = read_at(&f, 0, 4096)?;
    let cap = max.unwrap_or(ENTRY_CAP).min(ENTRY_CAP);
    let fits = |size: u64| -> Result<u64> {
        if size > ENTRY_CAP && max.is_none() {
            bail!(
                "{} is too large to open from inside the archive ({}; the limit is {})",
                leaf(name),
                fmt::bytes(size),
                fmt::bytes(ENTRY_CAP)
            );
        }
        Ok(size.min(cap))
    };
    let missing = || anyhow::anyhow!("no entry named {name:?} in this archive");
    let codec = Codec::sniff(&head);
    if codec == Codec::Plain {
        if is_tar(&head) {
            let mut ar = tar::Archive::new(File::open(path)?);
            let mut found = None;
            for e in ar.entries_with_seek()? {
                let mut e = e?;
                if String::from_utf8_lossy(&e.path_bytes()) != name {
                    continue;
                }
                let t = e.header().entry_type();
                if t.is_dir() {
                    bail!("{name} is a folder");
                }
                if t.is_file() || t.is_contiguous() {
                    found = Some((e.raw_file_position(), e.size()));
                } else {
                    // links and sparse files: read through the tar reader
                    let n = fits(e.size())?;
                    let mut buf = Vec::new();
                    (&mut e).take(n).read_to_end(&mut buf)?;
                    return Ok(part(name, buf));
                }
                break;
            }
            let (pos, size) = found.ok_or_else(missing)?;
            let n = fits(size)?;
            let mut f = f;
            f.seek(SeekFrom::Start(pos))?;
            return Ok(Part {
                name: leaf(name).to_string(),
                len: Some(n),
                body: Box::new(f.take(n)),
            });
        }
        if let Some(v) = iso_volume(&f)? {
            let e = iso_find(&f, &v, name)?;
            if e.dir {
                bail!("{name} is a folder");
            }
            let n = fits(e.size)?;
            let mut f = f;
            f.seek(SeekFrom::Start(e.lba * v.block))?;
            return Ok(Part {
                name: e.name,
                len: Some(n),
                body: Box::new(f.take(n)),
            });
        }
        if let Some(d) = zip_dir(&f, len)? {
            let mut hit = None;
            zip_walk(&f, &d, Instant::now() + SCAN_BUDGET, |e| {
                if e.name == name {
                    hit = Some(e);
                    false
                } else {
                    true
                }
            })?;
            let e = hit.ok_or_else(missing)?;
            if e.dir {
                bail!("{name} is a folder");
            }
            let n = fits(e.size)?;
            return Ok(Part {
                name: leaf(name).to_string(),
                len: Some(n),
                body: Box::new(zip_open(f, &e)?.take(n)),
            });
        }
        bail!("not an archive BackDown can read");
    }
    let mut r = codec.open(File::open(path)?)?;
    let mut first = vec![0u8; 512];
    let got = fill(&mut r, &mut first).context("can't decompress this file")?;
    first.truncate(got);
    if is_tar(&first) {
        // sequential: decompress up to the entry (within the scan budget), then read it whole
        let stream = Deadline {
            inner: io::Cursor::new(first).chain(r),
            until: Instant::now() + SCAN_BUDGET,
        };
        let mut ar = tar::Archive::new(stream);
        for e in ar.entries()? {
            let mut e = e.map_err(|err| {
                if err.kind() == io::ErrorKind::TimedOut {
                    anyhow::anyhow!(
                        "gave up looking for {name} after {} s",
                        SCAN_BUDGET.as_secs()
                    )
                } else {
                    err.into()
                }
            })?;
            if String::from_utf8_lossy(&e.path_bytes()) != name {
                continue;
            }
            if e.header().entry_type().is_dir() {
                bail!("{name} is a folder");
            }
            let n = fits(e.size())?;
            let mut buf = Vec::with_capacity(n as usize);
            (&mut e).take(n).read_to_end(&mut buf)?;
            return Ok(part(name, buf));
        }
        return Err(missing());
    }
    if name != single_name(path, codec, &head) {
        return Err(missing());
    }
    if codec == Codec::Gzip && len >= 18 && len <= u64::from(u32::MAX) {
        fits(le(&read_at(&f, len - 4, 4)?, 0, 4))?;
    }
    Ok(Part {
        name: leaf(name).to_string(),
        len: None,
        body: Box::new(io::Cursor::new(first).chain(r).take(cap)),
    })
}

// ---- slide decks: PowerPoint (pptx) and OpenDocument (odp)

#[derive(Default)]
struct Slide {
    title: String,
    paras: Vec<(String, u32)>,
    images: Vec<String>,
    notes: Vec<String>,
}

impl Slide {
    /// No title placeholder (text boxes only, as many generators write): a short first line is
    /// the title.
    fn settle(&mut self) {
        if self.title.is_empty()
            && let Some((t, 0)) = self.paras.first()
            && t.len() <= 200
            && !t.contains('\n')
        {
            self.title = self.paras.remove(0).0;
        }
    }

    /// Slide `n` for the viewer; its text only while the deck's `room` for text lasts.
    fn json(&self, n: usize, room: &mut usize) -> Value {
        let len = self.paras.iter().map(|(t, _)| t.len()).sum::<usize>()
            + self.notes.iter().map(String::len).sum::<usize>();
        let fits = len <= *room;
        *room = room.saturating_sub(len);
        json!({
            "n": n,
            "title": self.title,
            "paras": if fits { self.paras.iter().map(|(t, l)| json!({ "t": t, "lvl": l })).collect() } else { Vec::new() },
            "images": self.images,
            "notes": if fits { self.notes.join("\n") } else { String::new() },
            "cut": !fits,
        })
    }
}

/// The folder part of a package part name ("ppt/slides/slide1.xml" -> "ppt/slides").
fn part_dir(name: &str) -> &str {
    name.rsplit_once('/').map_or("", |(d, _)| d)
}

/// A relative target resolved against a folder inside the package.
fn resolve(dir: &str, target: &str) -> String {
    let mut out: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        dir.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

/// A part's relationships: id -> (resolved target, type).
fn rels(z: &Zip, part: &str) -> Result<HashMap<String, (String, String)>> {
    let dir = part_dir(part);
    let file = part.rsplit('/').next().unwrap_or(part);
    let name = if dir.is_empty() {
        format!("_rels/{file}.rels")
    } else {
        format!("{dir}/_rels/{file}.rels")
    };
    let mut out = HashMap::new();
    let Some(bytes) = z.read(&name, XML_CAP)? else {
        return Ok(out);
    };
    let mut r = xml(&bytes);
    loop {
        match r.read_event()? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"Relationship" => {
                if attr(&e, b"TargetMode").as_deref() == Some("External") {
                    continue;
                }
                if let (Some(id), Some(target)) = (attr(&e, b"Id"), attr(&e, b"Target")) {
                    let kind = attr(&e, b"Type").unwrap_or_default();
                    out.insert(id, (resolve(dir, &target), kind));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// A paragraph: its text, bullet level, and the placeholder type of its shape ("title", "body",
/// "" for none).
type Para = (String, u32, String);

/// A PresentationML part's paragraphs, plus the images it embeds.
fn ooxml_text(
    bytes: &[u8],
    rels: &HashMap<String, (String, String)>,
) -> Result<(Vec<Para>, Vec<String>)> {
    let mut r = xml(bytes);
    let (mut paras, mut images) = (Vec::new(), Vec::new());
    let (mut ph, mut para, mut lvl, mut in_t) = (String::new(), None::<String>, 0u32, false);
    loop {
        let ev = r.read_event()?;
        let empty = matches!(ev, Event::Empty(_));
        match ev {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                b"sp" if !empty => ph.clear(),
                b"ph" => ph = attr(&e, b"type").unwrap_or_else(|| "body".into()),
                b"p" if !empty => {
                    para = Some(String::new());
                    lvl = 0;
                }
                b"pPr" => lvl = attr(&e, b"lvl").and_then(|v| v.parse().ok()).unwrap_or(0),
                b"t" if !empty => in_t = true,
                b"br" => {
                    if let Some(p) = para.as_mut() {
                        p.push('\n');
                    }
                }
                b"blip" => {
                    if let Some((target, _)) = attr_ns(&e, b"embed").and_then(|id| rels.get(&id))
                        && !images.contains(target)
                        && images.len() < 50
                    {
                        images.push(target.clone());
                    }
                }
                _ => {}
            },
            Event::Text(t) if in_t => {
                if let Some(p) = para.as_mut() {
                    p.push_str(&t.decode().unwrap_or_default());
                }
            }
            Event::GeneralRef(g) if in_t => {
                if let (Some(p), Some(c)) = (para.as_mut(), entity(&g)) {
                    p.push(c);
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                b"t" => in_t = false,
                b"p" => {
                    if let Some(p) = para.take() {
                        let p = p.trim();
                        if !p.is_empty() && paras.len() < 500 {
                            paras.push((clip(p, 10_000).into_owned(), lvl, ph.clone()));
                        }
                    }
                }
                b"sp" => ph.clear(),
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok((paras, images))
}

fn pptx(z: &Zip) -> Result<Value> {
    let pres = z
        .read("ppt/presentation.xml", XML_CAP)?
        .context("not a PowerPoint deck (no ppt/presentation.xml)")?;
    let prels = rels(z, "ppt/presentation.xml")?;
    let mut order = Vec::new();
    let mut r = xml(&pres);
    loop {
        match r.read_event()? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"sldId" => {
                if let Some((target, _)) = attr_ns(&e, b"id").and_then(|id| prels.get(&id)) {
                    order.push(target.clone());
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if order.is_empty() {
        // no slide list: take ppt/slides/slideN.xml in number order
        let mut names: Vec<(u32, String)> = z
            .entries
            .keys()
            .filter_map(|n| {
                let num = n.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?;
                Some((num.parse().ok()?, n.clone()))
            })
            .collect();
        names.sort();
        order = names.into_iter().map(|(_, n)| n).collect();
    }
    let (mut slides, mut room) = (Vec::new(), 2 * TEXT_CAP);
    for (i, name) in order.iter().take(MAX_SLIDES).enumerate() {
        let Some(bytes) = z.read(name, XML_CAP)? else {
            continue;
        };
        let srels = rels(z, name)?;
        let (paras, images) = ooxml_text(&bytes, &srels)?;
        let mut s = Slide {
            images,
            ..Default::default()
        };
        for (t, lvl, ph) in paras {
            match ph.as_str() {
                "title" | "ctrTitle" => {
                    if !s.title.is_empty() {
                        s.title.push(' ');
                    }
                    s.title.push_str(&t.replace('\n', " "));
                }
                "dt" | "ftr" | "sldNum" | "hdr" => {}
                _ => s.paras.push((t, lvl)),
            }
        }
        if let Some((notes, _)) = srels.values().find(|(_, k)| k.ends_with("/notesSlide"))
            && let Some(nb) = z.read(notes, XML_CAP)?
        {
            let (np, _) = ooxml_text(&nb, &HashMap::new())?;
            // the notes placeholder, or plain text boxes (not the slide image, number, header...)
            s.notes = np
                .into_iter()
                .filter(|(_, _, ph)| ph == "body" || ph.is_empty())
                .map(|(t, _, _)| t)
                .collect();
        }
        s.settle();
        slides.push(s.json(i + 1, &mut room));
    }
    let thumb = [
        "docProps/thumbnail.jpeg",
        "docProps/thumbnail.jpg",
        "docProps/thumbnail.png",
    ]
    .into_iter()
    .find(|n| z.get(n).is_some());
    Ok(json!({ "format": "pptx", "count": order.len(), "slides": slides, "thumbnail": thumb }))
}

fn odp(z: &Zip) -> Result<Value> {
    let bytes = z
        .read("content.xml", XML_CAP)?
        .context("not an OpenDocument presentation (no content.xml)")?;
    let mut r = xml(&bytes);
    let (mut slides, mut room, mut count) = (Vec::new(), 2 * TEXT_CAP, 0usize);
    let mut cur: Option<Slide> = None;
    let (mut class, mut in_notes, mut lists) = (String::new(), false, 0u32);
    let (mut para, mut para_lvl) = (None::<String>, 0u32);
    loop {
        let ev = r.read_event()?;
        let empty = matches!(ev, Event::Empty(_));
        match ev {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                b"page" if !empty => cur = Some(Slide::default()),
                b"frame" => class = attr(&e, b"class").unwrap_or_default(),
                b"notes" if !empty => in_notes = true,
                b"list" if !empty => lists += 1,
                b"p" | b"h" if !empty => {
                    para = Some(String::new());
                    para_lvl = lists.saturating_sub(1);
                }
                b"s" => {
                    if let Some(p) = para.as_mut() {
                        let n = attr(&e, b"c")
                            .and_then(|c| c.parse().ok())
                            .unwrap_or(1usize);
                        p.extend(std::iter::repeat_n(' ', n.min(100)));
                    }
                }
                b"tab" => {
                    if let Some(p) = para.as_mut() {
                        p.push('\t');
                    }
                }
                b"line-break" => {
                    if let Some(p) = para.as_mut() {
                        p.push('\n');
                    }
                }
                b"image" => {
                    if let (Some(s), Some(href)) = (cur.as_mut(), attr(&e, b"href"))
                        && !in_notes
                        && !href.contains("://")
                    {
                        let href = resolve("", &href);
                        if z.get(&href).is_some()
                            && !s.images.contains(&href)
                            && s.images.len() < 50
                        {
                            s.images.push(href);
                        }
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some(p) = para.as_mut() {
                    p.push_str(&t.decode().unwrap_or_default());
                }
            }
            Event::GeneralRef(g) => {
                if let (Some(p), Some(c)) = (para.as_mut(), entity(&g)) {
                    p.push(c);
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                b"p" | b"h" => {
                    if let (Some(s), Some(p)) = (cur.as_mut(), para.take()) {
                        let p = p.trim();
                        if p.is_empty() {
                        } else if in_notes {
                            s.notes.push(p.to_string());
                        } else if class == "title" {
                            if !s.title.is_empty() {
                                s.title.push(' ');
                            }
                            s.title.push_str(&p.replace('\n', " "));
                        } else if s.paras.len() < 500 {
                            s.paras.push((clip(p, 10_000).into_owned(), para_lvl));
                        }
                    }
                }
                b"list" => lists = lists.saturating_sub(1),
                b"frame" => class.clear(),
                b"notes" => in_notes = false,
                b"page" => {
                    if let Some(mut s) = cur.take() {
                        count += 1;
                        if slides.len() < MAX_SLIDES {
                            s.settle();
                            slides.push(s.json(count, &mut room));
                        }
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    let thumb = z
        .get("Thumbnails/thumbnail.png")
        .map(|_| "Thumbnails/thumbnail.png");
    Ok(json!({ "format": "odp", "count": count, "slides": slides, "thumbnail": thumb }))
}

/// A deck's slides in order: title, paragraphs with bullet levels, embedded images (by their entry
/// name, for `entry`) and speaker notes.
pub fn slides(path: &Path) -> Result<Value> {
    let z = Zip::open(path)?;
    if z.get("ppt/presentation.xml").is_some() {
        pptx(&z)
    } else if z.get("content.xml").is_some() {
        odp(&z)
    } else {
        bail!("not a slide deck BackDown can read")
    }
}

// ---- mail: single messages (.eml) and mbox files

pub(crate) fn blank(line: &[u8]) -> bool {
    line == b"\n" || line == b"\r\n"
}

fn has_clock(l: &[u8]) -> bool {
    l.windows(5).any(|w| {
        w[0].is_ascii_digit()
            && w[1].is_ascii_digit()
            && w[2] == b':'
            && w[3].is_ascii_digit()
            && w[4].is_ascii_digit()
    })
}

/// An mbox separator: "From " at the start of a line, after a blank line (or looking like
/// "From sender Mon Jan  1 12:00:00 2024", for writers that don't leave one).
pub(crate) fn is_from_line(line: &[u8], prev_blank: bool) -> bool {
    line.starts_with(b"From ") && (prev_blank || has_clock(line))
}

/// One line into `buf`, keeping at most `cap` bytes of it; returns the whole line's length.
pub(crate) fn line_capped(
    r: &mut impl BufRead,
    buf: &mut Vec<u8>,
    cap: usize,
) -> io::Result<usize> {
    buf.clear();
    let mut total = 0;
    loop {
        let chunk = r.fill_buf()?;
        if chunk.is_empty() {
            return Ok(total);
        }
        let (n, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        let keep = n.min(cap.saturating_sub(buf.len()));
        buf.extend_from_slice(&chunk[..keep]);
        r.consume(n);
        total += n;
        if done {
            return Ok(total);
        }
    }
}

pub(crate) fn addrs(a: Option<&mail_parser::Address>) -> String {
    a.map(|a| {
        a.iter()
            .filter_map(|x| match (x.name(), x.address()) {
                (Some(n), Some(e)) if !n.is_empty() => Some(format!("{n} <{e}>")),
                (_, Some(e)) => Some(e.to_string()),
                (Some(n), None) => Some(n.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(", ")
    })
    .unwrap_or_default()
}

/// The bytes of one message: the whole .eml, or the mbox message whose "From " line is at `at`
/// (">From " unescaped). Stops at `MAIL_CAP`; the flag says so.
fn message_bytes(path: &Path, at: Option<u64>) -> Result<(Vec<u8>, bool)> {
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(at.unwrap_or(0)))?;
    let mut r = BufReader::new(f.take(MAIL_CAP));
    let (mut out, mut line) = (Vec::new(), Vec::new());
    let (mut mbox, mut first, mut prev_blank) = (at.is_some(), true, true);
    let (mut read, mut ended) = (0u64, false);
    loop {
        line.clear();
        let n = r.read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        read += n as u64;
        if first {
            first = false;
            if line.starts_with(b"From ") {
                mbox = true;
                continue;
            }
            if at.is_some() {
                bail!("no message starts at this position");
            }
        } else if mbox && is_from_line(&line, prev_blank) {
            ended = true;
            break;
        }
        prev_blank = blank(&line);
        if mbox && line.starts_with(b">From ") {
            out.extend_from_slice(&line[1..]);
        } else {
            out.extend_from_slice(&line);
        }
    }
    Ok((out, !ended && read >= MAIL_CAP))
}

fn attachment_name(p: &mail_parser::MessagePart, i: usize) -> String {
    use mail_parser::MimeHeaders;
    if let Some(n) = p
        .attachment_name()
        .map(leaf)
        .filter(|n| !n.trim().is_empty())
    {
        return n.to_string();
    }
    if let Some(m) = p.message() {
        return format!(
            "{}.eml",
            m.subject().unwrap_or("message").replace(['/', '\\'], "_")
        );
    }
    let ext = p
        .content_type()
        .and_then(|c| c.subtype())
        .filter(|s| s.len() <= 8 && s.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(|s| format!(".{}", s.to_ascii_lowercase()))
        .unwrap_or_default();
    format!("attachment-{}{ext}", i + 1)
}

fn message_json(raw: &[u8], truncated: bool) -> Result<Value> {
    use mail_parser::{MessageParser, MimeHeaders, PartType};
    let msg = MessageParser::default()
        .parse(raw)
        .context("not a mail message")?;
    let (mut text, mut html) = (String::new(), false);
    for p in msg.text_bodies() {
        let t = match &p.body {
            PartType::Text(t) => t.to_string(),
            PartType::Html(h) => {
                html = true;
                html_text(h)
            }
            _ => continue,
        };
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(t.trim_end());
        if text.len() > TEXT_CAP {
            break;
        }
    }
    let cut = text.len() > TEXT_CAP;
    if cut {
        text.truncate(text.floor_char_boundary(TEXT_CAP));
    }
    let attachments: Vec<Value> = msg
        .attachments()
        .enumerate()
        .map(|(i, p)| {
            let ct = p.content_type().map(|c| match c.subtype() {
                Some(s) => format!("{}/{s}", c.ctype()),
                None => c.ctype().to_string(),
            });
            json!({ "part": i, "name": attachment_name(p, i), "type": ct, "size": p.len() })
        })
        .collect();
    Ok(json!({
        "from": addrs(msg.from()),
        "to": addrs(msg.to()),
        "cc": addrs(msg.cc()),
        "reply_to": addrs(msg.reply_to()),
        "subject": msg.subject(),
        "date": msg.date().map(|d| d.to_timestamp()),
        "date_text": msg.date().map(|d| d.to_rfc3339()),
        "message_id": msg.message_id(),
        "text": text,
        "from_html": html,
        "attachments": attachments,
        "truncated": truncated || cut,
    }))
}

/// One message, parsed: headers, the text body (HTML converted to plain text here, never rendered)
/// and its attachments' names, types and sizes. `at`: an mbox message's offset from `mbox`.
pub fn mail(path: &Path, at: Option<u64>) -> Result<Value> {
    let (raw, truncated) = message_bytes(path, at)?;
    message_json(&raw, truncated)
}

/// Attachment `n` of a message (as numbered by `mail`), decoded.
pub fn attachment(path: &Path, at: Option<u64>, n: usize) -> Result<Part> {
    let (raw, _) = message_bytes(path, at)?;
    let msg = mail_parser::MessageParser::default()
        .parse(&raw)
        .context("not a mail message")?;
    let p = msg
        .attachment(u32::try_from(n)?)
        .context("no such attachment")?;
    let name = attachment_name(p, n);
    Ok(part(&name, p.contents().to_vec()))
}

struct MboxMsg {
    at: u64,
    head: Vec<u8>,
    in_head: bool,
}

/// A page of an mbox's messages from byte `offset` (0, or a `next` from an earlier page): date,
/// sender, subject and where each starts. With `q`, only messages whose sender or subject contain
/// it. Stops after `limit` messages or the time budget; `next` says where to carry on.
pub fn mbox(path: &Path, offset: u64, q: &str, limit: usize) -> Result<Value> {
    let limit = limit.clamp(1, 1000);
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    f.seek(SeekFrom::Start(offset))?;
    let mut r = BufReader::with_capacity(1 << 18, f);
    let needle = q.trim().to_lowercase();
    let until = Instant::now() + MBOX_BUDGET;
    let parser = mail_parser::MessageParser::default();
    let (mut pos, mut line, mut prev_blank) = (offset, Vec::new(), true);
    let (mut cur, mut out, mut next) = (None::<MboxMsg>, Vec::new(), None);
    let mut lines = 0u64;
    let finish = |m: MboxMsg, end: u64, out: &mut Vec<Value>| {
        let h = parser.parse_headers(&m.head);
        let from = addrs(h.as_ref().and_then(|h| h.from()));
        let subject = h
            .as_ref()
            .and_then(|h| h.subject())
            .unwrap_or_default()
            .to_string();
        if needle.is_empty()
            || from.to_lowercase().contains(&needle)
            || subject.to_lowercase().contains(&needle)
        {
            out.push(json!({
                "at": m.at,
                "size": end - m.at,
                "from": from,
                "subject": subject,
                "date": h.as_ref().and_then(|h| h.date()).map(|d| d.to_timestamp()),
            }));
        }
    };
    loop {
        let n = line_capped(&mut r, &mut line, 2048)? as u64;
        if n == 0 {
            if let Some(m) = cur.take() {
                finish(m, pos, &mut out);
            }
            break;
        }
        lines += 1;
        if is_from_line(&line, prev_blank) {
            if let Some(m) = cur.take() {
                finish(m, pos, &mut out);
            }
            if out.len() >= limit || (pos > offset && Instant::now() > until) {
                next = Some(pos);
                break;
            }
            cur = Some(MboxMsg {
                at: pos,
                head: Vec::new(),
                in_head: true,
            });
        } else if let Some(m) = cur.as_mut() {
            if m.in_head {
                if blank(&line) {
                    m.in_head = false;
                } else if m.head.len() < 64 << 10 {
                    m.head.extend_from_slice(&line);
                }
            } else if lines.is_multiple_of(65_536) && m.at > offset && Instant::now() > until {
                // deep inside a long message: come back to it next time
                next = Some(m.at);
                break;
            }
        }
        pos += n;
        prev_blank = blank(&line);
    }
    Ok(json!({ "messages": out, "next": next, "scanned_to": next.unwrap_or(size), "size": size }))
}

/// HTML mail as plain text: tags dropped (never rendered), blocks and <br> as line breaks, list
/// items bulleted, table cells spaced, entities decoded, <head>/<style>/<script> skipped.
fn html_text(html: &str) -> String {
    fn line_break(out: &mut String) {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
    }
    let mut out = String::with_capacity(html.len() / 3);
    let mut rest = html;
    let mut skip: Option<String> = None;
    let mut pre = 0usize;
    while !rest.is_empty() {
        let lt = rest.find('<').unwrap_or(rest.len());
        if skip.is_none() {
            push_html_text(&mut out, &rest[..lt], pre > 0);
        }
        rest = &rest[lt..];
        if rest.is_empty() {
            break;
        }
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |i| &after[i + 3..]);
            continue;
        }
        let gt = rest.find('>').map_or(rest.len(), |i| i + 1);
        let tag = rest[1..gt].trim_end_matches('>');
        rest = &rest[gt..];
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| !c.is_ascii_alphanumeric())
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if let Some(s) = &skip {
            if closing && *s == name {
                skip = None;
            }
            continue;
        }
        match name.as_str() {
            "head" | "style" | "script" | "title" | "template" | "noscript"
                if !closing && !tag.ends_with('/') =>
            {
                skip = Some(name.clone())
            }
            "br" => out.push('\n'),
            "pre" => {
                pre = if closing {
                    pre.saturating_sub(1)
                } else {
                    pre + 1
                };
                line_break(&mut out);
            }
            "li" if !closing => {
                line_break(&mut out);
                out.push_str("• ");
            }
            "td" | "th" if !closing => {
                if !out.is_empty() && !out.ends_with(['\n', ' ']) {
                    out.push_str("   ");
                }
            }
            "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "blockquote" | "table" => {
                line_break(&mut out);
                if !out.is_empty() && !out.ends_with("\n\n") {
                    out.push('\n');
                }
            }
            "div" | "tr" | "ul" | "ol" | "dl" | "dt" | "dd" | "li" | "section" | "article"
            | "header" | "footer" | "hr" | "center" | "address" | "figure" | "caption" => {
                line_break(&mut out)
            }
            _ => {}
        }
    }
    // tidy: no invisible padding characters, no trailing spaces, at most one blank line in a row
    let mut tidy = String::with_capacity(out.len());
    let mut blanks = 0;
    for l in out.lines() {
        let l: String = l
            .chars()
            .filter(|c| {
                !matches!(
                    c,
                    '\u{200b}'..='\u{200d}' | '\u{ad}' | '\u{feff}' | '\u{34f}' | '\u{2060}'
                )
            })
            .map(|c| if c == '\u{a0}' { ' ' } else { c })
            .collect();
        let l = l.trim();
        if l.is_empty() {
            blanks += 1;
            if blanks > 1 || tidy.is_empty() {
                continue;
            }
        } else {
            blanks = 0;
        }
        tidy.push_str(l);
        tidy.push('\n');
    }
    tidy.trim_end().to_string()
}

fn push_html_text(out: &mut String, text: &str, pre: bool) {
    let text = decode_entities(text);
    if pre {
        out.push_str(&text);
        return;
    }
    let mut space = false;
    for c in text.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c') {
            space = true;
            continue;
        }
        if space && !out.is_empty() && !out.ends_with(['\n', ' ']) {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    if space && !out.is_empty() && !out.ends_with(['\n', ' ']) {
        out.push(' ');
    }
}

fn decode_entities(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let semi = rest
            .char_indices()
            .skip(1)
            .take(12)
            .find(|&(_, c)| !(c.is_ascii_alphanumeric() || c == '#'))
            .filter(|&(_, c)| c == ';')
            .map(|(j, _)| j);
        match semi.and_then(|j| Some((j, entity_char(&rest[1..j])?))) {
            Some((j, c)) => {
                out.push(c);
                rest = &rest[j + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn entity_char(name: &str) -> Option<char> {
    if let Some(n) = name.strip_prefix('#') {
        let v = match n.strip_prefix(['x', 'X']) {
            Some(h) => u32::from_str_radix(h, 16).ok()?,
            None => n.parse().ok()?,
        };
        return char::from_u32(v);
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "shy" => '\u{ad}',
        "zwnj" => '\u{200c}',
        "zwj" => '\u{200d}',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "sbquo" => '‚',
        "ldquo" => '“',
        "rdquo" => '”',
        "bdquo" => '„',
        "laquo" => '«',
        "raquo" => '»',
        "bull" => '•',
        "middot" => '·',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "deg" => '°',
        "times" => '×',
        "divide" => '÷',
        "para" => '¶',
        "sect" => '§',
        "iexcl" => '¡',
        "iquest" => '¿',
        "aacute" => 'á',
        "agrave" => 'à',
        "acirc" => 'â',
        "auml" => 'ä',
        "atilde" => 'ã',
        "aring" => 'å',
        "ccedil" => 'ç',
        "eacute" => 'é',
        "egrave" => 'è',
        "ecirc" => 'ê',
        "euml" => 'ë',
        "iacute" => 'í',
        "igrave" => 'ì',
        "icirc" => 'î',
        "iuml" => 'ï',
        "ntilde" => 'ñ',
        "oacute" => 'ó',
        "ograve" => 'ò',
        "ocirc" => 'ô',
        "ouml" => 'ö',
        "otilde" => 'õ',
        "oslash" => 'ø',
        "uacute" => 'ú',
        "ugrave" => 'ù',
        "ucirc" => 'û',
        "uuml" => 'ü',
        "szlig" => 'ß',
        "Aacute" => 'Á',
        "Eacute" => 'É',
        "Auml" => 'Ä',
        "Ouml" => 'Ö',
        "Uuml" => 'Ü',
        "Ntilde" => 'Ñ',
        "Ccedil" => 'Ç',
        _ => return None,
    })
}

// ---- disk images: partition tables, filesystems by their magic (backdown_core::magic), ISO 9660,
// virtual disks

/// A power-of-two size the way formats document it (64 KiB, 2 MiB).
fn binary(n: u64) -> String {
    match n {
        n if n >= 1 << 30 && n.is_multiple_of(1 << 30) => format!("{} GiB", n >> 30),
        n if n >= 1 << 20 && n.is_multiple_of(1 << 20) => format!("{} MiB", n >> 20),
        n if n >= 1 << 10 && n.is_multiple_of(1 << 10) => format!("{} KiB", n >> 10),
        n => format!("{n} bytes"),
    }
}

/// A partition table: its kind, the partitions, and facts about the disk.
type Table = (&'static str, Vec<Value>, Vec<(String, String)>);

/// The partition table at `base` (GPT, or MBR with its extended partitions), each partition with a
/// filesystem guess. None if there's no table there.
fn partitions(f: &File, base: u64) -> Result<Option<Table>> {
    let mbr = read_at(f, base, 512)?;
    if !has(&mbr, 510, &[0x55, 0xAA]) {
        return Ok(None);
    }
    let slot = |i: usize| &mbr[446 + 16 * i..462 + 16 * i];
    let sane = (0..4).all(|i| {
        let e = slot(i);
        matches!(e[0], 0 | 0x80) && (e[4] == 0 || (le(e, 8, 4) > 0 && le(e, 12, 4) > 0))
    });
    if !sane || (0..4).all(|i| slot(i)[4] == 0) {
        return Ok(None);
    }
    let mut parts = Vec::new();
    let mut details = Vec::new();
    let probe = |start: u64| probe_fs(f, base + start).ok().flatten();
    if (0..4).any(|i| slot(i)[4] == 0xEE) {
        for ss in [512u64, 4096] {
            let h = read_at(f, base + ss, 512)?;
            if !has(&h, 0, b"EFI PART") {
                continue;
            }
            details.push(("Disk GUID".into(), guid(&h[56..72])));
            let (lba, n, size) = (le(&h, 72, 8), le(&h, 80, 4).min(256), le(&h, 84, 4));
            if !(128..=4096).contains(&size) {
                bail!("damaged GPT header");
            }
            let table = read_at(f, base + lba * ss, (n * size) as usize)?;
            for (i, e) in table.chunks_exact(size as usize).enumerate() {
                if e[..16].iter().all(|&b| b == 0) {
                    continue;
                }
                let t = guid(&e[..16]);
                let (first, last) = (le(e, 32, 8), le(e, 40, 8));
                let start = first * ss;
                parts.push(json!({
                    "index": i + 1,
                    "type": gpt_type(&t),
                    "code": t,
                    "name": Some(utf16(&e[56..128], false)).filter(|s| !s.is_empty()),
                    "start": start,
                    "size": (last.saturating_sub(first) + 1) * ss,
                    "fs": probe(start),
                }));
            }
            return Ok(Some(("GPT", parts, details)));
        }
    }
    details.push(("Disk signature".into(), format!("{:08X}", le(&mbr, 440, 4))));
    for i in 0..4 {
        let e = slot(i);
        if e[4] == 0 {
            continue;
        }
        let (start, len) = (le(e, 8, 4) * 512, le(e, 12, 4) * 512);
        let extended = matches!(e[4], 0x05 | 0x0F | 0x85);
        parts.push(json!({
            "index": i + 1,
            "type": mbr_type(e[4]),
            "code": format!("0x{:02X}", e[4]),
            "start": start,
            "size": len,
            "bootable": e[0] == 0x80,
            "fs": if extended { None } else { probe(start) },
        }));
        if extended {
            // logical partitions: a chain of EBRs, each relative to the extended partition's start
            let mut ebr = start;
            for n in 0..64 {
                let b = read_at(f, base + ebr, 512)?;
                if !has(&b, 510, &[0x55, 0xAA]) || b[446 + 4] == 0 {
                    break;
                }
                let s = ebr + le(&b, 446 + 8, 4) * 512;
                parts.push(json!({
                    "index": 5 + n,
                    "type": mbr_type(b[446 + 4]),
                    "code": format!("0x{:02X}", b[446 + 4]),
                    "start": s,
                    "size": le(&b, 446 + 12, 4) * 512,
                    "bootable": b[446] == 0x80,
                    "fs": probe(s),
                }));
                let next = le(&b, 462 + 8, 4);
                if next == 0 {
                    break;
                }
                ebr = start + next * 512;
            }
        }
    }
    Ok(Some(("MBR", parts, details)))
}

/// A raw disk's layout at `base`: its partition table, else the filesystem that fills it.
fn raw_layout(f: &File, base: u64, out: &mut serde_json::Map<String, Value>) -> Result<bool> {
    // a volume boot record also ends in 55 AA, so a filesystem at sector 0 wins over an "MBR"
    // (ISO 9660 starts at 32 KiB, though, and hybrid images carry a partition table too)
    let fs = probe_fs(f, base)?;
    let table = match &fs {
        Some(v) if v.kind != "ISO 9660" => None,
        _ => partitions(f, base)?,
    };
    if let Some((table, parts, details)) = table {
        out.insert("table".into(), json!(table));
        out.insert("partitions".into(), json!(parts));
        for (k, v) in details {
            detail(out, &k, v);
        }
        return Ok(true);
    }
    if let Some(fs) = fs {
        out.insert("fs".into(), json!(fs));
        return Ok(true);
    }
    Ok(false)
}

struct IsoVol {
    block: u64,
    root: (u64, u64),
    joliet: bool,
    rock_ridge: bool,
    pvd: Vec<u8>,
    boot: bool,
    udf: bool,
}

struct IsoEnt {
    name: String,
    lba: u64,
    size: u64,
    dir: bool,
    mtime: Option<i64>,
}

/// The ISO 9660 volume descriptors; names come from Rock Ridge if the tree has them, else Joliet,
/// else the plain 8.3-ish names.
fn iso_volume(f: &File) -> Result<Option<IsoVol>> {
    let (mut pvd, mut svd, mut boot, mut udf) = (None, None, false, false);
    for i in 16..48u64 {
        let d = read_at(f, i * 2048, 2048)?;
        if d.len() < 2048 {
            break;
        }
        match &d[1..6] {
            b"CD001" => match d[0] {
                0 => boot = true,
                1 if pvd.is_none() => pvd = Some(d),
                2 if matches!(&d[88..91], b"%/@" | b"%/C" | b"%/E") => svd = Some(d),
                _ => {}
            },
            b"BEA01" | b"NSR02" | b"NSR03" | b"TEA01" => udf = true,
            _ => break,
        }
    }
    let Some(pvd) = pvd else {
        return Ok(None);
    };
    let block = match le(&pvd, 128, 2) {
        512 | 1024 | 2048 | 4096 => le(&pvd, 128, 2),
        _ => 2048,
    };
    let mut v = IsoVol {
        block,
        root: (le(&pvd, 158, 4), le(&pvd, 166, 4)),
        joliet: false,
        rock_ridge: true,
        pvd,
        boot,
        udf,
    };
    let (_, rr) = iso_dir(f, &v, v.root)?;
    v.rock_ridge = rr;
    if !rr && let Some(s) = svd {
        v.root = (le(&s, 158, 4), le(&s, 166, 4));
        v.joliet = true;
    }
    Ok(Some(v))
}

/// A directory's entries (at most `MAX_ENTRIES`, 4 MB of records), and whether any carried a Rock
/// Ridge name.
fn iso_dir(f: &File, v: &IsoVol, (lba, len): (u64, u64)) -> Result<(Vec<IsoEnt>, bool)> {
    let data = read_at(f, lba * v.block, len.min(4 << 20) as usize)?;
    let (mut out, mut rr, mut i) = (Vec::<IsoEnt>::new(), false, 0usize);
    let block = v.block as usize;
    while i < data.len() && out.len() < MAX_ENTRIES {
        let rl = data[i] as usize;
        if rl == 0 {
            i = (i / block + 1) * block; // records never cross a block boundary
            continue;
        }
        if rl < 34 || i + rl > data.len() {
            break;
        }
        let r = &data[i..i + rl];
        i += rl;
        let nl = (r[32] as usize).min(rl - 33);
        let raw = &r[33..33 + nl];
        if nl == 1 && raw[0] <= 1 {
            continue; // "." and ".."
        }
        let mut name = String::new();
        if v.rock_ridge && !v.joliet {
            let mut su = &r[(33 + nl + (1 - nl % 2)).min(rl)..];
            while su.len() >= 4 {
                let n = su[2] as usize;
                if n < 4 || n > su.len() {
                    break;
                }
                if &su[..2] == b"NM" && n > 5 {
                    rr = true;
                    name.push_str(&String::from_utf8_lossy(&su[5..n]));
                }
                su = &su[n..];
            }
        }
        if name.is_empty() {
            name = if v.joliet {
                utf16(raw, true)
            } else {
                String::from_utf8_lossy(raw).into_owned()
            };
            if let Some((stem, ver)) = name.rsplit_once(';')
                && ver.chars().all(|c| c.is_ascii_digit())
            {
                name = stem.to_string();
            }
            if name.ends_with('.') && !name.starts_with('.') {
                name.pop();
            }
        }
        let size = le(r, 10, 4);
        // a file over 4 GiB is several records with the same name
        if let Some(prev) = out.last_mut()
            && prev.name == name
            && !prev.dir
        {
            prev.size += size;
            continue;
        }
        let d = &r[18..25];
        let mtime = (d[1] >= 1 && d[1] <= 12 && d[2] >= 1).then(|| {
            days_from_civil(1900 + i64::from(d[0]), i64::from(d[1]), i64::from(d[2])) * 86_400
                + i64::from(d[3]) * 3600
                + i64::from(d[4]) * 60
                + i64::from(d[5])
                - i64::from(d[6] as i8) * 15 * 60
        });
        out.push(IsoEnt {
            name,
            lba: le(r, 2, 4),
            size,
            dir: r[25] & 2 != 0,
            mtime,
        });
    }
    Ok((out, rr))
}

/// Walks `path` ("a/b/c", from the ISO's root) through the directory tree.
fn iso_find(f: &File, v: &IsoVol, path: &str) -> Result<IsoEnt> {
    let mut cur = IsoEnt {
        name: String::new(),
        lba: v.root.0,
        size: v.root.1,
        dir: true,
        mtime: None,
    };
    for (depth, seg) in path.split('/').filter(|s| !s.is_empty()).enumerate() {
        if !cur.dir || depth > 64 {
            bail!("no {path:?} in this image");
        }
        let (list, _) = iso_dir(f, v, (cur.lba, cur.size))?;
        cur = match list.iter().position(|e| e.name == seg) {
            Some(i) => list.into_iter().nth(i),
            None => list.into_iter().find(|e| e.name.eq_ignore_ascii_case(seg)),
        }
        .with_context(|| format!("no {path:?} in this image"))?;
    }
    Ok(cur)
}

fn iso_json(
    f: &File,
    v: &IsoVol,
    dir: Option<&str>,
    out: &mut serde_json::Map<String, Value>,
) -> Result<()> {
    let p = &v.pvd;
    let label = field(p, 40, 32);
    let mut details = vec![
        ("Volume".to_string(), label.clone()),
        ("Size".into(), fmt::bytes(le(p, 80, 4) * v.block)),
        (
            "Names".into(),
            if v.rock_ridge {
                "Rock Ridge (Unix)".into()
            } else if v.joliet {
                "Joliet (Windows)".into()
            } else {
                "ISO 9660".into()
            },
        ),
    ];
    for (k, at, n) in [
        ("System", 8, 32),
        ("Publisher", 318, 128),
        ("Application", 574, 128),
    ] {
        let s = field(p, at, n);
        if !s.is_empty() {
            details.push((k.into(), s));
        }
    }
    let c = field(p, 813, 14);
    if c.len() == 14 && c.chars().all(|x| x.is_ascii_digit()) && !c.starts_with("0000") {
        details.push((
            "Created".into(),
            format!(
                "{}-{}-{} {}:{}",
                &c[..4],
                &c[4..6],
                &c[6..8],
                &c[8..10],
                &c[10..12]
            ),
        ));
    }
    if v.boot {
        details.push(("Bootable".into(), "yes (El Torito)".into()));
    }
    if v.udf {
        details.push(("Also".into(), "a UDF filesystem (not listed here)".into()));
    }
    let path = dir.unwrap_or("").trim_matches('/');
    let here = iso_find(f, v, path)?;
    if !here.dir {
        bail!("{path} is a file");
    }
    let (list, _) = iso_dir(f, v, (here.lba, here.size))?;
    out.insert("label".into(), json!(label));
    out.insert("details".into(), json!(details));
    out.insert("path".into(), json!(path));
    out.insert(
        "entries".into(),
        json!(
            list.iter()
                .map(|e| json!({ "name": e.name, "size": e.size, "dir": e.dir, "mtime": e.mtime }))
                .collect::<Vec<_>>()
        ),
    );
    out.insert("truncated".into(), json!(list.len() >= MAX_ENTRIES));
    Ok(())
}

fn detail(out: &mut serde_json::Map<String, Value>, k: &str, v: impl Into<String>) {
    if let Some(d) = out
        .entry("details")
        .or_insert_with(|| json!([]))
        .as_array_mut()
    {
        d.push(json!([k, v.into()]));
    }
}

/// VMDK descriptor text: create type, adapter, parent, extents (and their total size).
fn vmdk_descriptor(text: &str, out: &mut serde_json::Map<String, Value>) -> u64 {
    let mut total = 0;
    let mut extents = Vec::new();
    for l in text.lines().map(str::trim) {
        if let Some((k, v)) = l.split_once('=') {
            let v = v.trim().trim_matches('"');
            match k.trim() {
                "createType" => detail(out, "Type", v),
                "ddb.adapterType" => detail(out, "Adapter", v),
                "parentFileNameHint" => detail(out, "Parent", v),
                "ddb.virtualHWVersion" => detail(out, "Hardware version", v),
                _ => {}
            }
            continue;
        }
        let mut w = l.split_whitespace();
        if let (Some("RW" | "RDONLY" | "NOACCESS"), Some(n), Some(kind)) =
            (w.next(), w.next(), w.next())
            && let Ok(n) = n.parse::<u64>()
        {
            total += n * 512;
            let file = l.split('"').nth(1).unwrap_or("");
            if extents.len() < 20 {
                extents.push(format!("{kind} {} {file}", fmt::bytes(n * 512)));
            }
        }
    }
    if !extents.is_empty() {
        detail(out, "Extents", extents.join("; "));
    }
    total
}

const VHDX_METADATA: &str = "8B7CA206-4790-4B9A-B8FE-575F050F886E";
const VHDX_SIZE: &str = "2FA54224-CD1B-4876-B211-5DBED83BF4B8";
const VHDX_PARAMS: &str = "CAA16737-FA36-4D43-B3B6-33F0AA44E76B";
const VHDX_SECTOR: &str = "8141BF1D-A96F-4709-BA47-F233A8FAAB5F";

fn vhdx(f: &File, head: &[u8], out: &mut serde_json::Map<String, Value>) -> Result<Option<u64>> {
    let creator = utf16(head.get(8..520).unwrap_or_default(), false);
    if !creator.is_empty() {
        detail(out, "Created by", creator);
    }
    let rt = read_at(f, 192 << 10, 64 << 10)?;
    if !has(&rt, 0, b"regi") {
        return Ok(None);
    }
    let n = le(&rt, 8, 4).min(2047) as usize;
    let Some(meta) = (0..n)
        .map(|i| &rt[16 + 32 * i..48 + 32 * i])
        .find(|e| guid(&e[..16]) == VHDX_METADATA)
        .map(|e| le(e, 16, 8))
    else {
        return Ok(None);
    };
    let mt = read_at(f, meta, 64 << 10)?;
    if !has(&mt, 0, b"metadata") {
        return Ok(None);
    }
    let mut size = None;
    for i in 0..(le(&mt, 10, 2) as usize).min(2047) {
        let e = &mt[32 + 32 * i..64 + 32 * i];
        let at = meta + le(e, 16, 4);
        match guid(&e[..16]).as_str() {
            VHDX_SIZE => size = Some(le(&read_at(f, at, 8)?, 0, 8)),
            VHDX_SECTOR => detail(
                out,
                "Sector size",
                format!("{} bytes", le(&read_at(f, at, 4)?, 0, 4)),
            ),
            VHDX_PARAMS => {
                let p = read_at(f, at, 8)?;
                detail(out, "Block size", binary(le(&p, 0, 4)));
                detail(
                    out,
                    "Kind",
                    if le(&p, 4, 4) & 2 != 0 {
                        "differencing (needs its parent)"
                    } else if le(&p, 4, 4) & 1 != 0 {
                        "fixed"
                    } else {
                        "dynamic"
                    },
                );
            }
            _ => {}
        }
    }
    Ok(size)
}

/// A UDIF "mish" block (one partition's chunk table, base64 in the plist): its size in bytes and
/// the compressions its chunks use.
fn mish(b64: &str) -> Option<(u64, Vec<&'static str>)> {
    use base64::Engine;
    let clean: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
    let m = base64::engine::general_purpose::STANDARD
        .decode(clean)
        .ok()?;
    if !has(&m, 0, b"mish") {
        return None;
    }
    let n = (be(&m, 200, 4) as usize).min(1 << 20);
    let chunks = m.get(204..204 + 40 * n).unwrap_or_default();
    let kinds = chunks
        .as_chunks::<40>()
        .0
        .iter()
        .filter_map(|c| match be(c, 0, 4) {
            0x8000_0004 => Some("ADC"),
            0x8000_0005 => Some("zlib"),
            0x8000_0006 => Some("bzip2"),
            0x8000_0007 => Some("LZFSE"),
            0x8000_0008 => Some("LZMA"),
            0x0000_0001 => Some("raw"),
            _ => None,
        })
        .collect();
    Some((be(&m, 16, 8) * 512, kinds))
}

/// Apple UDIF (.dmg): the "koly" trailer and, from its plist, each partition and how its chunks
/// are compressed.
fn dmg(f: &File, k: &[u8], out: &mut serde_json::Map<String, Value>) -> Result<u64> {
    let sectors = be(k, 492, 8);
    detail(out, "Data", fmt::bytes(be(k, 32, 8)));
    if be(k, 12, 4) & 4 != 0 {
        detail(out, "Flags", "internet-enabled");
    }
    let (xml_at, xml_len) = (be(k, 216, 8), be(k, 224, 8));
    let mut kinds = std::collections::BTreeSet::new();
    let mut parts = Vec::new();
    if xml_len > 0 && xml_len <= XML_CAP {
        let bytes = read_at(f, xml_at, xml_len as usize)?;
        let mut r = xml(&bytes);
        let (mut key, mut depth, mut blkx, mut name) = (String::new(), 0usize, None, String::new());
        loop {
            match r.read_event()? {
                Event::Start(e) => {
                    depth += 1;
                    match e.local_name().as_ref() {
                        b"key" => {
                            key = r.read_text(e.name())?.decode()?.into_owned();
                            depth -= 1;
                        }
                        b"array" if key == "blkx" && blkx.is_none() => blkx = Some(depth),
                        b"dict" if blkx.is_some_and(|b| depth == b + 1) => name.clear(),
                        b"string" if blkx.is_some_and(|b| depth == b + 2) => {
                            let t = r.read_text(e.name())?.decode()?.into_owned();
                            depth -= 1;
                            if key == "Name" || (key == "CFName" && name.is_empty()) {
                                name = t;
                            }
                        }
                        b"data" if blkx.is_some_and(|b| depth == b + 2) && key == "Data" => {
                            let t = r.read_text(e.name())?.decode()?.into_owned();
                            depth -= 1;
                            if let Some((size, used)) = mish(&t) {
                                kinds.extend(used);
                                if parts.len() < 64 {
                                    parts.push(json!({ "name": name, "size": size }));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Event::End(e) => {
                    if e.local_name().as_ref() == b"array" && blkx == Some(depth) {
                        blkx = Some(usize::MAX); // done with the one we wanted
                    }
                    depth = depth.saturating_sub(1);
                }
                Event::Eof => break,
                _ => {}
            }
        }
    }
    let packed: Vec<&str> = kinds.iter().copied().filter(|k| *k != "raw").collect();
    detail(
        out,
        "Compression",
        if packed.is_empty() {
            "none (read/write or read-only image)".to_string()
        } else {
            let fmt_name = match packed[0] {
                "zlib" => " (UDZO)",
                "bzip2" => " (UDBZ)",
                "LZFSE" => " (ULFO)",
                "LZMA" => " (ULMO)",
                _ => "",
            };
            format!("{}{fmt_name}", packed.join(", "))
        },
    );
    out.insert("compressed".into(), json!(!packed.is_empty()));
    if !parts.is_empty() {
        out.insert("dmg_parts".into(), json!(parts));
    }
    Ok(sectors * 512)
}

/// What a disk image holds, reading only headers and tables: an ISO's files (the folder `dir`),
/// a raw image's partitions and their filesystems, or a virtual disk's format and size.
pub fn disk(path: &Path, dir: Option<&str>) -> Result<Value> {
    let f = File::open(path)?;
    let len = f.metadata()?.len();
    let head = read_at(&f, 0, 4096)?;
    let tail = if len >= 512 {
        read_at(&f, len - 512, 512)?
    } else {
        Vec::new()
    };
    let mut out = serde_json::Map::new();
    out.insert("file_size".into(), json!(len));
    fn put(out: &mut serde_json::Map<String, Value>, format: &str, size: Option<u64>) {
        out.insert("format".into(), json!(format));
        if let Some(s) = size.filter(|&s| s > 0) {
            out.insert("virtual_size".into(), json!(s));
        }
    }
    if has(&head, 0, b"QFI\xfb") {
        let v = be(&head, 4, 4);
        let (size, crypt) = if v == 1 {
            (be(&head, 24, 8), be(&head, 36, 4))
        } else {
            (be(&head, 24, 8), be(&head, 32, 4))
        };
        put(
            &mut out,
            &format!("qcow{}", if v == 1 { "" } else { "2" }),
            Some(size),
        );
        detail(&mut out, "Version", v.to_string());
        let cluster = if v == 1 {
            head[32] as u64
        } else {
            be(&head, 20, 4)
        };
        if cluster < 32 {
            detail(&mut out, "Cluster size", binary(1 << cluster));
        }
        if crypt != 0 {
            detail(
                &mut out,
                "Encrypted",
                if crypt == 1 { "AES" } else { "LUKS" },
            );
        }
        if v >= 2 && be(&head, 60, 4) > 0 {
            detail(&mut out, "Snapshots", be(&head, 60, 4).to_string());
        }
        if v >= 3 && be(&head, 72, 8) & 1 != 0 {
            detail(&mut out, "State", "dirty (not closed cleanly)");
        }
        let (bo, bl) = (be(&head, 8, 8), be(&head, 16, 4).min(1023));
        if bo > 0 && bl > 0 {
            detail(
                &mut out,
                "Backing file",
                String::from_utf8_lossy(&read_at(&f, bo, bl as usize)?),
            );
        }
    } else if has(&head, 0, b"KDMV") {
        put(&mut out, "vmdk", Some(le(&head, 12, 8) * 512));
        let flags = le(&head, 8, 4);
        if flags & (1 << 16) != 0 {
            detail(&mut out, "Grains", "compressed (stream-optimized)");
        }
        let (at, n) = (
            le(&head, 28, 8) * 512,
            (le(&head, 36, 8) * 512).min(64 << 10),
        );
        if at > 0 && n > 0 {
            let text = read_at(&f, at, n as usize)?;
            vmdk_descriptor(&String::from_utf8_lossy(&text).replace('\0', ""), &mut out);
        }
    } else if head.starts_with(b"# Disk DescriptorFile") {
        let text = String::from_utf8_lossy(&read_at(&f, 0, 64 << 10)?).into_owned();
        let total = vmdk_descriptor(&text, &mut out);
        put(&mut out, "vmdk", Some(total));
        detail(
            &mut out,
            "Note",
            "a descriptor: the data is in the extent files it names",
        );
    } else if has(&head, 0, b"vhdxfile") {
        let size = vhdx(&f, &head, &mut out)?;
        put(&mut out, "vhdx", size);
    } else if le(&head, 0x40, 4) == 0xBEDA_107F {
        let size = le(&head, 0x170, 8);
        put(&mut out, "vdi", Some(size));
        let kind = le(&head, 0x4C, 4);
        detail(
            &mut out,
            "Kind",
            match kind {
                1 => "dynamic",
                2 => "fixed",
                3 => "undo",
                4 => "differencing (needs its parent)",
                _ => "unknown",
            },
        );
        let comment = field(&head, 0x54, 256);
        if !comment.is_empty() {
            detail(&mut out, "Comment", comment);
        }
        detail(
            &mut out,
            "Allocated",
            format!(
                "{} of {}",
                fmt::bytes(le(&head, 0x184, 4) * le(&head, 0x178, 4)),
                fmt::bytes(size)
            ),
        );
        if kind == 2 {
            raw_layout(&f, le(&head, 0x158, 4), &mut out)?;
        }
    } else if has(&tail, 0, b"conectix") || has(&head, 0, b"conectix") {
        let ft = if has(&tail, 0, b"conectix") {
            &tail[..]
        } else {
            &head[..512]
        };
        put(&mut out, "vhd", Some(be(ft, 48, 8)));
        let kind = be(ft, 60, 4);
        detail(
            &mut out,
            "Kind",
            match kind {
                2 => "fixed",
                3 => "dynamic",
                4 => "differencing (needs its parent)",
                _ => "unknown",
            },
        );
        detail(&mut out, "Created by", field(ft, 28, 4));
        if kind == 4 {
            let dh = read_at(&f, be(ft, 16, 8), 1024)?;
            if has(&dh, 0, b"cxsparse") {
                detail(&mut out, "Parent", utf16(&dh[64..576], true));
            }
        }
        if kind == 2 {
            raw_layout(&f, 0, &mut out)?;
        }
    } else if has(&tail, 0, b"koly") {
        let size = dmg(&f, &tail, &mut out)?;
        put(&mut out, "dmg", Some(size));
    } else if has(&head, 0, b"encrcdsa") || has(&tail, 0, b"cdsaencr") {
        put(&mut out, "dmg", None);
        detail(
            &mut out,
            "Encrypted",
            "yes (needs its password; nothing more to show)",
        );
    } else if let Some(v) = iso_volume(&f)? {
        put(&mut out, "iso9660", None);
        iso_json(&f, &v, dir, &mut out)?;
        // a hybrid image (most Linux installers) also carries a partition table
        if let Some((table, parts, _)) = partitions(&f, 0)? {
            out.insert("table".into(), json!(table));
            out.insert("partitions".into(), json!(parts));
        }
    } else {
        put(&mut out, "raw", Some(len));
        if !raw_layout(&f, 0, &mut out)? {
            out.insert("format".into(), json!("unknown"));
        }
    }
    Ok(Value::Object(out))
}

// the files inside a disk image's filesystems (FAT, exFAT, ext2/3/4, NTFS)
mod fsview;
pub use fsview::{fs_entry, fs_list};

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(bytes: &[u8], suffix: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new().suffix(suffix).tempfile().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    fn zip_of(parts: &[(&str, &[u8])]) -> Vec<u8> {
        use zip::write::SimpleFileOptions;
        let mut w = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (i, (name, data)) in parts.iter().enumerate() {
            let method = if i % 2 == 0 {
                zip::CompressionMethod::Deflated
            } else {
                zip::CompressionMethod::Stored
            };
            let o = SimpleFileOptions::default().compression_method(method);
            if name.ends_with('/') {
                w.add_directory(*name, o).unwrap();
            } else {
                w.start_file(*name, o).unwrap();
                w.write_all(data).unwrap();
            }
        }
        w.finish().unwrap().into_inner()
    }

    fn read_part(p: Part) -> Vec<u8> {
        let mut out = Vec::new();
        let mut body = p.body;
        body.read_to_end(&mut out).unwrap();
        out
    }

    fn names(v: &Value) -> Vec<String> {
        v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn zip_listing_and_entries() {
        let z = zip_of(&[
            ("docs/", b""),
            ("docs/a.txt", b"hello hello hello hello"),
            ("b.bin", &[0, 1, 2, 3]),
            ("π/ünï.txt", b"unicode"),
        ]);
        let f = tmp(&z, ".zip");
        let v = archive(f.path(), 100, false).unwrap();
        assert_eq!(v["format"], "zip");
        assert_eq!(v["total"], 4);
        assert_eq!(v["complete"], true);
        assert_eq!(names(&v), ["docs/", "docs/a.txt", "b.bin", "π/ünï.txt"]);
        assert_eq!(v["entries"][0]["dir"], true);
        assert_eq!(v["entries"][1]["size"], 23);
        assert_eq!(v["total_size"], 23 + 4 + 7);
        assert!(v["entries"][1]["mtime"].as_i64().unwrap() > 0);
        let p = entry(f.path(), "docs/a.txt", None).unwrap();
        assert_eq!((p.name.as_str(), p.len), ("a.txt", Some(23)));
        assert_eq!(read_part(p), b"hello hello hello hello");
        assert_eq!(
            read_part(entry(f.path(), "b.bin", None).unwrap()),
            [0, 1, 2, 3]
        );
        assert_eq!(
            read_part(entry(f.path(), "π/ünï.txt", None).unwrap()),
            b"unicode"
        );
        assert_eq!(
            read_part(entry(f.path(), "docs/a.txt", Some(5)).unwrap()),
            b"hello"
        );
        assert!(entry(f.path(), "../etc/passwd", None).is_err());
        assert!(entry(f.path(), "docs/", None).is_err());
        // a listing cap still counts everything
        let v = archive(f.path(), 1, false).unwrap();
        assert_eq!((names(&v).len(), v["total"].as_u64()), (1, Some(4)));
    }

    #[test]
    fn zip_behind_a_stub() {
        // a self-extractor: bytes in front, offsets in the directory don't know about them
        let mut z = b"MZ this is a stub".repeat(10);
        z.extend(zip_of(&[("x.txt", b"inside")]));
        let f = tmp(&z, ".exe");
        assert_eq!(names(&archive(f.path(), 10, false).unwrap()), ["x.txt"]);
        assert_eq!(
            read_part(entry(f.path(), "x.txt", None).unwrap()),
            b"inside"
        );
    }

    fn tar_bytes() -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Directory);
        h.set_size(0);
        h.set_mode(0o755);
        h.set_mtime(1_700_000_000);
        b.append_data(&mut h, "dir/", io::empty()).unwrap();
        for (name, data) in [
            ("dir/one.txt", &b"first file"[..]),
            (
                &*format!("dir/{}.txt", "long".repeat(40)),
                &b"a long name"[..],
            ),
            ("two.bin", &[9u8; 3000][..]),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_mtime(1_700_000_000);
            b.append_data(&mut h, name, data).unwrap();
        }
        b.into_inner().unwrap()
    }

    #[test]
    fn tar_in_every_wrapper() {
        let plain = tar_bytes();
        let long = format!("dir/{}.txt", "long".repeat(40));
        let gz = {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            e.write_all(&plain).unwrap();
            e.finish().unwrap()
        };
        let bz = {
            let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
            e.write_all(&plain).unwrap();
            e.finish().unwrap()
        };
        let xz = {
            let mut e =
                lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::default()).unwrap();
            e.write_all(&plain).unwrap();
            e.finish().unwrap()
        };
        let zst = ruzstd::encoding::compress_to_vec(
            &plain[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        for (bytes, format) in [
            (plain.clone(), "tar"),
            (gz, "tar.gz"),
            (bz, "tar.bz2"),
            (xz, "tar.xz"),
            (zst, "tar.zst"),
        ] {
            let f = tmp(&bytes, ".t");
            let v = archive(f.path(), 100, false).unwrap();
            assert_eq!(v["format"], format);
            assert_eq!(v["total"], 4, "{format}: {v}");
            assert_eq!(
                names(&v),
                ["dir/", "dir/one.txt", long.as_str(), "two.bin"],
                "{format}"
            );
            assert_eq!(v["entries"][0]["dir"], true);
            assert_eq!(v["entries"][3]["size"], 3000);
            assert_eq!(v["entries"][1]["mtime"], 1_700_000_000);
            assert_eq!(
                read_part(entry(f.path(), "dir/one.txt", None).unwrap()),
                b"first file",
                "{format}"
            );
            assert_eq!(
                read_part(entry(f.path(), &long, None).unwrap()),
                b"a long name",
                "{format}"
            );
            assert_eq!(
                read_part(entry(f.path(), "two.bin", Some(10)).unwrap()),
                [9u8; 10]
            );
            assert!(entry(f.path(), "nope", None).is_err());
        }
    }

    #[test]
    fn a_lone_compressed_file() {
        let body = b"just some text, compressed on its own\n".repeat(50);
        let mut e = flate2::GzBuilder::new()
            .filename("notes.txt")
            .mtime(1_600_000_000)
            .write(Vec::new(), flate2::Compression::default());
        e.write_all(&body).unwrap();
        let f = tmp(&e.finish().unwrap(), ".gz");
        let v = archive(f.path(), 10, false).unwrap();
        assert_eq!(v["format"], "gz");
        assert_eq!(names(&v), ["notes.txt"]);
        assert_eq!(v["entries"][0]["size"], body.len());
        assert_eq!(v["entries"][0]["mtime"], 1_600_000_000);
        assert_eq!(read_part(entry(f.path(), "notes.txt", None).unwrap()), body);
        // no name inside: ours minus the extension
        let zst = ruzstd::encoding::compress_to_vec(
            &body[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let f = tempfile::Builder::new().tempdir().unwrap();
        let p = f.path().join("server.log.zst");
        std::fs::write(&p, zst).unwrap();
        let v = archive(&p, 10, true).unwrap();
        assert_eq!(
            (v["format"].as_str(), names(&v)),
            (Some("zst"), vec!["server.log".to_string()])
        );
        assert_eq!(v["entries"][0]["size"], body.len());
        assert_eq!(
            read_part(entry(&p, "server.log", Some(4)).unwrap()),
            b"just"
        );
    }

    const XLSX_NS: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

    fn xlsx() -> Vec<u8> {
        let rel = |id: &str, kind: &str, target: &str| {
            format!(
                r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}" Target="{target}"/>"#
            )
        };
        let rels = |body: String| {
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{body}</Relationships>"#
            )
        };
        let types = r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/></Types>"#;
        let root = rels(rel("rId1", "officeDocument", "xl/workbook.xml"));
        let wb = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><workbook {XLSX_NS}><sheets><sheet name="Data" sheetId="1" r:id="rId1"/><sheet name="Other" sheetId="2" r:id="rId2"/></sheets></workbook>"#
        );
        let wbrels = rels(
            rel("rId1", "worksheet", "worksheets/sheet1.xml")
                + &rel("rId2", "worksheet", "worksheets/sheet2.xml")
                + &rel("rId3", "sharedStrings", "sharedStrings.xml"),
        );
        let sst = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><sst {XLSX_NS} count="3" uniqueCount="3"><si><t>name</t></si><si><t>n</t></si><si><t>a</t></si></sst>"#
        );
        let s1 = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet {XLSX_NS}><dimension ref="A1:C4"/><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row><row r="2"><c r="A2" t="s"><v>2</v></c><c r="B2"><v>1</v></c></row><row r="3"><c r="A3" t="inlineStr"><is><t>b &amp; c</t></is></c><c r="B3"><v>2.5</v></c></row><row r="4"><c r="C4" t="b"><v>1</v></c></row></sheetData></worksheet>"#
        );
        let s2 = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet {XLSX_NS}><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>other</t></is></c></row></sheetData></worksheet>"#
        );
        zip_of(&[
            ("[Content_Types].xml", types.as_bytes()),
            ("_rels/.rels", root.as_bytes()),
            ("xl/workbook.xml", wb.as_bytes()),
            ("xl/_rels/workbook.xml.rels", wbrels.as_bytes()),
            ("xl/sharedStrings.xml", sst.as_bytes()),
            ("xl/worksheets/sheet1.xml", s1.as_bytes()),
            ("xl/worksheets/sheet2.xml", s2.as_bytes()),
        ])
    }

    #[test]
    fn spreadsheet_bomb_is_refused() {
        // the central directory claims the shared strings unpack to 200 MB: refused before
        // calamine would inflate them
        let mut z = xlsx();
        let name = b"xl/sharedStrings.xml";
        let at = z.windows(name.len()).rposition(|w| w == name).unwrap() - 46;
        assert_eq!(&z[at..at + 4], b"PK\x01\x02");
        z[at + 24..at + 28].copy_from_slice(&(200u32 << 20).to_le_bytes());
        let err = sheet(tmp(&z, ".xlsx").path(), None, 0, 10).unwrap_err();
        assert!(
            err.to_string().contains("sharedStrings.xml unpacks to"),
            "{err}"
        );
    }

    #[test]
    fn spreadsheet_pages() {
        let f = tmp(&xlsx(), ".xlsx");
        let v = sheet(f.path(), None, 0, 500).unwrap();
        assert_eq!(
            v["sheets"],
            json!([{ "name": "Data", "hidden": false }, { "name": "Other", "hidden": false }])
        );
        assert_eq!(v["sheet"], "Data");
        assert_eq!(v["rows"][0], json!(["name", "n"]));
        assert_eq!(v["rows"][1], json!(["a", 1.0]));
        assert_eq!(v["rows"][2], json!(["b & c", 2.5]));
        assert_eq!(v["rows"][3], json!([null, null, true]));
        assert_eq!(
            (
                v["width"].as_u64(),
                v["total_rows"].as_u64(),
                v["more"].as_bool()
            ),
            (Some(3), Some(4), Some(false))
        );
        let v = sheet(f.path(), Some("Data"), 1, 2).unwrap();
        assert_eq!(v["rows"], json!([["a", 1.0], ["b & c", 2.5]]));
        assert_eq!(v["more"], true);
        let v = sheet(f.path(), Some("Other"), 0, 10).unwrap();
        assert_eq!(v["rows"], json!([["other"]]));
        assert!(sheet(tmp(b"not a sheet", ".xlsx").path(), None, 0, 10).is_err());
    }

    const P: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;

    fn pptx_bytes() -> Vec<u8> {
        let rels = |body: &str| {
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{body}</Relationships>"#
            )
        };
        let r = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let pres = format!(
            r#"<p:presentation {P}><p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId3"/></p:sldIdLst></p:presentation>"#
        );
        let prels = rels(&format!(
            r#"<Relationship Id="rId2" Type="{r}/slide" Target="slides/slide1.xml"/><Relationship Id="rId3" Type="{r}/slide" Target="slides/slide2.xml"/>"#
        ));
        let s1 = format!(
            r#"<p:sld {P}><p:cSld><p:spTree>
            <p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Hello &amp; welcome</a:t></a:r></a:p></p:txBody></p:sp>
            <p:sp><p:nvSpPr><p:cNvPr id="3" name="Body"/><p:cNvSpPr/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>First</a:t></a:r></a:p><a:p><a:pPr lvl="1"/><a:r><a:t>Nested</a:t></a:r><a:br/><a:r><a:t>line</a:t></a:r></a:p><a:p/></p:txBody></p:sp>
            <p:pic><p:blipFill><a:blip r:embed="rId2"/></p:blipFill></p:pic>
            <p:sp><p:nvSpPr><p:cNvPr id="4" name="Num"/><p:cNvSpPr/><p:nvPr><p:ph type="sldNum"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:fld type="slidenum"><a:t>1</a:t></a:fld></a:p></p:txBody></p:sp>
            </p:spTree></p:cSld></p:sld>"#
        );
        let s1rels = rels(&format!(
            r#"<Relationship Id="rId1" Type="{r}/slideLayout" Target="../slideLayouts/slideLayout1.xml"/><Relationship Id="rId2" Type="{r}/image" Target="../media/image1.png"/>"#
        ));
        let s2 = format!(
            r#"<p:sld {P}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="T"/><p:cNvSpPr/><p:nvPr><p:ph type="ctrTitle"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Second</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#
        );
        let s2rels = rels(&format!(
            r#"<Relationship Id="rId9" Type="{r}/notesSlide" Target="../notesSlides/notesSlide2.xml"/>"#
        ));
        let notes = format!(
            r#"<p:notes {P}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="I"/><p:cNvSpPr/><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="N"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Speaker note</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>"#
        );
        zip_of(&[
            ("ppt/presentation.xml", pres.as_bytes()),
            ("ppt/_rels/presentation.xml.rels", prels.as_bytes()),
            ("ppt/slides/slide1.xml", s1.as_bytes()),
            ("ppt/slides/_rels/slide1.xml.rels", s1rels.as_bytes()),
            ("ppt/slides/slide2.xml", s2.as_bytes()),
            ("ppt/slides/_rels/slide2.xml.rels", s2rels.as_bytes()),
            ("ppt/notesSlides/notesSlide2.xml", notes.as_bytes()),
            ("ppt/media/image1.png", b"\x89PNG fake"),
            ("docProps/thumbnail.jpeg", b"\xff\xd8 fake"),
        ])
    }

    #[test]
    fn pptx_slides() {
        let f = tmp(&pptx_bytes(), ".pptx");
        let v = slides(f.path()).unwrap();
        assert_eq!(
            (v["format"].as_str(), v["count"].as_u64()),
            (Some("pptx"), Some(2))
        );
        let s = &v["slides"][0];
        assert_eq!(s["title"], "Hello & welcome");
        assert_eq!(
            s["paras"],
            json!([{ "t": "First", "lvl": 0 }, { "t": "Nested\nline", "lvl": 1 }])
        );
        assert_eq!(s["images"], json!(["ppt/media/image1.png"]));
        assert_eq!(v["slides"][1]["title"], "Second");
        assert_eq!(v["slides"][1]["notes"], "Speaker note");
        assert_eq!(v["thumbnail"], "docProps/thumbnail.jpeg");
        // the images are served by name out of the same zip
        assert_eq!(
            read_part(entry(f.path(), "ppt/media/image1.png", None).unwrap()),
            b"\x89PNG fake"
        );
    }

    #[test]
    fn odp_slides() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:presentation>
            <draw:page draw:name="page1"><draw:frame presentation:class="title"><draw:text-box><text:p>Plan<text:s text:c="2"/>2025</text:p></draw:text-box></draw:frame>
              <draw:frame presentation:class="outline"><draw:text-box><text:list><text:list-item><text:p>Top</text:p><text:list><text:list-item><text:p>Under</text:p></text:list-item></text:list></text:list-item></text:list></draw:text-box></draw:frame>
              <draw:frame><draw:image xlink:href="Pictures/pic.png"/></draw:frame>
              <presentation:notes><draw:frame presentation:class="notes"><draw:text-box><text:p>Say hi</text:p></draw:text-box></draw:frame></presentation:notes></draw:page>
            <draw:page draw:name="page2"><draw:frame presentation:class="title"><draw:text-box><text:p>End</text:p></draw:text-box></draw:frame></draw:page>
            </office:presentation></office:body></office:document-content>"#;
        let z = zip_of(&[
            (
                "mimetype",
                b"application/vnd.oasis.opendocument.presentation",
            ),
            ("content.xml", content.as_bytes()),
            ("Pictures/pic.png", b"png"),
        ]);
        let v = slides(tmp(&z, ".odp").path()).unwrap();
        assert_eq!(
            (v["format"].as_str(), v["count"].as_u64()),
            (Some("odp"), Some(2))
        );
        let s = &v["slides"][0];
        assert_eq!(s["title"], "Plan  2025");
        assert_eq!(
            s["paras"],
            json!([{ "t": "Top", "lvl": 0 }, { "t": "Under", "lvl": 1 }])
        );
        assert_eq!(s["images"], json!(["Pictures/pic.png"]));
        assert_eq!(s["notes"], "Say hi");
        assert_eq!(v["slides"][1]["title"], "End");
    }

    const EML: &str = "From: Ada Lovelace <ada@example.com>\r\nTo: Bob <bob@example.com>, carol@example.com\r\nCc: Dan <dan@example.com>\r\nSubject: =?UTF-8?B?w4lub3JtZQ==?= news\r\nDate: Tue, 2 Jan 2024 10:00:00 +0100\r\nMessage-ID: <1@example.com>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"B\"\r\n\r\n--B\r\nContent-Type: multipart/alternative; boundary=\"A\"\r\n\r\n--A\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello Bob,\r\nthe plain part.\r\n--A\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Hello <b>Bob</b></p>\r\n--A--\r\n--B\r\nContent-Type: application/pdf; name=\"report.pdf\"\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--B--\r\n";

    #[test]
    fn eml_message() {
        let f = tmp(EML.as_bytes(), ".eml");
        let v = mail(f.path(), None).unwrap();
        assert_eq!(v["from"], "Ada Lovelace <ada@example.com>");
        assert_eq!(v["to"], "Bob <bob@example.com>, carol@example.com");
        assert_eq!(v["cc"], "Dan <dan@example.com>");
        assert_eq!(v["subject"], "Énorme news");
        assert_eq!(v["date"], 1_704_186_000);
        assert_eq!(v["text"], "Hello Bob,\r\nthe plain part.");
        assert_eq!(v["from_html"], false);
        assert_eq!(
            v["attachments"],
            json!([{ "part": 0, "name": "report.pdf", "type": "application/pdf", "size": 9 }])
        );
        let p = attachment(f.path(), None, 0).unwrap();
        assert_eq!(p.name, "report.pdf");
        assert_eq!(read_part(p), b"%PDF-1.4\n");
        assert!(attachment(f.path(), None, 1).is_err());
    }

    #[test]
    fn html_only_mail_becomes_text() {
        let raw = "From: a@example.com\r\nSubject: promo\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><head><style>p{color:red}</style><title>t</title></head><body><script>alert(1)</script><h1>Big&nbsp;news</h1><p>Line one<br>line&#32;two &amp; more</p><ul><li>first</li><li>second</li></ul><table><tr><td>a</td><td>b</td></tr></table><!-- hidden --></body></html>\r\n";
        let v = mail(tmp(raw.as_bytes(), ".eml").path(), None).unwrap();
        assert_eq!(v["from_html"], true);
        assert_eq!(
            v["text"],
            "Big news\n\nLine one\nline two & more\n\n• first\n• second\n\na   b"
        );
    }

    fn mbox_text() -> String {
        let msg = |n: u32, from: &str, subject: &str, body: &str| {
            format!(
                "From {from} Mon Jan  {n} 10:00:00 2024\nFrom: {from}\nSubject: {subject}\nDate: Mon, {n} Jan 2024 10:00:00 +0000\n\n{body}\n\n"
            )
        };
        msg(
            1,
            "ann@example.com",
            "Lunch",
            "See you at noon.\n>From here on, it's escaped.",
        ) + &msg(2, "bob@example.com", "Invoice 42", "Please pay.")
            + &msg(3, "ann@example.com", "Re: Invoice 42", "Paid.")
    }

    #[test]
    fn mbox_pages_and_messages() {
        let text = mbox_text();
        let f = tmp(text.as_bytes(), ".mbox");
        let v = mbox(f.path(), 0, "", 200).unwrap();
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(v["next"], Value::Null);
        assert_eq!(msgs[0]["at"], 0);
        assert_eq!(msgs[1]["subject"], "Invoice 42");
        assert_eq!(msgs[1]["from"], "bob@example.com");
        let second = text.find("From bob").unwrap() as u64;
        assert_eq!(msgs[1]["at"], second);
        assert_eq!(msgs[0]["size"], second);
        // paging: two per page, then carry on from `next`
        let p1 = mbox(f.path(), 0, "", 2).unwrap();
        assert_eq!(p1["messages"].as_array().unwrap().len(), 2);
        let next = p1["next"].as_u64().unwrap();
        let p2 = mbox(f.path(), next, "", 2).unwrap();
        assert_eq!(p2["messages"][0]["subject"], "Re: Invoice 42");
        assert_eq!(p2["next"], Value::Null);
        // search over sender and subject
        let s = mbox(f.path(), 0, "INVOICE", 200).unwrap();
        assert_eq!(s["messages"].as_array().unwrap().len(), 2);
        let s = mbox(f.path(), 0, "ann@", 200).unwrap();
        assert_eq!(s["messages"].as_array().unwrap().len(), 2);
        // one message by its offset, ">From" unescaped, and nothing of the next one
        let m = mail(f.path(), Some(0)).unwrap();
        assert_eq!(m["subject"], "Lunch");
        assert_eq!(m["text"], "See you at noon.\nFrom here on, it's escaped.");
        let m = mail(f.path(), Some(second)).unwrap();
        assert_eq!(
            (m["subject"].as_str(), m["text"].as_str()),
            (Some("Invoice 42"), Some("Please pay."))
        );
        assert!(mail(f.path(), Some(5)).is_err());
    }

    fn put(b: &mut [u8], at: usize, v: u64, n: usize) {
        for i in 0..n {
            b[at + i] = (v >> (8 * i)) as u8;
        }
    }

    fn put_be(b: &mut [u8], at: usize, v: u64, n: usize) {
        for i in 0..n {
            b[at + i] = (v >> (8 * (n - 1 - i))) as u8;
        }
    }

    fn guid_bytes(s: &str) -> Vec<u8> {
        let h: Vec<u8> = s
            .replace('-', "")
            .as_bytes()
            .chunks(2)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
            .collect();
        let mut g = vec![h[3], h[2], h[1], h[0], h[5], h[4], h[7], h[6]];
        g.extend_from_slice(&h[8..]);
        g
    }

    #[test]
    fn mbr_with_filesystems() {
        let mut img = vec![0u8; 3 << 20];
        img[510] = 0x55;
        img[511] = 0xAA;
        let (e1, e2) = (446, 462);
        img[e1] = 0x80;
        img[e1 + 4] = 0x83;
        put(&mut img, e1 + 8, 2048, 4);
        put(&mut img, e1 + 12, 2048, 4);
        img[e2 + 4] = 0x0C;
        put(&mut img, e2 + 8, 4096, 4);
        put(&mut img, e2 + 12, 2048, 4);
        // ext4 at 1 MiB: 1 KiB blocks, 1024 of them, extents, label "root"
        let sb = (1 << 20) + 1024;
        put(&mut img, sb + 4, 1024, 4);
        put(&mut img, sb + 56, 0xEF53, 2);
        put(&mut img, sb + 96, 0x40, 4);
        img[sb + 120..sb + 124].copy_from_slice(b"root");
        // FAT32 at 2 MiB, label "BOOT"
        let v = 2 << 20;
        img[v..v + 3].copy_from_slice(&[0xEB, 0x58, 0x90]);
        put(&mut img, v + 11, 512, 2);
        img[v + 13] = 8;
        put(&mut img, v + 14, 32, 2);
        img[v + 16] = 2;
        put(&mut img, v + 32, 2048, 4);
        img[v + 71..v + 82].copy_from_slice(b"BOOT       ");
        img[v + 82..v + 90].copy_from_slice(b"FAT32   ");
        img[v + 510] = 0x55;
        img[v + 511] = 0xAA;
        let d = disk(tmp(&img, ".img").path(), None).unwrap();
        assert_eq!(
            (d["format"].as_str(), d["table"].as_str()),
            (Some("raw"), Some("MBR"))
        );
        let p = d["partitions"].as_array().unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(
            (
                p[0]["type"].as_str(),
                p[0]["start"].as_u64(),
                p[0]["size"].as_u64()
            ),
            (Some("Linux"), Some(1 << 20), Some(1 << 20))
        );
        assert_eq!(p[0]["bootable"], true);
        assert_eq!(
            p[0]["fs"],
            json!({ "kind": "ext4", "label": "root", "size": 1 << 20 })
        );
        assert_eq!(
            p[1]["fs"],
            json!({ "kind": "FAT32", "label": "BOOT", "size": 1 << 20 })
        );
    }

    #[test]
    fn gpt_with_filesystems() {
        let mut img = vec![0u8; 2 << 20];
        img[446 + 4] = 0xEE;
        put(&mut img, 446 + 8, 1, 4);
        put(&mut img, 446 + 12, 0xFFFF_FFFF, 4);
        img[510] = 0x55;
        img[511] = 0xAA;
        let h = 512;
        img[h..h + 8].copy_from_slice(b"EFI PART");
        img[h + 56..h + 72].copy_from_slice(&guid_bytes("11111111-2222-3333-4444-555555555555"));
        put(&mut img, h + 72, 2, 8);
        put(&mut img, h + 80, 128, 4);
        put(&mut img, h + 84, 128, 4);
        let e = 1024;
        img[e..e + 16].copy_from_slice(&guid_bytes("0FC63DAF-8483-4772-8E79-3D69D8477DE4"));
        put(&mut img, e + 32, 2048, 8);
        put(&mut img, e + 40, 3071, 8);
        for (i, c) in "data".encode_utf16().enumerate() {
            put(&mut img, e + 56 + 2 * i, u64::from(c), 2);
        }
        let e = 1024 + 128;
        img[e..e + 16].copy_from_slice(&guid_bytes("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7"));
        put(&mut img, e + 32, 3072, 8);
        put(&mut img, e + 40, 4095, 8);
        // XFS in the first, NTFS in the second
        let x = 2048 * 512;
        img[x..x + 4].copy_from_slice(b"XFSB");
        put_be(&mut img, x + 4, 4096, 4);
        put_be(&mut img, x + 8, 128, 8);
        img[x + 108..x + 114].copy_from_slice(b"xfsvol");
        let n = 3072 * 512;
        img[n + 3..n + 11].copy_from_slice(b"NTFS    ");
        put(&mut img, n + 11, 512, 2);
        put(&mut img, n + 40, 1024, 8);
        let d = disk(tmp(&img, ".img").path(), None).unwrap();
        assert_eq!(d["table"], "GPT");
        let p = d["partitions"].as_array().unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(
            (p[0]["type"].as_str(), p[0]["name"].as_str()),
            (Some("Linux filesystem"), Some("data"))
        );
        assert_eq!(
            p[0]["fs"],
            json!({ "kind": "XFS", "label": "xfsvol", "size": 128 * 4096 })
        );
        assert_eq!(p[1]["type"], "Microsoft basic data");
        assert_eq!(p[1]["fs"]["kind"], "NTFS");
        assert_eq!(p[1]["fs"]["size"], 1024 * 512);
        assert!(
            d["details"]
                .to_string()
                .contains("11111111-2222-3333-4444-555555555555")
        );
    }

    #[test]
    fn a_bare_filesystem_image() {
        let mut img = vec![0u8; 1 << 20];
        let sb = 0x10000;
        img[sb + 0x40..sb + 0x48].copy_from_slice(b"_BHRfS_M");
        put(&mut img, sb + 0x70, 1 << 30, 8);
        img[sb + 0x12B..sb + 0x12B + 4].copy_from_slice(b"pool");
        let d = disk(tmp(&img, ".img").path(), None).unwrap();
        assert_eq!(d["table"], Value::Null);
        assert_eq!(
            d["fs"],
            json!({ "kind": "btrfs", "label": "pool", "size": 1u64 << 30 })
        );
        let d = disk(tmp(&[7u8; 5000], ".bin").path(), None).unwrap();
        assert_eq!(d["format"], "unknown");
    }

    fn iso_record(name: &[u8], lba: u64, len: u64, dir: bool) -> Vec<u8> {
        let nl = name.len();
        let rl = 33 + nl + (1 - nl % 2);
        let mut r = vec![0u8; rl];
        r[0] = rl as u8;
        put(&mut r, 2, lba, 4);
        put_be(&mut r, 6, lba, 4);
        put(&mut r, 10, len, 4);
        put_be(&mut r, 14, len, 4);
        r[18..25].copy_from_slice(&[124, 1, 2, 3, 4, 5, 0]);
        r[25] = if dir { 2 } else { 0 };
        r[32] = nl as u8;
        r[33..33 + nl].copy_from_slice(name);
        r
    }

    fn iso_bytes() -> Vec<u8> {
        let mut img = vec![0u8; 22 * 2048];
        let p = 16 * 2048;
        img[p] = 1;
        img[p + 1..p + 6].copy_from_slice(b"CD001");
        img[p + 6] = 1;
        img[p + 40..p + 72].copy_from_slice(format!("{:<32}", "TESTDISC").as_bytes());
        put(&mut img, p + 80, 22, 4);
        put(&mut img, p + 128, 2048, 2);
        let root = iso_record(&[0], 18, 2048, true);
        img[p + 156..p + 156 + 34].copy_from_slice(&root);
        let t = 17 * 2048;
        img[t] = 255;
        img[t + 1..t + 6].copy_from_slice(b"CD001");
        let mut dir = |at: usize, recs: Vec<Vec<u8>>| {
            let mut o = at;
            for r in recs {
                img[o..o + r.len()].copy_from_slice(&r);
                o += r.len();
            }
        };
        dir(
            18 * 2048,
            vec![
                iso_record(&[0], 18, 2048, true),
                iso_record(&[1], 18, 2048, true),
                iso_record(b"DOCS", 19, 2048, true),
                iso_record(b"README.TXT;1", 20, 5, false),
            ],
        );
        dir(
            19 * 2048,
            vec![
                iso_record(&[0], 19, 2048, true),
                iso_record(&[1], 18, 2048, true),
                iso_record(b"A.TXT;1", 21, 3, false),
            ],
        );
        img[20 * 2048..20 * 2048 + 5].copy_from_slice(b"hello");
        img[21 * 2048..21 * 2048 + 3].copy_from_slice(b"abc");
        img
    }

    #[test]
    fn iso_listing_and_files() {
        let f = tmp(&iso_bytes(), ".iso");
        let d = disk(f.path(), None).unwrap();
        assert_eq!(
            (d["format"].as_str(), d["label"].as_str()),
            (Some("iso9660"), Some("TESTDISC"))
        );
        assert_eq!(names(&d), ["DOCS", "README.TXT"]);
        assert_eq!(d["entries"][0]["dir"], true);
        assert_eq!(d["entries"][1]["size"], 5);
        assert_eq!(
            d["entries"][1]["mtime"],
            days_from_civil(2024, 1, 2) * 86_400 + 3 * 3600 + 4 * 60 + 5
        );
        let d = disk(f.path(), Some("/DOCS")).unwrap();
        assert_eq!(names(&d), ["A.TXT"]);
        assert_eq!(
            read_part(entry(f.path(), "DOCS/A.TXT", None).unwrap()),
            b"abc"
        );
        assert_eq!(
            read_part(entry(f.path(), "/readme.txt", None).unwrap()),
            b"hello"
        );
        assert!(disk(f.path(), Some("/NOPE")).is_err());
        assert!(entry(f.path(), "DOCS", None).is_err());
    }

    #[test]
    fn virtual_disks() {
        let mut q = vec![0u8; 512];
        q[..4].copy_from_slice(b"QFI\xfb");
        put_be(&mut q, 4, 3, 4);
        put_be(&mut q, 20, 16, 4);
        put_be(&mut q, 24, 10 << 30, 8);
        let d = disk(tmp(&q, ".qcow2").path(), None).unwrap();
        assert_eq!(
            (d["format"].as_str(), d["virtual_size"].as_u64()),
            (Some("qcow2"), Some(10 << 30))
        );

        let mut v = vec![0u8; 1024];
        v[..40].copy_from_slice(b"<<< Oracle VM VirtualBox Disk Image >>>\n");
        put(&mut v, 0x40, 0xBEDA_107F, 4);
        put(&mut v, 0x44, 0x0001_0001, 4);
        put(&mut v, 0x4C, 1, 4);
        put(&mut v, 0x170, 8 << 30, 8);
        put(&mut v, 0x178, 1 << 20, 4);
        put(&mut v, 0x184, 3, 4);
        let d = disk(tmp(&v, ".vdi").path(), None).unwrap();
        assert_eq!(
            (d["format"].as_str(), d["virtual_size"].as_u64()),
            (Some("vdi"), Some(8 << 30))
        );
        assert!(d["details"].to_string().contains("dynamic"));

        let mut h = vec![0u8; (1 << 20) + 512];
        let ft = 1 << 20;
        h[ft..ft + 8].copy_from_slice(b"conectix");
        h[ft + 28..ft + 32].copy_from_slice(b"test");
        put_be(&mut h, ft + 48, 1 << 20, 8);
        put_be(&mut h, ft + 60, 2, 4);
        let d = disk(tmp(&h, ".vhd").path(), None).unwrap();
        assert_eq!(
            (d["format"].as_str(), d["virtual_size"].as_u64()),
            (Some("vhd"), Some(1 << 20))
        );
        assert!(d["details"].to_string().contains("fixed"));

        let desc = "# Disk DescriptorFile\nversion=1\ncreateType=\"monolithicFlat\"\n\n# Extent description\nRW 2097152 FLAT \"disk-flat.vmdk\" 0\n\nddb.adapterType = \"lsilogic\"\n";
        let d = disk(tmp(desc.as_bytes(), ".vmdk").path(), None).unwrap();
        assert_eq!(
            (d["format"].as_str(), d["virtual_size"].as_u64()),
            (Some("vmdk"), Some(1 << 30))
        );
        assert!(d["details"].to_string().contains("monolithicFlat"));

        let mut x = vec![0u8; 320 << 10];
        x[..8].copy_from_slice(b"vhdxfile");
        let rt = 192 << 10;
        x[rt..rt + 4].copy_from_slice(b"regi");
        put(&mut x, rt + 8, 1, 4);
        x[rt + 16..rt + 32].copy_from_slice(&guid_bytes(VHDX_METADATA));
        put(&mut x, rt + 32, 256 << 10, 8);
        let mt = 256 << 10;
        x[mt..mt + 8].copy_from_slice(b"metadata");
        put(&mut x, mt + 10, 1, 2);
        x[mt + 32..mt + 48].copy_from_slice(&guid_bytes(VHDX_SIZE));
        put(&mut x, mt + 48, 0x8000, 4);
        put(&mut x, mt + 0x8000, 20 << 30, 8);
        let d = disk(tmp(&x, ".vhdx").path(), None).unwrap();
        assert_eq!(
            (d["format"].as_str(), d["virtual_size"].as_u64()),
            (Some("vhdx"), Some(20 << 30))
        );
    }

    #[test]
    fn dmg_trailer_and_plist() {
        use base64::Engine;
        let mut mish = vec![0u8; 204 + 2 * 40];
        mish[..4].copy_from_slice(b"mish");
        put_be(&mut mish, 16, 2048, 8);
        put_be(&mut mish, 200, 2, 4);
        put_be(&mut mish, 204, 0x8000_0005, 4);
        put_be(&mut mish, 244, 0xFFFF_FFFF, 4);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&mish);
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>resource-fork</key><dict><key>blkx</key><array><dict><key>Attributes</key><string>0x0050</string><key>CFName</key><string>disk image (Apple_HFS : 1)</string><key>Data</key><data>\n{b64}\n</data><key>ID</key><string>0</string><key>Name</key><string>disk image (Apple_HFS : 1)</string></dict></array></dict></dict></plist>"
        );
        let mut img = vec![0u8; 1000];
        img.extend_from_slice(plist.as_bytes());
        let mut k = vec![0u8; 512];
        k[..4].copy_from_slice(b"koly");
        put_be(&mut k, 4, 4, 4);
        put_be(&mut k, 8, 512, 4);
        put_be(&mut k, 32, 1000, 8);
        put_be(&mut k, 216, 1000, 8);
        put_be(&mut k, 224, plist.len() as u64, 8);
        put_be(&mut k, 492, 2048, 8);
        img.extend_from_slice(&k);
        let d = disk(tmp(&img, ".dmg").path(), None).unwrap();
        assert_eq!(
            (d["format"].as_str(), d["virtual_size"].as_u64()),
            (Some("dmg"), Some(1 << 20))
        );
        assert_eq!(d["compressed"], true);
        assert!(d["details"].to_string().contains("zlib (UDZO)"));
        assert_eq!(
            d["dmg_parts"],
            json!([{ "name": "disk image (Apple_HFS : 1)", "size": 1 << 20 }])
        );
    }

    #[test]
    fn small_helpers() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2024, 3, 1), 19_783);
        assert_eq!(dos_time(0x5821, 0x6000), Some(1_704_110_400)); // 2024-01-01 12:00:00
        assert_eq!(cp437(b"caf\x82"), "café");
        assert_eq!(resolve("ppt/slides", "../media/a.png"), "ppt/media/a.png");
        assert_eq!(resolve("ppt", "/ppt/slides/s.xml"), "ppt/slides/s.xml");
        assert_eq!(
            guid(&guid_bytes("C12A7328-F81F-11D2-BA4B-00A0C93EC93B")),
            "C12A7328-F81F-11D2-BA4B-00A0C93EC93B"
        );
        assert!(is_from_line(b"From x Mon Jan  1 00:00:00 2024\n", false));
        assert!(!is_from_line(b"From here we go\n", false));
        assert!(is_from_line(b"From here we go\n", true));
        assert_eq!(
            decode_entities("a &amp; b &#x41;&#66; &bogus; &"),
            "a & b AB &bogus; &"
        );
        // a deck of plain text boxes: the first short line stands in for the title
        let mut s = Slide {
            paras: vec![("Loose title".into(), 0), ("body".into(), 0)],
            ..Default::default()
        };
        s.settle();
        assert_eq!((s.title.as_str(), s.paras.len()), ("Loose title", 1));
    }
}
