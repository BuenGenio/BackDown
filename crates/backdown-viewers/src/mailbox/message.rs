//! One message as the list shows it: its flags (Maildir's file-name info, or an mbox's Status /
//! X-Mozilla-Status headers) and its headers.

use std::hash::{Hash, Hasher};

use crate::peek;

use super::*;

// ---- flags: Maildir's file-name info, or an mbox's Status / X-Mozilla-Status headers

// the same bits as mail merge's (backdown_core::mailmerge), which reads the same file names
pub use backdown_core::mailmerge::{DRAFT, FLAGGED, PASSED, REPLIED, SEEN, TRASHED};
/// The mailbox says whether this message was read (without it, nothing is shown as unread).
pub const KNOWN: u8 = 128;

/// A Maildir message's flags from its file name: "…:2,FRS" (or "!2," / ";2," where ':' can't be
/// in a file name). Lower-case letters are Dovecot keywords and are left out. A Maildir always
/// knows whether a message was read, so a name without flags (in new/) is unread.
pub fn maildir_flags(name: &str) -> u8 {
    KNOWN | backdown_core::mailmerge::maildir_flags(name)
}

/// When a Maildir message was delivered: the Unix time its unique name starts with.
pub fn delivered(name: &str) -> Option<i64> {
    let digits = name.split('.').next()?;
    if (9..=11).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()) {
        digits.parse().ok()
    } else {
        None
    }
}

pub(super) fn unread(f: u8) -> bool {
    f & KNOWN != 0 && f & SEEN == 0
}

/// mutt's Status / X-Status and Thunderbird's X-Mozilla-Status, when an mbox has them.
pub(super) fn status_flags(h: &mail_parser::Message) -> u8 {
    let mut f = 0;
    if let Some(v) = h
        .header_raw("X-Mozilla-Status")
        .and_then(|v| u32::from_str_radix(v.trim(), 16).ok())
    {
        f |= KNOWN;
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
    if let Some(v) = h.header_raw("Status") {
        f |= KNOWN;
        if v.contains('R') {
            f |= SEEN;
        }
    }
    if let Some(v) = h.header_raw("X-Status") {
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

// ---- one message's headers

/// What threading and the lists need of one message.
#[derive(Debug, Clone, Default)]
pub struct Head {
    /// Where it is: an mbox message's byte offset, or a Maildir message's index id.
    pub key: i64,
    pub size: u64,
    /// Message-ID, hashed: a mailbox of 100k messages keeps no id strings.
    pub id: Option<u64>,
    /// What it replies to, oldest first (References, then In-Reply-To); the last is its parent.
    pub refs: Vec<u64>,
    pub subject: String,
    pub from: String,
    pub date: Option<i64>,
    pub flags: u8,
}

pub(super) fn hash_id(id: &str) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    id.trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .hash(&mut h);
    h.finish()
}

pub(super) fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() > max {
        format!("{}…", &s[..s.floor_char_boundary(max)])
    } else {
        s.to_string()
    }
}

/// A message's headers, parsed; `date` stands in for a missing or broken Date header.
pub(super) fn head_of(raw: &[u8], key: i64, size: u64, date: Option<i64>, flags: u8) -> Head {
    let Some(h) = mail_parser::MessageParser::default().parse_headers(raw) else {
        return Head {
            key,
            size,
            date,
            flags,
            ..Head::default()
        };
    };
    let id = h
        .message_id()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(hash_id);
    let mut refs: Vec<u64> = h
        .references()
        .as_text_list()
        .unwrap_or_default()
        .iter()
        .map(|r| hash_id(r))
        .collect();
    if let Some(p) = h
        .in_reply_to()
        .as_text_list()
        .and_then(|l| l.first())
        .map(|r| hash_id(r))
        && !refs.contains(&p)
    {
        refs.push(p);
    }
    refs.retain(|&r| Some(r) != id);
    refs.dedup();
    if refs.len() > MAX_REFS {
        refs.drain(..refs.len() - MAX_REFS);
    }
    Head {
        key,
        size,
        id,
        refs,
        subject: clip(h.subject().unwrap_or_default(), SUBJECT_MAX),
        from: clip(&peek::addrs(h.from()), FROM_MAX),
        date: h
            .date()
            .filter(|d| d.is_valid())
            .map(|d| d.to_timestamp())
            .or(date),
        flags: flags | status_flags(&h),
    }
}

/// Reply and forward prefixes, lower-case, as mail clients in many languages write them.
pub(super) const PREFIXES: &[&str] = &[
    "re",
    "res",
    "sv",
    "antw",
    "ref",
    "aw",
    "απ",
    "השב",
    "vá",
    "r",
    "rif",
    "bls",
    "odp",
    "ynt",
    "atb",
    "رد",
    "回复",
    "答复",
    "fwd",
    "fw",
    "rv",
    "enc",
    "vs",
    "doorst",
    "vl",
    "tr",
    "wg",
    "πρθ",
    "הועבר",
    "továbbítás",
    "i",
    "fs",
    "trs",
    "vb",
    "pd",
    "i̇lt",
    "yml",
    "回覆",
    "轉寄",
    "转发",
];

/// A subject without its leading reply and forward prefixes ("Re:", "AW:", "SV:", "Fwd:",
/// "Re[2]:", "RE :", …) and, with `tags`, mailing-list tags ("[dev] Re: …"); and whether it had
/// a prefix.
pub fn strip_subject(s: &str, tags: bool) -> (&str, bool) {
    let mut rest = s.trim();
    let mut reply = false;
    loop {
        if tags
            && let Some(r) = rest.strip_prefix('[')
            && let Some(end) = r.find(']')
            && end <= 60
            && !r[..end].contains('[')
            && !r[end + 1..].trim().is_empty()
        {
            rest = r[end + 1..].trim_start();
            continue;
        }
        let word = rest
            .find(|c: char| !c.is_alphabetic())
            .map_or(rest, |i| &rest[..i]);
        if word.is_empty() || word.len() > 24 || !PREFIXES.contains(&word.to_lowercase().as_str()) {
            break;
        }
        let mut after = &rest[word.len()..];
        // a count: "Re[2]:", "Re(2):", "Re^2:"
        for (open, close) in [("[", "]"), ("(", ")")] {
            if let Some((n, tail)) = after.strip_prefix(open).and_then(|x| x.split_once(close))
                && !n.is_empty()
                && n.bytes().all(|b| b.is_ascii_digit())
            {
                after = tail;
            }
        }
        if let Some(tail) = after.strip_prefix('^') {
            after = tail.trim_start_matches(|c: char| c.is_ascii_digit());
        }
        let after = after.trim_start();
        let Some(tail) = after.strip_prefix(':').or_else(|| after.strip_prefix('：')) else {
            break;
        };
        rest = tail.trim_start();
        reply = true;
    }
    (rest, reply)
}

/// What subjects are compared by: no prefixes or tags, spaces collapsed, lower-case.
pub fn subject_key(s: &str) -> String {
    strip_subject(s, true)
        .0
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
