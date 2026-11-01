//! Reading a mailbox: an mbox's headers (bodies skipped), or the files the index lists in a
//! Maildir's cur/ and new/.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use anyhow::Result;
use backdown_core::{Db, browse, paths};
use rusqlite::params;

use crate::peek;

use super::*;

// ---- reading an mbox: headers only, bodies skipped

/// How a step of reading stopped.
pub(super) enum Stop {
    Done,
    Budget,
    Full,
}

pub(super) struct Pending {
    pub(super) at: u64,
    pub(super) head: Vec<u8>,
    pub(super) in_head: bool,
    pub(super) envelope: Option<i64>,
}

/// Headers of the messages in an mbox from byte `from` (0, or where the last step stopped: always
/// a "From " line) until the end, the time budget, or `room` more messages. Returns where to carry
/// on.
pub(super) fn scan_mbox(
    path: &Path,
    from: u64,
    until: Instant,
    room: usize,
    out: &mut Vec<Head>,
) -> Result<(u64, Stop)> {
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(from))?;
    let mut r = BufReader::with_capacity(1 << 18, f);
    let (mut pos, mut prev_blank, mut lines, mut added) = (from, true, 0u64, 0usize);
    let (mut cur, mut line) = (None::<Pending>, Vec::new());
    let finish = |m: Pending, end: u64| head_of(&m.head, m.at as i64, end - m.at, m.envelope, 0);
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            if let Some(m) = cur.take() {
                out.push(finish(m, pos));
            }
            return Ok((pos, Stop::Done));
        }
        // a body line is skipped without copying it, unless it might be a "From " line
        if !cur.as_ref().is_some_and(|m| m.in_head) && buf.len() >= 5 && !buf.starts_with(b"From ")
        {
            let blank = buf[0] == b'\n' || buf.starts_with(b"\r\n");
            pos += r.skip_until(b'\n')? as u64;
            prev_blank = blank;
            lines += 1;
            if lines.is_multiple_of(65_536)
                && Instant::now() > until
                && let Some(m) = &cur
                && m.at > from
            {
                return Ok((m.at, Stop::Budget)); // deep in a long message: come back to it
            }
            continue;
        }
        let n = peek::line_capped(&mut r, &mut line, 16 << 10)? as u64;
        if peek::is_from_line(&line, prev_blank) {
            if let Some(m) = cur.take() {
                out.push(finish(m, pos));
                added += 1;
            }
            if added >= room {
                return Ok((pos, Stop::Full));
            }
            if pos > from && Instant::now() > until {
                return Ok((pos, Stop::Budget));
            }
            cur = Some(Pending {
                at: pos,
                head: Vec::new(),
                in_head: true,
                envelope: envelope_date(&line),
            });
        } else if let Some(m) = cur.as_mut().filter(|m| m.in_head) {
            if peek::blank(&line) {
                m.in_head = false;
            } else if m.head.len() + line.len() <= HEAD_CAP {
                m.head.extend_from_slice(&line);
            }
        }
        pos += n;
        prev_blank = peek::blank(&line);
        lines += 1;
    }
}

/// The date on an mbox "From " line ("From ann@example.org Mon Jan  1 12:00:00 2024"), for a
/// message without a Date header.
pub(super) fn envelope_date(line: &[u8]) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let s = std::str::from_utf8(line).ok()?;
    let w: Vec<&str> = s.split_whitespace().collect();
    let mi = (2..w.len()).find(|&i| MONTHS.contains(&w[i].to_ascii_lowercase().as_str()))?;
    let month = MONTHS.iter().position(|m| w[mi].eq_ignore_ascii_case(m))? as i64 + 1;
    let day: i64 = w.get(mi + 1)?.parse().ok()?;
    let hms: Vec<i64> = w
        .get(mi + 2)?
        .split(':')
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    let year: i64 = w[mi + 3..]
        .iter()
        .find(|x| x.len() == 4 && x.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()?;
    if !(1..=31).contains(&day) || !(2..=3).contains(&hms.len()) {
        return None;
    }
    // days from 1970-01-01 to the date (proleptic Gregorian)
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + hms[0] * 3600 + hms[1] * 60 + hms.get(2).copied().unwrap_or(0))
}

// ---- reading a Maildir: the files the index lists in cur/ and new/

pub(super) struct MailFile {
    pub(super) id: i64,
    pub(super) path: PathBuf,
    pub(super) size: u64,
    pub(super) mtime: i64,
    pub(super) flags: u8,
    pub(super) time: i64,
}

pub(super) fn current_files(db: &Db, dir: &Path) -> Result<Vec<(i64, PathBuf, u64, i64)>> {
    let mut stmt = db
        .conn
        .prepare_cached("SELECT id, path, size, mtime FROM files WHERE dir = ?1 AND current = 1")?;
    let rows = stmt.query_map(params![paths::to_bytes(dir)], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            paths::from_bytes(&r.get::<_, Vec<u8>>(1)?),
            r.get::<_, i64>(2)? as u64,
            r.get::<_, i64>(3)?,
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A Maildir's messages, newest delivery first, so a partial read shows the latest mail.
pub(super) fn maildir_files(db: &Db, dir: &Path) -> Result<Vec<MailFile>> {
    let mut out = Vec::new();
    for sub in ["cur", "new"] {
        for (id, path, size, mtime) in current_files(db, &dir.join(sub))? {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            out.push(MailFile {
                id,
                size,
                mtime,
                flags: maildir_flags(&name),
                time: delivered(&name).unwrap_or(mtime),
                path,
            });
        }
    }
    out.sort_by(|x, y| y.time.cmp(&x.time).then(y.id.cmp(&x.id)));
    Ok(out)
}

/// A Maildir message's file, as the index knows it (unchanged since), or an identical copy.
pub(super) fn open_indexed(db: &Db, f: &MailFile) -> Option<File> {
    if let Ok(file) = File::open(&f.path)
        && let Ok(m) = file.metadata()
        && m.is_file()
        && m.len() == f.size
        && m.modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .is_some_and(|d| d.as_secs() as i64 == f.mtime)
    {
        return Some(file);
    }
    let (p, _) = browse::resolve_content(db, f.id).ok()??;
    File::open(p).ok()
}

/// A message file's header block (up to `HEAD_CAP`).
pub(super) fn read_head(file: File) -> io::Result<Vec<u8>> {
    let mut r = BufReader::with_capacity(8 << 10, file.take(4 * HEAD_CAP as u64));
    let (mut head, mut line) = (Vec::new(), Vec::new());
    loop {
        let n = peek::line_capped(&mut r, &mut line, 16 << 10)?;
        if n == 0 || peek::blank(&line) || head.len() + line.len() > HEAD_CAP {
            return Ok(head);
        }
        head.extend_from_slice(&line);
    }
}
