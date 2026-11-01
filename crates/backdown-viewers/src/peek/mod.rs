//! Read-only looks inside container files, for the browser's viewers: spreadsheets, slide decks,
//! mail (.eml and mbox), archives (zip and tar, plain or compressed) and disk images.
//!
//! `serve` resolves an index id to a path and hands it here; nothing in this module picks files by
//! itself, writes anything or runs anything. Every call reads a bounded amount: headers, directories
//! and tables, one page of rows or messages, one entry of at most `ENTRY_CAP` bytes. Walks through
//! compressed streams and big mailboxes also stop on a time budget and say where to carry on, so a
//! multi-gigabyte file can't pin a request thread.

use std::borrow::Cow;
use std::io::{self, Read};
use std::time::{Duration, Instant};

use backdown_core::magic::le;
use quick_xml::Reader as Xml;
use quick_xml::events::{BytesRef, BytesStart};

mod archives;
mod disk;
mod mail;
mod sheets;
mod slides;

pub use archives::*;
pub use disk::*;
pub use mail::*;
pub use sheets::*;
pub use slides::*;

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

// the files inside a disk image's filesystems (FAT, exFAT, ext2/3/4, NTFS)
mod fsview;
pub use fsview::{fs_entry, fs_list, fs_region, region_entry, region_list};

#[cfg(test)]
mod tests;
