//! One message: its flags (Maildir's file-name info, or an mbox's Status headers), its headers,
//! and its date.

use sha2::{Digest, Sha256};

use super::*;

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
pub(super) fn mbox_flags(h: &Headers) -> u8 {
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
pub(super) struct Headers(Vec<(String, String)>);

impl Headers {
    pub(super) fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Where the header block ends: the offset of the body, just after the blank line; `None` when
/// there's no blank line (then it's all headers, or not a message at all).
pub(super) fn head_end(msg: &[u8]) -> Option<usize> {
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
pub(super) fn body_start(msg: &[u8]) -> usize {
    head_end(msg).unwrap_or(msg.len())
}

pub(super) fn parse_headers(block: &[u8]) -> Headers {
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
pub(super) fn message_id(h: &Headers) -> Option<String> {
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
pub(super) fn body_key(body: &[u8]) -> [u8; 32] {
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
pub(super) const VOLATILE: &[&str] = &[
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
pub(super) fn identity(msg: &[u8], h: &Headers) -> ([u8; 32], bool) {
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

pub(super) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

pub(super) fn civil(t: i64) -> (i64, i64, i64) {
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

pub(super) const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

pub(super) fn month(w: &str) -> Option<i64> {
    let w = w.get(..3)?.to_ascii_lowercase();
    MONTHS.iter().position(|m| *m == w).map(|i| i as i64 + 1)
}

pub(super) fn clock(w: &str) -> Option<i64> {
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
pub(super) fn envelope_date(line: &[u8]) -> Option<i64> {
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
pub(super) fn asctime(t: i64) -> String {
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
pub(super) fn delivered(name: &str) -> Option<i64> {
    let digits = name.split('.').next()?;
    if (9..=11).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()) {
        digits.parse().ok()
    } else {
        None
    }
}
