//! Filling the content search index (backdown_core::fts) from the recognised things, and
//! searching it: `backdown search`, `GET /api/content`, the daemon's "content" job.
//!
//! What gets indexed, per kind of thing:
//! - Mailboxes (Maildir, mail stores, Dovecot mdbox): one document per message with its subject,
//!   sender, recipients and date. Where Dovecot kept its own index (`dovecot.index.cache`), the
//!   headers come from there without opening a single message (`backdown_core::dovecot`); the rest
//!   are read from the message files (the header block only). Thunderbird mbox files are read
//!   message by message.
//! - Database dumps (mysqldump, phpMyAdmin, pg_dump, pg_dumpall, SQLite .dump; compressed or not):
//!   a document per table with its columns, and, at full depth, the text in its rows in chunks,
//!   taking only text-typed columns (a dump's `FULLTEXT KEY` says which columns the database itself
//!   indexed for search; those are listed with the table). mongodump folders: a document per
//!   collection. MySQL data directories: a document per table file.
//! - SQLite files: a document per table with its columns; a table that is itself a full-text
//!   index (FTS3/4/5) is noted and searched in place when a search runs, so SQLite's own index is
//!   reused rather than copied; at full depth the text columns of ordinary tables are indexed.
//!
//! Depth is a setting (`off`, `headers`, `full`; `headers` by default): headers and schemas are
//! cheap and enough to find a message or a table; full depth makes the index grow with the data
//! (capped per file). The daemon indexes after recognition; what was indexed for the same scan is
//! left alone, so a rebuild only touches drives that were scanned again.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Result, bail};
use backdown_core::fts::{self, Doc, Writer};
use backdown_core::recognise::{self, Entity};
use backdown_core::{Db, browse, dovecot, fmt, paths};
use rusqlite::params;
use serde_json::{Value, json};

use backdown_core::mbox;
use backdown_viewers::peek::{self, Codec};

/// How much to index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Depth {
    Off,
    Headers,
    Full,
}

impl Depth {
    pub fn parse(s: &str) -> Result<Depth> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" => Depth::Off,
            "headers" | "schema" => Depth::Headers,
            "full" | "text" => Depth::Full,
            _ => bail!("the depth is off, headers or full (not {s:?})"),
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Depth::Off => "off",
            Depth::Headers => "headers",
            Depth::Full => "full",
        }
    }
}

/// The setting (`scan_settings`, key "content"), `headers` unless changed.
pub fn depth(db: &Db) -> Depth {
    db.conn
        .query_row(
            "SELECT value FROM scan_settings WHERE key = 'content'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| Depth::parse(&v).ok())
        .unwrap_or(Depth::Headers)
}

pub fn set_depth(db: &Db, d: Depth) -> Result<()> {
    db.conn.execute(
        "INSERT OR REPLACE INTO scan_settings(key, value) VALUES ('content', ?1)",
        params![d.as_str()],
    )?;
    Ok(())
}

/// Text from one file at full depth, at most (what goes beyond is left out and noted).
const TEXT_CAP: u64 = 64 << 20;
/// A chunk of a table's text becomes one document at about this size.
const CHUNK: usize = 32 << 10;
/// A message's header block, at most.
const HEAD_CAP: u64 = 64 << 10;
/// Of a message body, at full depth.
const BODY_CAP: usize = 8 << 10;
/// Messages per transaction.
const BATCH: u64 = 5_000;

#[derive(Default, Debug, serde::Serialize)]
pub struct Report {
    pub entities: u64,
    pub skipped_fresh: u64,
    pub docs: u64,
    pub by_source: BTreeMap<String, u64>,
    pub notes: Vec<String>,
    pub stopped: bool,
    pub ms: u64,
}

struct Ctx<'a> {
    db: &'a Db,
    root_id: i64,
    scan: i64,
    depth: Depth,
    stop: &'a AtomicBool,
    progress: &'a mut dyn FnMut(&str),
    report: &'a mut Report,
    since_commit: u64,
}

impl Ctx<'_> {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
    fn count(&mut self, source: &str) {
        self.report.docs += 1;
        *self.report.by_source.entry(source.to_string()).or_default() += 1;
        self.since_commit += 1;
    }
    /// commit now and then, so hours of work aren't one transaction
    fn tick(&mut self, w: &mut Writer) -> Result<()> {
        if self.since_commit >= BATCH {
            w.checkpoint()?;
            self.since_commit = 0;
        }
        Ok(())
    }
}

/// Kinds of things with contents to index.
const INDEXABLE: &[&str] = &[
    "maildir",
    "mail-store",
    "mdbox",
    "thunderbird",
    "sql-dump",
    "sqlite-db",
    "mongo-dump",
    "mysql-data",
];

/// Index the contents of what's recognised on `roots` (all with stored recognition when `None`).
/// Entities whose documents already describe the current scan are skipped unless `force`.
pub fn build(
    db: &Db,
    roots: Option<&[i64]>,
    depth: Depth,
    force: bool,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(&str),
) -> Result<Report> {
    let t0 = Instant::now();
    let mut report = Report::default();
    if depth == Depth::Off {
        report.notes.push(
            "content indexing is off (Settings, or `backdown search --depth headers`)".into(),
        );
        return Ok(report);
    }
    let ids: Vec<i64> = match roots {
        Some(r) => r.to_vec(),
        None => recognise::root_ids(db)?,
    };
    'roots: for id in ids {
        let Some((r, _fresh)) = recognise::load(db, id)? else {
            continue;
        };
        if !Path::new(&r.root).is_dir() {
            report.notes.push(format!(
                "{}: not connected, so its contents weren't read",
                r.root
            ));
            continue;
        }
        for e in r
            .entities
            .iter()
            .filter(|e| INDEXABLE.contains(&e.kind.as_str()))
        {
            // a mailbox inside a store, a dump inside a system: the container indexes it
            if e.inside.as_deref().is_some_and(|k| {
                r.entities.iter().any(|c| {
                    c.key == k && INDEXABLE.contains(&c.kind.as_str()) && c.kind != "sql-dump"
                })
            }) {
                continue;
            }
            if stop.load(Ordering::Relaxed) {
                report.stopped = true;
                break 'roots;
            }
            if !force && fts::indexed_scan(db, id, &e.key)? == Some(r.scan) {
                report.skipped_fresh += 1;
                continue;
            }
            progress(&format!("{}: {}", e.label, e.title));
            fts::forget(db, id, Some(&e.key))?;
            let mut w = Writer::begin(db)?;
            let mut ctx = Ctx {
                db,
                root_id: id,
                scan: r.scan,
                depth,
                stop,
                progress,
                report: &mut report,
                since_commit: 0,
            };
            let res = index_entity(&mut ctx, &mut w, e);
            let stopped = ctx.stopped();
            match res {
                Ok(()) => {
                    w.commit()?;
                    report.entities += 1;
                }
                Err(err) => {
                    drop(w);
                    report
                        .notes
                        .push(format!("{} ({}): {err:#}", e.title, e.path));
                }
            }
            if stopped {
                report.stopped = true;
                break 'roots;
            }
        }
    }
    report.ms = t0.elapsed().as_millis() as u64;
    Ok(report)
}

fn index_entity(ctx: &mut Ctx, w: &mut Writer, e: &Entity) -> Result<()> {
    let p = PathBuf::from(&e.path);
    match e.kind.as_str() {
        "maildir" | "mail-store" => {
            for mb in maildirs_below(ctx.db, &p)? {
                if ctx.stopped() {
                    return Ok(());
                }
                index_maildir(ctx, w, e, &mb)?;
            }
        }
        "mdbox" => index_mdbox(ctx, w, e, &p)?,
        "thunderbird" => index_thunderbird(ctx, w, e, &p)?,
        "sql-dump" if e.is_file => index_dump(ctx, w, e, &p)?,
        "sql-dump" => {
            for (name, _, _) in files_in(ctx.db, &p)? {
                if ctx.stopped() {
                    return Ok(());
                }
                if recognise::is_sql_dump(&name.to_lowercase()) {
                    index_dump(ctx, w, e, &p.join(name))?;
                }
            }
        }
        "sqlite-db" => index_sqlite(ctx, w, e, &p)?,
        "mongo-dump" => index_mongo_dump(ctx, w, e, &p)?,
        "mysql-data" => index_mysql_data(ctx, w, e, &p)?,
        _ => {}
    }
    Ok(())
}

// ---------- the index as a file list

/// Files directly in a folder, current: (name, size, mtime).
fn files_in(db: &Db, dir: &Path) -> Result<Vec<(String, u64, i64)>> {
    let mut stmt = db.conn.prepare_cached(
        "SELECT path, size, mtime FROM files WHERE dir = ?1 AND current = 1 ORDER BY path",
    )?;
    let dirb = paths::to_bytes(dir);
    let rows = stmt.query_map(params![dirb], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, i64>(1)? as u64,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (p, size, mtime) = row?;
        let p = paths::from_bytes(&p);
        if let Some(n) = p.file_name() {
            out.push((n.to_string_lossy().into_owned(), size, mtime));
        }
    }
    Ok(out)
}

/// Every Maildir mailbox folder at or below `dir` (folders with a `cur` inside).
fn maildirs_below(db: &Db, dir: &Path) -> Result<Vec<PathBuf>> {
    let lo = paths::dir_prefix(dir);
    let mut hi = lo.clone();
    if let Some(l) = hi.last_mut() {
        *l += 1;
    }
    let mut stmt = db.conn.prepare_cached(
        "SELECT path FROM dirs WHERE current = 1 AND path >= ?1 AND path < ?2 AND lname = 'cur'",
    )?;
    let rows = stmt.query_map(params![lo, hi], |r| r.get::<_, Vec<u8>>(0))?;
    let mut out = Vec::new();
    for row in rows {
        let p = paths::from_bytes(&row?);
        if let Some(mb) = p.parent() {
            out.push(mb.to_path_buf());
        }
    }
    // the folder itself when it is a mailbox but its cur/ is named otherwise in the index
    if out.is_empty() && dir.join("cur").is_dir() {
        out.push(dir.to_path_buf());
    }
    out.sort();
    Ok(out)
}

// ---------- mail

const WANTED: &[&str] = &["subject", "from", "to", "cc", "date", "message-id"];

/// What a message says about itself, however it was read.
#[derive(Default)]
struct Head {
    subject: String,
    from: String,
    to: String,
    cc: String,
    date: Option<i64>,
    id: String,
    body: String,
}

impl Head {
    fn from_cached(h: &[(String, String)]) -> Head {
        let get = |k: &str| {
            h.iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        Head {
            subject: get("subject"),
            from: get("from"),
            to: get("to"),
            cc: get("cc"),
            date: mail_parser::DateTime::parse_rfc822(&get("date")).map(|d| d.to_timestamp()),
            id: get("message-id"),
            body: String::new(),
        }
    }

    fn from_raw(raw: &[u8], body: bool) -> Head {
        let parser = mail_parser::MessageParser::default();
        let m = if body {
            parser.parse(raw)
        } else {
            parser.parse_headers(raw)
        };
        let Some(m) = m else {
            return Head::default();
        };
        let text = if body {
            let mut t: String = m
                .body_text(0)
                .map(|t| t.chars().take(BODY_CAP).collect())
                .unwrap_or_default();
            if t.is_empty() {
                t = m
                    .body_html(0)
                    .map(|h| strip_tags(&h, BODY_CAP))
                    .unwrap_or_default();
            }
            t
        } else {
            String::new()
        };
        Head {
            subject: m.subject().unwrap_or_default().to_string(),
            from: peek::addrs(m.from()),
            to: peek::addrs(m.to()),
            cc: peek::addrs(m.cc()),
            date: m.date().filter(|d| d.is_valid()).map(|d| d.to_timestamp()),
            id: m.message_id().unwrap_or_default().to_string(),
            body: text,
        }
    }

    fn who(&self) -> String {
        let mut s = self.from.clone();
        if !self.to.is_empty() {
            s.push_str(" → ");
            s.push_str(&self.to);
        }
        s
    }

    fn rest(&self) -> String {
        let mut s = String::new();
        if !self.cc.is_empty() {
            s.push_str(&self.cc);
            s.push(' ');
        }
        s.push_str(&self.id);
        if !self.body.is_empty() {
            s.push('\n');
            s.push_str(&self.body);
        }
        s
    }
}

/// Text from HTML, roughly: tags and entities dropped.
fn strip_tags(html: &str, cap: usize) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut last_space = true;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                if !last_space {
                    out.push(' ');
                    last_space = true;
                }
            }
            _ if in_tag => {}
            c if c.is_whitespace() => {
                if !last_space {
                    out.push(' ');
                    last_space = true;
                }
            }
            c => {
                out.push(c);
                last_space = false;
            }
        }
        if out.len() >= cap {
            break;
        }
    }
    out
}

/// A message file's header block (and, with `body`, up to `MAIL_CAP` of it).
fn read_message(path: &Path, body: bool) -> Result<Vec<u8>> {
    let f = paths::open_read(path)?;
    if body {
        let mut buf = Vec::new();
        f.take((HEAD_CAP + 512) << 10).read_to_end(&mut buf)?;
        return Ok(buf);
    }
    let mut r = BufReader::with_capacity(8 << 10, f.take(4 * HEAD_CAP));
    let (mut head, mut line) = (Vec::new(), Vec::new());
    loop {
        let n = peek::line_capped(&mut r, &mut line, 16 << 10)?;
        if n == 0 || mbox::blank(&line) || head.len() as u64 + line.len() as u64 > HEAD_CAP {
            return Ok(head);
        }
        head.extend_from_slice(&line);
    }
}

/// The mailbox's name for people: "INBOX", ".Sent" → "Sent", "user/Maildir/.Lists.rust" → "Lists/rust".
fn mailbox_name(mb: &Path) -> String {
    let n = mb
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if n.eq_ignore_ascii_case("maildir") || n.eq_ignore_ascii_case("cur") {
        return "INBOX".into();
    }
    if let Some(sub) = n.strip_prefix('.') {
        return sub.replace('.', "/");
    }
    n
}

#[allow(clippy::too_many_arguments)]
fn put_message(
    ctx: &mut Ctx,
    w: &mut Writer,
    e: &Entity,
    path: &Path,
    locator: Option<&str>,
    h: &Head,
    fallback_date: Option<i64>,
    bytes: u64,
    source: &str,
) -> Result<()> {
    let title = if h.subject.trim().is_empty() {
        "(no subject)".to_string()
    } else {
        h.subject.clone()
    };
    w.put(&Doc {
        root_id: ctx.root_id,
        entity_key: &e.key,
        kind: "message",
        path,
        locator,
        title: &title,
        who: &h.who(),
        at: h.date.or(fallback_date),
        bytes,
        source,
        scan: ctx.scan,
        body: &h.rest(),
    })?;
    ctx.count(source);
    ctx.tick(w)
}

fn index_maildir(ctx: &mut Ctx, w: &mut Writer, e: &Entity, mb: &Path) -> Result<()> {
    let name = mailbox_name(mb);
    (ctx.progress)(&format!("{}: {name}", e.title));
    // the message files, by the part of the name before the flags
    let mut files: HashMap<String, (PathBuf, u64, i64)> = HashMap::new();
    for sub in ["cur", "new"] {
        for (n, size, mtime) in files_in(ctx.db, &mb.join(sub))? {
            let base = n.split(':').next().unwrap_or(&n).to_string();
            files.insert(base, (mb.join(sub).join(&n), size, mtime));
        }
    }
    let full = ctx.depth == Depth::Full;
    // Dovecot's own cache first: no message opened for those
    if !full && let Some(c) = dovecot::cached_headers(mb, WANTED) {
        let uids: HashMap<u32, String> = dovecot::uidlist(mb)
            .unwrap_or_default()
            .into_iter()
            .map(|(u, n)| (u, n.split(':').next().unwrap_or(&n).to_string()))
            .collect();
        let mut done = 0u64;
        for m in &c.messages {
            if ctx.stopped() {
                return Ok(());
            }
            let Some((path, size, mtime)) = uids.get(&m.uid).and_then(|b| files.remove(b)) else {
                continue; // expunged, or not in the uidlist: read from the file below if it's there
            };
            let h = Head::from_cached(&m.headers);
            let fallback =
                crate::mailbox::delivered(&path.file_name().unwrap_or_default().to_string_lossy())
                    .or(Some(mtime));
            put_message(
                ctx,
                w,
                e,
                &path,
                Some(&m.uid.to_string()),
                &h,
                fallback,
                size,
                "dovecot-cache",
            )?;
            done += 1;
        }
        if done > 0 {
            ctx.report.notes.push(format!(
                "{}: {} message headers from Dovecot's index, not from the messages{}",
                mb.display(),
                fmt::count(done),
                dovecot::fts_plugin(mb)
                    .map(|p| format!(
                        "; a {p} full-text index is here too, not readable by BackDown"
                    ))
                    .unwrap_or_default()
            ));
        }
    }
    // the rest from the files
    let mut rest: Vec<(String, (PathBuf, u64, i64))> = files.into_iter().collect();
    rest.sort_by_key(|a| std::cmp::Reverse(a.1.2));
    for (i, (_, (path, size, mtime))) in rest.into_iter().enumerate() {
        if ctx.stopped() {
            return Ok(());
        }
        if i % 2000 == 0 && i > 0 {
            (ctx.progress)(&format!(
                "{}: {name}: {} messages read",
                e.title,
                fmt::count(i as u64)
            ));
        }
        let Ok(raw) = read_message(&path, full) else {
            continue;
        };
        let h = Head::from_raw(&raw, full);
        let fallback =
            crate::mailbox::delivered(&path.file_name().unwrap_or_default().to_string_lossy())
                .or(Some(mtime));
        put_message(ctx, w, e, &path, None, &h, fallback, size, "message-file")?;
    }
    Ok(())
}

/// Dovecot mdbox: mailboxes/<name>/dbox-Mails has the index and cache per mailbox; the messages
/// themselves sit in storage/m.N, one after another, each with its own small header.
fn index_mdbox(ctx: &mut Ctx, w: &mut Writer, e: &Entity, root: &Path) -> Result<()> {
    let boxes_dir = root.join("mailboxes");
    let lo = paths::dir_prefix(&boxes_dir);
    let mut hi = lo.clone();
    if let Some(l) = hi.last_mut() {
        *l += 1;
    }
    let mut boxes: Vec<PathBuf> = {
        let mut stmt = ctx.db.conn.prepare_cached(
            "SELECT path FROM dirs WHERE current = 1 AND path >= ?1 AND path < ?2 AND lname = 'dbox-mails'",
        )?;
        let rows = stmt.query_map(params![lo, hi], |r| r.get::<_, Vec<u8>>(0))?;
        rows.map(|r| r.map(|p| paths::from_bytes(&p)))
            .collect::<rusqlite::Result<_>>()?
    };
    boxes.sort();
    let mut cached = 0u64;
    for d in &boxes {
        if ctx.stopped() {
            return Ok(());
        }
        let Some(c) = dovecot::cached_headers(d, WANTED) else {
            continue;
        };
        let name = d
            .parent()
            .and_then(|p| p.strip_prefix(&boxes_dir).ok())
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        (ctx.progress)(&format!("{}: {name}", e.title));
        for m in &c.messages {
            let h = Head::from_cached(&m.headers);
            put_message(
                ctx,
                w,
                e,
                d,
                Some(&format!("{name}#{}", m.uid)),
                &h,
                None,
                0,
                "dovecot-cache",
            )?;
            cached += 1;
        }
    }
    if cached > 0 {
        ctx.report.notes.push(format!(
            "{}: {} message headers from Dovecot's index",
            root.display(),
            fmt::count(cached)
        ));
        return Ok(());
    }
    // no usable cache: walk the storage files message by message (headers only)
    let storage = root.join("storage");
    let mut files = files_in(ctx.db, &storage)?;
    files.retain(|(n, _, _)| n.starts_with("m.") && n[2..].bytes().all(|b| b.is_ascii_digit()));
    files.sort_by_key(|(n, _, _)| n[2..].parse::<u64>().unwrap_or(0));
    for (n, _, mtime) in files {
        if ctx.stopped() {
            return Ok(());
        }
        let p = storage.join(&n);
        (ctx.progress)(&format!("{}: storage/{n}", e.title));
        let Ok(mut f) = paths::open_read(&p) else {
            continue;
        };
        // "2 M<hdr size hex> C<created hex>\n", then messages: "\x01\x02N <16 hex>\n" + bytes + metadata
        let mut first = Vec::new();
        {
            let mut r = BufReader::new(&mut f);
            r.read_until(b'\n', &mut first)?;
        }
        let line = String::from_utf8_lossy(&first);
        let hdr: u64 = line
            .split_whitespace()
            .find_map(|w| w.strip_prefix('M'))
            .and_then(|h| u64::from_str_radix(h, 16).ok())
            .unwrap_or(0);
        let mut at = first.len() as u64 + hdr;
        let len = f.metadata()?.len();
        let mut n_msgs = 0u64;
        while at + 20 < len {
            f.seek(SeekFrom::Start(at))?;
            let mut mh = [0u8; 21];
            if f.read_exact(&mut mh).is_err() || &mh[..3] != b"\x01\x02N" {
                break;
            }
            let Ok(size) = u64::from_str_radix(std::str::from_utf8(&mh[4..20]).unwrap_or(""), 16)
            else {
                break;
            };
            let body_at = at + 21;
            let mut raw = vec![0u8; size.min(HEAD_CAP) as usize];
            f.read_exact(&mut raw)?;
            let head_end = raw
                .windows(2)
                .position(|w| w == b"\n\n")
                .map_or(raw.len(), |i| i + 1);
            let h = Head::from_raw(&raw[..head_end], false);
            put_message(
                ctx,
                w,
                e,
                &p,
                Some(&body_at.to_string()),
                &h,
                Some(mtime),
                size,
                "mdbox-storage",
            )?;
            n_msgs += 1;
            // the metadata block after the message: "\n\x01\x03\n" lines up to an empty line
            at = body_at + size;
            f.seek(SeekFrom::Start(at))?;
            let mut r = BufReader::new(&mut f);
            let mut l = Vec::new();
            let mut consumed = 0u64;
            loop {
                l.clear();
                let k = r.read_until(b'\n', &mut l)?;
                consumed += k as u64;
                if k == 0 || (l == b"\n" && consumed > 1) {
                    break;
                }
            }
            at += consumed;
            if n_msgs.is_multiple_of(1000) {
                (ctx.progress)(&format!(
                    "{}: storage/{n}: {} messages",
                    e.title,
                    fmt::count(n_msgs)
                ));
            }
        }
    }
    Ok(())
}

/// Thunderbird: mbox files under ImapMail/ and Mail/ (no extension; a .msf beside each).
fn index_thunderbird(ctx: &mut Ctx, w: &mut Writer, e: &Entity, profile: &Path) -> Result<()> {
    let lo = paths::dir_prefix(profile);
    let mut hi = lo.clone();
    if let Some(l) = hi.last_mut() {
        *l += 1;
    }
    let mboxes: Vec<(PathBuf, u64, i64)> = {
        let mut stmt = ctx.db.conn.prepare_cached(
            "SELECT path, size, mtime FROM files WHERE current = 1 AND path >= ?1 AND path < ?2 AND lname LIKE '%.msf'",
        )?;
        let rows = stmt.query_map(params![lo, hi], |r| {
            Ok((
                paths::from_bytes(&r.get::<_, Vec<u8>>(0)?),
                r.get::<_, i64>(1)? as u64,
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (msf, _, _) = row?;
            let mbox = msf.with_extension("");
            if let Ok(m) = std::fs::metadata(&mbox)
                && m.is_file()
                && m.len() > 0
            {
                out.push((mbox, m.len(), 0));
            }
        }
        out
    };
    let full = ctx.depth == Depth::Full;
    for (mbox, size, _) in mboxes {
        if ctx.stopped() {
            return Ok(());
        }
        (ctx.progress)(&format!(
            "{}: {}",
            e.title,
            mbox.file_name().unwrap_or_default().to_string_lossy()
        ));
        let f = paths::open_read(&mbox)?;
        let mut r = BufReader::with_capacity(1 << 16, f);
        let (mut line, mut cur, mut at, mut start, mut prev_blank) =
            (Vec::new(), Vec::new(), 0u64, 0u64, true);
        let mut in_head = true;
        let cap = if full {
            HEAD_CAP + (BODY_CAP as u64) * 4
        } else {
            HEAD_CAP
        };
        let flush = |ctx: &mut Ctx, w: &mut Writer, cur: &[u8], start: u64| -> Result<()> {
            if cur.is_empty() {
                return Ok(());
            }
            let h = Head::from_raw(cur, full);
            put_message(
                ctx,
                w,
                e,
                &mbox,
                Some(&start.to_string()),
                &h,
                None,
                0,
                "mbox",
            )
        };
        loop {
            line.clear();
            let n = peek::line_capped(&mut r, &mut line, 1 << 20)?;
            if n == 0 {
                flush(ctx, w, &cur, start)?;
                break;
            }
            if mbox::is_from_line(&line, prev_blank) && (at == 0 || prev_blank) {
                flush(ctx, w, &cur, start)?;
                cur.clear();
                start = at;
                in_head = true;
            } else {
                if mbox::blank(&line) {
                    in_head = false;
                }
                if (in_head || full) && (cur.len() as u64) < cap {
                    cur.extend_from_slice(&line);
                }
            }
            prev_blank = mbox::blank(&line);
            at += n as u64;
            if at > size + (1 << 20) {
                break;
            }
        }
    }
    Ok(())
}

// ---------- database dumps

/// A table as the dump describes it.
#[derive(Default, Debug)]
struct TableDef {
    columns: Vec<(String, String)>, // name, type (lowercase)
    fulltext: Vec<String>,
}

impl TableDef {
    /// Does column `i` hold text worth indexing (unknown columns count as text)?
    fn textual(&self, i: usize) -> bool {
        match self.columns.get(i) {
            None => true,
            Some((_, ty)) => {
                !(ty.contains("blob")
                    || ty.contains("binary")
                    || ty.contains("bytea")
                    || ty.contains("int")
                    || ty.contains("decimal")
                    || ty.contains("numeric")
                    || ty.contains("float")
                    || ty.contains("double")
                    || ty.contains("real")
                    || ty.contains("bool")
                    || ty.contains("date")
                    || ty.contains("time")
                    || ty.contains("year")
                    || ty.contains("bit")
                    || ty.contains("serial")
                    || ty.contains("uuid")
                    || ty.contains("geometry")
                    || ty.contains("point"))
            }
        }
    }
}

/// Collects a table's text and emits a document per chunk.
struct TextSink {
    table: String,
    db: String,
    buf: String,
    chunk: u32,
    total: u64,
}

fn unquote_ident(s: &str) -> String {
    s.trim()
        .trim_matches(|c| c == '`' || c == '"' || c == '\'' || c == '[' || c == ']')
        .to_string()
}

/// The database an identifier is qualified with, and its name: "`shop`.`orders`" → ("shop", "orders").
fn split_ident(s: &str) -> (Option<String>, String) {
    let s = s.trim().trim_end_matches(';');
    let mut parts: Vec<String> = Vec::new();
    let (mut cur, mut q) = (String::new(), None::<char>);
    for c in s.chars() {
        match (q, c) {
            (None, '`' | '"') => q = Some(c),
            (Some(x), c) if c == x => q = None,
            (None, '.') => {
                parts.push(std::mem::take(&mut cur));
            }
            (None, c) if c.is_whitespace() || c == '(' => break,
            (_, c) => cur.push(c),
        }
    }
    parts.push(cur);
    let name = parts.pop().unwrap_or_default();
    (parts.pop().filter(|p| !p.is_empty()), name)
}

fn index_dump(ctx: &mut Ctx, w: &mut Writer, e: &Entity, path: &Path) -> Result<()> {
    (ctx.progress)(&format!(
        "dump {}",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let f = paths::open_read(path)?;
    let mut magic = [0u8; 8];
    let n = (&f).read(&mut magic)?;
    (&f).seek(SeekFrom::Start(0))?;
    if magic[..n].starts_with(b"PGDMP") || magic[..n].starts_with(b"SQLite format 3") {
        return Ok(()); // the custom format and database files aren't text
    }
    let codec = Codec::sniff(&magic[..n]);
    let src = codec.open(f)?;
    let mut r = BufReader::with_capacity(1 << 20, src);
    let full = ctx.depth == Depth::Full;
    let mut tables: HashMap<String, TableDef> = HashMap::new();
    let mut sinks: HashMap<String, TextSink> = HashMap::new();
    // the header names the database for mysqldump and phpMyAdmin dumps; USE and \connect change it
    let mut db = recognise::sniff_dump(path)
        .and_then(|h| h.database)
        .unwrap_or_default();
    let mut text_total = 0u64;
    let mut capped = false;
    let mut line = Vec::new();
    let mut create: Option<(String, TableDef, String)> = None; // (table, def, db)
    let mut copy: Option<(String, Vec<usize>)> = None; // COPY table (columns) FROM stdin: textual column positions
    let mut lines = 0u64;
    let file_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    loop {
        line.clear();
        let n = peek::line_capped(&mut r, &mut line, 64 << 20)?;
        if n == 0 {
            break;
        }
        lines += 1;
        if lines.is_multiple_of(20_000) {
            if ctx.stopped() {
                break;
            }
            (ctx.progress)(&format!(
                "dump {file_name}: {} tables, {} of text",
                tables.len(),
                fmt::bytes(text_total)
            ));
        }
        // COPY data lines: tab-separated, "\." ends them
        if let Some((table, cols)) = &copy {
            if line.starts_with(b"\\.") {
                copy = None;
                continue;
            }
            if full && !capped {
                let t = String::from_utf8_lossy(&line);
                let sink = sinks.entry(table.clone()).or_insert_with(|| TextSink {
                    table: table.clone(),
                    db: db.clone(),
                    buf: String::new(),
                    chunk: 0,
                    total: 0,
                });
                for (i, field) in t.trim_end_matches('\n').split('\t').enumerate() {
                    if (cols.is_empty() || cols.contains(&i))
                        && field != "\\N"
                        && field.len() > 1
                        && !field
                            .bytes()
                            .all(|b| b.is_ascii_digit() || b == b'.' || b == b'-')
                    {
                        sink.buf.push_str(field);
                        sink.buf.push(' ');
                    }
                }
                text_total += flush_sink(ctx, w, e, path, sink, false)?;
                capped = text_total > TEXT_CAP;
            }
            continue;
        }
        let t = String::from_utf8_lossy(&line);
        let tl = t.trim_start();
        // a CREATE TABLE in progress: columns and keys until the closing parenthesis
        if let Some((name, def, _)) = create.as_mut() {
            let s = tl.trim_end();
            if s.starts_with(')') {
                let (name, def, cdb) = create.take().unwrap();
                put_table(ctx, w, e, path, &cdb, &name, &def)?;
                tables.insert(name, def);
                continue;
            }
            let upper = s.to_ascii_uppercase();
            if upper.starts_with("FULLTEXT") {
                if let (Some(a), Some(b)) = (s.find('('), s.rfind(')')) {
                    def.fulltext.extend(
                        s[a + 1..b]
                            .split(',')
                            .map(unquote_ident)
                            .filter(|c| !c.is_empty()),
                    );
                }
            } else if !(upper.starts_with("PRIMARY KEY")
                || upper.starts_with("KEY ")
                || upper.starts_with("UNIQUE")
                || upper.starts_with("INDEX")
                || upper.starts_with("CONSTRAINT")
                || upper.starts_with("FOREIGN")
                || upper.starts_with("CHECK")
                || upper.starts_with("SPATIAL")
                || upper.starts_with("--"))
            {
                // `name` type …  /  "name" type …  /  name type …
                let (col, rest) = match s.chars().next() {
                    Some(q @ ('`' | '"')) => {
                        let end = s[1..].find(q).map_or(s.len(), |i| i + 1);
                        (s[1..end].to_string(), s.get(end + 1..).unwrap_or(""))
                    }
                    _ => {
                        let end = s.find(char::is_whitespace).unwrap_or(s.len());
                        (s[..end].to_string(), &s[end..])
                    }
                };
                let ty = rest
                    .trim()
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if !col.is_empty() {
                    def.columns.push((col, ty));
                }
            }
            let _ = name;
            continue;
        }
        let upper: String = tl.chars().take(24).collect::<String>().to_ascii_uppercase();
        if upper.starts_with("USE ") {
            db = unquote_ident(tl[4..].trim().trim_end_matches(';'));
        } else if tl.starts_with("\\connect ") || tl.starts_with("\\c ") {
            db = tl.split_whitespace().nth(1).unwrap_or("").to_string();
        } else if upper.starts_with("CREATE TABLE") || upper.starts_with("CREATE UNLOGGED TABLE") {
            let after = tl["CREATE".len()..].trim_start();
            let after = after
                .strip_prefix("UNLOGGED")
                .map_or(after, str::trim_start);
            let after = after["TABLE".len()..].trim_start();
            let after = after
                .strip_prefix("IF NOT EXISTS")
                .map_or(after, str::trim_start);
            let (qdb, name) = split_ident(after);
            let cdb = qdb.unwrap_or_else(|| db.clone());
            if tl.trim_end().ends_with(");")
                || tl.trim_end().ends_with(")")
                    && tl.contains('(')
                    && tl.matches('(').count() == tl.matches(')').count()
            {
                // one-line table (SQLite's .dump, hand-written scripts)
                let mut def = TableDef::default();
                if let (Some(a), Some(b)) = (tl.find('('), tl.rfind(')')) {
                    for c in tl[a + 1..b].split(',') {
                        let mut it = c.trim().splitn(2, char::is_whitespace);
                        let col = unquote_ident(it.next().unwrap_or(""));
                        let ty = it.next().unwrap_or("").trim().to_ascii_lowercase();
                        if !col.is_empty()
                            && !col.eq_ignore_ascii_case("primary")
                            && !col.eq_ignore_ascii_case("unique")
                            && !col.eq_ignore_ascii_case("foreign")
                        {
                            def.columns.push((col, ty));
                        }
                    }
                }
                put_table(ctx, w, e, path, &cdb, &name, &def)?;
                tables.insert(name, def);
            } else {
                create = Some((name, TableDef::default(), cdb));
            }
        } else if upper.starts_with("INSERT INTO") || upper.starts_with("REPLACE INTO") {
            if !full || capped {
                continue;
            }
            let after = tl[tl.find("INTO").unwrap_or(0) + 4..].trim_start();
            let (_, name) = split_ident(after);
            let def = tables.get(&name);
            let Some(vals) = t.find(" VALUES").or_else(|| t.find(" values")) else {
                continue;
            };
            let sink = sinks.entry(name.clone()).or_insert_with(|| TextSink {
                table: name.clone(),
                db: db.clone(),
                buf: String::new(),
                chunk: 0,
                total: 0,
            });
            take_literals(&t[vals + 7..], def, &mut sink.buf);
            text_total += flush_sink(ctx, w, e, path, sink, false)?;
            capped = text_total > TEXT_CAP;
        } else if upper.starts_with("COPY ") && tl.contains("FROM stdin") {
            let after = tl[5..].trim_start();
            let (_, name) = split_ident(after);
            let cols = match (tl.find('('), tl.find(')')) {
                (Some(a), Some(b)) if a < b => tl[a + 1..b]
                    .split(',')
                    .map(unquote_ident)
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            let textual: Vec<usize> = match tables.get(&name) {
                Some(def) if !cols.is_empty() => cols
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| {
                        def.columns
                            .iter()
                            .position(|(n, _)| n == *c)
                            .is_none_or(|i| def.textual(i))
                    })
                    .map(|(i, _)| i)
                    .collect(),
                _ => Vec::new(),
            };
            copy = Some((name, textual));
        }
    }
    for (_, mut sink) in sinks.drain() {
        flush_sink(ctx, w, e, path, &mut sink, true)?;
    }
    if capped {
        ctx.report.notes.push(format!(
            "{}: text indexed up to {} only",
            path.display(),
            fmt::bytes(TEXT_CAP)
        ));
    }
    Ok(())
}

/// Quoted string values in an INSERT's VALUES, for the textual columns, appended to `out`.
fn take_literals(vals: &str, def: Option<&TableDef>, out: &mut String) {
    let b = vals.as_bytes();
    let (mut i, mut depth, mut col) = (0usize, 0u32, 0usize);
    while i < b.len() {
        match b[i] {
            b'(' => {
                depth += 1;
                col = 0;
                i += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b',' if depth > 0 => {
                col += 1;
                i += 1;
            }
            b'\'' | b'"' if depth > 0 => {
                let q = b[i];
                let start = i + 1;
                let mut j = start;
                let mut escaped = false;
                while j < b.len() {
                    if b[j] == b'\\' {
                        j += 2;
                        continue;
                    }
                    if b[j] == q {
                        if b.get(j + 1) == Some(&q) {
                            escaped = true;
                            j += 2;
                            continue;
                        }
                        break;
                    }
                    j += 1;
                }
                let end = j.min(b.len());
                if def.is_none_or(|d| d.textual(col)) && end > start + 1 {
                    let s = &vals[start..end];
                    let s = if s.contains('\\') || escaped {
                        s.replace("\\'", "'")
                            .replace("''", "'")
                            .replace("\\n", " ")
                            .replace("\\\"", "\"")
                    } else {
                        s.to_string()
                    };
                    if s.chars().take(64).any(char::is_alphabetic) {
                        out.push_str(&s);
                        out.push(' ');
                    }
                }
                i = end + 1;
            }
            _ => i += 1,
        }
    }
}

fn flush_sink(
    ctx: &mut Ctx,
    w: &mut Writer,
    e: &Entity,
    path: &Path,
    s: &mut TextSink,
    all: bool,
) -> Result<u64> {
    let mut written = 0u64;
    while s.buf.len() >= CHUNK || (all && !s.buf.trim().is_empty()) {
        let cut = if s.buf.len() >= CHUNK {
            s.buf[..CHUNK].rfind(' ').map_or(CHUNK, |i| i + 1)
        } else {
            s.buf.len()
        };
        let chunk: String = s.buf.drain(..cut).collect();
        s.chunk += 1;
        s.total += chunk.len() as u64;
        written += chunk.len() as u64;
        let title = format!("{} (rows)", s.table);
        w.put(&Doc {
            root_id: ctx.root_id,
            entity_key: &e.key,
            kind: "rows",
            path,
            locator: Some(&format!("{}#{}", s.table, s.chunk)),
            title: &title,
            who: &s.db,
            at: None,
            bytes: chunk.len() as u64,
            source: "dump",
            scan: ctx.scan,
            body: &chunk,
        })?;
        ctx.count("dump");
        ctx.tick(w)?;
        if !all {
            break;
        }
    }
    Ok(written)
}

fn put_table(
    ctx: &mut Ctx,
    w: &mut Writer,
    e: &Entity,
    path: &Path,
    db: &str,
    name: &str,
    def: &TableDef,
) -> Result<()> {
    let mut body = def
        .columns
        .iter()
        .map(|(c, t)| {
            if t.is_empty() {
                c.clone()
            } else {
                format!("{c} {t}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    if !def.fulltext.is_empty() {
        body.push_str(&format!("; fulltext index on {}", def.fulltext.join(", ")));
    }
    w.put(&Doc {
        root_id: ctx.root_id,
        entity_key: &e.key,
        kind: "table",
        path,
        locator: Some(name),
        title: name,
        who: db,
        at: None,
        bytes: 0,
        source: "dump",
        scan: ctx.scan,
        body: &body,
    })?;
    ctx.count("dump");
    ctx.tick(w)
}

// ---------- SQLite files

fn open_sqlite_ro(path: &Path) -> Result<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    // immutable: no locks, no journal, nothing written even to a read-only medium
    let uri = format!(
        "file:{}?immutable=1",
        path.display()
            .to_string()
            .replace('?', "%3F")
            .replace('#', "%23")
    );
    Ok(rusqlite::Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?)
}

/// Tables in a SQLite file: (name, sql, is an FTS virtual table).
fn sqlite_tables(c: &rusqlite::Connection) -> Result<Vec<(String, String, bool)>> {
    let mut stmt = c.prepare("SELECT name, COALESCE(sql, '') FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (name, sql) = row?;
        let up = sql.to_ascii_uppercase();
        let fts = up.contains("VIRTUAL TABLE") && up.contains("USING FTS");
        out.push((name, sql, fts));
    }
    // FTS shadow tables aren't tables to a person
    let fts_names: Vec<String> = out
        .iter()
        .filter(|(_, _, f)| *f)
        .map(|(n, _, _)| n.clone())
        .collect();
    out.retain(|(n, _, f)| *f || !fts_names.iter().any(|x| n.starts_with(&format!("{x}_"))));
    Ok(out)
}

fn index_sqlite(ctx: &mut Ctx, w: &mut Writer, e: &Entity, path: &Path) -> Result<()> {
    let c = open_sqlite_ro(path)?;
    let full = ctx.depth == Depth::Full;
    let mut text_total = 0u64;
    let mut fts_tables = 0;
    for (name, sql, is_fts) in sqlite_tables(&c)? {
        if ctx.stopped() {
            return Ok(());
        }
        let cols: Vec<(String, String)> = {
            let mut stmt = c.prepare(&format!(
                "PRAGMA table_info(\"{}\")",
                name.replace('"', "\"\"")
            ))?;
            stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?.to_ascii_lowercase(),
                ))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        let rows: i64 = c
            .query_row(
                &format!("SELECT COUNT(*) FROM \"{}\"", name.replace('"', "\"\"")),
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let mut body = cols
            .iter()
            .map(|(n, t)| format!("{n} {t}"))
            .collect::<Vec<_>>()
            .join(", ");
        if is_fts {
            fts_tables += 1;
            body.push_str("; a full-text index (searched in place)");
            let _ = sql;
        }
        w.put(&Doc {
            root_id: ctx.root_id,
            entity_key: &e.key,
            kind: if is_fts { "sqlite-fts" } else { "table" },
            path,
            locator: Some(&name),
            title: &name,
            who: &format!("{} rows", fmt::count(rows.max(0) as u64)),
            at: None,
            bytes: 0,
            source: if is_fts { "sqlite-fts" } else { "sqlite" },
            scan: ctx.scan,
            body: &body,
        })?;
        ctx.count(if is_fts { "sqlite-fts" } else { "sqlite" });
        if !full || is_fts || text_total > TEXT_CAP {
            continue;
        }
        // the text columns' values, in chunks
        let def = TableDef {
            columns: cols.clone(),
            fulltext: Vec::new(),
        };
        let textual: Vec<&str> = cols
            .iter()
            .enumerate()
            .filter(|(i, _)| def.textual(*i))
            .map(|(_, (n, _))| n.as_str())
            .collect();
        if textual.is_empty() {
            continue;
        }
        let sel = textual
            .iter()
            .map(|n| format!("\"{}\"", n.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = c.prepare(&format!(
            "SELECT {sel} FROM \"{}\"",
            name.replace('"', "\"\"")
        ))?;
        let mut rows = stmt.query([])?;
        let mut sink = TextSink {
            table: name.clone(),
            db: String::new(),
            buf: String::new(),
            chunk: 0,
            total: 0,
        };
        while let Some(row) = rows.next()? {
            for i in 0..textual.len() {
                if let Ok(Some(s)) = row.get::<_, Option<String>>(i)
                    && s.len() > 1
                    && s.chars().take(64).any(char::is_alphabetic)
                {
                    sink.buf.push_str(&s);
                    sink.buf.push(' ');
                }
            }
            if sink.buf.len() >= CHUNK {
                text_total += flush_sink(ctx, w, e, path, &mut sink, false)?;
                if text_total > TEXT_CAP || ctx.stopped() {
                    break;
                }
            }
        }
        flush_sink(ctx, w, e, path, &mut sink, true)?;
    }
    if fts_tables > 0 {
        ctx.report.notes.push(format!(
            "{}: {fts_tables} full-text table{} searched in place",
            path.display(),
            if fts_tables == 1 { "" } else { "s" }
        ));
    }
    Ok(())
}

/// A search inside the SQLite full-text tables the index knows of: (file, table, matching rows).
fn search_sqlite_fts(db: &Db, q: &str, limit: usize) -> Vec<Value> {
    let words: Vec<String> = q
        .split_whitespace()
        .map(|w| format!("\"{}\"", w.replace('"', "")))
        .filter(|w| w.len() > 2)
        .collect();
    if words.is_empty() {
        return Vec::new();
    }
    let match_q = words.join(" ");
    let Ok(mut stmt) = db.conn.prepare(
        "SELECT d.root_id, r.path, d.entity_key, d.path, d.key, d.locator FROM content_docs d JOIN roots r ON r.id = d.root_id WHERE d.kind = 'sqlite-fts' LIMIT 200",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            paths::from_bytes(&r.get::<_, Vec<u8>>(1)?),
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<String>>(5)?,
        ))
    }) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (root_id, root, entity_key, path, key, table) in rows.flatten() {
        let Some(table) = table else { continue };
        let p = Path::new(&path);
        if !p.is_file() {
            continue;
        }
        let Ok(c) = open_sqlite_ro(p) else { continue };
        let tq = format!("\"{}\"", table.replace('"', "\"\""));
        let n: i64 = match c.query_row(
            &format!(
                "SELECT COUNT(*) FROM (SELECT rowid FROM {tq} WHERE {tq} MATCH ?1 LIMIT 10000)"
            ),
            params![match_q],
            |r| r.get(0),
        ) {
            Ok(n) => n,
            Err(_) => continue,
        };
        if n > 0 {
            out.push(json!({
                "root_id": root_id, "root": root.display().to_string(), "entity_key": entity_key,
                "kind": "sqlite-rows", "path": path, "key": key, "locator": table,
                "title": format!("{} matching row{} in {table}", fmt::count(n as u64), if n == 1 { "" } else { "s" }),
                "who": p.file_name().unwrap_or_default().to_string_lossy(), "at": Value::Null, "bytes": 0,
                "source": "sqlite-fts", "dir_key": browse::key(p.parent().unwrap_or(p)),
            }));
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

// ---------- mongodump folders and MySQL data directories: names only

fn index_mongo_dump(ctx: &mut Ctx, w: &mut Writer, e: &Entity, root: &Path) -> Result<()> {
    let own = files_in(ctx.db, root)?;
    let dbs: Vec<PathBuf> = if own
        .iter()
        .any(|(n, _, _)| n.ends_with(".bson") || n.ends_with(".bson.gz"))
    {
        vec![root.to_path_buf()]
    } else {
        let mut stmt = ctx
            .db
            .conn
            .prepare_cached("SELECT path FROM dirs WHERE parent = ?1 AND current = 1")?;
        let rows = stmt.query_map(params![paths::to_bytes(root)], |r| r.get::<_, Vec<u8>>(0))?;
        rows.map(|r| r.map(|p| paths::from_bytes(&p)))
            .collect::<rusqlite::Result<_>>()?
    };
    for d in dbs {
        let dbname = d
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        for (n, size, mtime) in files_in(ctx.db, &d)? {
            let Some(coll) = n
                .strip_suffix(".bson")
                .or_else(|| n.strip_suffix(".bson.gz"))
            else {
                continue;
            };
            // the metadata file lists the collection's indexes: their keys are its searchable fields
            let meta = recognise::read_small(&d.join(format!("{coll}.metadata.json")), 1 << 20)
                .unwrap_or_default();
            let keys: Vec<String> = meta
                .split("\"key\"")
                .skip(1)
                .filter_map(|s| s.split('{').nth(1))
                .filter_map(|s| s.split('}').next())
                .flat_map(|s| {
                    s.split(',')
                        .filter_map(|kv| kv.split(':').next())
                        .map(|k| k.trim().trim_matches('"').to_string())
                })
                .filter(|k| !k.is_empty())
                .collect();
            let body = if keys.is_empty() {
                String::new()
            } else {
                format!("indexed fields: {}", keys.join(", "))
            };
            w.put(&Doc {
                root_id: ctx.root_id,
                entity_key: &e.key,
                kind: "table",
                path: &d.join(&n),
                locator: Some(coll),
                title: coll,
                who: &dbname,
                at: Some(mtime),
                bytes: size,
                source: "mongodump",
                scan: ctx.scan,
                body: &body,
            })?;
            ctx.count("mongodump");
        }
    }
    Ok(())
}

fn index_mysql_data(ctx: &mut Ctx, w: &mut Writer, e: &Entity, root: &Path) -> Result<()> {
    let mut stmt = ctx
        .db
        .conn
        .prepare_cached("SELECT path FROM dirs WHERE parent = ?1 AND current = 1")?;
    let dbs: Vec<PathBuf> = stmt
        .query_map(params![paths::to_bytes(root)], |r| r.get::<_, Vec<u8>>(0))?
        .map(|r| r.map(|p| paths::from_bytes(&p)))
        .collect::<rusqlite::Result<_>>()?;
    for d in dbs {
        let dbname = d
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if matches!(
            dbname.as_str(),
            "mysql" | "performance_schema" | "sys" | "information_schema"
        ) || dbname.starts_with('#')
        {
            continue;
        }
        let mut seen: Vec<String> = Vec::new();
        for (n, size, mtime) in files_in(ctx.db, &d)? {
            let lower = n.to_ascii_lowercase();
            let Some(table) = lower
                .strip_suffix(".ibd")
                .or_else(|| lower.strip_suffix(".frm"))
                .or_else(|| lower.strip_suffix(".myd"))
            else {
                continue;
            };
            if seen.iter().any(|t| t == table) {
                continue;
            }
            seen.push(table.to_string());
            w.put(&Doc {
                root_id: ctx.root_id,
                entity_key: &e.key,
                kind: "table",
                path: &d.join(&n),
                locator: Some(table),
                title: table,
                who: &dbname,
                at: Some(mtime),
                bytes: size,
                source: "mysql-data",
                scan: ctx.scan,
                body: "",
            })?;
            ctx.count("mysql-data");
        }
    }
    Ok(())
}

// ---------- searching

/// Search contents: the FTS index, plus SQLite full-text tables searched in place.
pub fn search(
    db: &Db,
    q: &str,
    kind: Option<&str>,
    root_id: Option<i64>,
    limit: usize,
) -> Result<Value> {
    let found = fts::search(
        db,
        &fts::Query {
            q,
            kind,
            root_id,
            entity_key: None,
            limit,
        },
    )?;
    let mut v = serde_json::to_value(&found)?;
    // the folder to open for each hit, with the file selected
    for h in v["hits"].as_array_mut().into_iter().flatten() {
        let p = PathBuf::from(h["path"].as_str().unwrap_or(""));
        let dir =
            if h["kind"] == "message" && h["locator"].as_str().is_some_and(|l| l.contains('#')) {
                p.clone() // an mdbox mailbox folder
            } else {
                p.parent().map(Path::to_path_buf).unwrap_or(p.clone())
            };
        h["dir_key"] = json!(browse::key(&dir));
    }
    if kind.is_none_or(|k| k == "sqlite-rows") && !q.trim().is_empty() {
        let extra = search_sqlite_fts(db, q, 50);
        if !extra.is_empty() {
            let n = extra.len() as u64;
            if let Some(hits) = v["hits"].as_array_mut() {
                hits.extend(extra);
            }
            v["total"] = json!(found.total + n);
        }
    }
    // drives whose names make the hits readable
    v["roots"] = json!(
        browse::roots(db)?
            .into_iter()
            .map(|r| json!({ "id": r.id, "path": r.path, "name": r.name, "key": r.key }))
            .collect::<Vec<_>>()
    );
    Ok(v)
}

/// What's indexed: per drive, kind and source.
pub fn status(db: &Db) -> Result<Value> {
    let cov = fts::coverage(db)?;
    let docs: u64 = cov.iter().map(|c| c.docs).sum();
    Ok(json!({ "depth": depth(db).as_str(), "docs": docs, "coverage": cov }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_of_textual_columns_only() {
        let def = TableDef {
            columns: vec![
                ("id".into(), "int".into()),
                ("name".into(), "varchar(80)".into()),
                ("blob".into(), "longblob".into()),
                ("note".into(), "text".into()),
            ],
            ..TableDef::default()
        };
        let mut out = String::new();
        take_literals(
            "(1,'Alice O\\'Brien','\\0\\0binary',\"a note\"),(2,'Bob','x','');",
            Some(&def),
            &mut out,
        );
        assert_eq!(out, "Alice O'Brien a note Bob ");
        let mut out = String::new();
        take_literals("(1,'It''s fine')", None, &mut out);
        assert_eq!(out, "It's fine ");
    }

    #[test]
    fn identifiers_split() {
        assert_eq!(
            split_ident("`shop`.`orders` ("),
            (Some("shop".into()), "orders".into())
        );
        assert_eq!(
            split_ident("public.users (id"),
            (Some("public".into()), "users".into())
        );
        assert_eq!(split_ident("\"Users\";"), (None, "Users".into()));
        assert_eq!(
            mailbox_name(Path::new("/m/alice/Maildir/.Sent.2020")),
            "Sent/2020"
        );
        assert_eq!(mailbox_name(Path::new("/m/alice/Maildir")), "INBOX");
        assert_eq!(
            strip_tags("<p>Hello <b>world</b></p>\n<br>bye", 100),
            "Hello world bye"
        );
    }
}

// ---------- the command

/// `backdown search`: build when asked, then find.
#[allow(clippy::too_many_arguments)]
pub fn cli(
    db: &Db,
    q: &str,
    kind: Option<&str>,
    build_first: bool,
    force: bool,
    depth_arg: Option<&str>,
    limit: usize,
    json: bool,
) -> Result<std::process::ExitCode> {
    use std::process::ExitCode;
    if let Some(dp) = depth_arg {
        let dp = Depth::parse(dp)?;
        set_depth(db, dp)?;
        if !json {
            eprintln!(
                "content indexing depth: {} (the service follows it from the next scan)",
                dp.as_str()
            );
        }
    }
    if build_first {
        let stop = AtomicBool::new(false);
        let d = depth(db);
        let rep = build(db, None, d, force, &stop, &mut |s| {
            crate::live(&format!("indexing: {s}"))
        })?;
        crate::clear();
        if json && q.trim().is_empty() {
            println!("{}", serde_json::to_string_pretty(&rep)?);
            return Ok(ExitCode::SUCCESS);
        }
        if !json {
            eprintln!(
                "indexed {} document{} from {} thing{} ({} unchanged), depth {}",
                fmt::count(rep.docs),
                if rep.docs == 1 { "" } else { "s" },
                rep.entities,
                if rep.entities == 1 { "" } else { "s" },
                rep.skipped_fresh,
                d.as_str()
            );
            for n in &rep.notes {
                eprintln!("  {n}");
            }
        }
    }
    if q.trim().is_empty() {
        let st = status(db)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&st)?);
        } else {
            println!(
                "Content index: {} documents, depth {}",
                fmt::count(st["docs"].as_u64().unwrap_or(0)),
                st["depth"].as_str().unwrap_or("")
            );
            for c in st["coverage"].as_array().into_iter().flatten() {
                println!(
                    "  {:<40} {:<12} {:<14} {:>10} docs in {} thing{}",
                    c["root"].as_str().unwrap_or(""),
                    c["kind"].as_str().unwrap_or(""),
                    c["source"].as_str().unwrap_or(""),
                    fmt::count(c["docs"].as_u64().unwrap_or(0)),
                    c["entities"].as_u64().unwrap_or(0),
                    if c["entities"].as_u64() == Some(1) {
                        ""
                    } else {
                        "s"
                    }
                );
            }
            if st["docs"].as_u64() == Some(0) {
                println!(
                    "Nothing indexed yet: `backdown search --build`, or let the service do it after its next scan."
                );
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    let v = search(db, q, kind, None, limit)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(ExitCode::SUCCESS);
    }
    let hits = v["hits"].as_array().cloned().unwrap_or_default();
    let total = v["total"].as_u64().unwrap_or(0);
    if hits.is_empty() {
        println!("Nothing matches \"{q}\" in the indexed contents.");
        return Ok(ExitCode::from(1));
    }
    println!(
        "{} match{}{} ({} ms)",
        fmt::count(total),
        if total == 1 { "" } else { "es" },
        if total > hits.len() as u64 {
            format!(", the best {} shown", hits.len())
        } else {
            String::new()
        },
        v["ms"].as_u64().unwrap_or(0)
    );
    for h in &hits {
        let at = h["at"].as_i64().map(fmt::day).unwrap_or_default();
        let root = h["root"].as_str().unwrap_or("");
        let path = h["path"].as_str().unwrap_or("");
        let rel = Path::new(path)
            .strip_prefix(root)
            .map_or(path.to_string(), |p| p.display().to_string());
        println!(
            "  {:<8} {:<48} {:<36} {}",
            h["kind"].as_str().unwrap_or(""),
            clip(h["title"].as_str().unwrap_or(""), 48),
            clip(h["who"].as_str().unwrap_or(""), 36),
            at
        );
        println!(
            "           {}:{}{}",
            Path::new(root)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.to_string()),
            rel,
            h["locator"]
                .as_str()
                .filter(|l| !l.is_empty() && h["kind"] != "message")
                .map(|l| format!(" ({l})"))
                .unwrap_or_default()
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() > max {
        let cut: String = s.chars().take(max - 1).collect();
        format!("{cut}…")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod build_tests {
    use super::*;
    use backdown_core::index;

    fn write(p: &Path, b: &[u8]) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b).unwrap();
    }

    /// A drive with a Maildir, a dump and a SQLite file with a full-text table; indexed and
    /// recognised, then its contents indexed and searched.
    #[test]
    fn indexes_mail_dumps_and_sqlite_then_finds() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("drive");
        let mb = d.join("mail/example.org/alice/Maildir");
        for f in ["cur", "new", "tmp"] {
            std::fs::create_dir_all(mb.join(f)).unwrap();
        }
        write(
            &mb.join("cur/1500000000.M1P1.host:2,S"),
            b"From: Bob <bob@example.org>\nTo: alice@example.org\nSubject: Invoice for the garden work\nDate: Fri, 14 Jul 2017 10:00:00 +0000\nMessage-ID: <1@example.org>\n\nPlease find the invoice attached. Roses and tulips.\n",
        );
        write(
            &mb.join("new/1600000000.M2P2.host"),
            b"From: carol@example.net\nTo: alice@example.org\nSubject: Re: holiday photos\n\nHere they are\n",
        );
        write(&mb.join("dovecot-uidlist"), b"3 V1 N3\n");
        // the same user's second mailbox makes a store of two, so recognition finds a maildir
        write(
            &d.join("mail/example.org/bob/Maildir/cur/1.M3P3.host:2,"),
            b"Subject: x\n\ny",
        );
        for f in ["new", "tmp"] {
            std::fs::create_dir_all(d.join("mail/example.org/bob/Maildir").join(f)).unwrap();
        }
        write(
            &d.join("dumps/shop.sql"),
            format!(
                "-- MariaDB dump 10.19\n-- Host: db1    Database: shop\n\nCREATE TABLE `customers` (\n  `id` int(11) NOT NULL,\n  `name` varchar(80) DEFAULT NULL,\n  `notes` text,\n  `photo` longblob,\n  PRIMARY KEY (`id`),\n  FULLTEXT KEY `ft` (`notes`)\n) ENGINE=InnoDB;\n\nINSERT INTO `customers` VALUES (1,'Alice Example','Likes marigolds very much','\\0\\0bin'),(2,'Bob','Allergic to pollen',NULL);\n{}",
                "-- filler\n".repeat(120)
            )
            .as_bytes(),
        );
        {
            std::fs::create_dir_all(d.join("data")).unwrap();
            let c = rusqlite::Connection::open(d.join("data/notes.sqlite")).unwrap();
            c.execute_batch(
                "CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT, body TEXT);
                 INSERT INTO notes(title, body) VALUES ('Shopping', 'buy saffron and cardamom'), ('Ideas', 'a lighthouse at dawn');
                 CREATE VIRTUAL TABLE memo USING fts5(text);
                 INSERT INTO memo(text) VALUES ('the quick brown fox'), ('saffron risotto recipe');",
            )
            .unwrap();
        }
        let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
        index::index(
            &mut db,
            &d,
            &index::Options {
                one_file_system: true,
                allow_empty: false,
            },
            &mut |_| {},
        )
        .unwrap();
        let root = recognise::root_ids(&db).unwrap()[0];
        let r = recognise::recognise(&db, root, true, &mut |_| {}).unwrap();
        recognise::store(&db, &r).unwrap();
        assert!(
            r.entities.iter().any(|e| e.kind == "sqlite-db"),
            "{:?}",
            r.entities.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        let stop = AtomicBool::new(false);
        let rep = build(&db, None, Depth::Headers, false, &stop, &mut |_| {}).unwrap();
        assert!(
            rep.notes.iter().any(|n| n.contains("full-text table")),
            "{:?}",
            rep.notes
        );
        let hits = |q: &str| search(&db, q, None, None, 20).unwrap();
        // mail by subject, sender, recipient
        let v = hits("invoice");
        assert_eq!(v["total"], 1, "{v}");
        assert_eq!(v["hits"][0]["kind"], "message");
        assert_eq!(v["hits"][0]["title"], "Invoice for the garden work");
        assert!(
            v["hits"][0]["who"]
                .as_str()
                .unwrap()
                .contains("bob@example.org")
        );
        assert_eq!(v["hits"][0]["source"], "message-file");
        assert!(v["hits"][0]["dir_key"].is_string());
        assert_eq!(hits("carol@example.net")["total"], 1);
        assert_eq!(
            hits("holiday")["hits"][0]["at"],
            1_600_000_000,
            "no Date header: the delivery time from the file name"
        );
        // the dump's tables, with the columns the database itself indexed for search
        let v = hits("customers");
        assert!(
            v["hits"]
                .as_array()
                .unwrap()
                .iter()
                .any(|h| h["kind"] == "table" && h["who"] == "shop"),
            "{v}"
        );
        assert_eq!(
            hits("marigolds")["total"],
            0,
            "row text isn't indexed at headers depth"
        );
        // SQLite: tables by name, and its own full-text table searched in place
        assert!(
            hits("notes").as_object().unwrap()["hits"]
                .as_array()
                .unwrap()
                .iter()
                .any(|h| h["kind"] == "table")
        );
        let v = hits("saffron");
        let kinds: Vec<&str> = v["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["sqlite-rows"], "{v}");
        assert!(
            v["hits"][0]["title"]
                .as_str()
                .unwrap()
                .starts_with("1 matching row in memo")
        );
        // nothing is redone for the same scan
        let rep2 = build(&db, None, Depth::Headers, false, &stop, &mut |_| {}).unwrap();
        assert_eq!(rep2.docs, 0);
        assert!(rep2.skipped_fresh >= 3, "{rep2:?}");
        // full depth: the rows' text too (text columns only), from dumps and SQLite tables
        let rep3 = build(&db, None, Depth::Full, true, &stop, &mut |_| {}).unwrap();
        assert!(rep3.docs > rep.docs, "{rep3:?} vs {rep:?}");
        let v = hits("marigolds");
        assert_eq!(v["hits"][0]["kind"], "rows", "{v}");
        assert_eq!(v["hits"][0]["who"], "shop");
        assert_eq!(hits("lighthouse")["hits"][0]["kind"], "rows");
        assert_eq!(hits("bin")["total"], 0, "blob columns are left out");
        assert_eq!(hits("tulips")["total"], 1, "the body, at full depth");
        // coverage, and forgetting a drive's documents
        let st = status(&db).unwrap();
        assert!(st["docs"].as_u64().unwrap() >= 6);
        assert!(fts::forget(&db, root, None).unwrap() > 0);
        assert_eq!(hits("invoice")["total"], 0);
    }
}
