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

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use backdown_core::mailmerge::letters;
use backdown_core::{Db, browse};
use serde_json::{Value, json};

mod cache;
mod message;
mod read;
mod sniff;
mod threads;

use cache::*;
pub use message::*;
use read::*;
pub use sniff::*;
pub use threads::*;

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
mod tests;
