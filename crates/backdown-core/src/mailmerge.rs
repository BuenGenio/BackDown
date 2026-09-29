//! Merging mailboxes: copies of the same mailbox taken at different times (a Maildir, Maildir++
//! folders, mbox files, a Thunderbird folder of mbox files) made into one Maildir, every message
//! once. And the way back out: a Maildir folder exported as one mbox file (mboxrd).
//!
//! **Which messages are the same.** By Message-ID, together with the message's body (line endings
//! and mbox quoting evened out): two copies of one message that a server or a mail program gave
//! different headers to (Status, X-UID, an extra Received line) are one message; two different
//! messages that happen to share an ID (a mailer that reuses them, drafts saved over and over) are
//! both kept. A message without a Message-ID is matched by its main headers and body instead.
//!
//! **Where each goes.** The newest snapshot that has a message decides its folder(s): a message
//! filed from INBOX into Archive between two backups ends up in Archive only, and one deleted
//! since the older backup is kept, in the folder it was in then. Snapshots are ordered by their
//! newest message. Folders that only one snapshot has are kept.
//!
//! **Flags.** Seen, replied, flagged and forwarded are combined from every copy (read anywhere
//! means read); draft and deleted come from the copy that's kept. Mbox flags are read from
//! Status / X-Status (mutt, Pine) and X-Mozilla-Status (Thunderbird).
//!
//! **What's written.** Each message's bytes exactly as its source holds them (an mbox message
//! without its "From " line and with its ">From " quoting undone), into `cur/` of the right folder
//! under a name made from its delivery time and content hash, so running again finds what's
//! there. Each is written to `tmp/`, flushed, renamed into place, read back and compared, and
//! journaled. Nothing is ever written to a source, and nothing at the destination is overwritten.
//!
//! Mail stores (a server's folder of domains and users) merge mailbox by mailbox: each mailbox is
//! matched with the same one (the same path inside the store) in the other snapshots.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Db, fastcopy, fmt, paths};

/// The tables (made by `Db::open`): each merge (or export), and each message it wrote.
pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS mail_merges (
    id         INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL,
    kind       TEXT NOT NULL,         -- merge | export
    sources    TEXT NOT NULL,         -- JSON list of paths
    dest       TEXT NOT NULL,         -- the Maildir (or the mbox file) written
    plan_id    INTEGER,               -- a filing plan's merge item, if it came from one
    item       INTEGER,
    state      TEXT NOT NULL,         -- new | running | done | partial | failed | stopped
    summary    TEXT,
    report     TEXT                   -- JSON: Report
);
CREATE TABLE IF NOT EXISTS mail_merge_log (
    id       INTEGER PRIMARY KEY,
    merge_id INTEGER NOT NULL REFERENCES mail_merges(id),
    dest     BLOB NOT NULL,           -- the message file written
    folder   TEXT NOT NULL,
    source   BLOB NOT NULL,           -- where its bytes came from: a Maildir file, or an mbox
    offset   INTEGER,                 -- in an mbox: where the message starts
    size     INTEGER NOT NULL,
    sha256   BLOB NOT NULL,           -- of the bytes written, as read back
    flags    TEXT NOT NULL,
    at       INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS mail_merge_log_dest ON mail_merge_log(merge_id, dest);
";

pub const SEEN: u8 = 1;
pub const REPLIED: u8 = 2;
pub const FLAGGED: u8 = 4;
pub const TRASHED: u8 = 8;
pub const DRAFT: u8 = 16;
pub const PASSED: u8 = 32;
/// combined from every copy
const UNION: u8 = SEEN | REPLIED | FLAGGED | PASSED;

/// Maildir's info separator: ':' where file names may hold it, '!' on Windows.
const INFO: &str = if cfg!(windows) { "!2," } else { ":2," };

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn push(v: &mut Vec<String>, s: String) {
    if v.len() < 30 {
        v.push(s);
    }
}

// ---------- flags

/// A Maildir message's flags from its name ("…:2,FRS"; "!2," and ";2," where ':' can't be used).
/// Lower-case letters are Dovecot keywords, which mean something only with that mailbox's own
/// keyword list, so they're left out.
pub fn maildir_flags(name: &str) -> u8 {
    let Some(i) = [":2,", "!2,", ";2,"]
        .iter()
        .filter_map(|s| name.rfind(s))
        .max()
    else {
        return 0;
    };
    name[i + 3..].chars().fold(0, |f, c| {
        f | match c {
            'S' => SEEN,
            'R' => REPLIED,
            'F' => FLAGGED,
            'T' => TRASHED,
            'D' => DRAFT,
            'P' => PASSED,
            _ => 0,
        }
    })
}

/// Flags as Maildir letters, in the order Maildir wants them.
pub fn letters(f: u8) -> String {
    [
        (DRAFT, 'D'),
        (FLAGGED, 'F'),
        (PASSED, 'P'),
        (REPLIED, 'R'),
        (SEEN, 'S'),
        (TRASHED, 'T'),
    ]
    .iter()
    .filter(|(bit, _)| f & bit != 0)
    .map(|(_, c)| *c)
    .collect()
}

/// An mbox message's flags from Status / X-Status (mutt, Pine) and X-Mozilla-Status (Thunderbird).
fn mbox_flags(h: &Headers) -> u8 {
    let mut f = 0;
    if let Some(v) = h
        .get("x-mozilla-status")
        .and_then(|v| u32::from_str_radix(v.trim(), 16).ok())
    {
        for (bit, flag) in [
            (0x1, SEEN),
            (0x2, REPLIED),
            (0x4, FLAGGED),
            (0x8, TRASHED),
            (0x1000, PASSED),
        ] {
            if v & bit != 0 {
                f |= flag;
            }
        }
    }
    if h.get("status").is_some_and(|v| v.contains('R')) {
        f |= SEEN;
    }
    if let Some(v) = h.get("x-status") {
        for c in v.trim().chars() {
            f |= match c {
                'A' => REPLIED,
                'F' => FLAGGED,
                'D' => TRASHED,
                'T' => DRAFT,
                _ => 0,
            };
        }
    }
    f
}

// ---------- one message's headers

/// A message's header fields (names lower-cased, folded lines joined), in order.
struct Headers(Vec<(String, String)>);

impl Headers {
    fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Where the header block ends: the offset of the body, just after the blank line; `None` when
/// there's no blank line (then it's all headers, or not a message at all).
fn head_end(msg: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i < msg.len() {
        let end = msg[i..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(msg.len(), |p| i + p + 1);
        let line = &msg[i..end];
        if line == b"\n" || line == b"\r\n" {
            return Some(end);
        }
        i = end;
    }
    None
}

/// Where the header block ends (the whole message if it has no body).
fn body_start(msg: &[u8]) -> usize {
    head_end(msg).unwrap_or(msg.len())
}

fn parse_headers(block: &[u8]) -> Headers {
    let mut out: Vec<(String, String)> = Vec::new();
    for raw in block.split(|&b| b == b'\n') {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        if line.is_empty() {
            break;
        }
        if line[0] == b' ' || line[0] == b'\t' {
            if let Some((_, v)) = out.last_mut() {
                v.push(' ');
                v.push_str(String::from_utf8_lossy(line).trim());
            }
            continue;
        }
        if let Some(i) = line.iter().position(|&b| b == b':') {
            let name = String::from_utf8_lossy(&line[..i])
                .trim()
                .to_ascii_lowercase();
            if !name.is_empty() && !name.contains(' ') {
                out.push((
                    name,
                    String::from_utf8_lossy(&line[i + 1..]).trim().to_string(),
                ));
            }
        }
    }
    Headers(out)
}

/// The Message-ID without its angle brackets and spaces, if there's a usable one.
fn message_id(h: &Headers) -> Option<String> {
    let v = h.get("message-id")?.trim();
    let id = match (v.find('<'), v.rfind('>')) {
        (Some(a), Some(b)) if b > a + 1 => &v[a + 1..b],
        _ => v,
    };
    let id: String = id.chars().filter(|c| !c.is_whitespace()).collect();
    (!id.is_empty()).then_some(id)
}

/// A body evened out for comparing copies: CRLF as LF, mbox's ">From " quoting undone at any
/// depth, trailing blank lines and spaces dropped.
fn body_key(body: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    let mut text = body;
    while let Some(last) = text.last() {
        if last.is_ascii_whitespace() {
            text = &text[..text.len() - 1];
        } else {
            break;
        }
    }
    for line in text.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let unq = line.iter().position(|&b| b != b'>').unwrap_or(line.len());
        let line = if unq > 0 && line[unq..].starts_with(b"From ") {
            &line[unq..]
        } else {
            line
        };
        h.update(line);
        h.update(b"\n");
    }
    h.finalize().into()
}

/// Header fields that mail programs and servers add or change on their copy of a message (flags,
/// UIDs, sizes): left out when copies are compared by their headers.
const VOLATILE: &[&str] = &[
    "status",
    "x-status",
    "x-mozilla-status",
    "x-mozilla-status2",
    "x-mozilla-keys",
    "x-uid",
    "x-keywords",
    "x-imap",
    "x-imapbase",
    "x-imap-uid",
    "content-length",
    "lines",
    "x-evolution",
    "x-evolution-source",
];

/// Which message this is (see the module doc), and whether it had a Message-ID.
fn identity(msg: &[u8], h: &Headers) -> ([u8; 32], bool) {
    let mut k = Sha256::new();
    // no blank line after the headers: not a proper message, so all of it is what it is
    let Some(start) = head_end(msg) else {
        k.update(b"raw\0");
        k.update(body_key(msg));
        return (k.finalize().into(), false);
    };
    let body = body_key(&msg[start..]);
    let has_id = match message_id(h) {
        Some(id) => {
            k.update(b"id\0");
            k.update(id.as_bytes());
            true
        }
        None => {
            k.update(b"headers\0");
            for (name, v) in &h.0 {
                if VOLATILE.contains(&name.as_str()) {
                    continue;
                }
                let v: Vec<&str> = v.split_whitespace().collect();
                k.update(name.as_bytes());
                k.update(b":");
                k.update(v.join(" ").as_bytes());
                k.update(b"\n");
            }
            false
        }
    };
    k.update(b"\0");
    k.update(body);
    (k.finalize().into(), has_id)
}

// ---------- dates

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

fn civil(t: i64) -> (i64, i64, i64) {
    let z = t.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

fn month(w: &str) -> Option<i64> {
    let w = w.get(..3)?.to_ascii_lowercase();
    MONTHS.iter().position(|m| *m == w).map(|i| i as i64 + 1)
}

fn clock(w: &str) -> Option<i64> {
    let p: Vec<i64> = w
        .split(':')
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    match p.as_slice() {
        [h, m] if *h < 24 && *m < 60 => Some(h * 3600 + m * 60),
        [h, m, s] if *h < 24 && *m < 60 && *s <= 60 => Some(h * 3600 + m * 60 + s),
        _ => None,
    }
}

/// An RFC 5322 date ("Tue, 1 Jul 2003 10:52:37 +0200", with the usual variations), as Unix time.
pub fn rfc2822(s: &str) -> Option<i64> {
    let s = s.split('(').next().unwrap_or(s); // "(CEST)" comments
    let w: Vec<&str> = s
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|x| !x.is_empty())
        .collect();
    let mi = w.iter().position(|x| month(x).is_some())?;
    let m = month(w[mi])?;
    // "1 Jul 2003" or "Jul 1 2003"
    let (d, rest) = match (
        mi.checked_sub(1).and_then(|i| w[i].parse::<i64>().ok()),
        w.get(mi + 1),
    ) {
        (Some(d), _) => (d, mi + 1),
        (None, Some(x)) => (x.parse::<i64>().ok()?, mi + 2),
        _ => return None,
    };
    let mut y: i64 = w.get(rest)?.parse().ok()?;
    if y < 50 {
        y += 2000;
    } else if y < 1000 {
        y += 1900;
    }
    let secs = w.get(rest + 1).and_then(|x| clock(x)).unwrap_or(0);
    let zone = w.get(rest + 2).map_or(0, |z| {
        let b = z.as_bytes();
        if (b.first() == Some(&b'+') || b.first() == Some(&b'-')) && z.len() == 5 {
            let n: i64 = z[1..].parse().unwrap_or(0);
            let off = (n / 100) * 3600 + (n % 100) * 60;
            if b[0] == b'-' { -off } else { off }
        } else {
            match z.to_ascii_uppercase().as_str() {
                "EDT" => -4 * 3600,
                "EST" | "CDT" => -5 * 3600,
                "CST" | "MDT" => -6 * 3600,
                "MST" | "PDT" => -7 * 3600,
                "PST" => -8 * 3600,
                _ => 0,
            }
        }
    });
    if !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400 + secs - zone)
}

/// The date on an mbox "From " line ("From ann@example.org Mon Jan  1 12:00:00 2024").
fn envelope_date(line: &[u8]) -> Option<i64> {
    let s = String::from_utf8_lossy(line);
    let w: Vec<&str> = s.split_whitespace().collect();
    let mi = (2..w.len()).find(|&i| month(w[i]).is_some() && w[i].len() == 3)?;
    let m = month(w[mi])?;
    let d: i64 = w.get(mi + 1)?.parse().ok()?;
    let secs = clock(w.get(mi + 2)?)?;
    let y: i64 = w[mi + 3..]
        .iter()
        .find(|x| x.len() == 4 && x.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()?;
    if !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400 + secs)
}

/// "Mon Jan  1 12:00:00 2024", as an mbox "From " line has it (UTC).
fn asctime(t: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    let (y, m, d) = civil(t);
    let s = t.rem_euclid(86_400);
    let month = MONTHS[(m - 1) as usize];
    format!(
        "{} {}{} {d:>2} {:02}:{:02}:{:02} {y}",
        DAYS[t.div_euclid(86_400).rem_euclid(7) as usize],
        month[..1].to_ascii_uppercase(),
        &month[1..],
        s / 3600,
        s % 3600 / 60,
        s % 60
    )
}

/// When a Maildir message was delivered: the Unix time its unique name starts with.
fn delivered(name: &str) -> Option<i64> {
    let digits = name.split('.').next()?;
    if (9..=11).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()) {
        digits.parse().ok()
    } else {
        None
    }
}

// ---------- mbox files

fn blank(line: &[u8]) -> bool {
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

/// An mbox separator: "From " at the start of a line, after a blank line (or looking like an
/// envelope, "From sender Mon Jan  1 12:00:00 2024", for writers that don't leave one). The same
/// rule as the browser's mbox viewer.
fn is_from_line(line: &[u8], prev_blank: bool) -> bool {
    line.starts_with(b"From ") && (prev_blank || has_clock(line))
}

/// Undo mbox quoting: one '>' off every line that is '>'s then "From " (mboxrd; for the older
/// mboxo, which quoted only "From ", it's the same except for ">>From " lines, which are rare).
fn unquote(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    for line in raw.split_inclusive(|&b| b == b'\n') {
        let q = line.iter().position(|&b| b != b'>').unwrap_or(line.len());
        if q > 0 && line[q..].starts_with(b"From ") {
            out.extend_from_slice(&line[1..]);
        } else {
            out.extend_from_slice(line);
        }
    }
    out
}

/// Quote for mboxrd: one more '>' on every line that is '>'s then "From ".
fn quote(msg: &[u8], out: &mut Vec<u8>) {
    for line in msg.split_inclusive(|&b| b == b'\n') {
        let q = line.iter().position(|&b| b != b'>').unwrap_or(line.len());
        if line[q..].starts_with(b"From ") {
            out.push(b'>');
        }
        out.extend_from_slice(line);
    }
}

/// One message of an mbox: its "From " line and where its content is (quoted, as stored).
struct MboxMsg {
    envelope: Vec<u8>,
    /// the first byte after the "From " line, and the stored length (without the blank line
    /// that separates it from the next message)
    start: u64,
    len: u64,
    /// the content, unquoted
    content: Vec<u8>,
}

/// Reads an mbox one message at a time.
struct MboxReader<R: BufRead> {
    r: R,
    pos: u64,
    next_from: Option<(Vec<u8>, u64)>,
    started: bool,
    line: Vec<u8>,
}

impl<R: BufRead> MboxReader<R> {
    fn new(r: R) -> Self {
        MboxReader {
            r,
            pos: 0,
            next_from: None,
            started: false,
            line: Vec::new(),
        }
    }

    fn read_line(&mut self) -> io::Result<usize> {
        self.line.clear();
        let n = self.r.read_until(b'\n', &mut self.line)?;
        self.pos += n as u64;
        Ok(n)
    }

    fn next_msg(&mut self) -> io::Result<Option<MboxMsg>> {
        if !self.started {
            self.started = true;
            // the first line must be a "From " line: anything else isn't an mbox
            if self.read_line()? == 0 {
                return Ok(None);
            }
            if !self.line.starts_with(b"From ") {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "not an mbox file (it doesn't start with a \"From \" line)",
                ));
            }
            self.next_from = Some((self.line.clone(), self.pos));
        }
        let Some((envelope, start)) = self.next_from.take() else {
            return Ok(None);
        };
        let mut raw = Vec::new();
        let mut prev_blank = true;
        loop {
            if self.read_line()? == 0 {
                break;
            }
            if is_from_line(&self.line, prev_blank) {
                self.next_from = Some((self.line.clone(), self.pos));
                break;
            }
            prev_blank = blank(&self.line);
            raw.extend_from_slice(&self.line);
        }
        // the blank line before the next "From " belongs to the separator, not the message
        if raw.ends_with(b"\r\n\r\n") {
            raw.truncate(raw.len() - 2);
        } else if raw.ends_with(b"\n\n") || (self.next_from.is_some() && raw == b"\n") {
            raw.truncate(raw.len() - 1);
        }
        Ok(Some(MboxMsg {
            envelope,
            start,
            len: raw.len() as u64,
            content: unquote(&raw),
        }))
    }
}

/// The content of the mbox message stored at `start` for `len` bytes, unquoted.
fn mbox_content(path: &Path, start: u64, len: u64) -> io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(start))?;
    let mut raw = vec![0u8; len as usize];
    f.read_exact(&mut raw)?;
    Ok(unquote(&raw))
}

fn looks_mbox(path: &Path) -> bool {
    let mut head = [0u8; 5];
    File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && &head == b"From "
}

// ---------- finding the mail in a source

/// Where one folder's messages are.
#[derive(Debug, Clone, PartialEq)]
enum Loc {
    /// a Maildir folder: its cur/ and new/
    Maildir(PathBuf),
    Mbox(PathBuf),
}

#[derive(Debug, Clone)]
struct FolderAt {
    /// "INBOX", "Sent", "Archive.2019" (Maildir++ names, '.' between levels)
    name: String,
    loc: Loc,
}

/// One mailbox of a source: its folders. `key` places it in a store ("" for a lone mailbox).
#[derive(Debug, Clone)]
struct MailboxAt {
    key: String,
    folders: Vec<FolderAt>,
}

#[derive(Debug, Clone)]
struct SourceAt {
    path: PathBuf,
    /// "Maildir", "mbox", "mbox folder", "mail store"
    kind: &'static str,
    mailboxes: Vec<MailboxAt>,
}

fn is_maildir(p: &Path) -> bool {
    p.join("cur").is_dir() || p.join("new").is_dir()
}

/// A mailbox's top: a Maildir, or a folder holding Maildir++ ".Folder"s (an INBOX that was
/// never delivered to may lack its own cur/).
fn is_mailbox_root(p: &Path) -> bool {
    is_maildir(p)
        || fs::read_dir(p).is_ok_and(|rd| {
            rd.filter_map(|e| e.ok()).any(|e| {
                e.file_name().to_string_lossy().starts_with('.')
                    && e.file_type().is_ok_and(|t| t.is_dir())
                    && is_maildir(&e.path())
            })
        })
}

fn folder_name(parts: &[String]) -> String {
    let n = parts.join(".");
    if n.eq_ignore_ascii_case("inbox") {
        "INBOX".into()
    } else {
        n
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A Maildir's folders: itself (INBOX), Maildir++ ".Folder" subfolders, and nested folders
/// (Dovecot's LAYOUT=fs).
fn maildir_folders(root: &Path) -> io::Result<Vec<FolderAt>> {
    let mut out = vec![FolderAt {
        name: "INBOX".into(),
        loc: Loc::Maildir(root.to_path_buf()),
    }];
    fn nested(dir: &Path, parts: &mut Vec<String>, out: &mut Vec<FolderAt>, depth: usize) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        let mut kids: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        kids.sort();
        for k in kids {
            let n = file_name(&k);
            if n.starts_with('.') || matches!(n.as_str(), "cur" | "new" | "tmp") || !k.is_dir() {
                continue;
            }
            parts.push(n);
            if is_maildir(&k) {
                out.push(FolderAt {
                    name: folder_name(parts),
                    loc: Loc::Maildir(k.clone()),
                });
            }
            if depth < 8 {
                nested(&k, parts, out, depth + 1);
            }
            parts.pop();
        }
    }
    let mut kids: Vec<PathBuf> = fs::read_dir(root)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    kids.sort();
    for k in &kids {
        let n = file_name(k);
        if let Some(sub) = n.strip_prefix('.')
            && !sub.is_empty()
            && sub != "."
            && k.is_dir()
            && is_maildir(k)
        {
            out.push(FolderAt {
                name: folder_name(&[sub.to_string()]),
                loc: Loc::Maildir(k.clone()),
            });
        }
    }
    nested(root, &mut Vec::new(), &mut out, 0);
    Ok(out)
}

/// A folder of mbox files, Thunderbird's way: each file a folder, "Name.sbd/" holding Name's
/// subfolders; the .msf files beside them are Thunderbird's indexes.
fn mbox_folders(dir: &Path, parts: &mut Vec<String>, out: &mut Vec<FolderAt>, depth: usize) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut kids: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    kids.sort();
    for k in kids {
        let n = file_name(&k);
        if n.starts_with('.') {
            continue;
        }
        if k.is_file() && looks_mbox(&k) {
            let stem = n.strip_suffix(".mbox").unwrap_or(&n).to_string();
            parts.push(stem);
            out.push(FolderAt {
                name: folder_name(parts),
                loc: Loc::Mbox(k.clone()),
            });
            parts.pop();
        } else if k.is_dir()
            && let Some(sub) = n.strip_suffix(".sbd")
            && depth < 8
        {
            parts.push(sub.to_string());
            mbox_folders(&k, parts, out, depth + 1);
            parts.pop();
        }
    }
}

/// Mailboxes (Maildirs not inside another) below a store's folder, keyed by their path in it.
fn store_mailboxes(root: &Path) -> io::Result<Vec<MailboxAt>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<MailboxAt>, depth: usize) -> io::Result<()> {
        let mut kids: Vec<PathBuf> = fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        kids.sort();
        for k in kids {
            if !k.is_dir() || k.is_symlink() {
                continue;
            }
            if is_mailbox_root(&k) {
                let key = k
                    .strip_prefix(root)
                    .map(|r| r.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                out.push(MailboxAt {
                    key,
                    folders: maildir_folders(&k)?,
                });
            } else if depth < 6 && !file_name(&k).starts_with('.') {
                walk(root, &k, out, depth + 1)?;
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out, 0)?;
    Ok(out)
}

/// What a source path holds.
fn discover(path: &Path) -> Result<SourceAt> {
    let meta = fs::metadata(path).with_context(|| format!("can't open {}", path.display()))?;
    if meta.is_file() {
        if !looks_mbox(path) {
            bail!(
                "{} isn't an mbox file (it doesn't start with a \"From \" line)",
                path.display()
            );
        }
        let n = file_name(path);
        let stem = n
            .strip_suffix(".mbox")
            .or_else(|| n.strip_suffix(".mbx"))
            .unwrap_or(&n);
        let name = if stem.eq_ignore_ascii_case("mbox") {
            "INBOX".to_string()
        } else {
            folder_name(&[stem.to_string()])
        };
        return Ok(SourceAt {
            path: path.to_path_buf(),
            kind: "mbox",
            mailboxes: vec![MailboxAt {
                key: String::new(),
                folders: vec![FolderAt {
                    name,
                    loc: Loc::Mbox(path.to_path_buf()),
                }],
            }],
        });
    }
    if is_mailbox_root(path) {
        return Ok(SourceAt {
            path: path.to_path_buf(),
            kind: "Maildir",
            mailboxes: vec![MailboxAt {
                key: String::new(),
                folders: maildir_folders(path)?,
            }],
        });
    }
    // user/Maildir
    let inner = path.join("Maildir");
    if is_mailbox_root(&inner) {
        return Ok(SourceAt {
            path: path.to_path_buf(),
            kind: "Maildir",
            mailboxes: vec![MailboxAt {
                key: String::new(),
                folders: maildir_folders(&inner)?,
            }],
        });
    }
    let mut folders = Vec::new();
    mbox_folders(path, &mut Vec::new(), &mut folders, 0);
    if !folders.is_empty() {
        return Ok(SourceAt {
            path: path.to_path_buf(),
            kind: "mbox folder",
            mailboxes: vec![MailboxAt {
                key: String::new(),
                folders,
            }],
        });
    }
    let boxes = store_mailboxes(path)?;
    if boxes.is_empty() {
        bail!(
            "no mail found in {} (a Maildir, mbox files or a store of Maildirs)",
            path.display()
        );
    }
    Ok(SourceAt {
        path: path.to_path_buf(),
        kind: "mail store",
        mailboxes: boxes,
    })
}

// ---------- reading every message once

/// Where a message's bytes are.
#[derive(Debug, Clone)]
enum At {
    File(PathBuf),
    Mbox { file: u32, start: u64, len: u64 },
}

/// One copy of a message, in one folder of one source.
#[derive(Debug, Clone)]
struct Occ {
    src: u16,
    folder: u32,
    at: At,
    size: u64,
    sha: [u8; 32],
    flags: u8,
    time: i64,
}

/// Everything read from the sources, by mailbox and identity.
#[derive(Default)]
struct Catalog {
    folders: Vec<String>,
    mboxes: Vec<PathBuf>,
    occs: Vec<Occ>,
    /// (mailbox key, identity) -> copies
    ids: BTreeMap<(String, [u8; 32]), Vec<u32>>,
    without_id: BTreeSet<(String, [u8; 32])>,
    /// per source: its newest message, and messages read
    as_of: Vec<Option<i64>>,
    messages: Vec<u64>,
    /// per source: (mailbox key, folder) it has
    has_folder: Vec<BTreeSet<(String, String)>>,
    unreadable: u64,
    problems: Vec<String>,
}

impl Catalog {
    fn folder_id(&mut self, name: &str) -> u32 {
        match self.folders.iter().position(|f| f == name) {
            Some(i) => i as u32,
            None => {
                self.folders.push(name.to_string());
                (self.folders.len() - 1) as u32
            }
        }
    }

    fn add(&mut self, key: &str, occ: Occ, content: &[u8]) {
        let h = parse_headers(&content[..body_start(content)]);
        let (id, has_id) = identity(content, &h);
        let s = occ.src as usize;
        self.messages[s] += 1;
        if occ.time > 0 {
            self.as_of[s] = Some(self.as_of[s].map_or(occ.time, |t: i64| t.max(occ.time)));
        }
        let k = (key.to_string(), id);
        if !has_id {
            self.without_id.insert(k.clone());
        }
        self.occs.push(occ);
        self.ids
            .entry(k)
            .or_default()
            .push((self.occs.len() - 1) as u32);
    }
}

fn read_maildir_folder(
    cat: &mut Catalog,
    src: u16,
    key: &str,
    folder: u32,
    dir: &Path,
    stop: &AtomicBool,
    tick: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    for sub in ["cur", "new"] {
        let d = dir.join(sub);
        let Ok(rd) = fs::read_dir(&d) else { continue };
        let mut names: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        names.sort();
        for p in names {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let n = file_name(&p);
            if n.starts_with('.') {
                continue;
            }
            let Ok(meta) = fs::symlink_metadata(&p) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let content = match fs::read(&p) {
                Ok(c) => c,
                Err(e) => {
                    cat.unreadable += 1;
                    push(
                        &mut cat.problems,
                        format!("can't read {}: {e}", p.display()),
                    );
                    continue;
                }
            };
            let time = delivered(&n)
                .or_else(|| {
                    meta.modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                })
                .unwrap_or(0);
            let occ = Occ {
                src,
                folder,
                at: At::File(p.clone()),
                size: content.len() as u64,
                sha: Sha256::digest(&content).into(),
                flags: maildir_flags(&n),
                time,
            };
            tick(1, occ.size);
            cat.add(key, occ, &content);
        }
    }
    Ok(())
}

fn read_mbox_folder(
    cat: &mut Catalog,
    src: u16,
    key: &str,
    folder: u32,
    path: &Path,
    stop: &AtomicBool,
    tick: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    let f = File::open(path).with_context(|| format!("can't read {}", path.display()))?;
    let file = match cat.mboxes.iter().position(|m| m == path) {
        Some(i) => i as u32,
        None => {
            cat.mboxes.push(path.to_path_buf());
            (cat.mboxes.len() - 1) as u32
        }
    };
    let mut r = MboxReader::new(BufReader::with_capacity(1 << 18, f));
    while let Some(m) = r
        .next_msg()
        .with_context(|| format!("reading {}", path.display()))?
    {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let h = parse_headers(&m.content[..body_start(&m.content)]);
        let time = envelope_date(&m.envelope)
            .or_else(|| h.get("date").and_then(rfc2822))
            .unwrap_or(0);
        let occ = Occ {
            src,
            folder,
            at: At::Mbox {
                file,
                start: m.start,
                len: m.len,
            },
            size: m.content.len() as u64,
            sha: Sha256::digest(&m.content).into(),
            flags: mbox_flags(&h),
            time,
        };
        tick(1, occ.size);
        cat.add(key, occ, &m.content);
    }
    Ok(())
}

// ---------- deciding

/// One message's place in the result.
#[derive(Debug, Clone)]
struct Placement {
    key: String,
    folder: u32,
    occ: u32,
    flags: u8,
}

/// What a merge found and did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceReport {
    pub path: String,
    /// "Maildir", "mbox", "mbox folder", "mail store"
    pub kind: String,
    /// its newest message: snapshots are ordered by this
    pub as_of: Option<i64>,
    pub messages: u64,
    /// messages no other snapshot has
    pub only_here: u64,
    pub folders: u64,
    /// folders no other snapshot has
    pub folders_only_here: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub dest: String,
    /// oldest snapshot first
    pub sources: Vec<SourceReport>,
    /// mailboxes merged (1 unless the sources are mail stores)
    pub mailboxes: u64,
    /// distinct messages (a message in two folders counts once)
    pub distinct: u64,
    /// messages in the result (a message kept in two folders counts twice)
    pub messages: u64,
    pub bytes: u64,
    /// copies left out because another copy of the same message is kept
    pub duplicates: u64,
    /// matched by their headers and body, having no Message-ID
    pub without_id: u64,
    /// the same message with different bytes in different snapshots (headers a server or program
    /// added): the newest copy is kept
    pub differing_copies: u64,
    /// in a different folder in an older snapshot: placed where the newest one has it
    pub moved: u64,
    /// whose flags were combined from more than one copy
    pub flags_merged: u64,
    /// in the result, per folder (mailbox key and folder name)
    pub folders: Vec<(String, u64)>,
    pub written: u64,
    pub written_bytes: u64,
    /// found already written (an earlier, interrupted run)
    pub resumed: u64,
    pub failed: u64,
    pub unreadable: u64,
    pub problems: Vec<String>,
    pub dry_run: bool,
}

/// Every copy read, what goes where decided.
struct Merge {
    sources: Vec<SourceAt>,
    cat: Catalog,
    placements: Vec<Placement>,
    report: Report,
}

/// `dest` made absolute through whatever of it exists (so it compares with the sources).
fn resolve_dest(dest: &Path) -> Result<PathBuf> {
    if !dest.is_absolute() {
        bail!("the merged mailbox must be a full path");
    }
    let mut existing = dest;
    let mut rest = Vec::new();
    while !existing.exists() {
        rest.push(existing.file_name().context("no such folder")?);
        existing = existing.parent().context("no such folder")?;
    }
    let mut p = dunce::canonicalize(existing)?;
    for c in rest.iter().rev() {
        p.push(c);
    }
    Ok(p)
}

fn load_sources(sources: &[PathBuf], dest: &Path) -> Result<Vec<SourceAt>> {
    if sources.is_empty() {
        bail!("nothing to merge");
    }
    let mut out = Vec::new();
    for s in sources {
        let s = dunce::canonicalize(s).with_context(|| format!("can't open {}", s.display()))?;
        if dest.starts_with(&s) || s.starts_with(dest) {
            bail!(
                "the merged mailbox can't be inside a source, or the other way round ({})",
                s.display()
            );
        }
        if out.iter().any(|x: &SourceAt| x.path == s) {
            bail!("{} is given twice", s.display());
        }
        out.push(discover(&s)?);
    }
    let stores = out.iter().filter(|s| s.kind == "mail store").count();
    if stores != 0 && stores != out.len() {
        bail!(
            "give mailboxes, or mail stores, not both: {} is a store of {} mailboxes",
            out.iter()
                .find(|s| s.kind == "mail store")
                .map(|s| s.path.display().to_string())
                .unwrap_or_default(),
            out.iter()
                .find(|s| s.kind == "mail store")
                .map_or(0, |s| s.mailboxes.len())
        );
    }
    if out.len() > u16::MAX as usize {
        bail!("too many sources");
    }
    Ok(out)
}

fn plan(
    sources: &[PathBuf],
    dest: &Path,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(&str, u64, u64),
) -> Result<Merge> {
    let sources = load_sources(sources, dest)?;
    let mut cat = Catalog {
        as_of: vec![None; sources.len()],
        messages: vec![0; sources.len()],
        has_folder: vec![BTreeSet::new(); sources.len()],
        ..Catalog::default()
    };
    let (mut n, mut b, mut last) = (0u64, 0u64, Instant::now());
    for (si, s) in sources.iter().enumerate() {
        for mb in &s.mailboxes {
            for f in &mb.folders {
                if stop.load(Ordering::Relaxed) {
                    bail!("stopped");
                }
                let folder = cat.folder_id(&f.name);
                cat.has_folder[si].insert((mb.key.clone(), f.name.clone()));
                let what = s.path.display().to_string();
                let mut tick = |dn: u64, db: u64| {
                    n += dn;
                    b += db;
                    if last.elapsed().as_millis() >= 250 {
                        last = Instant::now();
                        progress(&what, n, b);
                    }
                };
                match &f.loc {
                    Loc::Maildir(d) => read_maildir_folder(
                        &mut cat, si as u16, &mb.key, folder, d, stop, &mut tick,
                    )?,
                    Loc::Mbox(p) => {
                        read_mbox_folder(&mut cat, si as u16, &mb.key, folder, p, stop, &mut tick)?
                    }
                }
            }
        }
    }
    // snapshots by their newest message (ties: the order given)
    let mut order: Vec<usize> = (0..sources.len()).collect();
    order.sort_by_key(|&i| (cat.as_of[i].unwrap_or(i64::MIN), i));
    let mut rank = vec![0; sources.len()];
    for (r, &i) in order.iter().enumerate() {
        rank[i] = r;
    }
    let mut report = Report {
        dest: dest.display().to_string(),
        unreadable: cat.unreadable,
        problems: cat.problems.clone(),
        ..Report::default()
    };
    let mut only_here = vec![0u64; sources.len()];
    let mut placements = Vec::new();
    let mut per_folder: BTreeMap<(String, String), u64> = BTreeMap::new();
    let mut mailboxes: BTreeSet<&str> = BTreeSet::new();
    for ((key, id), occs) in &cat.ids {
        mailboxes.insert(key);
        report.distinct += 1;
        if cat.without_id.contains(&(key.clone(), *id)) {
            report.without_id += 1;
        }
        let copies: Vec<&Occ> = occs.iter().map(|&i| &cat.occs[i as usize]).collect();
        let srcs: BTreeSet<u16> = copies.iter().map(|o| o.src).collect();
        if srcs.len() == 1 {
            only_here[*srcs.iter().next().expect("one") as usize] += 1;
        }
        let newest = copies
            .iter()
            .map(|o| rank[o.src as usize])
            .max()
            .expect("at least one copy");
        let folders: BTreeSet<u32> = copies
            .iter()
            .filter(|o| rank[o.src as usize] == newest)
            .map(|o| o.folder)
            .collect();
        if copies.iter().any(|o| !folders.contains(&o.folder)) {
            report.moved += 1;
        }
        let union = copies.iter().fold(0u8, |f, o| f | (o.flags & UNION));
        for &f in &folders {
            // the newest snapshot's copy in this folder (the first if it has two)
            let chosen = occs
                .iter()
                .enumerate()
                .filter(|(_, i)| cat.occs[**i as usize].folder == f)
                .max_by_key(|(n, i)| {
                    (
                        rank[cat.occs[**i as usize].src as usize],
                        std::cmp::Reverse(*n),
                    )
                })
                .map(|(_, i)| *i)
                .expect("a copy in this folder");
            let c = &cat.occs[chosen as usize];
            if copies.iter().any(|o| o.folder == f && o.sha != c.sha) {
                report.differing_copies += 1;
            }
            let flags = union | (c.flags & (DRAFT | TRASHED));
            if flags != c.flags {
                report.flags_merged += 1;
            }
            report.messages += 1;
            report.bytes += c.size;
            *per_folder
                .entry((key.clone(), cat.folders[f as usize].clone()))
                .or_default() += 1;
            placements.push(Placement {
                key: key.clone(),
                folder: f,
                occ: chosen,
                flags,
            });
        }
    }
    report.mailboxes = mailboxes.len() as u64;
    report.duplicates = (cat.occs.len() as u64).saturating_sub(report.messages);
    report.folders = per_folder
        .into_iter()
        .map(|((k, f), n)| (if k.is_empty() { f } else { format!("{k}: {f}") }, n))
        .collect();
    for &i in &order {
        let s = &sources[i];
        let others: BTreeSet<&(String, String)> = cat
            .has_folder
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .flat_map(|(_, f)| f.iter())
            .collect();
        report.sources.push(SourceReport {
            path: s.path.display().to_string(),
            kind: s.kind.to_string(),
            as_of: cat.as_of[i],
            messages: cat.messages[i],
            only_here: only_here[i],
            folders: cat.has_folder[i].len() as u64,
            folders_only_here: cat.has_folder[i]
                .iter()
                .filter(|f| !others.contains(f))
                .map(|(k, f)| {
                    if k.is_empty() {
                        f.clone()
                    } else {
                        format!("{k}: {f}")
                    }
                })
                .collect(),
        });
    }
    // written in a stable order: mailbox, folder, delivery time
    placements.sort_by(|a, b| {
        (
            &a.key,
            &cat.folders[a.folder as usize],
            cat.occs[a.occ as usize].time,
        )
            .cmp(&(
                &b.key,
                &cat.folders[b.folder as usize],
                cat.occs[b.occ as usize].time,
            ))
    });
    Ok(Merge {
        sources,
        cat,
        placements,
        report,
    })
}

// ---------- writing

/// The folder's directory in the merged Maildir: the mailbox itself for INBOX, ".Name" otherwise.
fn folder_dir(dest: &Path, key: &str, folder: &str) -> PathBuf {
    let mb = if key.is_empty() {
        dest.to_path_buf()
    } else {
        key.split('/').fold(dest.to_path_buf(), |p, c| p.join(c))
    };
    if folder == "INBOX" {
        mb
    } else {
        mb.join(format!(".{folder}"))
    }
}

/// A message's file name: its delivery time and content hash (so running again finds it), and
/// its flags.
fn message_name(occ: &Occ, flags: u8) -> String {
    format!(
        "{}.M{}.backdown{INFO}{}",
        occ.time.max(0),
        &fmt::hex(&occ.sha)[..20],
        letters(flags)
    )
}

fn make_maildir(dir: &Path, sub: bool) -> io::Result<()> {
    for d in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.join(d))?;
    }
    if sub && !dir.join("maildirfolder").exists() {
        File::create(dir.join("maildirfolder"))?;
    }
    Ok(())
}

fn content_of(cat: &Catalog, occ: &Occ) -> io::Result<Vec<u8>> {
    match &occ.at {
        At::File(p) => fs::read(p),
        At::Mbox { file, start, len } => mbox_content(&cat.mboxes[*file as usize], *start, *len),
    }
}

fn source_of(cat: &Catalog, occ: &Occ) -> (Vec<u8>, Option<i64>) {
    match &occ.at {
        At::File(p) => (paths::to_bytes(p), None),
        At::Mbox { file, start, .. } => (
            paths::to_bytes(&cat.mboxes[*file as usize]),
            Some(*start as i64),
        ),
    }
}

/// Where the merged mailbox may go: new, empty, or holding only what this merge wrote.
fn check_dest(db: Option<&Db>, id: Option<i64>, dest: &Path) -> Result<()> {
    if !dest.is_absolute() {
        bail!("the merged mailbox must be a full path");
    }
    if !dest.exists() {
        return Ok(());
    }
    if !dest.is_dir() {
        bail!("{} exists and isn't a folder", dest.display());
    }
    let mut foreign = 0u64;
    let mut example = None;
    let mut q = match (db, id) {
        (Some(db), Some(_)) => Some(
            db.conn
                .prepare("SELECT 1 FROM mail_merge_log WHERE merge_id = ?1 AND dest = ?2")?,
        ),
        _ => None,
    };
    for e in walkdir::WalkDir::new(dest)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !e.file_type().is_file() {
            continue;
        }
        let in_mail = e
            .path()
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|n| n == "cur" || n == "new");
        if !in_mail {
            continue;
        }
        let ours = match (&mut q, id) {
            (Some(q), Some(id)) => q
                .query_row(params![id, paths::to_bytes(e.path())], |_| Ok(()))
                .optional()?
                .is_some(),
            _ => false,
        } || file_name(e.path()).contains(".backdown");
        if !ours {
            foreign += 1;
            example.get_or_insert_with(|| e.path().display().to_string());
        }
    }
    if foreign > 0 {
        bail!(
            "{} already holds {} messages that aren't from this merge (e.g. {}): merge into a new folder",
            dest.display(),
            fmt::count(foreign),
            example.unwrap_or_default()
        );
    }
    Ok(())
}

/// Write one message: tmp/, flushed, dated, renamed into cur/, read back. `Ok(false)` if it was
/// there already (an earlier run), verified.
fn write_message(
    dir: &Path,
    name: &str,
    content: &[u8],
    sha: &[u8; 32],
    time: i64,
) -> Result<bool> {
    let dest = dir.join("cur").join(name);
    if dest.exists() {
        let back = crate::apply::hash_file(&dest)?;
        if &back == sha {
            return Ok(false);
        }
        bail!("{} exists and differs: left as it is", dest.display());
    }
    let base = name.split(INFO).next().unwrap_or(name);
    let tmp = dir.join("tmp").join(base);
    let result = (|| -> Result<()> {
        let mut out = File::create(&tmp)?;
        out.write_all(content)?;
        out.sync_all()?;
        if time > 0 {
            let t = UNIX_EPOCH + std::time::Duration::from_secs(time as u64);
            out.set_times(fs::FileTimes::new().set_modified(t).set_accessed(t))?;
        }
        fastcopy::forget_cached(&out);
        drop(out);
        if dest.exists() {
            bail!("{} appeared while writing", dest.display());
        }
        fs::rename(&tmp, &dest)?;
        let back = crate::apply::hash_file(&dest)?;
        if &back != sha {
            let _ = fs::remove_file(&dest); // our own bad copy
            bail!(
                "{} read back differently: removed; try again (failing disk?)",
                dest.display()
            );
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map(|_| true)
}

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(f) = File::open(dir) {
        let _ = f.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

fn execute(
    m: &Merge,
    dest: &Path,
    journal: Option<(&Db, i64)>,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(&str, u64, u64),
) -> Result<Report> {
    let mut r = m.report.clone();
    let mut made: BTreeSet<PathBuf> = BTreeSet::new();
    let mut log = match journal {
        Some((db, _)) => Some(db.conn.prepare_cached(
            "INSERT INTO mail_merge_log(merge_id, dest, folder, source, offset, size, sha256, flags, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?),
        None => None,
    };
    let mut logged = match journal {
        Some((db, _)) => Some(
            db.conn
                .prepare_cached("SELECT 1 FROM mail_merge_log WHERE merge_id = ?1 AND dest = ?2")?,
        ),
        None => None,
    };
    let (mut n, mut last) = (0u64, Instant::now());
    for p in &m.placements {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let occ = &m.cat.occs[p.occ as usize];
        let folder = &m.cat.folders[p.folder as usize];
        let dir = folder_dir(dest, &p.key, folder);
        if !made.contains(&dir) {
            make_maildir(&dir, folder != "INBOX")
                .with_context(|| format!("can't create {}", dir.display()))?;
            made.insert(dir.clone());
        }
        n += 1;
        if last.elapsed().as_millis() >= 250 {
            last = Instant::now();
            progress(folder, n, r.written_bytes);
        }
        let name = message_name(occ, p.flags);
        let content = match content_of(&m.cat, occ) {
            Ok(c) if Sha256::digest(&c)[..] == occ.sha[..] => c,
            Ok(_) => {
                r.failed += 1;
                push(
                    &mut r.problems,
                    format!(
                        "a message in {} changed since it was read: skipped",
                        m.sources[occ.src as usize].path.display()
                    ),
                );
                continue;
            }
            Err(e) => {
                r.failed += 1;
                push(&mut r.problems, format!("can't read a message again: {e}"));
                continue;
            }
        };
        let path = dir.join("cur").join(&name);
        match write_message(&dir, &name, &content, &occ.sha, occ.time) {
            Ok(fresh) => {
                if fresh {
                    r.written += 1;
                    r.written_bytes += occ.size;
                } else {
                    r.resumed += 1;
                }
                if let (Some((_, id)), Some(log), Some(logged)) = (journal, &mut log, &mut logged) {
                    let key = paths::to_bytes(&path);
                    if logged
                        .query_row(params![id, key], |_| Ok(()))
                        .optional()?
                        .is_none()
                    {
                        let (src, offset) = source_of(&m.cat, occ);
                        log.execute(params![
                            id,
                            key,
                            if p.key.is_empty() {
                                folder.clone()
                            } else {
                                format!("{}: {folder}", p.key)
                            },
                            src,
                            offset,
                            occ.size as i64,
                            occ.sha.to_vec(),
                            letters(p.flags),
                            now()
                        ])?;
                    }
                }
            }
            Err(e) => {
                r.failed += 1;
                push(&mut r.problems, format!("{e:#}"));
            }
        }
    }
    for d in &made {
        sync_dir(&d.join("cur"));
    }
    progress("", n, r.written_bytes);
    Ok(r)
}

// ---------- the public face

/// What merging `sources` into `dest` would do. Reads every message of every source; writes
/// nothing, anywhere.
pub fn dry_run(
    sources: &[PathBuf],
    dest: &Path,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(&str, u64, u64),
) -> Result<Report> {
    let dest = resolve_dest(dest)?;
    check_dest(None, None, &dest)?;
    let m = plan(sources, &dest, stop, progress)?;
    let mut r = m.report;
    r.dry_run = true;
    Ok(r)
}

/// Record a merge before running it (the daemon runs it as a job).
pub fn record(
    db: &Db,
    sources: &[PathBuf],
    dest: &Path,
    plan_item: Option<(i64, i64)>,
) -> Result<i64> {
    let dest = resolve_dest(dest)?;
    db.conn.execute(
        "INSERT INTO mail_merges(created_at, kind, sources, dest, plan_id, item, state) VALUES (?1, 'merge', ?2, ?3, ?4, ?5, 'new')",
        params![
            now(),
            source_list(sources)?,
            dest.display().to_string(),
            plan_item.map(|p| p.0),
            plan_item.map(|p| p.1)
        ],
    )?;
    Ok(db.conn.last_insert_rowid())
}

fn source_list(sources: &[PathBuf]) -> Result<String> {
    let list: Vec<String> = sources
        .iter()
        .map(|s| {
            dunce::canonicalize(s)
                .unwrap_or_else(|_| s.clone())
                .display()
                .to_string()
        })
        .collect();
    Ok(serde_json::to_string(&list)?)
}

/// An unfinished merge of the same sources into the same place, to carry on with.
pub fn unfinished(db: &Db, sources: &[PathBuf], dest: &Path) -> Result<Option<i64>> {
    let dest = resolve_dest(dest)?;
    Ok(db
        .conn
        .query_row(
            "SELECT id FROM mail_merges WHERE kind = 'merge' AND sources = ?1 AND dest = ?2 AND state != 'done' ORDER BY id DESC LIMIT 1",
            params![source_list(sources)?, dest.display().to_string()],
            |r| r.get(0),
        )
        .optional()?)
}

/// The merge a filing plan's item made, if it made one.
pub fn for_plan_item(db: &Db, plan: i64, item: i64) -> Result<Option<i64>> {
    Ok(db
        .conn
        .query_row(
            "SELECT id FROM mail_merges WHERE plan_id = ?1 AND item = ?2 ORDER BY id DESC LIMIT 1",
            params![plan, item],
            |r| r.get(0),
        )
        .optional()?)
}

/// A recorded merge: (sources, dest, state).
pub fn load(db: &Db, id: i64) -> Result<(Vec<PathBuf>, PathBuf, String)> {
    let (sources, dest, state): (String, String, String) = db
        .conn
        .query_row(
            "SELECT sources, dest, state FROM mail_merges WHERE id = ?1 AND kind = 'merge'",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .with_context(|| format!("there's no mail merge {id}"))?;
    let sources: Vec<String> = serde_json::from_str(&sources)?;
    Ok((
        sources.into_iter().map(PathBuf::from).collect(),
        PathBuf::from(dest),
        state,
    ))
}

/// Merges and exports on record, newest first.
pub fn history(db: &Db) -> Result<Vec<serde_json::Value>> {
    let mut stmt = db.conn.prepare(
        "SELECT id, created_at, kind, sources, dest, state, summary, plan_id FROM mail_merges ORDER BY id DESC LIMIT 30",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(serde_json::json!({
            "id": r.get::<_, i64>(0)?, "created_at": r.get::<_, i64>(1)?, "kind": r.get::<_, String>(2)?,
            "sources": serde_json::from_str::<serde_json::Value>(&r.get::<_, String>(3)?).unwrap_or_default(),
            "dest": r.get::<_, String>(4)?, "state": r.get::<_, String>(5)?, "summary": r.get::<_, Option<String>>(6)?,
            "plan": r.get::<_, Option<i64>>(7)?,
        }))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The report a finished merge stored.
pub fn report_of(db: &Db, id: i64) -> Result<Option<Report>> {
    let r: Option<Option<String>> = db
        .conn
        .query_row(
            "SELECT report FROM mail_merges WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match r.flatten() {
        Some(s) => Some(serde_json::from_str(&s)?),
        None => None,
    })
}

/// One line about a merge.
pub fn summary(r: &Report) -> String {
    let mut s = format!(
        "{} {} distinct messages{} ({}) from {} snapshots; {} duplicate copies left out",
        if r.dry_run { "would merge" } else { "merged" },
        fmt::count(r.distinct),
        if r.mailboxes > 1 {
            format!(" in {} mailboxes", fmt::count(r.mailboxes))
        } else {
            String::new()
        },
        fmt::bytes(r.bytes),
        r.sources.len(),
        fmt::count(r.duplicates)
    );
    if !r.dry_run {
        s += &format!(
            "; {} written and verified{}",
            fmt::count(r.written),
            if r.resumed > 0 {
                format!(", {} already there", fmt::count(r.resumed))
            } else {
                String::new()
            }
        );
    }
    if r.failed + r.unreadable > 0 {
        s += &format!(
            "; {} failed or unreadable",
            fmt::count(r.failed + r.unreadable)
        );
    }
    s
}

/// Carry out merge `id` (made by `record`): read every source, write the merged Maildir, journal
/// each message. Running it again carries on where it stopped.
pub fn run(
    db: &Db,
    id: i64,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(&str, u64, u64),
) -> Result<Report> {
    let r = (|| -> Result<Report> {
        let (sources, dest, _) = load(db, id)?;
        check_dest(Some(db), Some(id), &dest)?;
        db.conn.execute(
            "UPDATE mail_merges SET state = 'running' WHERE id = ?1",
            params![id],
        )?;
        let m = plan(&sources, &dest, stop, progress)?;
        fs::create_dir_all(&dest).with_context(|| format!("can't create {}", dest.display()))?;
        execute(&m, &dest, Some((db, id)), stop, progress)
    })();
    match &r {
        Ok(rep) => {
            let state = if stop.load(Ordering::Relaxed) {
                "stopped"
            } else if rep.failed + rep.unreadable > 0 {
                "partial"
            } else {
                "done"
            };
            db.conn.execute(
                "UPDATE mail_merges SET state = ?2, summary = ?3, report = ?4 WHERE id = ?1",
                params![id, state, summary(rep), serde_json::to_string(rep)?],
            )?;
        }
        Err(e) => {
            let _ = db.conn.execute(
                "UPDATE mail_merges SET state = ?2, summary = ?3 WHERE id = ?1",
                params![
                    id,
                    if stop.load(Ordering::Relaxed) {
                        "stopped"
                    } else {
                        "failed"
                    },
                    format!("{e:#}")
                ],
            );
        }
    }
    r
}

// ---------- mbox export

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExportReport {
    pub maildir: String,
    pub mbox: String,
    pub messages: u64,
    pub bytes: u64,
    /// messages that didn't end in a line break, given one (mbox can't store them otherwise)
    pub newline_added: u64,
    /// ">From " quoting added (lines in bodies that start with "From ")
    pub quoted_lines: u64,
}

/// The envelope sender for a "From " line: Return-Path, else the From address, else
/// MAILER-DAEMON (as mail programs write it).
fn envelope_sender(h: &Headers) -> String {
    let pick = |v: &str| -> Option<String> {
        let a = match (v.find('<'), v.rfind('>')) {
            (Some(i), Some(j)) if j > i + 1 => &v[i + 1..j],
            _ => v.split_whitespace().find(|w| w.contains('@'))?,
        };
        let a = a.trim_matches(|c: char| c == '"' || c == '<' || c == '>');
        (!a.is_empty() && !a.contains(char::is_whitespace)).then(|| a.to_string())
    };
    h.get("return-path")
        .and_then(pick)
        .or_else(|| h.get("from").and_then(pick))
        .unwrap_or_else(|| "MAILER-DAEMON".into())
}

/// Export one Maildir folder (its cur/ and new/: not its subfolders) as one mboxrd file, oldest
/// message first. Written beside `out` under a temporary name, flushed, read back and compared
/// message by message, then renamed into place. Never overwrites a file.
pub fn export(
    db: Option<&Db>,
    maildir: &Path,
    out: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<ExportReport> {
    if !is_maildir(maildir) {
        bail!(
            "{} isn't a Maildir folder (no cur/ or new/)",
            maildir.display()
        );
    }
    if out.exists() {
        bail!("{} exists: pick a new file name", out.display());
    }
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if parent.starts_with(maildir) {
        bail!("write the mbox outside the Maildir");
    }
    fs::create_dir_all(parent)?;
    let mut msgs: Vec<(i64, PathBuf)> = Vec::new();
    for sub in ["cur", "new"] {
        let Ok(rd) = fs::read_dir(maildir.join(sub)) else {
            continue;
        };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            let n = file_name(&p);
            if n.starts_with('.') || !e.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            let t = delivered(&n)
                .or_else(|| {
                    e.metadata()
                        .ok()?
                        .modified()
                        .ok()?
                        .duration_since(UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_secs() as i64)
                })
                .unwrap_or(0);
            msgs.push((t, p));
        }
    }
    msgs.sort();
    let tmp = parent.join(format!(".{}.backdown-partial", file_name(out)));
    let mut rep = ExportReport {
        maildir: maildir.display().to_string(),
        mbox: out.display().to_string(),
        ..ExportReport::default()
    };
    // what each message should read back as
    let mut expect: Vec<[u8; 32]> = Vec::with_capacity(msgs.len());
    let result = (|| -> Result<()> {
        let mut w = io::BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
        let mut buf = Vec::new();
        for (t, p) in &msgs {
            let mut content = fs::read(p).with_context(|| format!("can't read {}", p.display()))?;
            if !content.ends_with(b"\n") {
                content.push(b'\n');
                rep.newline_added += 1;
            }
            let h = parse_headers(&content[..body_start(&content)]);
            let when = if *t > 0 {
                *t
            } else {
                h.get("date").and_then(rfc2822).unwrap_or(0)
            };
            buf.clear();
            buf.extend_from_slice(
                format!("From {} {}\n", envelope_sender(&h), asctime(when)).as_bytes(),
            );
            let before = buf.len();
            quote(&content, &mut buf);
            rep.quoted_lines += (buf.len() - before - content.len()) as u64;
            buf.push(b'\n');
            w.write_all(&buf)?;
            expect.push(Sha256::digest(&content).into());
            rep.messages += 1;
            rep.bytes += content.len() as u64;
            progress(rep.messages, rep.bytes);
        }
        let f = w.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()?;
        fastcopy::forget_cached(&f);
        drop(f);
        // read it back as any mbox reader would
        let mut r = MboxReader::new(BufReader::new(File::open(&tmp)?));
        let mut i = 0usize;
        while let Some(m) = r.next_msg()? {
            let sum: [u8; 32] = Sha256::digest(&m.content).into();
            if expect.get(i) != Some(&sum) {
                bail!(
                    "message {} of the mbox read back differently: the export was removed",
                    i + 1
                );
            }
            i += 1;
        }
        if i != expect.len() {
            bail!(
                "the mbox read back {} messages instead of {}: the export was removed",
                i,
                expect.len()
            );
        }
        if out.exists() {
            bail!("{} appeared while exporting", out.display());
        }
        fs::rename(&tmp, out)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    if let Some(db) = db {
        db.conn.execute(
            "INSERT INTO mail_merges(created_at, kind, sources, dest, state, summary, report) VALUES (?1, 'export', ?2, ?3, 'done', ?4, ?5)",
            params![
                now(),
                serde_json::to_string(&[maildir.display().to_string()])?,
                out.display().to_string(),
                format!(
                    "exported {} messages ({}) as mboxrd",
                    fmt::count(rep.messages),
                    fmt::bytes(rep.bytes)
                ),
                serde_json::to_string(&rep)?
            ],
        )?;
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_both_ways() {
        assert_eq!(
            maildir_flags("1600000000.M1P2.host:2,FRS"),
            SEEN | REPLIED | FLAGGED
        );
        assert_eq!(maildir_flags("1600000000.M1P2.host!2,Sab"), SEEN);
        assert_eq!(maildir_flags("1600000000.M1P2.host"), 0);
        assert_eq!(letters(SEEN | REPLIED | FLAGGED | DRAFT), "DFRS");
        let h = parse_headers(b"Status: RO\nX-Status: AF\nX-Mozilla-Status: 0008\n");
        assert_eq!(mbox_flags(&h), SEEN | REPLIED | FLAGGED | TRASHED);
    }

    #[test]
    fn headers_unfold_and_ids_normalise() {
        let h = parse_headers(
            b"Subject: a long\n\tsubject\r\nMessage-ID:\n <abc@example.org>\nX-Empty:\n\nbody: not a header\n",
        );
        assert_eq!(h.get("subject"), Some("a long subject"));
        assert_eq!(message_id(&h).as_deref(), Some("abc@example.org"));
        assert!(h.get("body").is_none());
        let msg = b"Message-ID: <x@example.org>\n\nHello\r\n>From here\n\n";
        let same = b"Message-ID: <x@example.org>\nStatus: RO\n\nHello\nFrom here\n";
        let (a, _) = identity(msg, &parse_headers(msg));
        let (b, _) = identity(same, &parse_headers(same));
        assert_eq!(
            a, b,
            "headers a program adds and mbox quoting don't make another message"
        );
        let other = b"Message-ID: <x@example.org>\n\nHello again\n";
        assert_ne!(identity(other, &parse_headers(other)).0, a);
        // without a Message-ID: every header but the volatile ones, and the body
        let id = |m: &[u8]| identity(m, &parse_headers(m));
        let a = id(b"From: a@example.org\nSubject: x\n\nsame\n");
        assert!(!a.1);
        assert_eq!(
            a,
            id(b"From: a@example.org\nStatus: RO\nSubject: x\n\nsame\n")
        );
        assert_ne!(a, id(b"From: b@example.org\nSubject: x\n\nsame\n"));
        // not a message at all (no blank line after headers): all of it counts, so two different
        // ones never pass for one
        assert_ne!(
            id(b"\x01\x02 binary\nno blank line"),
            id(b"\x01\x03 binary\nno blank line")
        );
    }

    #[test]
    fn dates() {
        assert_eq!(
            rfc2822("Tue, 1 Jul 2003 10:52:37 +0200"),
            Some(1_057_049_557)
        );
        assert_eq!(
            rfc2822("1 Jul 2003 08:52:37 GMT (UTC)"),
            Some(1_057_049_557)
        );
        assert_eq!(rfc2822("Tue, 01 Jul 03 04:52:37 EDT"), Some(1_057_049_557));
        assert_eq!(rfc2822("garbage"), None);
        assert_eq!(
            envelope_date(b"From ann@example.org Tue Jul  1 08:52:37 2003\n"),
            Some(1_057_049_557)
        );
        assert_eq!(asctime(1_057_049_557), "Tue Jul  1 08:52:37 2003");
        assert_eq!(asctime(0), "Thu Jan  1 00:00:00 1970");
    }

    #[test]
    fn mboxrd_quoting_round_trips() {
        let msg = b"Subject: x\n\nFrom the start\n>From quoted\n>>From deeper\nFromage\n";
        let mut q = Vec::new();
        quote(msg, &mut q);
        assert_eq!(
            q,
            b"Subject: x\n\n>From the start\n>>From quoted\n>>>From deeper\nFromage\n"
        );
        assert_eq!(unquote(&q), msg);
    }

    #[test]
    fn an_mbox_reads_message_by_message() {
        let mbox = b"From a@example.org Mon Jan  1 12:00:00 2024\nSubject: one\n\nbody\n>From x\n\nFrom b@example.org Tue Jan  2 12:00:00 2024\nSubject: two\n\nlast\n";
        let mut r = MboxReader::new(&mbox[..]);
        let one = r.next_msg().unwrap().unwrap();
        assert_eq!(one.content, b"Subject: one\n\nbody\nFrom x\n");
        assert_eq!(
            &mbox[one.start as usize..(one.start + one.len) as usize],
            b"Subject: one\n\nbody\n>From x\n"
        );
        let two = r.next_msg().unwrap().unwrap();
        assert_eq!(two.content, b"Subject: two\n\nlast\n");
        assert!(r.next_msg().unwrap().is_none());
        let mut bad = MboxReader::new(&b"Subject: not an mbox\n"[..]);
        assert!(bad.next_msg().is_err());
    }
}
