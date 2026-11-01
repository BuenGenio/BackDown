//! Files whose name says nothing: are they mail?

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use anyhow::Result;

use crate::peek;

// ---- files whose name says nothing: mail?

pub(super) fn header_name(line: &[u8]) -> Option<&[u8]> {
    let i = line.iter().position(|&b| b == b':')?;
    (i > 0 && line[..i].iter().all(|b| b.is_ascii_graphic())).then(|| &line[..i])
}

/// An mbox starts with a "From " envelope line ("From ann@example.org Mon Jan  1 12:00:00 2024"),
/// then the first message's headers.
pub(super) fn looks_mbox(b: &[u8]) -> bool {
    let mut lines = b.split(|&c| c == b'\n');
    lines
        .next()
        .is_some_and(|l| l.len() < 1000 && peek::is_from_line(l, false))
        && lines.next().and_then(header_name).is_some()
}

/// A single message (an .eml without its extension, or a Maildir file) starts with a block of
/// headers, some of them ones only mail has.
pub(super) fn looks_message(b: &[u8], whole: bool) -> bool {
    const MAIL: [&str; 20] = [
        "from",
        "to",
        "cc",
        "subject",
        "date",
        "message-id",
        "received",
        "return-path",
        "delivered-to",
        "mime-version",
        "reply-to",
        "in-reply-to",
        "references",
        "x-mailer",
        "user-agent",
        "content-type",
        "sender",
        "x-original-to",
        "dkim-signature",
        "list-id",
    ];
    const TELLING: [&str; 7] = [
        "from",
        "date",
        "subject",
        "message-id",
        "received",
        "return-path",
        "delivered-to",
    ];
    // a buffer cut short ends in a partial line: leave it out
    let b = if whole {
        b
    } else {
        &b[..b.iter().rposition(|&c| c == b'\n').map_or(0, |i| i + 1)]
    };
    let (mut known, mut telling, mut lines) = (0, false, 0);
    for l in b.split(|&c| c == b'\n') {
        let l = l.strip_suffix(b"\r").unwrap_or(l);
        if l.is_empty() {
            return lines > 0 && known >= 2 && telling; // the end of the header block
        }
        if l[0] == b' ' || l[0] == b'\t' {
            if lines == 0 {
                return false;
            }
            continue;
        }
        let Some(name) = header_name(l) else {
            return false;
        };
        let name = String::from_utf8_lossy(name).to_ascii_lowercase();
        if MAIL.contains(&name.as_str()) {
            known += 1;
            telling |= TELLING.contains(&name.as_str());
        }
        lines += 1;
    }
    known >= 3 && telling // headers all the way (long Received chains): judge by what's there
}

/// What a file whose name says nothing might be, from its first 8 KiB: "mbox", "message", or
/// neither.
pub fn kind(path: &Path) -> Result<Option<&'static str>> {
    let mut buf = vec![0; 8 << 10];
    let mut f = File::open(path)?;
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let whole = n < buf.len();
    buf.truncate(n);
    Ok(if looks_mbox(&buf) {
        Some("mbox")
    } else if looks_message(&buf, whole) {
        Some("message")
    } else {
        None
    })
}
