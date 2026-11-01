//! Mailboxes as conversations, for the browser: an mbox file or a Maildir folder, read as threads.
//!
//! The same rules as peek.rs: serve.rs hands over an index id (an mbox) or an indexed folder (a
//! Maildir); nothing here picks files by itself, writes anything or runs anything, and every
//! request reads a bounded amount. Only headers are read (at most `HEAD_CAP` bytes of each
//! message; an mbox's bodies are skipped a buffer at a time), on a time budget per request, and
//! what has been read so far stays in a small in-memory cache, so "keep reading" carries on where
//! the last request stopped. A Maildir's messages are the files the index lists in its cur/ and
//! new/, each opened by its index id (or an identical copy, like /api/content), never by a path
//! from the browser; one message is then shown by /api/mail, like an .eml.
//!
//! Threads follow Jamie Zawinski's algorithm (<https://www.jwz.org/doc/threading.html>):
//! Message-ID, References and In-Reply-To link each reply to what it answers (a message that
//! isn't here becomes an empty placeholder, links that would make a loop are refused, a duplicate
//! stays next to its original), placeholders are pruned, and what is still unlinked is joined by
//! subject: a reply ("Re:", "AW:", "SV:", "Fwd:", …) whose subject matches a conversation active
//! within the 30 days before it joins that conversation.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use backdown_core::mailmerge::letters;
use backdown_core::{Db, browse, paths};
use rusqlite::params;
use serde_json::{Value, json};

use crate::peek;

/// One request's share of reading a mailbox.
const STEP_BUDGET: Duration = Duration::from_millis(2500);
/// Opening a thread whose mailbox fell out of the cache reads it again, up to this long.
const REREAD_BUDGET: Duration = Duration::from_secs(10);
/// The most of one message's header block kept (the rest is skipped).
const HEAD_CAP: usize = 64 << 10;
/// References kept per message: its nearest ancestors, plenty to place a reply.
const MAX_REFS: usize = 20;
/// Messages threaded per mailbox, and kept in the cache across mailboxes.
const MAX_MESSAGES: usize = 250_000;
const CACHE_MESSAGES: usize = 500_000;
const CACHE_MAILBOXES: usize = 6;
/// A reply found only by its subject joins a conversation last active at most this long before.
const SUBJECT_WINDOW: i64 = 30 * 86_400;
/// How far up a chain of replies the loop check walks before refusing a link.
const MAX_WALK: usize = 5_000;
const SUBJECT_MAX: usize = 200;
const FROM_MAX: usize = 120;
/// Threads per page, and messages shown of one thread.
const PAGE_MAX: usize = 500;
const THREAD_MAX: usize = 2_000;

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

fn unread(f: u8) -> bool {
    f & KNOWN != 0 && f & SEEN == 0
}

/// mutt's Status / X-Status and Thunderbird's X-Mozilla-Status, when an mbox has them.
fn status_flags(h: &mail_parser::Message) -> u8 {
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

fn hash_id(id: &str) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    id.trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .hash(&mut h);
    h.finish()
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() > max {
        format!("{}…", &s[..s.floor_char_boundary(max)])
    } else {
        s.to_string()
    }
}

/// A message's headers, parsed; `date` stands in for a missing or broken Date header.
fn head_of(raw: &[u8], key: i64, size: u64, date: Option<i64>, flags: u8) -> Head {
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
const PREFIXES: &[&str] = &[
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

// ---- threading

/// One message's place in a thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Index into the messages threaded.
    pub msg: usize,
    /// How far it is indented: 0 for the thread's start (and siblings with no common parent here).
    pub depth: u32,
    /// Another copy of a message already in the mailbox (same Message-ID).
    pub dup: bool,
    /// It answers a message that isn't in this mailbox.
    pub orphan: bool,
}

/// A conversation: its messages in reading order (each reply after what it answers, siblings by
/// date), and when it began and was last active.
#[derive(Debug, Clone)]
pub struct Thread {
    pub items: Vec<Item>,
    pub first: Option<i64>,
    pub last: Option<i64>,
    /// The earliest-read message: its key names the thread, and stays the same as more is read.
    pub first_read: usize,
    newest: usize,
}

struct Node {
    msg: Option<usize>,
    parent: Option<usize>,
    children: Vec<usize>,
}

struct Arena {
    nodes: Vec<Node>,
}

impl Arena {
    fn add(&mut self, msg: Option<usize>) -> usize {
        self.nodes.push(Node {
            msg,
            parent: None,
            children: Vec::new(),
        });
        self.nodes.len() - 1
    }

    /// Whether `a` is `b` or one of its ancestors: linking `a` below `b` would make a loop. A
    /// chain too long to walk counts as a loop, so no link is made.
    fn above(&self, a: usize, b: usize) -> bool {
        if a == b {
            return true;
        }
        if self.nodes[a].children.is_empty() {
            return false; // a leaf is nobody's ancestor
        }
        let mut at = self.nodes[b].parent;
        for _ in 0..MAX_WALK {
            match at {
                None => return false,
                Some(p) if p == a => return true,
                Some(p) => at = self.nodes[p].parent,
            }
        }
        true
    }

    fn link(&mut self, parent: usize, child: usize) {
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
    }

    fn date(&self, msgs: &[Head], n: usize) -> Option<i64> {
        self.nodes[n].msg.and_then(|m| msgs[m].date)
    }

    /// The earliest and latest date below `top` (itself included).
    fn span(&self, msgs: &[Head], top: usize) -> (Option<i64>, Option<i64>) {
        let (mut first, mut last) = (None::<i64>, None::<i64>);
        let mut stack = vec![top];
        while let Some(n) = stack.pop() {
            if let Some(d) = self.date(msgs, n) {
                first = Some(first.map_or(d, |f| f.min(d)));
                last = Some(last.map_or(d, |l| l.max(d)));
            }
            stack.extend_from_slice(&self.nodes[n].children);
        }
        (first, last)
    }

    fn unlink(&mut self, child: usize) {
        if let Some(p) = self.nodes[child].parent.take() {
            let c = &mut self.nodes[p].children;
            if let Some(i) = c.iter().position(|&x| x == child) {
                c.remove(i);
            }
        }
    }
}

/// A thread's top, for joining by subject.
struct Top {
    node: usize,
    key: String,
    reply: bool,
    first: Option<i64>,
    last: Option<i64>,
}

/// Threads of `msgs`, newest activity first. Every message is in exactly one thread.
pub fn thread(msgs: &[Head]) -> Vec<Thread> {
    let mut a = Arena {
        nodes: Vec::with_capacity(msgs.len() + msgs.len() / 2),
    };
    let mut by_id: HashMap<u64, usize> = HashMap::with_capacity(msgs.len());
    let mut dup = vec![false; msgs.len()];
    let mut dups = Vec::new(); // (a duplicate's node, its original's node)

    // 1. a node per Message-ID; References link each reply below what it answers
    for (i, m) in msgs.iter().enumerate() {
        let me = match m.id {
            Some(id) => match by_id.get(&id).copied() {
                Some(n) if a.nodes[n].msg.is_none() => {
                    a.nodes[n].msg = Some(i);
                    n
                }
                Some(n) => {
                    dup[i] = true;
                    let d = a.add(Some(i));
                    dups.push((d, n));
                    d
                }
                None => {
                    let n = a.add(Some(i));
                    by_id.insert(id, n);
                    n
                }
            },
            None => a.add(Some(i)),
        };
        let mut prev: Option<usize> = None;
        for &r in m.refs.iter().filter(|&&r| Some(r) != m.id) {
            let n = match by_id.get(&r) {
                Some(&n) => n,
                None => {
                    let n = a.add(None);
                    by_id.insert(r, n);
                    n
                }
            };
            // what other messages said stands; only a missing link is added, never a loop
            if let Some(p) = prev
                && a.nodes[n].parent.is_none()
                && !a.above(n, p)
            {
                a.link(p, n);
            }
            prev = Some(n);
        }
        // the message itself knows best what it replies to: its last reference
        let parent = prev.filter(|&p| !a.above(me, p));
        if a.nodes[me].parent != parent {
            a.unlink(me);
            if let Some(p) = parent {
                a.link(p, me);
            }
        }
    }
    // a duplicate that doesn't say what it answers goes next to its original
    for (d, n) in dups {
        if a.nodes[d].parent.is_none() && !a.above(d, n) {
            a.link(n, d);
        }
    }
    let orphan: Vec<bool> = msgs
        .iter()
        .map(|m| {
            m.refs
                .iter()
                .rev()
                .find(|&&r| Some(r) != m.id)
                .is_some_and(|r| by_id.get(r).is_none_or(|&n| a.nodes[n].msg.is_none()))
        })
        .collect();

    // 2 and 3. the roots; placeholders go, their children taking their place (not at the top
    // level, where a placeholder with several children stays to hold them together)
    let roots: Vec<usize> = (0..a.nodes.len())
        .filter(|&n| a.nodes[n].parent.is_none())
        .collect();
    let mut order = Vec::with_capacity(a.nodes.len());
    let mut stack = roots.clone();
    while let Some(n) = stack.pop() {
        order.push(n);
        stack.extend_from_slice(&a.nodes[n].children);
    }
    for &n in order.iter().rev() {
        // children before parents: each child's own children are pruned already
        let kids = std::mem::take(&mut a.nodes[n].children);
        let mut keep = Vec::with_capacity(kids.len());
        for c in kids {
            if a.nodes[c].msg.is_some() {
                keep.push(c);
                continue;
            }
            let grand = std::mem::take(&mut a.nodes[c].children);
            for &g in &grand {
                a.nodes[g].parent = Some(n);
            }
            keep.extend(grand);
            a.nodes[c].parent = None;
        }
        a.nodes[n].children = keep;
    }
    let mut tops = Vec::new();
    for r in roots {
        let n = &a.nodes[r];
        if n.msg.is_some() || n.children.len() > 1 {
            tops.push(r);
        } else if let [c] = n.children[..] {
            a.nodes[r].children.clear();
            a.nodes[c].parent = None;
            tops.push(c);
        }
    }

    // 4. what's still apart: a reply joins the conversation with its subject that was active
    // shortly before it
    let mut tops: Vec<Top> = tops
        .into_iter()
        .map(|node| {
            // a placeholder's subject is its earliest child's
            let msg = a.nodes[node].msg.or_else(|| {
                a.nodes[node]
                    .children
                    .iter()
                    .filter_map(|&c| a.nodes[c].msg)
                    .min_by_key(|&m| (msgs[m].date.is_none(), msgs[m].date, m))
            });
            let (key, reply) = msg.map_or((String::new(), false), |m| {
                (
                    subject_key(&msgs[m].subject),
                    strip_subject(&msgs[m].subject, true).1,
                )
            });
            let (first, last) = a.span(msgs, node);
            Top {
                node,
                key,
                reply,
                first,
                last,
            }
        })
        .collect();
    let mut by_subject: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, t) in tops.iter().enumerate() {
        if !t.key.is_empty() {
            by_subject.entry(t.key.as_str()).or_default().push(i);
        }
    }
    let mut groups: Vec<Vec<usize>> = by_subject.into_values().filter(|g| g.len() > 1).collect();
    groups.sort(); // the same result every time
    let mut gone = vec![false; tops.len()];
    for mut g in groups {
        g.sort_by_key(|&i| (tops[i].first.is_none(), tops[i].first, tops[i].node));
        let mut anchor: Option<usize> = None;
        for i in g {
            let near = anchor.is_some_and(|k| {
                matches!((tops[k].last, tops[i].first), (Some(l), Some(f)) if f - l <= SUBJECT_WINDOW)
            });
            match anchor {
                Some(k) if tops[i].reply && near => {
                    let joined = join(&mut a, &tops[k], tops[i].node);
                    tops[k].node = joined;
                    tops[k].last = tops[k].last.max(tops[i].last);
                    gone[i] = true;
                }
                _ => anchor = Some(i),
            }
        }
    }

    // 5. each thread in reading order
    let mut out = Vec::new();
    for (t, _) in tops.iter().zip(&gone).filter(|(_, g)| !**g) {
        let mut items = Vec::new();
        let mut stack = vec![(t.node, 0u32)];
        while let Some((n, depth)) = stack.pop() {
            let below = match a.nodes[n].msg {
                Some(m) => {
                    items.push(Item {
                        msg: m,
                        depth,
                        dup: dup[m],
                        orphan: orphan[m],
                    });
                    depth + 1
                }
                None => depth,
            };
            let mut kids = a.nodes[n].children.clone();
            kids.sort_by_key(|&c| {
                let m = a.nodes[c].msg;
                (a.date(msgs, c).unwrap_or(i64::MAX), m.unwrap_or(usize::MAX))
            });
            stack.extend(kids.into_iter().rev().map(|c| (c, below)));
        }
        let (first, last) = a.span(msgs, t.node);
        let first_read = items.iter().map(|x| x.msg).min().unwrap_or(0);
        let newest = items.iter().map(|x| x.msg).max().unwrap_or(0);
        out.push(Thread {
            items,
            first,
            last,
            first_read,
            newest,
        });
    }
    out.sort_by(|x, y| y.last.cmp(&x.last).then(y.newest.cmp(&x.newest)));
    out
}

/// Joins the thread `r` (its top node) to the conversation `k`, returning `k`'s top node: below
/// the original message when `k` starts with one, else side by side under a placeholder.
fn join(a: &mut Arena, k: &Top, r: usize) -> usize {
    let moved = if a.nodes[r].msg.is_some() {
        vec![r]
    } else {
        std::mem::take(&mut a.nodes[r].children)
    };
    let top = if a.nodes[k.node].msg.is_none() || !k.reply {
        k.node
    } else {
        let d = a.add(None);
        a.link(d, k.node);
        d
    };
    for c in moved {
        a.link(top, c);
    }
    top
}

// ---- reading an mbox: headers only, bodies skipped

/// How a step of reading stopped.
enum Stop {
    Done,
    Budget,
    Full,
}

struct Pending {
    at: u64,
    head: Vec<u8>,
    in_head: bool,
    envelope: Option<i64>,
}

/// Headers of the messages in an mbox from byte `from` (0, or where the last step stopped: always
/// a "From " line) until the end, the time budget, or `room` more messages. Returns where to carry
/// on.
fn scan_mbox(
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
fn envelope_date(line: &[u8]) -> Option<i64> {
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

struct MailFile {
    id: i64,
    path: PathBuf,
    size: u64,
    mtime: i64,
    flags: u8,
    time: i64,
}

fn current_files(db: &Db, dir: &Path) -> Result<Vec<(i64, PathBuf, u64, i64)>> {
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
fn maildir_files(db: &Db, dir: &Path) -> Result<Vec<MailFile>> {
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
fn open_indexed(db: &Db, f: &MailFile) -> Option<File> {
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
fn read_head(file: File) -> io::Result<Vec<u8>> {
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

// ---- what has been read of each mailbox, kept between requests

enum Source {
    Mbox(PathBuf),
    Maildir(Vec<MailFile>),
}

struct Scan {
    src: Source,
    heads: Vec<Head>,
    /// Where reading carries on: a byte offset (mbox) or a file number (Maildir).
    pos: u64,
    /// Bytes (mbox) or files (Maildir) in all.
    size: u64,
    done: bool,
    /// Stopped at `MAX_MESSAGES`.
    capped: bool,
    /// Maildir messages whose bytes couldn't be read.
    unreadable: u64,
    threads: Option<(usize, Arc<Vec<Thread>>)>,
}

impl Scan {
    fn new(src: Source, size: u64) -> Scan {
        Scan {
            src,
            heads: Vec::new(),
            pos: 0,
            size,
            done: false,
            capped: false,
            unreadable: 0,
            threads: None,
        }
    }

    /// Reads on until `until` (always at least one message), the end, or `MAX_MESSAGES`.
    fn step(&mut self, db: &Db, until: Instant) -> Result<()> {
        if self.done || self.capped {
            return Ok(());
        }
        let room = MAX_MESSAGES.saturating_sub(self.heads.len());
        match &self.src {
            Source::Mbox(path) => {
                let (pos, stop) = scan_mbox(path, self.pos, until, room, &mut self.heads)?;
                self.pos = pos;
                match stop {
                    Stop::Done => self.done = true,
                    Stop::Full => self.capped = true,
                    Stop::Budget => {}
                }
            }
            Source::Maildir(files) => {
                let start = self.pos;
                while let Some(f) = files.get(self.pos as usize) {
                    if self.heads.len() >= MAX_MESSAGES {
                        self.capped = true;
                        return Ok(());
                    }
                    if self.pos > start && Instant::now() > until {
                        return Ok(());
                    }
                    match open_indexed(db, f).map(read_head) {
                        Some(Ok(raw)) => {
                            self.heads
                                .push(head_of(&raw, f.id, f.size, Some(f.time), f.flags))
                        }
                        _ => self.unreadable += 1,
                    }
                    self.pos += 1;
                }
                self.done = true;
            }
        }
        Ok(())
    }

    fn threads(&mut self) -> Arc<Vec<Thread>> {
        if let Some((n, t)) = &self.threads
            && *n == self.heads.len()
        {
            return Arc::clone(t);
        }
        let t = Arc::new(thread(&self.heads));
        self.threads = Some((self.heads.len(), Arc::clone(&t)));
        t
    }

    fn status(&self) -> serde_json::Map<String, Value> {
        let v = json!({
            "kind": match self.src { Source::Mbox(_) => "mbox", Source::Maildir(_) => "maildir" },
            "messages": self.heads.len(),
            "read": self.pos,
            "size": self.size,
            "done": self.done,
            "capped": self.capped,
            "unreadable": self.unreadable,
        });
        match v {
            Value::Object(m) => m,
            _ => unreachable!(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Key {
    Mbox(PathBuf, u64, i64),
    Maildir(PathBuf, i64),
}

struct Cached {
    key: Key,
    scan: Arc<Mutex<Scan>>,
    messages: Arc<AtomicUsize>,
    used: Instant,
}

static CACHE: Mutex<Vec<Cached>> = Mutex::new(Vec::new());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

type Entry = (Arc<Mutex<Scan>>, Arc<AtomicUsize>, bool);

/// The mailbox's read-so-far state, and whether it is new (nothing read yet).
fn cached(key: Key, make: impl FnOnce() -> Result<Scan>) -> Result<Entry> {
    let find = |c: &mut Vec<Cached>| {
        c.iter_mut().find(|e| e.key == key).map(|e| {
            e.used = Instant::now();
            (Arc::clone(&e.scan), Arc::clone(&e.messages), false)
        })
    };
    if let Some(hit) = find(&mut lock(&CACHE)) {
        return Ok(hit);
    }
    let scan = make()?; // outside the lock: listing a big Maildir takes a moment
    let mut c = lock(&CACHE);
    if let Some(hit) = find(&mut c) {
        return Ok(hit);
    }
    let entry = Cached {
        key,
        scan: Arc::new(Mutex::new(scan)),
        messages: Arc::new(AtomicUsize::new(0)),
        used: Instant::now(),
    };
    let out = (Arc::clone(&entry.scan), Arc::clone(&entry.messages), true);
    c.push(entry);
    // the least recently used go first, down to the limits (never the one just added)
    while c.len() > 1
        && (c.len() > CACHE_MAILBOXES
            || c.iter()
                .map(|e| e.messages.load(Ordering::Relaxed))
                .sum::<usize>()
                > CACHE_MESSAGES)
    {
        let (i, _) = c[..c.len() - 1]
            .iter()
            .enumerate()
            .min_by_key(|(_, e)| e.used)
            .expect("more than one entry");
        c.remove(i);
    }
    Ok(out)
}

fn source(db: &Db, id: i64) -> Result<(PathBuf, u64)> {
    browse::resolve_content(db, id)?
        .context("this version's bytes aren't on any indexed drive any more")
}

fn mbox_entry(db: &Db, id: i64) -> Result<Entry> {
    let (path, size) = source(db, id)?;
    let mtime = std::fs::metadata(&path)?
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    cached(Key::Mbox(path.clone(), size, mtime), || {
        Ok(Scan::new(Source::Mbox(path), size))
    })
}

/// Whether the index has a folder (or, from an index without folder rows, files in it).
fn has_dir(db: &Db, dir: &Path) -> Result<bool> {
    let b = paths::to_bytes(dir);
    Ok(db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM dirs WHERE path = ?1 AND current = 1)
             OR EXISTS(SELECT 1 FROM files WHERE dir = ?1 AND current = 1)",
        params![b],
        |r| r.get(0),
    )?)
}

fn is_maildir(db: &Db, dir: &Path) -> Result<bool> {
    Ok(has_dir(db, &dir.join("cur"))? || has_dir(db, &dir.join("new"))?)
}

fn maildir_entry(db: &Db, dir: &Path) -> Result<Entry> {
    let listing = browse::list(db, dir, None).context("not an indexed folder")?;
    if !is_maildir(db, dir)? {
        bail!(
            "{} isn't a Maildir (it has no cur/ or new/ folder)",
            dir.display()
        );
    }
    cached(Key::Maildir(dir.to_path_buf(), listing.latest_scan), || {
        let files = maildir_files(db, dir)?;
        let n = files.len() as u64;
        Ok(Scan::new(Source::Maildir(files), n))
    })
}

fn leaf_name(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The Maildirs directly inside `dir` (its cur/, new/ and tmp/ aside).
fn maildirs_in(db: &Db, dir: &Path) -> Result<Vec<PathBuf>> {
    let kids: Vec<PathBuf> = {
        let mut stmt = db
            .conn
            .prepare_cached("SELECT path FROM dirs WHERE parent = ?1 AND current = 1")?;
        let rows = stmt.query_map(params![paths::to_bytes(dir)], |r| r.get::<_, Vec<u8>>(0))?;
        rows.map(|r| r.map(|b| paths::from_bytes(&b)))
            .collect::<rusqlite::Result<_>>()?
    };
    let mut out = Vec::new();
    for k in kids {
        if !matches!(leaf_name(&k).as_str(), "cur" | "new" | "tmp") && is_maildir(db, &k)? {
            out.push(k);
        }
    }
    Ok(out)
}

/// The Maildir's family, for a side list: an inbox and the Maildirs inside it (Maildir++'s
/// ".Sent", ".Drafts", …, or Dovecot's "fs" layout), or Maildirs side by side in a folder that
/// isn't one (mail/INBOX, mail/Sent); empty when it's alone. And the family's top folder, whose
/// name is the mailbox's.
fn folders(db: &Db, dir: &Path) -> Result<(Vec<Value>, PathBuf)> {
    let parent = dir.parent().filter(|p| !p.as_os_str().is_empty());
    let in_maildir = match parent {
        Some(p) => is_maildir(db, p)?,
        None => false,
    };
    let (top, mut family) = match parent {
        // a sub-folder (Maildir++'s ".Sent", or "fs" layout's "Sent"): its inbox's family
        Some(p) if in_maildir => (p.to_path_buf(), maildirs_in(db, p)?),
        _ => {
            let below = maildirs_in(db, dir)?;
            let beside = match parent {
                Some(p) if below.is_empty() => maildirs_in(db, p)?,
                _ => Vec::new(),
            };
            match parent {
                Some(p) if beside.len() > 1 => (p.to_path_buf(), beside),
                _ => (dir.to_path_buf(), below),
            }
        }
    };
    if family.is_empty() {
        return Ok((Vec::new(), top));
    }
    family.sort_by_key(|k| leaf_name(k).trim_start_matches('.').to_lowercase());
    let count = |d: &Path| -> Result<i64> {
        Ok(db.conn.query_row(
            "SELECT COUNT(*) FROM files WHERE dir = ?1 AND current = 1",
            params![paths::to_bytes(d)],
            |r| r.get(0),
        )?)
    };
    let mut out = Vec::new();
    for (i, p) in std::iter::once(&top).chain(&family).enumerate() {
        if i == 0 && !is_maildir(db, p)? {
            continue; // a folder of mailboxes, not one itself
        }
        let n = leaf_name(p);
        let shown = if i == 0 {
            "Inbox".to_string()
        } else if let Some(sub) = n.strip_prefix('.') {
            sub.replace('.', " / ")
        } else {
            n
        };
        let new = count(&p.join("new"))?;
        out.push(json!({
            "name": shown,
            "key": browse::key(p),
            "count": count(&p.join("cur"))? + new,
            "new": new,
            "here": p == dir,
        }));
    }
    Ok((out, top))
}

// ---- files whose name says nothing: mail?

fn header_name(line: &[u8]) -> Option<&[u8]> {
    let i = line.iter().position(|&b| b == b':')?;
    (i > 0 && line[..i].iter().all(|b| b.is_ascii_graphic())).then(|| &line[..i])
}

/// An mbox starts with a "From " envelope line ("From ann@example.org Mon Jan  1 12:00:00 2024"),
/// then the first message's headers.
fn looks_mbox(b: &[u8]) -> bool {
    let mut lines = b.split(|&c| c == b'\n');
    lines
        .next()
        .is_some_and(|l| l.len() < 1000 && peek::is_from_line(l, false))
        && lines.next().and_then(header_name).is_some()
}

/// A single message (an .eml without its extension, or a Maildir file) starts with a block of
/// headers, some of them ones only mail has.
fn looks_message(b: &[u8], whole: bool) -> bool {
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

// ---- the endpoints

/// "Ann Example <ann@example.org>" -> ("Ann Example", "ann@example.org").
fn person(from: &str) -> (&str, &str) {
    match from.rsplit_once(" <") {
        Some((name, addr)) if addr.ends_with('>') && !name.trim().is_empty() => {
            (name.trim().trim_matches('"'), addr.trim_end_matches('>'))
        }
        _ => (from, from),
    }
}

fn people<'h>(heads: &'h [Head], t: &Thread) -> Vec<&'h str> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for it in &t.items {
        let (name, addr) = person(&heads[it.msg].from);
        if !addr.is_empty() && seen.insert(addr.to_lowercase()) {
            out.push(name);
        }
    }
    out
}

fn thread_subject<'h>(heads: &'h [Head], t: &Thread) -> &'h str {
    t.items
        .iter()
        .map(|it| strip_subject(&heads[it.msg].subject, false).0)
        .find(|s| !s.is_empty())
        .unwrap_or("")
}

fn summary(heads: &[Head], t: &Thread, hits: usize) -> Value {
    let who = people(heads, t);
    // flagged or replied if any message is; a draft or in the trash if all of them are
    let any = t.items.iter().fold(0, |f, it| f | heads[it.msg].flags);
    let all = t.items.iter().fold(!0, |f, it| f & heads[it.msg].flags);
    let flags = (any & (FLAGGED | REPLIED)) | (all & (DRAFT | TRASHED));
    json!({
        "key": heads[t.first_read].key,
        "subject": thread_subject(heads, t),
        "who": who.iter().take(4).collect::<Vec<_>>(),
        "people": who.len(),
        "count": t.items.len(),
        "first": t.first,
        "last": t.last,
        "unread": t.items.iter().filter(|it| unread(heads[it.msg].flags)).count(),
        "flags": letters(flags),
        "hits": hits,
    })
}

/// Search: a sender or a subject that contains the (lower-case) words.
fn matches(h: &Head, q: &str) -> bool {
    h.from.to_lowercase().contains(q) || h.subject.to_lowercase().contains(q)
}

/// A page of the mailbox's threads (or, with `flat`, single messages), newest activity first.
fn page(s: &mut Scan, q: &str, flat: bool, offset: usize, limit: usize) -> Value {
    let (threads, total): (Vec<Value>, usize) = if flat {
        let heads = &s.heads;
        let mut idx: Vec<usize> = (0..heads.len())
            .filter(|&i| q.is_empty() || matches(&heads[i], q))
            .collect();
        idx.sort_by(|&x, &y| heads[y].date.cmp(&heads[x].date).then(y.cmp(&x)));
        let one = |i: usize| Thread {
            items: vec![Item {
                msg: i,
                depth: 0,
                dup: false,
                orphan: false,
            }],
            first: heads[i].date,
            last: heads[i].date,
            first_read: i,
            newest: i,
        };
        (
            idx.iter()
                .skip(offset)
                .take(limit)
                .map(|&i| {
                    let mut v = summary(heads, &one(i), usize::from(!q.is_empty()));
                    v["subject"] = json!(heads[i].subject); // as written, "Re:" and all
                    v
                })
                .collect(),
            idx.len(),
        )
    } else {
        let all = s.threads();
        let heads = &s.heads;
        let mut total = 0;
        let mut out = Vec::new();
        for t in all.iter() {
            let hits = if q.is_empty() {
                0
            } else {
                t.items
                    .iter()
                    .filter(|it| matches(&heads[it.msg], q))
                    .count()
            };
            if !q.is_empty() && hits == 0 {
                continue;
            }
            if total >= offset && out.len() < limit {
                out.push(summary(heads, t, hits));
            }
            total += 1;
        }
        (out, total)
    };
    let mut v = s.status();
    v.insert("threads".into(), json!(threads));
    v.insert("total".into(), json!(total));
    v.insert("offset".into(), json!(offset));
    Value::Object(v)
}

/// One thread, in reading order: every message's headers, depth and flags; `hit` marks the ones
/// the search `q` matches.
fn one_thread(s: &mut Scan, key: i64, q: &str, flat: bool) -> Result<Value> {
    let Some(i) = s.heads.iter().position(|h| h.key == key) else {
        bail!("that message isn't in what has been read of this mailbox")
    };
    let all = s.threads();
    let heads = &s.heads;
    let single;
    let t = if flat {
        single = Thread {
            items: vec![Item {
                msg: i,
                depth: 0,
                dup: false,
                orphan: false,
            }],
            first: heads[i].date,
            last: heads[i].date,
            first_read: i,
            newest: i,
        };
        &single
    } else {
        all.iter()
            .find(|t| t.items.iter().any(|it| it.msg == i))
            .context("that message isn't in any thread")?
    };
    let msgs: Vec<Value> = t
        .items
        .iter()
        .take(THREAD_MAX)
        .map(|it| {
            let h = &heads[it.msg];
            json!({
                "key": h.key,
                "from": h.from,
                "subject": h.subject,
                "date": h.date,
                "size": h.size,
                "depth": it.depth,
                "flags": letters(h.flags),
                "unread": unread(h.flags),
                "dup": it.dup,
                "orphan": it.orphan,
                "hit": !q.is_empty() && matches(h, q),
            })
        })
        .collect();
    let mut v = s.status();
    v.insert("thread".into(), summary(heads, t, 0));
    v.insert("who".into(), json!(people(heads, t)));
    v.insert("messages".into(), json!(msgs));
    Ok(Value::Object(v))
}

/// /api/threads, /api/thread and /api/mail-kind. A mailbox is `id` (an mbox file's index id) or
/// `dir` (a Maildir folder's key); `q` searches senders and subjects, `flat=1` lists messages
/// one by one, `more=1` reads another slice of a mailbox that isn't read to the end yet.
pub fn api<'a>(db: &Db, route: &str, param: &dyn Fn(&str) -> Option<&'a str>) -> Result<Value> {
    let id = || -> Result<i64> { param("id").context("missing id")?.parse().context("bad id") };
    if route == "/api/mail-kind" {
        return Ok(json!({ "kind": kind(&source(db, id()?)?.0)? }));
    }
    let dir = param("dir").map(browse::path_of).transpose()?;
    let (scan, count, fresh) = match &dir {
        Some(d) => maildir_entry(db, d)?,
        None => mbox_entry(db, id()?)?,
    };
    let mut s = lock(&scan);
    let q = param("q").unwrap_or("").trim().to_lowercase();
    let flat = param("flat") == Some("1");
    let num = |name: &str, default: usize| -> Result<usize> {
        param(name).map_or(Ok(default), |v| {
            v.parse().with_context(|| format!("bad {name}"))
        })
    };
    let v = match route {
        "/api/threads" => {
            if fresh || param("more") == Some("1") {
                s.step(db, Instant::now() + STEP_BUDGET)?;
            }
            let offset = num("offset", 0)?;
            let mut v = page(
                &mut s,
                &q,
                flat,
                offset,
                num("limit", 100)?.clamp(1, PAGE_MAX),
            );
            if let Some(d) = dir.as_deref().filter(|_| offset == 0) {
                let (list, top) = folders(db, d)?;
                v["folders"] = json!(list);
                v["name"] = json!(leaf_name(&top));
            }
            v
        }
        "/api/thread" => {
            let key: i64 = param("key").context("missing key")?.parse()?;
            // a mailbox dropped from the cache is read again as far as this message
            let deadline = Instant::now() + REREAD_BUDGET;
            while !s.heads.iter().any(|h| h.key == key)
                && !s.done
                && !s.capped
                && Instant::now() < deadline
            {
                s.step(db, (Instant::now() + STEP_BUDGET).min(deadline))?;
            }
            one_thread(&mut s, key, &q, flat)?
        }
        _ => bail!("no such endpoint"),
    };
    count.store(s.heads.len(), Ordering::Relaxed);
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(key: i64, id: &str, refs: &[&str], subject: &str, date: i64) -> Head {
        Head {
            key,
            size: 100,
            id: (!id.is_empty()).then(|| hash_id(id)),
            refs: refs.iter().map(|r| hash_id(r)).collect(),
            subject: subject.into(),
            from: format!("Person {key} <p{key}@example.org>"),
            date: Some(date),
            flags: 0,
        }
    }

    /// Each thread as (message key, depth) pairs, checking that every message is in exactly one.
    fn shape(msgs: &[Head]) -> Vec<Vec<(i64, u32)>> {
        let threads = thread(msgs);
        let mut seen: Vec<usize> = threads
            .iter()
            .flat_map(|t| t.items.iter().map(|it| it.msg))
            .collect();
        seen.sort();
        assert_eq!(seen, (0..msgs.len()).collect::<Vec<_>>());
        threads
            .iter()
            .map(|t| {
                t.items
                    .iter()
                    .map(|it| (msgs[it.msg].key, it.depth))
                    .collect()
            })
            .collect()
    }

    const DAY: i64 = 86_400;

    #[test]
    fn references_and_in_reply_to() {
        let msgs = vec![
            msg(1, "a@example.org", &[], "Plans", 10 * DAY),
            msg(
                2,
                "b@example.org",
                &["a@example.org"],
                "Re: Plans",
                11 * DAY,
            ),
            // In-Reply-To only (no References)
            msg(
                3,
                "c@example.org",
                &["b@example.org"],
                "Re: Plans",
                12 * DAY,
            ),
            // References to the whole chain, answering b too; earlier than c
            msg(
                4,
                "d@example.org",
                &["a@example.org", "b@example.org"],
                "Re: Plans",
                11 * DAY + 5,
            ),
            // something else, newer
            msg(5, "e@example.org", &[], "Other", 20 * DAY),
        ];
        assert_eq!(
            shape(&msgs),
            vec![vec![(5, 0)], vec![(1, 0), (2, 1), (4, 2), (3, 2)]]
        );
    }

    #[test]
    fn replies_arriving_before_their_parent() {
        // the reply is read first; its parent turns up later in the file
        let msgs = vec![
            msg(1, "b@example.org", &["a@example.org"], "Re: Q", 2 * DAY),
            msg(2, "a@example.org", &[], "Q", DAY),
        ];
        let t = thread(&msgs);
        assert_eq!(shape(&msgs), vec![vec![(2, 0), (1, 1)]]);
        assert_eq!(t[0].first_read, 0); // the thread is still named by what was read first
        assert!(!t[0].items[1].orphan);
    }

    #[test]
    fn missing_parents() {
        let msgs = vec![
            // two answers to a message that isn't here: kept together, side by side
            msg(1, "b@example.org", &["gone@example.org"], "Re: Lost", DAY),
            msg(
                2,
                "c@example.org",
                &["gone@example.org"],
                "Re: Lost",
                2 * DAY,
            ),
            // one answer to another missing one: it simply starts its thread
            msg(
                3,
                "d@example.org",
                &["other@example.org"],
                "Re: Alone",
                3 * DAY,
            ),
            // a missing grandparent: the chain holds below it
            msg(
                4,
                "f@example.org",
                &["root@example.org", "e@example.org"],
                "Re: Deep",
                4 * DAY,
            ),
            msg(
                5,
                "g@example.org",
                &["root@example.org", "e@example.org", "f@example.org"],
                "Re: Deep",
                5 * DAY,
            ),
        ];
        assert_eq!(
            shape(&msgs),
            vec![vec![(4, 0), (5, 1)], vec![(3, 0)], vec![(1, 0), (2, 0)]]
        );
        let t = thread(&msgs);
        let orphans: Vec<bool> = t
            .iter()
            .flat_map(|t| t.items.iter().map(|i| i.orphan))
            .collect();
        assert_eq!(orphans, [true, false, true, true, true]);
    }

    #[test]
    fn subject_fallback() {
        let msgs = vec![
            msg(1, "a@example.org", &[], "Budget 2024", 10 * DAY),
            // a reply whose client dropped the references
            msg(2, "b@example.org", &[], "AW: Budget 2024", 12 * DAY),
            msg(3, "c@example.org", &[], "SV: Re: budget  2024", 13 * DAY),
            // the same subject but not a reply: a conversation of its own
            msg(4, "d@example.org", &[], "Budget 2024", 14 * DAY),
            // a reply long after: not joined
            msg(5, "e@example.org", &[], "Re: Budget 2024", 200 * DAY),
            // a forward with a list tag, soon after the second conversation
            msg(6, "f@example.org", &[], "[team] Fwd: Budget 2024", 15 * DAY),
            // no subject is never a reason to join
            msg(7, "g@example.org", &[], "", 10 * DAY),
            msg(8, "h@example.org", &[], "Re:", 10 * DAY + 1),
        ];
        assert_eq!(
            shape(&msgs),
            vec![
                vec![(5, 0)],
                vec![(4, 0), (6, 1)],
                vec![(1, 0), (2, 1), (3, 1)],
                vec![(8, 0)],
                vec![(7, 0)],
            ]
        );
    }

    #[test]
    fn replies_without_their_original_join_by_subject() {
        let msgs = vec![
            msg(1, "b@example.org", &[], "Re: Trip", DAY),
            msg(2, "c@example.org", &[], "Re: Trip", 2 * DAY),
        ];
        assert_eq!(shape(&msgs), vec![vec![(1, 0), (2, 0)]]);
    }

    #[test]
    fn loops_are_refused() {
        let msgs = vec![
            // each claims to answer the other
            msg(1, "a@example.org", &["b@example.org"], "x", DAY),
            msg(2, "b@example.org", &["a@example.org"], "y", 2 * DAY),
            // a message that references itself, and a three-way loop in References
            msg(3, "c@example.org", &["c@example.org"], "z", 3 * DAY),
            msg(
                4,
                "d@example.org",
                &["e@example.org", "f@example.org", "e@example.org"],
                "w",
                4 * DAY,
            ),
            msg(5, "e@example.org", &["d@example.org"], "v", 5 * DAY),
        ];
        let s = shape(&msgs);
        assert_eq!(s.iter().map(Vec::len).sum::<usize>(), 5);
        assert!(s.contains(&vec![(3, 0)]));
        // a and b end up in one thread, one below the other
        assert!(s.contains(&vec![(1, 0), (2, 1)]) || s.contains(&vec![(2, 0), (1, 1)]));
    }

    #[test]
    fn duplicates_stay_with_their_original() {
        let msgs = vec![
            msg(1, "a@example.org", &[], "Hello", DAY),
            msg(2, "b@example.org", &["a@example.org"], "Re: Hello", 2 * DAY),
            // the same message again (a copy in another folder, or saved twice)
            msg(3, "a@example.org", &[], "Hello", DAY),
            msg(4, "b@example.org", &["a@example.org"], "Re: Hello", 2 * DAY),
            // and no Message-ID at all
            msg(5, "", &[], "Hello there", 3 * DAY),
        ];
        assert_eq!(
            shape(&msgs),
            vec![vec![(5, 0)], vec![(1, 0), (3, 1), (2, 1), (4, 1)]]
        );
        let t = thread(&msgs);
        let dups: Vec<(i64, bool)> = t[1]
            .items
            .iter()
            .map(|it| (msgs[it.msg].key, it.dup))
            .collect();
        assert_eq!(dups, [(1, false), (3, true), (2, false), (4, true)]);
    }

    #[test]
    fn deep_chains_and_many_messages() {
        // a long conversation, each message answering the one before
        let mut msgs = Vec::new();
        for i in 0..3000i64 {
            let refs: Vec<String> = (i.saturating_sub(10)..i)
                .map(|j| format!("m{j}@example.org"))
                .collect();
            let refs: Vec<&str> = refs.iter().map(String::as_str).collect();
            msgs.push(msg(
                i,
                &format!("m{i}@example.org"),
                &refs,
                "Re: long",
                DAY + i,
            ));
        }
        let t = thread(&msgs);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].items.len(), 3000);
        assert_eq!(t[0].items[2999].depth, 2999);
    }

    #[test]
    fn subjects() {
        for (s, want, reply) in [
            ("Re: Plans", "Plans", true),
            ("RE: re: Fwd: Plans", "Plans", true),
            ("AW: Plans", "Plans", true),
            ("SV: Plans", "Plans", true),
            ("Re[2]: Plans", "Plans", true),
            ("Re(3): Plans", "Plans", true),
            ("RE : Plans", "Plans", true),
            ("回复：Plans", "Plans", true),
            ("Plans", "Plans", false),
            ("Re-organising", "Re-organising", false),
            ("Regarding: Plans", "Regarding: Plans", false),
            ("Re:", "", true),
        ] {
            assert_eq!(strip_subject(s, false), (want, reply), "{s}");
        }
        assert_eq!(strip_subject("[dev] Re: Plans", true), ("Plans", true));
        assert_eq!(
            strip_subject("[dev] Re: Plans", false),
            ("[dev] Re: Plans", false)
        );
        assert_eq!(strip_subject("[only a tag]", true), ("[only a tag]", false));
        assert_eq!(subject_key("Re:  Budget\t2024 "), "budget 2024");
    }

    #[test]
    fn maildir_names() {
        let seen = KNOWN | SEEN;
        for (name, want) in [
            ("1698765432.M5P6Q7.host,S=1234:2,S", seen),
            (
                "1698765432.M5P6Q7.host,S=1234:2,FRS",
                seen | FLAGGED | REPLIED,
            ),
            ("1698765432.M5P6Q7.host:2,T", KNOWN | TRASHED),
            ("1698765432.M5P6Q7.host:2,DP", KNOWN | DRAFT | PASSED),
            ("1698765432.M5P6Q7.host:2,", KNOWN),
            ("1698765432.M5P6Q7.host", KNOWN), // new/: not read yet
            ("1698765432.M5P6Q7.host!2,S", seen), // where ':' can't be in a name
            ("1698765432.M5P6Q7.host:2,Sab", seen), // keywords
            ("1698765432.M5P6Q7.host:1,S", KNOWN), // experimental info: no flags
        ] {
            assert_eq!(maildir_flags(name), want, "{name}");
        }
        assert!(unread(maildir_flags("1698765432.x.host")));
        assert!(!unread(maildir_flags("1698765432.x.host:2,RS")));
        assert!(!unread(0)); // an mbox without status headers: unknown, so not shown as unread
        assert_eq!(letters(maildir_flags("1.x:2,SRF")), "FRS");
        assert_eq!(delivered("1698765432.M5P6Q7.host:2,S"), Some(1_698_765_432));
        assert_eq!(delivered("notes.txt"), None);
    }

    #[test]
    fn envelope_dates() {
        assert_eq!(
            envelope_date(b"From ann@example.org Mon Jan  1 12:00:00 2024\n"),
            Some(1_704_110_400)
        );
        assert_eq!(
            envelope_date(b"From 1234@xxx Wed Jan 03 14:35:03 +0000 2024\n"),
            Some(1_704_292_503)
        );
        assert_eq!(envelope_date(b"From - \n"), None);
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn eml(id: &str, reply_to: &[&str], from: &str, subject: &str, date: &str) -> String {
        let refs = if reply_to.is_empty() {
            String::new()
        } else {
            let ids: Vec<String> = reply_to.iter().map(|r| format!("<{r}>")).collect();
            format!(
                "In-Reply-To: {}\nReferences: {}\n",
                ids.last().unwrap(),
                ids.join(" ")
            )
        };
        format!(
            "From: {from}\nTo: team@example.org\nSubject: {subject}\nDate: {date}\nMessage-ID: <{id}>\n{refs}\nHello from {from}.\n"
        )
    }

    fn call(db: &Db, route: &str, args: &[(&str, &str)]) -> Result<Value> {
        let args: Vec<(String, String)> = args
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        api(db, route, &|name| {
            args.iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        })
    }

    fn keys(v: &Value) -> Vec<i64> {
        v["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["key"].as_i64().unwrap())
            .collect()
    }

    #[test]
    fn indexed_mbox_and_maildir() {
        use backdown_core::index;
        let tmp = tempfile::tempdir().unwrap();
        let drive = tmp.path().join("drive");
        // Thunderbird's way: an mbox called "Inbox", no extension
        let a = eml(
            "a@example.org",
            &[],
            "Ann <ann@example.org>",
            "Plans",
            "Mon, 1 Jan 2024 10:00:00 +0000",
        );
        let b = eml(
            "b@example.org",
            &["a@example.org"],
            "Bob <bob@example.org>",
            "Re: Plans",
            "Tue, 2 Jan 2024 10:00:00 +0000",
        );
        let c = eml(
            "c@example.org",
            &["a@example.org", "b@example.org"],
            "Ann <ann@example.org>",
            "Re: Plans",
            "Wed, 3 Jan 2024 10:00:00 +0000",
        );
        let d = eml(
            "d@example.org",
            &[],
            "Cy <cy@example.org>",
            "Invoice 42",
            "Thu, 4 Jan 2024 10:00:00 +0000",
        );
        let mbox: String = [&a, &b, &d, &c]
            .iter()
            .map(|m| format!("From - Mon Jan  1 10:00:00 2024\n{m}\n"))
            .collect();
        write(&drive.join("Mail/Inbox"), &mbox);
        // a Maildir++: the inbox in cur/ and new/, a Sent folder, and what isn't mail
        let md = drive.join("Maildir");
        write(&md.join("cur/1704103200.M1P1.mx.example.org,S=300:2,S"), &a);
        write(
            &md.join("cur/1704189600.M2P2.mx.example.org,S=300:2,FRS"),
            &b,
        );
        write(&md.join("new/1704276000.M3P3.mx.example.org"), &c);
        write(&md.join("tmp/1704362400.M4P4.mx.example.org"), &d);
        write(&md.join("dovecot-uidlist"), "3 V1 N4\n");
        write(&md.join(".Sent/cur/1704190000.M5P5.mx.example.org:2,S"), &b);
        write(&md.join(".Sent/maildirfolder"), "");
        // Dovecot's "fs" layout: Maildirs side by side in a folder that isn't one
        write(
            &drive.join("mail/INBOX/cur/1704103200.M6P6.mx.example.org:2,S"),
            &a,
        );
        write(
            &drive.join("mail/INBOX/new/1704189600.M7P7.mx.example.org"),
            &b,
        );
        write(
            &drive.join("mail/Sent/cur/1704276000.M8P8.mx.example.org:2,S"),
            &c,
        );

        let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
        let opts = index::Options {
            one_file_system: true,
            allow_empty: false,
        };
        index::index(&mut db, &drive, &opts, &mut |_| {}).unwrap();
        let root = dunce::canonicalize(&drive).unwrap();
        let file_id = |p: &str| -> String {
            db.conn
                .query_row(
                    "SELECT id FROM files WHERE path = ?1 AND current = 1",
                    params![paths::to_bytes(&root.join(p))],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
                .to_string()
        };

        // the mbox: recognised without an extension, threaded, searchable
        let inbox = file_id("Mail/Inbox");
        let k = call(&db, "/api/mail-kind", &[("id", &inbox)]).unwrap();
        assert_eq!(k["kind"], "mbox");
        let v = call(&db, "/api/threads", &[("id", &inbox)]).unwrap();
        assert_eq!(
            (v["kind"].as_str(), v["done"].as_bool()),
            (Some("mbox"), Some(true))
        );
        assert_eq!(
            (v["messages"].as_u64(), v["total"].as_u64()),
            (Some(4), Some(2))
        );
        let t = &v["threads"][0]; // newest activity first: Invoice (4 Jan) before Plans (3 Jan)
        assert_eq!(
            (t["subject"].as_str(), t["count"].as_u64()),
            (Some("Invoice 42"), Some(1))
        );
        let t = &v["threads"][1];
        assert_eq!(
            (t["subject"].as_str(), t["count"].as_u64()),
            (Some("Plans"), Some(3))
        );
        assert_eq!(t["who"], json!(["Ann", "Bob"]));
        assert_eq!(
            (t["first"].as_i64(), t["last"].as_i64()),
            (Some(1_704_103_200), Some(1_704_276_000))
        );
        let key = t["key"].as_i64().unwrap().to_string();
        let th = call(
            &db,
            "/api/thread",
            &[("id", &inbox), ("key", &key), ("q", "bob")],
        )
        .unwrap();
        let depths: Vec<u64> = th["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["depth"].as_u64().unwrap())
            .collect();
        assert_eq!(depths, [0, 1, 2]);
        let hits: Vec<bool> = th["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["hit"].as_bool().unwrap())
            .collect();
        assert_eq!(hits, [false, true, false]);
        // each message's key is where it starts, for /api/mail
        let path = root.join("Mail/Inbox");
        for (at, subject) in keys(&th)
            .into_iter()
            .zip(["Plans", "Re: Plans", "Re: Plans"])
        {
            assert_eq!(
                peek::mail(&path, Some(at as u64)).unwrap()["subject"],
                subject
            );
        }
        let s = call(&db, "/api/threads", &[("id", &inbox), ("q", "INVOICE")]).unwrap();
        assert_eq!(s["total"], 1);
        let f = call(&db, "/api/threads", &[("id", &inbox), ("flat", "1")]).unwrap();
        assert_eq!(f["total"], 4);
        assert_eq!(f["threads"][1]["subject"], "Re: Plans");
        let p = call(
            &db,
            "/api/threads",
            &[("id", &inbox), ("offset", "1"), ("limit", "1")],
        )
        .unwrap();
        assert_eq!(
            (p["threads"].as_array().unwrap().len(), p["total"].as_u64()),
            (1, Some(2))
        );

        // the Maildir: cur/ and new/ (not tmp/), flags from the names, its Sent folder alongside
        let dir = browse::key(&root.join("Maildir"));
        let v = call(&db, "/api/threads", &[("dir", &dir)]).unwrap();
        assert_eq!(
            (
                v["kind"].as_str(),
                v["messages"].as_u64(),
                v["total"].as_u64()
            ),
            (Some("maildir"), Some(3), Some(1))
        );
        assert_eq!(v["threads"][0]["unread"], 1);
        assert_eq!(v["threads"][0]["flags"], "FR");
        let folders: Vec<(&str, u64, bool)> = v["folders"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                (
                    f["name"].as_str().unwrap(),
                    f["count"].as_u64().unwrap(),
                    f["here"].as_bool().unwrap(),
                )
            })
            .collect();
        assert_eq!(folders, [("Inbox", 3, true), ("Sent", 1, false)]);
        let key = v["threads"][0]["key"].as_i64().unwrap().to_string();
        let th = call(&db, "/api/thread", &[("dir", &dir), ("key", &key)]).unwrap();
        let msgs = th["messages"].as_array().unwrap();
        let flags: Vec<(&str, bool)> = msgs
            .iter()
            .map(|m| (m["flags"].as_str().unwrap(), m["unread"].as_bool().unwrap()))
            .collect();
        assert_eq!(flags, [("S", false), ("FRS", false), ("", true)]);
        // messages are index ids: /api/mail reads them like an .eml
        let (p, _) = source(&db, msgs[0]["key"].as_i64().unwrap()).unwrap();
        assert_eq!(peek::mail(&p, None).unwrap()["subject"], "Plans");
        let one = file_id("Maildir/new/1704276000.M3P3.mx.example.org");
        assert_eq!(
            call(&db, "/api/mail-kind", &[("id", &one)]).unwrap()["kind"],
            "message"
        );
        // a Maildir++ sub-folder lists its family too, from the inbox down
        let sent = browse::key(&root.join("Maildir/.Sent"));
        let v = call(&db, "/api/threads", &[("dir", &sent)]).unwrap();
        assert_eq!(v["total"], 1);
        assert_eq!(v["folders"][1]["here"], true);
        assert_eq!(v["name"], "Maildir"); // the mailbox is named by its top folder
        let inbox = browse::key(&root.join("mail/INBOX"));
        let v = call(&db, "/api/threads", &[("dir", &inbox)]).unwrap();
        assert_eq!(
            (v["messages"].as_u64(), v["name"].as_str()),
            (Some(2), Some("mail"))
        );
        let names: Vec<(&str, u64, bool)> = v["folders"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                (
                    f["name"].as_str().unwrap(),
                    f["new"].as_u64().unwrap(),
                    f["here"].as_bool().unwrap(),
                )
            })
            .collect();
        assert_eq!(names, [("INBOX", 1, true), ("Sent", 0, false)]);
        // anything else isn't a mailbox
        assert!(
            call(
                &db,
                "/api/threads",
                &[("dir", &browse::key(&root.join("Mail")))]
            )
            .is_err()
        );
        assert!(call(&db, "/api/threads", &[("dir", &browse::key(tmp.path()))]).is_err());
        let uid = file_id("Maildir/dovecot-uidlist");
        assert_eq!(
            call(&db, "/api/mail-kind", &[("id", &uid)]).unwrap()["kind"],
            Value::Null
        );
    }

    #[test]
    fn what_mail_looks_like() {
        let mbox = b"From ann@example.org Mon Jan  1 12:00:00 2024\nFrom: ann@example.org\nSubject: hi\n\nbody\n";
        assert!(looks_mbox(mbox));
        assert!(!looks_mbox(
            b"From my point of view, at 12:30 we left.\nIt was late.\n"
        ));
        let eml = b"Return-Path: <ann@example.org>\r\nReceived: from mx.example.org\r\n\tby mx.example.com\r\nFrom: Ann <ann@example.org>\r\nSubject: hi\r\n\r\nbody\r\n";
        assert!(looks_message(eml, true));
        assert!(!looks_message(b"title: notes\nauthor: me\n\ntext\n", true));
        assert!(!looks_message(b"just some text\nFrom: x\n", true));
        assert!(!looks_message(b"", true));
    }
}
