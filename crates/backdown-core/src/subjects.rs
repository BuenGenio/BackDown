//! Who and what the recognised things are about: people and databases, across every drive.
//!
//! An entity (recognise.rs) is a place: a folder or file with what it is. A *subject* is what
//! those places are about, and it isn't bound to one path: the same person has a mailbox in the
//! mail server's store on one drive, a Nextcloud account on another and a home folder in a
//! system image on a third; the same database was dumped every night for years, and its live
//! data directory sits in a server backup. BackDown gathers those *mentions* into one subject per
//! person or database, so the question "what do I have of Alice?" or "which copies of the shop
//! database are there?" has one answer.
//!
//! Each kind of subject is one extractor function (`EXTRACTORS`) over the entities of one root
//! and the index; adding a kind means adding a function and a line in `KINDS`. Mentions carry
//! typed aliases (an email address, a login on a host, a database's engine and name) and
//! subjects that share one are the same subject: merging happens per root here and across roots
//! when they're read (`all`), so a drive that's scanned again only recomputes its own.
//!
//! Stored like entities: one JSON document per root (`subjects`), with the scan it describes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::recognise::{self, Entity, RootEntities};
use crate::{Db, browse, fmt, mailstores, paths};

/// Kinds, with a label for one and for many, in the order they're listed.
pub const KINDS: &[(&str, &str, &str)] = &[
    ("person", "Person", "People"),
    ("database", "Database", "Databases"),
];

type Extractor = fn(&Ctx) -> Result<Vec<Subject>>;

/// One extractor per kind, run in this order.
const EXTRACTORS: &[(&str, Extractor)] = &[("person", people), ("database", databases)];

/// A name a subject goes by. Aliases of an identifying kind (`identifies`) make two subjects one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Alias {
    /// "email", "login" (with `scope` the host or service), "name" (a person's full name, which
    /// never identifies on its own), "database" (engine family and name)
    pub kind: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl Alias {
    fn new(kind: &str, value: impl Into<String>, scope: Option<String>) -> Self {
        Alias {
            kind: kind.into(),
            value: value.into(),
            scope,
        }
    }
    fn identifies(&self) -> bool {
        matches!(self.kind.as_str(), "email" | "login" | "database")
    }
    /// what two aliases must share to be the same
    fn id(&self) -> String {
        format!(
            "{}:{}:{}",
            self.kind,
            self.value.trim().to_lowercase(),
            self.scope.as_deref().unwrap_or("").trim().to_lowercase()
        )
    }
    /// "alice@example.org", "alice on cloud.example.org", "Alice Example"
    pub fn show(&self) -> String {
        match (&self.kind[..], &self.scope) {
            ("login", Some(s)) => format!("{} on {s}", self.value),
            _ => self.value.clone(),
        }
    }
}

/// One place a subject appears.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Mention {
    pub root_id: i64,
    pub root: String,
    /// the recognised thing it's in
    pub entity_key: String,
    pub entity_kind: String,
    pub entity_label: String,
    pub entity_title: String,
    /// the place itself (the mailbox folder, the dump file), which may be the entity
    pub path: String,
    pub key: String,
    pub is_file: bool,
    /// what it is here: "Mailbox", "Nextcloud account", "Home folder", "Mail identity",
    /// "Website contact", "Dump", "Live data", "Database file", "Used by"
    pub role: String,
    /// one line more: "1,204 messages", "MariaDB, dumped 4 Mar 2021", "WordPress site shop.example.org"
    pub detail: String,
    pub files: u64,
    pub bytes: u64,
    pub oldest: Option<i64>,
    pub newest: Option<i64>,
}

/// A person, a database, … with every place it appears.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Subject {
    /// see `KINDS`
    pub kind: String,
    pub label: String,
    /// stable across roots: the kind and its first identifying alias
    pub key: String,
    /// for people: "Alice Example", else the best alias
    pub name: String,
    pub aliases: Vec<Alias>,
    /// derived from the mentions when the subject is put together ("Mailboxes" → "3, 12,040 messages")
    pub facts: Vec<(String, String)>,
    pub mentions: Vec<Mention>,
}

/// Everything found on one root, as of one scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootSubjects {
    pub root_id: i64,
    pub root: String,
    pub scan: i64,
    pub computed_at: i64,
    pub read_drive: bool,
    pub subjects: Vec<Subject>,
}

/// What an extractor works from.
pub struct Ctx<'a> {
    pub db: &'a Db,
    pub root: &'a RootEntities,
    pub read_drive: bool,
}

fn label(kind: &str) -> String {
    KINDS
        .iter()
        .find(|(k, _, _)| *k == kind)
        .map_or(kind, |(_, l, _)| l)
        .to_string()
}

/// "People" for "person".
pub fn plural(kind: &str) -> String {
    KINDS
        .iter()
        .find(|(k, _, _)| *k == kind)
        .map_or(kind, |(_, _, l)| l)
        .to_string()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Everything on one root, from what was recognised there.
pub fn compute(db: &Db, r: &RootEntities, read_drive: bool) -> Result<RootSubjects> {
    let read_drive = read_drive && Path::new(&r.root).is_dir();
    let ctx = Ctx {
        db,
        root: r,
        read_drive,
    };
    let mut all = Vec::new();
    for (_, f) in EXTRACTORS {
        all.extend(f(&ctx)?);
    }
    Ok(RootSubjects {
        root_id: r.root_id,
        root: r.root.clone(),
        scan: r.scan,
        computed_at: now(),
        read_drive,
        subjects: merge(all),
    })
}

// ---------- building blocks

/// A subject as an extractor makes it: one mention, its aliases, its facts; merged later.
fn one(kind: &str, aliases: Vec<Alias>, facts: Vec<(&str, String)>, m: Mention) -> Subject {
    let mut s = Subject {
        kind: kind.into(),
        label: label(kind),
        key: String::new(),
        name: String::new(),
        aliases,
        facts: facts.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        mentions: vec![m],
    };
    name_and_key(&mut s);
    s
}

fn mention(ctx: &Ctx, e: &Entity, p: &Path, is_file: bool, role: &str, detail: String) -> Mention {
    Mention {
        root_id: ctx.root.root_id,
        root: ctx.root.root.clone(),
        entity_key: e.key.clone(),
        entity_kind: e.kind.clone(),
        entity_label: e.label.clone(),
        entity_title: e.title.clone(),
        path: p.display().to_string(),
        key: browse::key(p),
        is_file,
        role: role.into(),
        detail,
        files: 0,
        bytes: 0,
        oldest: None,
        newest: None,
    }
}

/// The entity itself as the place.
fn at_entity(ctx: &Ctx, e: &Entity, role: &str, detail: String) -> Mention {
    let mut m = mention(ctx, e, Path::new(&e.path), e.is_file, role, detail);
    m.files = e.files;
    m.bytes = e.bytes;
    m.oldest = e.oldest;
    m.newest = e.newest;
    m
}

fn fact<'a>(e: &'a Entity, k: &str) -> Option<&'a str> {
    e.facts
        .iter()
        .find(|(key, _)| key == k)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
}

/// A folder's totals from the index.
fn dir_totals(db: &Db, path: &[u8]) -> Result<Option<(u64, u64)>> {
    Ok(db
        .conn
        .query_row(
            "SELECT files, bytes FROM dirs WHERE path = ?1 AND current = 1",
            params![path],
            |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
        )
        .optional()?)
}

fn dir_exists(db: &Db, path: &[u8]) -> Result<bool> {
    Ok(db
        .conn
        .query_row(
            "SELECT 1 FROM dirs WHERE path = ?1 AND current = 1",
            params![path],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Files directly in a folder: (name, size, mtime).
fn files_in(db: &Db, dir: &[u8]) -> Result<Vec<(String, u64, i64)>> {
    let sep = recognise::sep();
    let mut stmt = db.conn.prepare_cached(
        "SELECT path, size, mtime FROM files WHERE dir = ?1 AND current = 1 ORDER BY path",
    )?;
    let rows = stmt.query_map(params![dir], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, i64>(1)? as u64,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (p, size, mtime) = row?;
        if let Some(rest) = p.strip_prefix(dir).and_then(|r| r.strip_prefix(&sep[..])) {
            out.push((
                paths::from_bytes(rest).to_string_lossy().into_owned(),
                size,
                mtime,
            ));
        }
    }
    Ok(out)
}

/// A mailbox's messages (files in cur/ and new/, in it and its subfolders), its bytes, and the
/// dates of the oldest and newest.
fn measure_mailbox(db: &Db, mb: &[u8]) -> Result<(u64, u64, Option<i64>, Option<i64>)> {
    let p = paths::from_bytes(mb);
    let lo = paths::dir_prefix(&p);
    let mut hi = lo.clone();
    if let Some(last) = hi.last_mut() {
        *last += 1;
    }
    let sep = recognise::sep();
    let cur = [&sep[..], b"cur"].concat();
    let new = [&sep[..], b"new"].concat();
    let mut stmt = db.conn.prepare_cached(
        "SELECT dir, size, mtime FROM files WHERE current = 1 AND path >= ?1 AND path < ?2",
    )?;
    let rows = stmt.query_map(params![lo, hi], |r| {
        Ok((
            r.get::<_, Option<Vec<u8>>>(0)?,
            r.get::<_, i64>(1)? as u64,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let (mut n, mut bytes, mut oldest, mut newest) = (0u64, 0u64, None::<i64>, None::<i64>);
    for row in rows {
        let (dir, size, mtime) = row?;
        bytes += size;
        if dir.is_some_and(|d| d.ends_with(&cur) || d.ends_with(&new)) {
            n += 1;
            if mtime > 0 {
                oldest = Some(oldest.map_or(mtime, |o: i64| o.min(mtime)));
                newest = Some(newest.map_or(mtime, |o: i64| o.max(mtime)));
            }
        }
    }
    Ok((n, bytes, oldest, newest))
}

/// Is this folder a Maildir mailbox (cur/ in it, or in a Maildir/ inside it)? The mailbox folder.
fn as_mailbox(db: &Db, dir: &[u8]) -> Result<Option<Vec<u8>>> {
    if dir_exists(db, &recognise::join(dir, "cur"))? {
        return Ok(Some(dir.to_vec()));
    }
    let md = recognise::join(dir, "Maildir");
    if dir_exists(db, &recognise::join(&md, "cur"))? {
        return Ok(Some(md));
    }
    Ok(None)
}

fn is_email(s: &str) -> bool {
    let Some((l, d)) = s.split_once('@') else {
        return false;
    };
    !l.is_empty() && d.contains('.') && !d.ends_with('.') && !s.contains(char::is_whitespace)
}

/// "Alice Example <alice@example.org>" → (Some("Alice Example"), "alice@example.org")
fn split_identity(s: &str) -> (Option<String>, String) {
    match s.rsplit_once('<') {
        Some((name, addr)) => (
            Some(name.trim().trim_matches('"').to_string()).filter(|n| !n.is_empty()),
            addr.trim_end_matches('>').trim().to_string(),
        ),
        None => (None, s.trim().to_string()),
    }
}

/// The host of the system an entity sits in, if a `system` entity with one encloses it.
fn host_of(e: &Entity, all: &[Entity]) -> Option<String> {
    let mut at = e.inside.as_deref();
    while let Some(k) = at {
        let c = all.iter().find(|x| x.key == k)?;
        if c.kind == "system"
            && let Some(h) = fact(c, "Host name")
        {
            return Some(h.to_string());
        }
        at = c.inside.as_deref();
    }
    None
}

// ---------- people

fn people(ctx: &Ctx) -> Result<Vec<Subject>> {
    let db = ctx.db;
    let es = &ctx.root.entities;
    let mut out = Vec::new();
    // the Nextcloud servers on this drive: where their users were reachable, for the data dirs
    let nc_servers: Vec<&Entity> = es.iter().filter(|e| e.kind == "nextcloud-server").collect();
    for e in es {
        let p = Path::new(&e.path);
        let pb = paths::to_bytes(p);
        match e.kind.as_str() {
            // store/<domain>/<user>[/Maildir]: one mailbox per user
            "mail-store" => {
                let mut boxes: Vec<(Vec<u8>, String, Option<String>)> = Vec::new();
                let own = recognise::name_of(&pb);
                for c in recognise::children(db, &pb)? {
                    let cp = recognise::join(&pb, &c);
                    if let Some(mb) = as_mailbox(db, &cp)? {
                        let dom = recognise::looks_like_domain(&own).then(|| own.clone());
                        boxes.push((mb, c, dom));
                        continue;
                    }
                    for u in recognise::children(db, &cp)? {
                        let up = recognise::join(&cp, &u);
                        if let Some(mb) = as_mailbox(db, &up)? {
                            let dom = recognise::looks_like_domain(&c).then(|| c.clone());
                            boxes.push((mb, u, dom));
                        }
                    }
                }
                for (mb, user, dom) in boxes {
                    out.push(mailbox_person(ctx, e, &mb, &user, dom)?);
                }
            }
            "maildir" => {
                let user = recognise::name_of(&pb);
                let dom = recognise::parent_of(&pb)
                    .map(|d| recognise::name_of(&d))
                    .filter(|d| recognise::looks_like_domain(d));
                let mb = as_mailbox(db, &pb)?.unwrap_or_else(|| pb.clone());
                out.push(mailbox_person(ctx, e, &mb, &user, dom)?);
            }
            // prefs.js names the accounts and the identities that send from them
            "thunderbird" if ctx.read_drive => {
                let Some(text) = recognise::read_small(&p.join("prefs.js"), 4 << 20) else {
                    continue;
                };
                let prefs = mailstores::parse_prefs(&text);
                for a in mailstores::accounts(&prefs) {
                    let mut seen = BTreeSet::new();
                    for id in a.identities.iter().chain(a.emails.iter()) {
                        let (name, addr) = split_identity(id);
                        if !is_email(&addr) || !seen.insert(addr.to_lowercase()) {
                            continue;
                        }
                        let mut aliases = vec![Alias::new("email", &addr, None)];
                        if let Some(n) = name {
                            aliases.push(Alias::new("name", n, None));
                        }
                        let detail = match a.kind.as_str() {
                            "none" => format!("{} (local folders)", a.name),
                            k if !a.host.is_empty() => {
                                format!("{} ({} on {})", a.name, k.to_uppercase(), a.host)
                            }
                            _ => a.name.clone(),
                        };
                        let mut m = at_entity(ctx, e, "Mail identity", detail);
                        if !a.dir.is_empty() {
                            let d = p.join(&a.dir);
                            m.path = d.display().to_string();
                            m.key = browse::key(&d);
                            if let Some((f, b)) = dir_totals(db, &paths::to_bytes(&d))? {
                                m.files = f;
                                m.bytes = b;
                            }
                        }
                        out.push(one("person", aliases, vec![], m));
                    }
                }
            }
            // every user folder with files/ in it is an account
            "nextcloud" => {
                let server = nc_servers
                    .iter()
                    .find(|s| {
                        fact(s, "Data directory")
                            .is_some_and(|d| Path::new(d).file_name() == p.file_name())
                    })
                    .or(nc_servers.first());
                let address = server.and_then(|s| fact(s, "Address")).map(str::to_string);
                let mail_domain = server.and_then(|s| fact(s, "Mail domain")).map(str::to_string);
                for u in recognise::children(db, &pb)? {
                    if u.starts_with("appdata_")
                        || u.starts_with("updater-")
                        || u.starts_with('.')
                        || u.starts_with("files_external")
                    {
                        continue;
                    }
                    let up = recognise::join(&pb, &u);
                    if !dir_exists(db, &recognise::join(&up, "files"))? {
                        continue;
                    }
                    let mut aliases = Vec::new();
                    if is_email(&u) {
                        aliases.push(Alias::new("email", &u, None));
                    } else {
                        aliases.push(Alias::new(
                            "login",
                            &u,
                            Some(address.clone().unwrap_or_else(|| "Nextcloud".into())),
                        ));
                        if let Some(d) = &mail_domain {
                            aliases.push(Alias::new("email", format!("{u}@{d}"), None));
                        }
                    }
                    let (files, bytes) = dir_totals(db, &up)?.unwrap_or_default();
                    let mut m = mention(
                        ctx,
                        e,
                        &paths::from_bytes(&up),
                        false,
                        "Nextcloud account",
                        match &address {
                            Some(a) => format!("{} of files on {a}", fmt::bytes(bytes)),
                            None => format!("{} of files", fmt::bytes(bytes)),
                        },
                    );
                    m.files = files;
                    m.bytes = bytes;
                    out.push(one("person", aliases, vec![], m));
                }
            }
            "home-mac" | "home-linux" | "home-windows" => {
                let user = recognise::name_of(&pb);
                if user.is_empty()
                    || matches!(
                        user.to_ascii_lowercase().as_str(),
                        "shared" | "public" | "default" | "default user" | "all users" | "guest" | "root"
                    )
                {
                    continue;
                }
                let host = host_of(e, es);
                let os = match e.kind.as_str() {
                    "home-mac" => "a Mac",
                    "home-linux" => "Linux",
                    _ => "Windows",
                };
                let detail = match &host {
                    Some(h) => format!("{os} home on {h}"),
                    None => format!("{os} home folder"),
                };
                let aliases = vec![Alias::new("login", &user, host)];
                out.push(one("person", aliases, vec![], at_entity(ctx, e, "Home folder", detail)));
            }
            "website" => {
                if let Some(c) = fact(e, "Contact").filter(|c| is_email(c)) {
                    let mut aliases = vec![Alias::new("email", c, None)];
                    if let Some(n) = fact(e, "Contact name") {
                        aliases.push(Alias::new("name", n, None));
                    }
                    let m = at_entity(ctx, e, "Website contact", e.title.clone());
                    out.push(one("person", aliases, vec![], m));
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

fn mailbox_person(
    ctx: &Ctx,
    e: &Entity,
    mb: &[u8],
    user: &str,
    domain: Option<String>,
) -> Result<Subject> {
    let (messages, bytes, oldest, newest) = measure_mailbox(ctx.db, mb)?;
    let aliases = match &domain {
        Some(d) if !is_email(user) => vec![Alias::new("email", format!("{user}@{d}"), None)],
        _ if is_email(user) => vec![Alias::new("email", user, None)],
        _ => vec![Alias::new(
            "login",
            user,
            Some(fact(e, "Source host").unwrap_or("mail").to_string()),
        )],
    };
    let mut m = mention(
        ctx,
        e,
        &paths::from_bytes(mb),
        false,
        "Mailbox",
        format!("{} messages", fmt::count(messages)),
    );
    m.files = messages;
    m.bytes = bytes;
    m.oldest = oldest;
    m.newest = newest;
    Ok(one("person", aliases, vec![], m))
}

// ---------- databases

/// "MariaDB", "MySQL or MariaDB", "PostgreSQL (custom format)" → the family a name lives in.
fn family(engine: &str) -> &'static str {
    let e = engine.to_ascii_lowercase();
    if e.contains("mysql") || e.contains("maria") {
        "mysql"
    } else if e.contains("postgres") {
        "postgresql"
    } else if e.contains("mongo") {
        "mongodb"
    } else if e.contains("sqlite") {
        "sqlite"
    } else {
        "sql"
    }
}

fn family_label(f: &str) -> &'static str {
    match f {
        "mysql" => "MySQL or MariaDB",
        "postgresql" => "PostgreSQL",
        "mongodb" => "MongoDB",
        "sqlite" => "SQLite",
        _ => "SQL",
    }
}

/// "shop_2021-03-04", "backup-shop-20210304T0300", "shop.2021.03.04" → "shop"
pub fn name_from_file(stem: &str) -> Option<String> {
    let parts: Vec<&str> = stem
        .split(|c: char| c == '_' || c == '-' || c == '.' || c == ' ')
        .filter(|w| !w.is_empty())
        .filter(|w| {
            // dates, times and stamps are all digits (with T for times); "backup" says nothing
            let digits = w.bytes().filter(u8::is_ascii_digit).count();
            !(digits == w.len() || (digits >= 4 && digits + 1 >= w.len()))
                && !matches!(
                    w.to_ascii_lowercase().as_str(),
                    "backup" | "dump" | "db" | "database" | "full" | "daily" | "weekly" | "latest"
                )
        })
        .collect();
    let name = parts.join("_");
    (!name.is_empty()).then_some(name)
}

fn databases(ctx: &Ctx) -> Result<Vec<Subject>> {
    let db = ctx.db;
    let mut out = Vec::new();
    for e in &ctx.root.entities {
        let p = Path::new(&e.path);
        let pb = paths::to_bytes(p);
        match e.kind.as_str() {
            "sql-dump" if e.is_file => {
                let engine = fact(e, "Engine").map(str::to_string);
                let name = fact(e, "Database")
                    .map(str::to_string)
                    .or_else(|| name_from_file(recognise::dump_stem(&recognise::name_of(&pb))));
                let Some(name) = name else { continue };
                let host = fact(e, "Host").map(str::to_string);
                let dumped = fact(e, "Dumped").map(str::to_string);
                out.push(dump_subject(ctx, e, at_entity(ctx, e, "Dump", String::new()), &name, engine, host, dumped));
            }
            // a folder of dumps: each file is a dump of something
            "sql-dump" => {
                let mut n = 0;
                for (name, size, mtime) in files_in(db, &pb)? {
                    if !recognise::is_sql_dump(&name.to_lowercase()) {
                        continue;
                    }
                    let fp = p.join(&name);
                    // the drive says more, for the first few hundred
                    n += 1;
                    let head = (ctx.read_drive && n <= 300)
                        .then(|| recognise::sniff_dump(&fp))
                        .flatten()
                        .unwrap_or_default();
                    let dbn = head
                        .database
                        .clone()
                        .or_else(|| name_from_file(recognise::dump_stem(&name)));
                    let Some(dbn) = dbn else { continue };
                    let mut m = mention(ctx, e, &fp, true, "Dump", String::new());
                    m.files = 1;
                    m.bytes = size;
                    m.oldest = head.date.or(Some(mtime));
                    m.newest = m.oldest;
                    out.push(dump_subject(
                        ctx,
                        e,
                        m,
                        &dbn,
                        head.engine.map(str::to_string),
                        head.host.clone(),
                        head.date.map(fmt::day),
                    ));
                }
            }
            // dump/<database>/<collection>.bson, or one database's folder
            "mongo-dump" => {
                let own = files_in(db, &pb)?;
                let dbs: Vec<(Vec<u8>, String)> = if own.iter().any(|(n, _, _)| n.ends_with(".bson") || n.ends_with(".bson.gz")) {
                    vec![(pb.clone(), recognise::name_of(&pb))]
                } else {
                    recognise::children(db, &pb)?
                        .into_iter()
                        .map(|c| (recognise::join(&pb, &c), c))
                        .collect()
                };
                for (dp, name) in dbs {
                    let files = files_in(db, &dp)?;
                    let colls: Vec<&(String, u64, i64)> = files
                        .iter()
                        .filter(|(n, _, _)| n.ends_with(".bson") || n.ends_with(".bson.gz"))
                        .collect();
                    if colls.is_empty() {
                        continue;
                    }
                    let bytes: u64 = colls.iter().map(|(_, s, _)| s).sum();
                    let newest = colls.iter().map(|(_, _, t)| *t).max().filter(|t| *t > 0);
                    let mut m = mention(
                        ctx,
                        e,
                        &paths::from_bytes(&dp),
                        false,
                        "Dump",
                        format!(
                            "MongoDB, {} collection{}{}",
                            colls.len(),
                            if colls.len() == 1 { "" } else { "s" },
                            newest.map(|t| format!(", {}", fmt::day(t))).unwrap_or_default()
                        ),
                    );
                    m.files = colls.len() as u64;
                    m.bytes = bytes;
                    m.oldest = newest;
                    m.newest = newest;
                    out.push(one(
                        "database",
                        vec![Alias::new("database", &name, Some("mongodb".into()))],
                        vec![("Engine", "MongoDB".into())],
                        m,
                    ));
                }
            }
            // every folder with table files is a database
            "mysql-data" => {
                let engine = fact(e, "Engine").unwrap_or("MySQL or MariaDB").to_string();
                for c in recognise::children(db, &pb)? {
                    if c.starts_with('#')
                        || c.starts_with('.')
                        || matches!(
                            c.to_ascii_lowercase().as_str(),
                            "mysql" | "performance_schema" | "sys" | "information_schema"
                        )
                    {
                        continue;
                    }
                    let cp = recognise::join(&pb, &c);
                    let files = files_in(db, &cp)?;
                    let (mut frm, mut data, mut bytes) = (0u64, 0u64, 0u64);
                    for (n, s, _) in &files {
                        match n.rsplit_once('.').map(|(_, x)| x.to_ascii_lowercase()).as_deref() {
                            Some("frm") => frm += 1,
                            Some("ibd" | "myd" | "mad") => data += 1,
                            _ => continue,
                        }
                        bytes += s;
                    }
                    let tables = frm.max(data);
                    if tables == 0 {
                        continue;
                    }
                    let mut m = mention(
                        ctx,
                        e,
                        &paths::from_bytes(&cp),
                        false,
                        "Live data",
                        format!(
                            "{engine}, {tables} table{}",
                            if tables == 1 { "" } else { "s" }
                        ),
                    );
                    m.files = tables;
                    m.bytes = bytes;
                    m.newest = files.iter().map(|(_, _, t)| *t).max().filter(|t| *t > 0);
                    out.push(one(
                        "database",
                        vec![Alias::new("database", &c, Some("mysql".into()))],
                        vec![("Engine", engine.clone())],
                        m,
                    ));
                }
            }
            "sqlite-db" => {
                let name = recognise::name_of(&pb);
                let m = at_entity(ctx, e, "Database file", fmt::bytes(e.bytes));
                out.push(one(
                    "database",
                    vec![Alias::new("database", name.to_lowercase(), Some("sqlite".into()))],
                    vec![("Engine", "SQLite".into())],
                    m,
                ));
            }
            // what uses a database, from its configuration
            "website" | "nextcloud-server" => {
                let Some(name) = fact(e, "Database") else { continue };
                let engine = fact(e, "Database engine").unwrap_or("SQL").to_string();
                let fam = family(&engine);
                let mut facts = vec![("Engine", engine.clone())];
                if let Some(h) = fact(e, "Database host") {
                    facts.push(("Host", h.to_string()));
                }
                let m = at_entity(ctx, e, "Used by", e.title.clone());
                out.push(one(
                    "database",
                    vec![Alias::new("database", name, Some(fam.into()))],
                    facts,
                    m,
                ));
            }
            _ => {}
        }
    }
    Ok(out)
}

fn dump_subject(
    _ctx: &Ctx,
    _e: &Entity,
    mut m: Mention,
    name: &str,
    engine: Option<String>,
    host: Option<String>,
    dumped: Option<String>,
) -> Subject {
    let fam = engine.as_deref().map_or("sql", family);
    let eng = engine.clone().unwrap_or_else(|| "SQL".into());
    m.detail = match (&dumped, m.newest) {
        (Some(d), _) => format!("{eng}, dumped {d}"),
        (None, Some(t)) => format!("{eng}, {}", fmt::day(t)),
        _ => eng.clone(),
    };
    let mut facts = vec![("Engine", eng)];
    if let Some(h) = host {
        facts.push(("Host", h));
    }
    one(
        "database",
        vec![Alias::new("database", name, Some(fam.into()))],
        facts,
        m,
    )
}

// ---------- merging

/// Subjects that share an identifying alias become one; the result is named and summarised.
pub fn merge(subjects: Vec<Subject>) -> Vec<Subject> {
    // union-find over alias ids
    let n = subjects.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], i: usize) -> usize {
        let mut i = i;
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    let mut by_alias: HashMap<String, usize> = HashMap::new();
    for (i, s) in subjects.iter().enumerate() {
        for a in s.aliases.iter().filter(|a| a.identifies()) {
            let id = format!("{}|{}", s.kind, a.id());
            match by_alias.get(&id) {
                Some(&j) => {
                    let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                    if ri != rj {
                        parent[ri] = rj;
                    }
                }
                None => {
                    by_alias.insert(id, i);
                }
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut out = Vec::new();
    let mut taken: Vec<Option<Subject>> = subjects.into_iter().map(Some).collect();
    for (_, members) in groups {
        let mut it = members.into_iter();
        let mut s = taken[it.next().unwrap()].take().unwrap();
        for j in it {
            let o = taken[j].take().unwrap();
            for a in o.aliases {
                if !s.aliases.contains(&a) {
                    s.aliases.push(a);
                }
            }
            for (k, v) in o.facts {
                if !s.facts.iter().any(|(x, _)| *x == k) {
                    s.facts.push((k, v));
                }
            }
            for m in o.mentions {
                if !s
                    .mentions
                    .iter()
                    .any(|x| x.root_id == m.root_id && x.key == m.key && x.role == m.role)
                {
                    s.mentions.push(m);
                }
            }
        }
        name_and_key(&mut s);
        summarise(&mut s);
        out.push(s);
    }
    // the most material first
    out.sort_by(|a, b| {
        let ka = KINDS.iter().position(|(k, _, _)| *k == a.kind);
        let kb = KINDS.iter().position(|(k, _, _)| *k == b.kind);
        ka.cmp(&kb)
            .then(b.mentions.len().cmp(&a.mentions.len()))
            .then_with(|| {
                let bytes = |s: &Subject| s.mentions.iter().map(|m| m.bytes).sum::<u64>();
                bytes(b).cmp(&bytes(a))
            })
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

fn name_and_key(s: &mut Subject) {
    // emails before logins before the rest, so the key is the same whichever root saw it first
    let order = |a: &Alias| match a.kind.as_str() {
        "email" => 0,
        "database" => 0,
        "login" => 1,
        _ => 2,
    };
    s.aliases.sort_by(|a, b| order(a).cmp(&order(b)).then(a.id().cmp(&b.id())));
    s.aliases.dedup_by(|a, b| a.id() == b.id());
    let first = s.aliases.iter().find(|a| a.identifies());
    s.key = match first {
        Some(a) => format!("{}:{}", s.kind, a.id()),
        None => format!("{}:{}", s.kind, s.mentions.first().map_or("", |m| &m.key)),
    };
    s.name = s
        .aliases
        .iter()
        .find(|a| a.kind == "name")
        .or(first)
        .or(s.aliases.first())
        .map(|a| a.value.clone())
        .unwrap_or_else(|| "?".into());
}

/// Facts from the mentions: what there is of this subject, in one line per role.
fn summarise(s: &mut Subject) {
    let derived = [
        "Email addresses",
        "Logins",
        "Mailboxes",
        "Nextcloud accounts",
        "Home folders",
        "Mail identities",
        "Website contact",
        "Dumps",
        "Live data",
        "Database files",
        "Used by",
        "Drives",
        "Engine",
    ];
    let mut kept: Vec<(String, String)> = s
        .facts
        .iter()
        .filter(|(k, _)| !derived.contains(&k.as_str()) || k == "Engine")
        .cloned()
        .collect();
    let engine = kept.iter().position(|(k, _)| k == "Engine").map(|i| kept.remove(i).1);
    let mut facts: Vec<(String, String)> = Vec::new();
    if s.kind == "person" {
        let emails: Vec<String> = s
            .aliases
            .iter()
            .filter(|a| a.kind == "email")
            .map(|a| a.value.clone())
            .collect();
        if !emails.is_empty() {
            facts.push(("Email addresses".into(), list(&emails, 6)));
        }
        let logins: Vec<String> = s
            .aliases
            .iter()
            .filter(|a| a.kind == "login")
            .map(Alias::show)
            .collect();
        if !logins.is_empty() {
            facts.push(("Logins".into(), list(&logins, 6)));
        }
    }
    if s.kind == "database" {
        // every engine the mentions agree on, the most specific first
        let mut engines: Vec<String> = s
            .mentions
            .iter()
            .filter(|m| matches!(m.role.as_str(), "Dump" | "Live data"))
            .filter_map(|m| m.detail.split(',').next())
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty() && e != "SQL")
            .collect();
        if let Some(e) = engine {
            engines.insert(0, e);
        }
        engines.retain(|e| !e.chars().next().is_some_and(|c| c.is_ascii_digit()));
        let mut uniq: Vec<String> = Vec::new();
        for e in engines {
            if !uniq.contains(&e) {
                uniq.push(e);
            }
        }
        // "MySQL or MariaDB" says less than "MariaDB": drop it when something says more
        if uniq.len() > 1 {
            uniq.retain(|e| e != "MySQL or MariaDB" && e != "SQL");
        }
        let fam = s
            .aliases
            .iter()
            .find(|a| a.kind == "database")
            .and_then(|a| a.scope.clone())
            .unwrap_or_default();
        facts.push((
            "Engine".into(),
            if uniq.is_empty() {
                family_label(&fam).to_string()
            } else {
                uniq.join(", ")
            },
        ));
    }
    // one line per role, in the order the roles are listed in `Mention`
    let roles = [
        ("Mailbox", "Mailboxes", "messages"),
        ("Nextcloud account", "Nextcloud accounts", "files"),
        ("Home folder", "Home folders", "files"),
        ("Mail identity", "Mail identities", ""),
        ("Website contact", "Website contact", ""),
        ("Dump", "Dumps", ""),
        ("Live data", "Live data", "tables"),
        ("Database file", "Database files", ""),
        ("Used by", "Used by", ""),
    ];
    for (role, plural, unit) in roles {
        let ms: Vec<&Mention> = s.mentions.iter().filter(|m| m.role == role).collect();
        if ms.is_empty() {
            continue;
        }
        let n = ms.len();
        let files: u64 = ms.iter().map(|m| m.files).sum();
        let bytes: u64 = ms.iter().map(|m| m.bytes).sum();
        let oldest = ms.iter().filter_map(|m| m.oldest).min();
        let newest = ms.iter().filter_map(|m| m.newest).max();
        let mut parts = vec![if role == "Used by" {
            ms.iter()
                .map(|m| m.entity_title.as_str())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            n.to_string()
        }];
        if !unit.is_empty() && files > 0 {
            parts.push(format!("{} {unit}", fmt::count(files)));
        }
        if bytes > 0 {
            parts.push(fmt::bytes(bytes));
        }
        if let (Some(a), Some(b)) = (oldest, newest) {
            let (ya, yb) = (year(a), year(b));
            parts.push(if ya == yb {
                ya
            } else {
                format!("{ya}–{yb}")
            });
        }
        facts.push((plural.into(), parts.join(", ")));
    }
    let drives: BTreeSet<i64> = s.mentions.iter().map(|m| m.root_id).collect();
    if drives.len() > 1 {
        facts.push(("Drives".into(), drives.len().to_string()));
    }
    facts.extend(kept);
    s.facts = facts;
}

fn year(t: i64) -> String {
    fmt::day(t)
        .rsplit(' ')
        .next()
        .unwrap_or("")
        .to_string()
}

fn list(items: &[String], max: usize) -> String {
    let mut s = items
        .iter()
        .take(max)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if items.len() > max {
        s += &format!(" and {} more", items.len() - max);
    }
    s
}

// ---------- storage

/// Keep the result for the browser and reports. Needs a writable connection.
pub fn store(db: &Db, r: &RootSubjects) -> Result<()> {
    db.conn.execute(
        "INSERT INTO subjects(root_id, scan, computed_at, data) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(root_id) DO UPDATE SET scan = excluded.scan, computed_at = excluded.computed_at, data = excluded.data",
        params![r.root_id, r.scan, r.computed_at, serde_json::to_string(r)?],
    )?;
    Ok(())
}

/// What's stored for a root, and whether it describes the root's latest scan.
pub fn load(db: &Db, root_id: i64) -> Result<Option<(RootSubjects, bool)>> {
    let row: Option<(String, i64)> = db
        .conn
        .query_row(
            "SELECT s.data, r.last_scan FROM subjects s JOIN roots r ON r.id = s.root_id WHERE s.root_id = ?1",
            params![root_id],
            |r| Ok((r.get(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0))),
        )
        .optional()?;
    Ok(match row {
        Some((data, last)) => {
            let r: RootSubjects = serde_json::from_str(&data).context("reading stored subjects")?;
            let fresh = r.scan == last;
            Some((r, fresh))
        }
        None => None,
    })
}

/// Recognise-and-store for one root: entities first, then what they're about.
pub fn refresh(db: &Db, r: &RootEntities, read_drive: bool) -> Result<RootSubjects> {
    let s = compute(db, r, read_drive)?;
    store(db, &s).context("saving who and what things are about")?;
    Ok(s)
}

/// Everything stored, merged across roots.
#[derive(Debug, Clone, Serialize)]
pub struct All {
    pub subjects: Vec<Subject>,
    /// roots whose stored result is older than their latest scan
    pub stale: Vec<String>,
    /// roots with a finished scan but nothing stored yet
    pub missing: Vec<String>,
    pub roots: usize,
}

pub fn all(db: &Db) -> Result<All> {
    let mut subjects = Vec::new();
    let mut stale = Vec::new();
    let mut missing = Vec::new();
    let ids = recognise::root_ids(db)?;
    for id in &ids {
        match load(db, *id)? {
            Some((r, fresh)) => {
                if !fresh {
                    stale.push(r.root.clone());
                }
                subjects.extend(r.subjects);
            }
            None => {
                let root: Vec<u8> = db.conn.query_row(
                    "SELECT path FROM roots WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )?;
                missing.push(paths::from_bytes(&root).display().to_string());
            }
        }
    }
    Ok(All {
        subjects: merge(subjects),
        stale,
        missing,
        roots: ids.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_dump_files() {
        assert_eq!(name_from_file("shop_2021-03-04").as_deref(), Some("shop"));
        assert_eq!(name_from_file("backup-shop-20210304T0300").as_deref(), Some("shop"));
        assert_eq!(name_from_file("shop.2021.03.04").as_deref(), Some("shop"));
        assert_eq!(name_from_file("wp_prod").as_deref(), Some("wp_prod"));
        assert_eq!(name_from_file("2021-03-04"), None);
        assert_eq!(name_from_file("all-databases-2020").as_deref(), Some("all_databases"));
    }

    #[test]
    fn identities_split() {
        assert_eq!(
            split_identity("Alice Example <alice@example.org>"),
            (Some("Alice Example".into()), "alice@example.org".into())
        );
        assert_eq!(split_identity("bob@example.org"), (None, "bob@example.org".into()));
        assert!(is_email("a@b.co"));
        assert!(!is_email("alice"));
        assert!(!is_email("a@b"));
    }

    fn subj(kind: &str, aliases: Vec<Alias>, role: &str, root: i64) -> Subject {
        one(
            kind,
            aliases,
            vec![],
            Mention {
                root_id: root,
                root: format!("/r{root}"),
                entity_key: format!("e{root}"),
                entity_kind: "x".into(),
                entity_label: "X".into(),
                entity_title: "x".into(),
                path: format!("/r{root}/{role}"),
                key: format!("k{root}{role}"),
                is_file: false,
                role: role.into(),
                detail: String::new(),
                files: 10,
                bytes: 100,
                oldest: Some(1_500_000_000),
                newest: Some(1_600_000_000),
            },
        )
    }

    #[test]
    fn merging_by_alias() {
        let a = subj(
            "person",
            vec![Alias::new("email", "alice@example.org", None)],
            "Mailbox",
            1,
        );
        let b = subj(
            "person",
            vec![
                Alias::new("login", "alice", Some("cloud.example.org".into())),
                Alias::new("email", "Alice@Example.org", None),
            ],
            "Nextcloud account",
            2,
        );
        let c = subj(
            "person",
            vec![
                Alias::new("email", "alice@example.org", None),
                Alias::new("name", "Alice Example", None),
            ],
            "Mail identity",
            1,
        );
        let d = subj("person", vec![Alias::new("email", "bob@example.org", None)], "Mailbox", 1);
        let merged = merge(vec![a, b, c, d]);
        assert_eq!(merged.len(), 2, "{merged:#?}");
        let alice = &merged[0];
        assert_eq!(alice.name, "Alice Example");
        assert_eq!(alice.key, "person:email:alice@example.org:");
        assert_eq!(alice.mentions.len(), 3);
        assert!(alice.facts.iter().any(|(k, v)| k == "Mailboxes" && v.starts_with("1, 10 messages")), "{:?}", alice.facts);
        assert!(alice.facts.iter().any(|(k, v)| k == "Drives" && v == "2"));
        assert_eq!(merged[1].name, "bob@example.org");
        // a name alone never merges two people
        let x = subj("person", vec![Alias::new("name", "J Smith", None)], "Home folder", 1);
        let y = subj("person", vec![Alias::new("name", "J Smith", None)], "Home folder", 2);
        assert_eq!(merge(vec![x, y]).len(), 2);
        // databases: the same name in the same family, whatever said it
        let d1 = subj("database", vec![Alias::new("database", "shop", Some("mysql".into()))], "Dump", 1);
        let d2 = subj("database", vec![Alias::new("database", "Shop", Some("mysql".into()))], "Used by", 2);
        let d3 = subj("database", vec![Alias::new("database", "shop", Some("postgresql".into()))], "Dump", 2);
        let m = merge(vec![d1, d2, d3]);
        assert_eq!(m.len(), 2, "{m:#?}");
        assert_eq!(m[0].mentions.len(), 2);
    }
}
