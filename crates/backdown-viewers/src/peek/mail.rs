//! Mail: single messages (.eml) and mbox files, with their attachments.

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

// where an mbox's messages start: the same rule as mail merge's
pub(crate) use backdown_core::mbox::{blank, is_from_line};

use super::*;

// ---- mail: single messages (.eml) and mbox files

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
pub(super) fn message_bytes(path: &Path, at: Option<u64>) -> Result<(Vec<u8>, bool)> {
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

pub(super) fn attachment_name(p: &mail_parser::MessagePart, i: usize) -> String {
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

pub(super) fn message_json(raw: &[u8], truncated: bool) -> Result<Value> {
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

pub(super) struct MboxMsg {
    pub(super) at: u64,
    pub(super) head: Vec<u8>,
    pub(super) in_head: bool,
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
pub(super) fn html_text(html: &str) -> String {
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

pub(super) fn push_html_text(out: &mut String, text: &str, pre: bool) {
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

pub(super) fn decode_entities(s: &str) -> Cow<'_, str> {
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

pub(super) fn entity_char(name: &str) -> Option<char> {
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
