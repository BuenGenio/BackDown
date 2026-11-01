//! Other machines: a NAS or another computer whose own BackDown index is read over ssh.
//!
//! BackDown never listens on the network for this. Another machine is reached with the system's
//! own `ssh` (the person's keys, agent, known_hosts and ~/.ssh/config), which runs
//! `backdown serve-stdio` there: a read-only answerer speaking newline-delimited JSON (`serve`)
//! that can do nothing the person couldn't do by running `backdown` there themselves.
//!
//! What each machine's index holds is cached here (`sync`): its drives, and its current files with
//! their size, modification time and hashes, pulled incrementally by that machine's scan ids. With
//! it, duplicates, overlaps and "is this safe to wipe" can count a copy on another machine, but
//! only conservatively:
//!
//! 1. by SHA-256 only, never by name or size;
//! 2. only if that machine confirmed the file within the proof window (a setting, 7 days unless
//!    changed): its own scan saw it there, or it re-read the file for us;
//! 3. and a wipe deletes a file on the strength of a copy on another machine only after that
//!    machine has re-read its copy just then and it matched (`Verifier`); trusting the index, only
//!    after it has checked the copy is still there as scanned.
//!
//! Sizes and samples of the files there count as possible copies when hashing here (hash.rs), and
//! the files there whose size matches a file here are sampled, then hashed, by that machine on
//! request (without writing to its index), so a file whose only copy is on the NAS gets proved.
//!
//! Each sync also fetches that machine's drive health as its own index has it (`fetch_health`):
//! its drives' latest readings and verdicts, and its own latest drive-health notifications. A
//! drive there whose verdict got worse since the last sync becomes a notification here; the
//! first sight of a machine's drives never does. A BackDown there from before the `health`
//! request is noted ("update BackDown on nas to see its drive health"), and its sync goes on.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::alerts::{self, Limits, Note};
use crate::health::{self, Change, Health, Verdict};
use crate::{Db, browse, fmt, paths, report};

pub mod serve;
#[cfg(test)]
mod tests;

/// The protocol's version: `serve` and `sync` must agree on it.
pub const PROTO: u32 = 1;
/// The sample algorithm's version (hash.rs): samples from a machine with another aren't compared.
pub const SAMPLE: u32 = 1;
/// Remote copies count as proof when confirmed this many days ago at most, unless changed.
pub const PROOF_DAYS: i64 = 7;
/// Each machine is synced this often, unless changed.
pub const SYNC_HOURS: i64 = 6;

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

// ---------- addresses and names

/// Where a machine is: `ssh://[user@]host[:port]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Target {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
}

impl Target {
    pub fn url(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!(
            "ssh://{}{host}{}",
            self.user
                .as_ref()
                .map(|u| format!("{u}@"))
                .unwrap_or_default(),
            self.port.map(|p| format!(":{p}")).unwrap_or_default()
        )
    }
}

/// Read `ssh://[user@]host[:port]`, strictly: a host or user that could be taken for an ssh option
/// (`-oProxyCommand=…`), or holds anything but the characters host names and user names are made
/// of, is refused.
pub fn parse_url(url: &str) -> Result<Target> {
    let rest = url.trim().strip_prefix("ssh://").context(
        "a machine's address looks like ssh://nas.example.org or ssh://me@nas.example.org:2222",
    )?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.is_empty() {
        bail!("the address has no host name");
    }
    if rest.contains('/') || rest.contains('?') || rest.contains('#') {
        bail!(
            "the address is just ssh://[user@]host[:port]; say where the index is there with --remote-db"
        );
    }
    let (user, hostport) = match rest.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, rest),
    };
    if let Some(u) = user {
        let ok = !u.is_empty()
            && u.len() <= 64
            && !u.starts_with('-')
            && u.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
        if !ok {
            bail!("{u:?} isn't a user name ssh would take (letters, digits, . _ -)");
        }
    }
    let (host, port) = if let Some(b) = hostport.strip_prefix('[') {
        let (h, after) = b
            .split_once(']')
            .context("an IPv6 address needs its closing ]")?;
        let ok = !h.is_empty()
            && h.len() <= 64
            && h.contains(':')
            && h.chars()
                .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.' || c == '%');
        if !ok {
            bail!("[{h}] isn't an IPv6 address");
        }
        let port = match after {
            "" => None,
            p => Some(
                p.strip_prefix(':')
                    .context("expected :port after the address")?,
            ),
        };
        (h.to_string(), port)
    } else {
        let (h, p) = match hostport.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport, None),
        };
        let ok = !h.is_empty()
            && h.len() <= 253
            && !h.starts_with('-')
            && !h.starts_with('.')
            && h.chars()
                .all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c));
        if !ok {
            bail!(
                "{h:?} isn't a host name (letters, digits, . - _, not starting with - or .): it could be taken for an ssh option"
            );
        }
        (h.to_string(), p)
    };
    let port = port
        .map(|p| {
            p.parse::<u16>()
                .ok()
                .filter(|&n| n > 0)
                .with_context(|| format!("{p:?} isn't a port number"))
        })
        .transpose()?;
    Ok(Target {
        user: user.map(str::to_string),
        host,
        port,
    })
}

/// A machine's name: what the browser and the CLI call it.
pub fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    if !ok {
        bail!(
            "a machine's name is 1 to 32 letters, digits, . _ or -, starting with a letter or digit (like nas or office-pc)"
        );
    }
    Ok(())
}

/// A path on the other machine (its index, or where backdown is there): anything a file can be
/// called, except control characters and a leading `-`. It's quoted for that machine's shell.
pub fn check_remote_path(p: &str, what: &str) -> Result<()> {
    if p.is_empty() || p.len() > 1024 {
        bail!("{what} can't be empty or longer than 1024 bytes");
    }
    if p.starts_with('-') {
        bail!("{what} can't start with - (it could be taken for an option)");
    }
    if p.chars().any(char::is_control) {
        bail!("{what} can't contain control characters");
    }
    Ok(())
}

// ---------- the machines

/// How paths are spelled there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Unix,
    Windows,
}

#[derive(Debug, Clone, Serialize)]
pub struct Machine {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub db: Option<String>,
    pub command: Option<String>,
    pub created_at: i64,
    pub last_try: Option<i64>,
    pub last_sync: Option<i64>,
    pub last_error: Option<String>,
    /// what it said about itself last time: version, host, index, os, clock
    pub info: Value,
}

impl Machine {
    pub fn target(&self) -> Result<Target> {
        parse_url(&self.url)
    }
    pub fn os(&self) -> Os {
        if self.info["os"] == "windows" {
            Os::Windows
        } else {
            Os::Unix
        }
    }
    /// its clock minus ours, in seconds, as of the last contact
    pub fn skew(&self) -> i64 {
        self.info["skew"].as_i64().unwrap_or(0)
    }
    /// A path there, for people.
    pub fn display(&self, raw: &[u8]) -> String {
        display(self.os(), raw)
    }
    /// "nas (ssh://nas.example.org)"
    pub fn label(&self) -> String {
        format!("{} ({})", self.name, self.url)
    }
}

pub fn display(os: Os, raw: &[u8]) -> String {
    match os {
        Os::Unix => String::from_utf8_lossy(raw).into_owned(),
        Os::Windows => {
            let wide: Vec<u16> = raw
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            String::from_utf16_lossy(&wide)
        }
    }
}

fn sep(os: Os) -> &'static [u8] {
    match os {
        Os::Unix => b"/",
        Os::Windows => b"\\\0",
    }
}

/// The folder holding `raw` there (its own path when it has none).
pub fn parent(os: Os, raw: &[u8]) -> &[u8] {
    let s = sep(os);
    // separators, where they can be (UTF-16 code units on Windows)
    let seps: Vec<usize> = (0..raw.len().saturating_sub(s.len() - 1))
        .step_by(s.len())
        .filter(|&i| raw[i..].starts_with(s))
        .collect();
    match seps.as_slice() {
        [] => raw,
        // the top keeps its separator: "/" or "C:\"
        [only] => &raw[..only + s.len()],
        [.., last] => &raw[..*last],
    }
}

/// The last name of a path there (a file's or folder's own name), for people.
pub fn name_of(os: Os, raw: &[u8]) -> String {
    let p = parent(os, raw);
    let rest = if p.len() < raw.len() {
        let r = &raw[p.len()..];
        r.strip_prefix(sep(os)).unwrap_or(r)
    } else {
        raw
    };
    display(os, rest)
}

fn prefix(os: Os, dir: &[u8]) -> Vec<u8> {
    let mut p = dir.to_vec();
    if !p.ends_with(sep(os)) {
        p.extend_from_slice(sep(os));
    }
    p
}

/// Just past every path that starts with `p`.
fn past(p: &[u8]) -> Vec<u8> {
    let mut hi = p.to_vec();
    if let Some(l) = hi.last_mut() {
        *l += 1; // a separator never ends in 0xFF
    }
    hi
}

/// The remote tables are there (an index this version has opened for writing at least once).
pub fn has_tables(db: &Db) -> bool {
    db.conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'remote_files')",
            [],
            |r| r.get::<_, bool>(0),
        )
        .unwrap_or(false)
}

fn machine_row(r: &rusqlite::Row) -> rusqlite::Result<Machine> {
    Ok(Machine {
        id: r.get(0)?,
        name: r.get(1)?,
        url: r.get(2)?,
        db: r.get(3)?,
        command: r.get(4)?,
        created_at: r.get(5)?,
        last_try: r.get(6)?,
        last_sync: r.get(7)?,
        last_error: r.get(8)?,
        info: r
            .get::<_, Option<String>>(9)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null),
    })
}

const MACHINE_COLS: &str =
    "id, name, url, db, command, created_at, last_try, last_sync, last_error, info";

/// Remember a machine. Nothing is contacted: `sync` (or `serve::hello` through ssh) does that.
pub fn add(
    db: &Db,
    name: &str,
    url: &str,
    remote_db: Option<&str>,
    command: Option<&str>,
) -> Result<Machine> {
    check_name(name)?;
    let t = parse_url(url)?;
    if let Some(p) = remote_db {
        check_remote_path(p, "the index path there")?;
    }
    if let Some(c) = command {
        check_remote_path(c, "the backdown command there")?;
        // only ever BackDown itself: a slip can't have ssh run some other program there
        let file = c.rsplit(['/', '\\']).next().unwrap_or(c);
        if !file.starts_with("backdown") {
            bail!(
                "the backdown command there should be backdown itself, like /usr/local/bin/backdown"
            );
        }
    }
    let taken: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM remotes WHERE name = ?1)",
        [name],
        |r| r.get(0),
    )?;
    if taken {
        bail!("there's a machine called {name} already (`backdown remote remove {name}` first)");
    }
    db.conn.execute(
        "INSERT INTO remotes(name, url, db, command, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![name, t.url(), remote_db, command, now()],
    )?;
    get(db, db.conn.last_insert_rowid())
}

pub fn get(db: &Db, id: i64) -> Result<Machine> {
    db.conn
        .query_row(
            &format!("SELECT {MACHINE_COLS} FROM remotes WHERE id = ?1"),
            [id],
            machine_row,
        )
        .optional()?
        .with_context(|| format!("there's no machine {id}"))
}

/// A machine by its name (or its id).
pub fn find(db: &Db, which: &str) -> Result<Machine> {
    if !has_tables(db) {
        bail!("no machines yet: `backdown remote add NAME ssh://host`");
    }
    let by_name = db
        .conn
        .query_row(
            &format!("SELECT {MACHINE_COLS} FROM remotes WHERE name = ?1"),
            [which],
            machine_row,
        )
        .optional()?;
    match (by_name, which.parse::<i64>()) {
        (Some(m), _) => Ok(m),
        (None, Ok(id)) => get(db, id),
        (None, Err(_)) => bail!("there's no machine called {which} (`backdown remote list`)"),
    }
}

pub fn list(db: &Db) -> Result<Vec<Machine>> {
    if !has_tables(db) {
        return Ok(Vec::new());
    }
    let mut stmt = db
        .conn
        .prepare(&format!("SELECT {MACHINE_COLS} FROM remotes ORDER BY name"))?;
    let rows = stmt.query_map([], machine_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Forget a machine and everything cached about it (nothing there is touched).
pub fn remove(db: &Db, id: i64) -> Result<()> {
    db.conn.execute_batch("BEGIN")?;
    let r = (|| -> Result<()> {
        db.conn
            .execute("DELETE FROM remote_files WHERE remote_id = ?1", [id])?;
        db.conn
            .execute("DELETE FROM remote_roots WHERE remote_id = ?1", [id])?;
        db.conn
            .execute("DELETE FROM remote_health WHERE remote_id = ?1", [id])?;
        db.conn
            .execute("DELETE FROM remote_health_state WHERE remote_id = ?1", [id])?;
        db.conn
            .execute("DELETE FROM remote_volumes WHERE remote_id = ?1", [id])?;
        if db.conn.execute("DELETE FROM remotes WHERE id = ?1", [id])? == 0 {
            bail!("there's no machine {id}");
        }
        Ok(())
    })();
    db.conn
        .execute_batch(if r.is_ok() { "COMMIT" } else { "ROLLBACK" })?;
    r
}

/// Note an attempt to reach a machine: when, and what went wrong (None: it answered).
pub fn record_try(db: &Db, id: i64, error: Option<&str>) -> Result<()> {
    db.conn.execute(
        "UPDATE remotes SET last_try = ?2, last_error = ?3 WHERE id = ?1",
        params![id, now(), error],
    )?;
    Ok(())
}

/// Keep what a machine said about itself (its `hello`), with its clock's skew against ours.
pub fn record_hello(db: &Db, id: i64, hello: &Value, asked_at: i64) -> Result<Value> {
    let mut info = hello.clone();
    if let Some(t) = hello["now"].as_i64() {
        info["skew"] = json!(t - asked_at);
    }
    info["seen"] = json!(asked_at);
    db.conn.execute(
        "UPDATE remotes SET info = ?2 WHERE id = ?1",
        params![id, info.to_string()],
    )?;
    Ok(info)
}

// ---------- settings

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Settings {
    /// a copy there counts as proof when that machine confirmed it this many days ago at most
    pub proof_days: i64,
    /// sync each machine this often
    pub sync_hours: i64,
}

pub fn settings(db: &Db) -> Settings {
    let get = |k: &str, d: i64| -> i64 {
        db.conn
            .query_row(
                "SELECT value FROM remote_settings WHERE key = ?1",
                [k],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    Settings {
        proof_days: get("proof_days", PROOF_DAYS),
        sync_hours: get("sync_hours", SYNC_HOURS),
    }
}

pub fn set_setting(db: &Db, key: &str, value: i64) -> Result<Settings> {
    let (lo, hi) = match key {
        "proof_days" => (1, 90),
        "sync_hours" => (1, 24 * 7),
        _ => bail!("unknown setting {key}"),
    };
    if !(lo..=hi).contains(&value) {
        bail!("{key} must be between {lo} and {hi}");
    }
    db.conn.execute(
        "INSERT INTO remote_settings(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value.to_string()],
    )?;
    Ok(settings(db))
}

/// Remote copies confirmed at or after this time count as proof; None when there are none to count
/// (no machines, or an index without the remote tables).
pub fn cutoff(db: &Db) -> Result<Option<i64>> {
    if !has_tables(db) {
        return Ok(None);
    }
    let any: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM remote_files WHERE sha256 IS NOT NULL)",
        [],
        |r| r.get(0),
    )?;
    Ok(any.then(|| now() - settings(db).proof_days * 86_400))
}

/// SQL: the machine holding a copy of `f` that counts as proof as of the cutoff in `?{cutoff}`
/// (NULL when none): the scalar form of `proof_sql`.
pub fn proof_machine_sql(f: &str, cutoff: usize) -> String {
    format!(
        "(SELECT r.remote_id FROM remote_files r JOIN remote_roots rr ON rr.remote_id = r.remote_id AND rr.root_id = r.root_id
          WHERE {f}.sha256 IS NOT NULL AND r.sha256 = {f}.sha256 AND r.size = {f}.size AND r.error = 0
            AND MAX(COALESCE(rr.seen_at, 0), COALESCE(r.hashed_at, 0)) >= ?{cutoff} LIMIT 1)"
    )
}

/// SQL: a copy of `f` (a `files` row: `{f}.sha256`, `{f}.size`) on another machine that counts as
/// proof as of the cutoff in parameter `?{cutoff}`: hashed there, readable, and confirmed since.
pub fn proof_sql(f: &str, cutoff: usize) -> String {
    format!(
        "EXISTS (SELECT 1 FROM remote_files r JOIN remote_roots rr ON rr.remote_id = r.remote_id AND rr.root_id = r.root_id
                 WHERE r.sha256 = {f}.sha256 AND r.size = {f}.size AND r.error = 0
                   AND MAX(COALESCE(rr.seen_at, 0), COALESCE(r.hashed_at, 0)) >= ?{cutoff})"
    )
}

// ---------- copies there

/// A copy of some content on another machine, as last synced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteCopy {
    pub machine_id: i64,
    pub machine: String,
    pub url: String,
    pub rid: i64,
    pub root_id: i64,
    pub path: String,
    pub key: String,
    pub dir_key: String,
    /// when that machine last confirmed it: its scan saw it there, or it re-read it for us
    pub confirmed_at: Option<i64>,
    /// it counts as a verified copy: hashed there, readable, confirmed within the proof window
    pub counts: bool,
    pub why_not: Option<String>,
}

/// Every hashed copy of `sha` (`size` bytes) on other machines, the ones that count first.
pub fn copies_of(db: &Db, sha: &[u8], size: u64) -> Result<Vec<RemoteCopy>> {
    if !has_tables(db) {
        return Ok(Vec::new());
    }
    let s = settings(db);
    let cutoff = now() - s.proof_days * 86_400;
    let machines: HashMap<i64, Machine> = list(db)?.into_iter().map(|m| (m.id, m)).collect();
    let mut stmt = db.conn.prepare_cached(
        "SELECT r.remote_id, r.rid, r.root_id, r.path, MAX(COALESCE(rr.seen_at, 0), COALESCE(r.hashed_at, 0))
         FROM remote_files r JOIN remote_roots rr ON rr.remote_id = r.remote_id AND rr.root_id = r.root_id
         WHERE r.sha256 = ?1 AND r.size = ?2 AND r.error = 0 LIMIT 100",
    )?;
    let rows = stmt.query_map(params![sha, size as i64], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Vec<u8>>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (mid, rid, root_id, path, confirmed) = row?;
        let Some(m) = machines.get(&mid) else {
            continue;
        };
        let confirmed = (confirmed > 0).then_some(confirmed);
        let counts = confirmed.is_some_and(|c| c >= cutoff);
        out.push(RemoteCopy {
            machine_id: mid,
            machine: m.name.clone(),
            url: m.url.clone(),
            rid,
            root_id,
            path: m.display(&path),
            key: browse::key_bytes(&path),
            dir_key: browse::key_bytes(parent(m.os(), &path)),
            confirmed_at: confirmed,
            counts,
            why_not: (!counts).then(|| {
                format!(
                    "last confirmed {}: more than {} days ago, so it doesn't count until {} has been scanned and synced again",
                    confirmed.map_or("never".into(), fmt::day),
                    s.proof_days,
                    m.name
                )
            }),
        });
    }
    out.sort_by(|a, b| {
        b.counts
            .cmp(&a.counts)
            .then(a.machine.cmp(&b.machine))
            .then(a.path.cmp(&b.path))
    });
    Ok(out)
}

/// Looks up, for an assessment over many files, a copy on another machine that counts as proof.
pub struct Prover<'a> {
    stmt: Option<rusqlite::CachedStatement<'a>>,
    cutoff: i64,
    machines: HashMap<i64, Machine>,
    pub proof_days: i64,
}

impl<'a> Prover<'a> {
    pub fn new(db: &'a Db) -> Result<Self> {
        let cutoff = cutoff(db)?;
        let stmt = match cutoff {
            Some(_) => Some(db.conn.prepare_cached(
                "SELECT r.remote_id, MAX(COALESCE(rr.seen_at, 0), COALESCE(r.hashed_at, 0))
                 FROM remote_files r JOIN remote_roots rr ON rr.remote_id = r.remote_id AND rr.root_id = r.root_id
                 WHERE r.sha256 = ?1 AND r.size = ?2 AND r.error = 0
                   AND MAX(COALESCE(rr.seen_at, 0), COALESCE(r.hashed_at, 0)) >= ?3
                 ORDER BY 2 DESC LIMIT 1",
            )?),
            None => None,
        };
        Ok(Prover {
            stmt,
            cutoff: cutoff.unwrap_or(i64::MAX),
            machines: list(db)?.into_iter().map(|m| (m.id, m)).collect(),
            proof_days: settings(db).proof_days,
        })
    }

    /// A counting copy of `sha` (`size` bytes) on another machine: which machine ("nas
    /// (ssh://nas.example.org)"), and when it was last confirmed there.
    pub fn find(&mut self, sha: &[u8], size: u64) -> Result<Option<(String, i64)>> {
        let Some(stmt) = self.stmt.as_mut() else {
            return Ok(None);
        };
        let hit: Option<(i64, i64)> = stmt
            .query_row(params![sha, size as i64, self.cutoff], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        Ok(hit.and_then(|(mid, at)| self.machines.get(&mid).map(|m| (m.label(), at))))
    }
}

/// Add the copies on other machines to a duplicates report (`report::dupes` as JSON): `remote`
/// for each group, and `across`, content here once whose copy is on another machine.
pub fn annotate_dupes(
    db: &Db,
    dupes: &mut Value,
    min_size: u64,
    filter: &report::DupesFilter,
    limit: usize,
) -> Result<()> {
    if !has_tables(db) || list(db)?.is_empty() {
        return Ok(());
    }
    if let Some(groups) = dupes["largest"].as_array_mut() {
        for g in groups {
            let (Some(sha), Some(size)) = (g["sha256"].as_str(), g["size"].as_u64()) else {
                continue;
            };
            let Some(raw) = unhex(sha) else { continue };
            g["remote"] = json!(copies_of(db, &raw, size)?);
        }
    }
    dupes["across"] = json!(across(db, min_size, filter, limit)?);
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct AcrossGroup {
    pub sha256: String,
    pub size: u64,
    pub path: String,
    pub remote: Vec<RemoteCopy>,
}

#[derive(Debug, Serialize, Default)]
pub struct Across {
    /// files here (with no other copy here) whose content is on another machine
    pub files: u64,
    pub bytes: u64,
    /// of those, the ones whose copy there counts as proof
    pub proven_files: u64,
    pub proven_bytes: u64,
    pub largest: Vec<AcrossGroup>,
}

/// Content that exists once here and again on another machine, largest first: what `filter`
/// matches (report.rs), each file judged as the duplicates report judges a path.
pub fn across(
    db: &Db,
    min_size: u64,
    filter: &report::DupesFilter,
    limit: usize,
) -> Result<Across> {
    let mut a = Across::default();
    if !has_tables(db) {
        return Ok(a);
    }
    let cutoff = now() - settings(db).proof_days * 86_400;
    let mut stmt = db.conn.prepare(&format!(
        "SELECT f.sha256, f.size, f.path, {}, f.mtime, f.root_id FROM files f
         WHERE f.current = 1 AND f.error IS NULL AND f.size >= ?1 AND f.sha256 IS NOT NULL
           AND EXISTS (SELECT 1 FROM remote_files r WHERE r.sha256 = f.sha256 AND r.size = f.size AND r.error = 0)
           AND NOT EXISTS (SELECT 1 FROM files g WHERE g.current = 1 AND g.sha256 = f.sha256 AND g.id <> f.id
                           AND NOT (f.ino IS NOT NULL AND g.dev = f.dev AND g.ino = f.ino))
         ORDER BY f.size DESC, f.path",
        proof_sql("f", 2)
    ))?;
    let rows = stmt.query_map(params![min_size.max(1) as i64, cutoff], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, i64>(1)? as u64,
            r.get::<_, Vec<u8>>(2)?,
            r.get::<_, bool>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
        ))
    })?;
    let judge = report::Judge::new(db, filter)?;
    for row in rows {
        let (sha, size, path, proven, mtime, root_id) = row?;
        if !filter.size_ok(size)
            || (filter.per_path() && !judge.path(&paths::from_bytes(&path), mtime, root_id).1)
        {
            continue;
        }
        a.files += 1;
        a.bytes += size;
        if proven {
            a.proven_files += 1;
            a.proven_bytes += size;
        }
        if a.largest.len() < limit {
            a.largest.push(AcrossGroup {
                sha256: fmt::hex(&sha),
                size,
                path: paths::from_bytes(&path).display().to_string(),
                remote: copies_of(db, &sha, size)?,
            });
        }
    }
    Ok(a)
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

// ---------- what the browser shows

/// Every machine with its drives as last synced, how fresh that is, and whether its copies count.
pub fn overview(db: &Db) -> Result<Value> {
    let s = settings(db);
    let cutoff = now() - s.proof_days * 86_400;
    let mut out = Vec::new();
    for m in list(db)? {
        let mut stmt = db.conn.prepare_cached(
            "SELECT root_id, path, display, last_scan, scanned_at, files, bytes, synced_scan, seen_at,
                    (SELECT COUNT(*) FROM remote_files f WHERE f.remote_id = r.remote_id AND f.root_id = r.root_id),
                    (SELECT COUNT(sha256) FROM remote_files f WHERE f.remote_id = r.remote_id AND f.root_id = r.root_id)
             FROM remote_roots r WHERE remote_id = ?1 ORDER BY display",
        )?;
        let roots: Vec<Value> = stmt
            .query_map([m.id], |r| {
                let path: Vec<u8> = r.get(1)?;
                let seen: Option<i64> = r.get(8)?;
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "key": browse::key_bytes(&path),
                    "name": name_of(m.os(), &path),
                    "path": r.get::<_, String>(2)?,
                    "last_scan": r.get::<_, Option<i64>>(3)?,
                    "scanned_at": r.get::<_, Option<i64>>(4)?,
                    "files": r.get::<_, i64>(5)?,
                    "bytes": r.get::<_, i64>(6)?,
                    "synced_scan": r.get::<_, Option<i64>>(7)?,
                    "seen_at": seen,
                    "cached": r.get::<_, i64>(9)?,
                    "hashed": r.get::<_, i64>(10)?,
                    "counts": seen.is_some_and(|t| t >= cutoff),
                }))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut v = json!(m);
        v["roots"] = json!(roots);
        out.push(v);
    }
    Ok(json!({ "machines": out, "settings": s }))
}

#[derive(Debug, Serialize)]
pub struct CachedEntry {
    pub kind: &'static str,
    pub name: String,
    pub key: String,
    pub id: Option<i64>,
    pub size: u64,
    pub mtime: i64,
    pub sha256: Option<String>,
}

/// A folder there as last synced (for when the machine can't be reached): its files, and its
/// subfolders without sizes (only files are cached). Folder listings are found by skipping
/// through the cached paths, so they cost one lookup per entry.
pub fn cached_list(db: &Db, m: &Machine, dir: &[u8]) -> Result<Value> {
    let os = m.os();
    let (root_id, root_path, _): (i64, Vec<u8>, String) = {
        let mut stmt = db
            .conn
            .prepare("SELECT root_id, path, display FROM remote_roots WHERE remote_id = ?1")?;
        let roots: Vec<(i64, Vec<u8>, String)> = stmt
            .query_map([m.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        roots
            .into_iter()
            .filter(|(_, p, _)| dir == p.as_slice() || dir.starts_with(&prefix(os, p)))
            .max_by_key(|(_, p, _)| p.len())
            .with_context(|| {
                format!(
                    "{} isn't on a drive {} has indexed",
                    display(os, dir),
                    m.name
                )
            })?
    };
    let pre = prefix(os, dir);
    let hi = past(&pre);
    let sepb = sep(os);
    let mut stmt = db.conn.prepare_cached(
        "SELECT path, rid, size, mtime, sha256 FROM remote_files
         WHERE remote_id = ?1 AND path >= ?2 AND path < ?3 ORDER BY path LIMIT 1",
    )?;
    let mut entries = Vec::new();
    let mut cursor = pre.clone();
    let mut truncated = false;
    while let Some((path, rid, size, mtime, sha)) = stmt
        .query_row(params![m.id, cursor, hi], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<Vec<u8>>>(4)?,
            ))
        })
        .optional()?
    {
        if entries.len() >= serve::MAX_ENTRIES {
            truncated = true;
            break;
        }
        let rest = &path[pre.len()..];
        let at = (0..rest.len())
            .step_by(if os == Os::Windows { 2 } else { 1 })
            .find(|&i| rest[i..].starts_with(sepb));
        match at {
            Some(i) => {
                let child = [&pre[..], &rest[..i]].concat();
                entries.push(CachedEntry {
                    kind: "dir",
                    name: display(os, &rest[..i]),
                    key: browse::key_bytes(&child),
                    id: None,
                    size: 0,
                    mtime: 0,
                    sha256: None,
                });
                cursor = past(&[&child[..], sepb].concat());
            }
            None => {
                entries.push(CachedEntry {
                    kind: "file",
                    name: display(os, rest),
                    key: browse::key_bytes(&path),
                    id: Some(rid),
                    size: size as u64,
                    mtime,
                    sha256: sha.as_deref().map(fmt::hex),
                });
                cursor = [&path[..], &[0u8][..]].concat();
            }
        }
    }
    // the way back up, from the drive's top
    let mut crumbs = Vec::new();
    let mut at = dir.to_vec();
    loop {
        crumbs.push(json!({ "name": name_of(os, &at), "key": browse::key_bytes(&at) }));
        if at == root_path || !at.starts_with(&root_path) {
            break;
        }
        let up = parent(os, &at).to_vec();
        if up == at {
            break;
        }
        at = up;
    }
    crumbs.reverse();
    let seen: Option<i64> = db.conn.query_row(
        "SELECT seen_at FROM remote_roots WHERE remote_id = ?1 AND root_id = ?2",
        params![m.id, root_id],
        |r| r.get(0),
    )?;
    Ok(json!({
        "cached": true, "root_id": root_id, "name": name_of(os, dir), "path": display(os, dir),
        "key": browse::key_bytes(dir), "crumbs": crumbs, "entries": entries, "truncated": truncated,
        "seen_at": seen, "synced_at": m.last_sync,
    }))
}

/// path, size, mtime, sha256, error, hashed_at of a cached file
type CachedRow = (Vec<u8>, i64, i64, Option<Vec<u8>>, i64, Option<i64>);
/// a file as `changes` sends it: rid, path key, size, mtime, sample, sha256, error
type WireRow = (i64, String, i64, i64, Option<String>, Option<String>, i64);

/// One file there as last synced, with its copies here and on other machines.
pub fn cached_file(db: &Db, m: &Machine, rid: i64) -> Result<Value> {
    let (path, size, mtime, sha, error, hashed_at): CachedRow = db
        .conn
        .query_row(
            "SELECT path, size, mtime, sha256, error, hashed_at FROM remote_files WHERE remote_id = ?1 AND rid = ?2",
            params![m.id, rid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()?
        .with_context(|| format!("{} has no file {rid} (as last synced)", m.name))?;
    let mut v = json!({
        "cached": true, "id": rid, "name": name_of(m.os(), &path), "path": m.display(&path),
        "key": browse::key_bytes(&path), "dir_key": browse::key_bytes(parent(m.os(), &path)),
        "size": size, "mtime": mtime, "sha256": sha.as_deref().map(fmt::hex),
        "error": (error != 0).then_some(if error == 2 { "changed since its last scan there" } else { "couldn't be read there" }),
        "hashed_at": hashed_at, "versions": [], "copies": [],
    });
    if let Some(h) = &sha {
        v["local_copies"] = json!(local_copies(db, h, size as u64)?);
    }
    Ok(v)
}

/// Current files here with this content (for a file seen on another machine).
pub fn local_copies(db: &Db, sha: &[u8], size: u64) -> Result<Vec<browse::Copy>> {
    let mut stmt = db.conn.prepare_cached(
        "SELECT id, path FROM files WHERE current = 1 AND sha256 = ?1 AND size = ?2 ORDER BY path LIMIT 50",
    )?;
    let rows = stmt.query_map(params![sha, size as i64], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, p) = row?;
        let p = paths::from_bytes(&p);
        out.push(browse::Copy {
            id,
            path: p.display().to_string(),
            key: browse::key(&p),
            dir_key: browse::key(p.parent().unwrap_or(&p)),
        });
    }
    Ok(out)
}

// ---------- syncing

/// One side of a conversation with another machine's `backdown serve-stdio` (ssh in practice;
/// the server itself, in-process, in tests).
pub trait Peer {
    /// Ask `op` with `args` (an object); the answer's `data`, or its error.
    fn call(&mut self, op: &str, args: Value, timeout: Duration) -> Result<Value>;
}

/// Opens conversations with machines (for a wipe, which may need several).
pub trait Connect {
    fn connect(&mut self, m: &Machine) -> Result<Box<dyn Peer>>;
}

pub const ASK: Duration = Duration::from_secs(60);

#[derive(Debug, Default, Clone, Serialize)]
pub struct SyncStats {
    pub drives: u64,
    pub files: u64,
    /// files new or changed there since the last sync
    pub added: u64,
    /// no longer there
    pub removed: u64,
    /// sampled and hashed there for us, because a file of that size is here
    pub sampled: u64,
    pub hashed: u64,
    /// couldn't be read or had changed there
    pub failed: u64,
    /// still to be sampled or hashed there (next time)
    pub left: u64,
    /// the sizes or hashes cached changed: hashing here may find more copies now
    pub changed: bool,
    /// its drive health: "ok", "unsupported" (a BackDown there from before the request), "error",
    /// or "" when it wasn't asked
    pub health: &'static str,
    /// drives it reported
    pub health_drives: u64,
    /// notifications recorded here about drives there getting worse
    pub health_notes: u64,
    /// its drives and their free space (swarm.rs): "ok", "unsupported" (a BackDown there from
    /// before the request), or "error"
    pub volumes: &'static str,
}

pub struct SyncOptions<'a> {
    /// rows per request
    pub page: u32,
    /// how long to spend having files there sampled and hashed
    pub hash_budget: Duration,
    pub stop: Option<&'a AtomicBool>,
    /// fetch its drive health too, and tell here when a drive there gets worse
    pub health: bool,
    /// such a notification may go to the desktop: the ones to show now are put here, as they're
    /// recorded (so even when the sync fails afterwards). None: they're only listed, in Activity
    pub show: Option<&'a Mutex<Vec<Note>>>,
}

impl Default for SyncOptions<'_> {
    fn default() -> Self {
        SyncOptions {
            page: 5000,
            hash_budget: Duration::from_secs(20 * 60),
            stop: None,
            health: true,
            show: None,
        }
    }
}

#[derive(Deserialize)]
struct RootInfo {
    id: i64,
    key: String,
    path: String,
    last_scan: Option<i64>,
    scanned_at: Option<i64>,
    files: u64,
    bytes: u64,
}

fn stopped(o: &SyncOptions) -> bool {
    o.stop.is_some_and(|s| s.load(Ordering::Relaxed))
}

/// Say hello and check the protocol: what the machine says about itself, and when we asked.
pub fn greet(m: &Machine, peer: &mut dyn Peer) -> Result<(Value, i64)> {
    let asked = now();
    let h = peer.call("hello", json!({}), ASK)?;
    let proto = h["proto"].as_u64().unwrap_or(0);
    if proto != PROTO as u64 {
        bail!(
            "{} runs BackDown {} (protocol {proto}); this one is {} (protocol {PROTO}): update the older one",
            m.name,
            h["version"].as_str().unwrap_or("?"),
            env!("CARGO_PKG_VERSION")
        );
    }
    Ok((h, asked))
}

/// Say hello, check the protocol, and keep what the machine says about itself.
pub fn hello(db: &Db, m: &Machine, peer: &mut dyn Peer) -> Result<Value> {
    let (h, asked) = greet(m, peer)?;
    record_hello(db, m.id, &h, asked)
}

/// Bring the cache of machine `m` up to date through `peer`: its drives, the files changed there
/// since the last sync (by its scan ids), and hashes there for files whose size is also here.
pub fn sync(
    db: &Db,
    m: &Machine,
    peer: &mut dyn Peer,
    opts: &SyncOptions,
    progress: &mut dyn FnMut(&str),
) -> Result<SyncStats> {
    let info = hello(db, m, peer)?;
    let skew = info["skew"].as_i64().unwrap_or(0);
    let samples_ok = info["sample"].as_u64() == Some(SAMPLE as u64);
    // a different index there (replaced, or --db changed): its ids mean nothing here any more
    let ident = info["ident"].as_str().unwrap_or("").to_string();
    if m.info["ident"].as_str().is_some_and(|old| old != ident) {
        progress(&format!("{}'s index changed: starting afresh", m.name));
        db.conn
            .execute("DELETE FROM remote_files WHERE remote_id = ?1", [m.id])?;
        db.conn
            .execute("DELETE FROM remote_roots WHERE remote_id = ?1", [m.id])?;
    }
    let mut st = SyncStats::default();
    // first, as it's quick: a drive there going bad is news even when the files take a while
    if opts.health {
        fetch_health(db, m, peer, skew, opts.show, &mut st)?;
    }
    // its drives and how much room is left on them (the swarm's view of it)
    st.volumes = fetch_volumes(db, m, peer)?;
    let roots: Vec<RootInfo> =
        serde_json::from_value(peer.call("roots", json!({}), ASK)?["roots"].clone())
            .context("its list of drives")?;
    // drives it no longer has
    {
        let have: Vec<i64> = roots.iter().map(|r| r.id).collect();
        let mut stmt = db
            .conn
            .prepare("SELECT root_id FROM remote_roots WHERE remote_id = ?1")?;
        let cached: Vec<i64> = stmt
            .query_map([m.id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in cached.into_iter().filter(|id| !have.contains(id)) {
            db.conn.execute(
                "DELETE FROM remote_files WHERE remote_id = ?1 AND root_id = ?2",
                params![m.id, id],
            )?;
            db.conn.execute(
                "DELETE FROM remote_roots WHERE remote_id = ?1 AND root_id = ?2",
                params![m.id, id],
            )?;
            st.changed = true;
        }
    }
    for r in &roots {
        if stopped(opts) {
            break;
        }
        let raw = URL_SAFE_NO_PAD
            .decode(&r.key)
            .context("a drive's path from the other machine")?;
        let scanned_at = r.scanned_at.map(|t| t - skew);
        db.conn.execute(
            "INSERT INTO remote_roots(remote_id, root_id, path, display, last_scan, scanned_at, files, bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(remote_id, root_id) DO UPDATE SET path = excluded.path, display = excluded.display,
               last_scan = excluded.last_scan, scanned_at = excluded.scanned_at, files = excluded.files, bytes = excluded.bytes",
            params![m.id, r.id, raw, r.path, r.last_scan, scanned_at, r.files as i64, r.bytes as i64],
        )?;
        st.drives += 1;
        let Some(upto) = r.last_scan else { continue };
        let mut since: i64 = db
            .conn
            .query_row(
                "SELECT synced_scan FROM remote_roots WHERE remote_id = ?1 AND root_id = ?2",
                params![m.id, r.id],
                |x| x.get::<_, Option<i64>>(0),
            )?
            .unwrap_or(0);
        if since > upto {
            // its scans went backwards: an older copy of its index. Start this drive afresh
            db.conn.execute(
                "DELETE FROM remote_files WHERE remote_id = ?1 AND root_id = ?2",
                params![m.id, r.id],
            )?;
            since = 0;
        }
        if since == upto {
            continue;
        }
        // what stopped being current there since the last sync
        if since > 0 {
            let mut after = 0i64;
            loop {
                if stopped(opts) {
                    return Ok(st);
                }
                let page = peer.call(
                    "gone",
                    json!({ "root": r.id, "since": since, "after": after, "limit": opts.page }),
                    ASK,
                )?;
                let ids: Vec<i64> = serde_json::from_value(page["ids"].clone())?;
                db.conn.execute_batch("BEGIN")?;
                {
                    let mut del = db.conn.prepare_cached(
                        "DELETE FROM remote_files WHERE remote_id = ?1 AND rid = ?2",
                    )?;
                    for id in &ids {
                        st.removed += del.execute(params![m.id, id])? as u64;
                    }
                }
                db.conn.execute_batch("COMMIT")?;
                match page["next"].as_i64() {
                    Some(n) => after = n,
                    None => break,
                }
            }
        }
        // what's new or changed there, up to the scan it had finished when we asked
        let mut after = 0i64;
        loop {
            if stopped(opts) {
                return Ok(st);
            }
            let page = peer.call(
                "changes",
                json!({ "root": r.id, "since": since, "upto": upto, "after": after, "limit": opts.page }),
                ASK,
            )?;
            let rows: Vec<WireRow> =
                serde_json::from_value(page["rows"].clone()).context("its list of files")?;
            db.conn.execute_batch("BEGIN")?;
            {
                let mut put = db.conn.prepare_cached(
                    "INSERT OR REPLACE INTO remote_files(remote_id, rid, root_id, path, size, mtime, sample, sha256, error, hashed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL)",
                )?;
                for (rid, key, size, mtime, sample, sha, err) in &rows {
                    let path = URL_SAFE_NO_PAD.decode(key).context("a file's path")?;
                    let sample = sample.as_deref().filter(|_| samples_ok).and_then(unhex);
                    let sha = sha.as_deref().and_then(unhex).filter(|h| h.len() == 32);
                    put.execute(params![
                        m.id,
                        rid,
                        r.id,
                        path,
                        size,
                        mtime,
                        sample,
                        sha,
                        i64::from(*err != 0)
                    ])?;
                }
            }
            db.conn.execute_batch("COMMIT")?;
            st.added += rows.len() as u64;
            if !rows.is_empty() {
                st.changed = true;
            }
            progress(&format!(
                "{}: {} files new or changed there",
                r.path,
                fmt::count(st.added)
            ));
            match page["next"].as_i64() {
                Some(n) => after = n,
                None => break,
            }
        }
        db.conn.execute(
            "UPDATE remote_roots SET synced_scan = ?3, seen_at = ?4 WHERE remote_id = ?1 AND root_id = ?2",
            params![m.id, r.id, upto, scanned_at],
        )?;
    }
    if st.removed > 0 {
        st.changed = true;
    }
    hash_there(db, m, peer, opts, &mut st, progress)?;
    st.files = db.conn.query_row(
        "SELECT COUNT(*) FROM remote_files WHERE remote_id = ?1",
        [m.id],
        |r| r.get::<_, i64>(0),
    )? as u64;
    if !stopped(opts) {
        db.conn.execute(
            "UPDATE remotes SET last_sync = ?2, last_error = NULL, last_try = ?2 WHERE id = ?1",
            params![m.id, now()],
        )?;
    }
    Ok(st)
}

/// What to ask a machine to read for us.
#[derive(Clone, Copy, PartialEq)]
enum Want {
    /// a sample of files whose size a file here has
    Sample,
    /// the full hash of files whose sample a file here has
    Full,
    /// the full hash of files whose size a file here has: when its samples can't be compared with
    /// ours (another sample algorithm), so there's no sampling step
    FullBySize,
}

/// The steps of having files hashed on machine `m`.
fn wants(m: &Machine) -> &'static [Want] {
    if m.info["sample"].as_u64() == Some(SAMPLE as u64) {
        &[Want::Sample, Want::Full]
    } else {
        &[Want::FullBySize]
    }
}

/// Files there worth reading for `w`, largest first.
fn wanted(db: &Db, m: &Machine, w: Want, limit: usize) -> Result<Vec<(i64, i64)>> {
    let sql = match w {
        Want::Full => {
            "SELECT rid, size FROM remote_files r WHERE remote_id = ?1 AND sha256 IS NULL AND sample IS NOT NULL AND error = 0
               AND EXISTS (SELECT 1 FROM files f WHERE f.current = 1 AND f.error IS NULL AND f.size = r.size
                           AND (f.sample = r.sample OR (f.sample IS NULL AND f.sha256 IS NOT NULL)))
             ORDER BY size DESC LIMIT ?2"
        }
        Want::Sample => {
            "SELECT rid, size FROM remote_files r WHERE remote_id = ?1 AND sample IS NULL AND sha256 IS NULL AND error = 0 AND size > 0
               AND size IN (SELECT size FROM files WHERE current = 1 AND error IS NULL AND size > 0)
             ORDER BY size DESC LIMIT ?2"
        }
        Want::FullBySize => {
            "SELECT rid, size FROM remote_files r WHERE remote_id = ?1 AND sha256 IS NULL AND error = 0 AND size > 0
               AND size IN (SELECT size FROM files WHERE current = 1 AND error IS NULL AND size > 0)
             ORDER BY size DESC LIMIT ?2"
        }
    };
    let mut stmt = db.conn.prepare_cached(sql)?;
    let rows = stmt.query_map(params![m.id, limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// How much sampling and hashing is still waiting on that machine.
pub fn left_to_hash(db: &Db, m: &Machine) -> Result<u64> {
    let mut n = 0;
    for w in wants(m) {
        n += wanted(db, m, *w, 1_000_000)?.len() as u64;
    }
    Ok(n)
}

/// Some sampling or hashing is waiting on that machine (a sync would do it).
pub fn waiting(db: &Db, m: &Machine) -> Result<bool> {
    for w in wants(m) {
        if !wanted(db, m, *w, 1)?.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A hash answer, stored: the sample, the full hash (re-read there just now when `read`), or why not.
fn store_hash(db: &Db, m: &Machine, r: &Value, samples_ok: bool) -> Result<&'static str> {
    let Some(rid) = r["id"].as_i64() else {
        return Ok("skipped");
    };
    if r["gone"] == true {
        db.conn.execute(
            "DELETE FROM remote_files WHERE remote_id = ?1 AND rid = ?2",
            params![m.id, rid],
        )?;
        return Ok("failed");
    }
    if r["changed"] == true || r["error"].is_string() {
        db.conn.execute(
            "UPDATE remote_files SET error = ?3 WHERE remote_id = ?1 AND rid = ?2",
            params![m.id, rid, if r["changed"] == true { 2 } else { 1 }],
        )?;
        return Ok("failed");
    }
    let sample = r["sample"].as_str().filter(|_| samples_ok).and_then(unhex);
    let sha = r["sha256"]
        .as_str()
        .and_then(unhex)
        .filter(|h| h.len() == 32);
    let read_now = r["read"] == true && sha.is_some();
    db.conn.execute(
        "UPDATE remote_files SET sample = COALESCE(?3, sample), sha256 = COALESCE(?4, sha256),
           hashed_at = CASE WHEN ?5 THEN ?6 ELSE hashed_at END WHERE remote_id = ?1 AND rid = ?2",
        params![m.id, rid, sample, sha, read_now, now()],
    )?;
    Ok(if sha.is_some() { "hashed" } else { "sampled" })
}

fn hash_there(
    db: &Db,
    m: &Machine,
    peer: &mut dyn Peer,
    opts: &SyncOptions,
    st: &mut SyncStats,
    progress: &mut dyn FnMut(&str),
) -> Result<()> {
    let until = Instant::now() + opts.hash_budget;
    let m = &get(db, m.id)?; // as it just said hello: its sample algorithm
    let samples_ok = m.info["sample"].as_u64() == Some(SAMPLE as u64);
    for &want in wants(m) {
        let full = want != Want::Sample;
        let mut skip: Vec<i64> = Vec::new();
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() || stopped(opts) {
                break;
            }
            let batch: Vec<(i64, i64)> = wanted(db, m, want, 64 + skip.len())?
                .into_iter()
                .filter(|(id, _)| !skip.contains(id))
                .take(64)
                .collect();
            if batch.is_empty() {
                break;
            }
            let budget = left.as_secs().clamp(10, 300);
            let ids: Vec<i64> = batch.iter().map(|b| b.0).collect();
            let ans = peer.call(
                "hash",
                json!({ "ids": ids, "mode": if full { "full" } else { "sample" }, "budget": budget }),
                Duration::from_secs(budget + 30),
            )?;
            let results = ans["results"].as_array().cloned().unwrap_or_default();
            db.conn.execute_batch("BEGIN")?;
            for r in &results {
                match store_hash(db, m, r, samples_ok)? {
                    "hashed" => st.hashed += 1,
                    "sampled" => st.sampled += 1,
                    "failed" => st.failed += 1,
                    _ => {}
                }
            }
            db.conn.execute_batch("COMMIT")?;
            if !results.is_empty() {
                st.changed = true;
            }
            // a file the answer left out mustn't be asked for again and again
            for id in &ids {
                if !results.iter().any(|r| r["id"].as_i64() == Some(*id)) {
                    skip.push(*id);
                }
            }
            progress(&format!(
                "{}: {} sampled, {} hashed there",
                m.name,
                fmt::count(st.sampled),
                fmt::count(st.hashed)
            ));
            if ans["done"] == false && results.is_empty() {
                break; // out of time on one file: next time
            }
        }
    }
    st.left = left_to_hash(db, m)?;
    Ok(())
}

// ---------- its drives and free space (swarm.rs)

/// The most kept of a machine's storage report (its drives, file systems and indexed drives, as
/// JSON); a bigger one is refused rather than cut.
pub const MAX_VOLUMES_JSON: usize = 2 << 20;

/// Keep what a machine said about its storage (`volumes`), or why it couldn't say: a BackDown
/// there that doesn't know the request (an older one) is "unsupported", and what it said before
/// is dropped (it's no longer current); an error leaves the last report in place, with the error.
/// The state kept: "ok", "unsupported" or "error".
pub fn store_volumes(db: &Db, id: i64, answer: &Result<Value>) -> Result<&'static str> {
    let at = now();
    let (state, data, error) = match answer {
        Ok(v) => {
            let s = v.to_string();
            if s.len() > MAX_VOLUMES_JSON || !v["disks"].is_array() {
                (
                    "error",
                    None,
                    Some("its list of drives was too long or not one".to_string()),
                )
            } else {
                ("ok", Some(s), None)
            }
        }
        Err(e) if unknown_request(e, "volumes") => ("unsupported", None, None),
        Err(e) => ("error", None, Some(clip(&format!("{e:#}"), 500))),
    };
    db.conn.execute(
        "INSERT INTO remote_volumes(remote_id, state, data, error, asked_at, ok_at)
         VALUES (?1, ?2, ?3, ?4, ?5, CASE WHEN ?2 = 'ok' THEN ?5 END)
         ON CONFLICT(remote_id) DO UPDATE SET state = excluded.state, error = excluded.error,
           asked_at = excluded.asked_at,
           data = CASE excluded.state WHEN 'ok' THEN excluded.data WHEN 'unsupported' THEN NULL ELSE data END,
           ok_at = CASE excluded.state WHEN 'ok' THEN excluded.ok_at WHEN 'unsupported' THEN NULL ELSE ok_at END",
        params![id, state, data, error, at],
    )?;
    Ok(state)
}

/// Ask machine `m` about its storage and keep the answer (`store_volumes`); a failure there is
/// noted, and the sync goes on.
fn fetch_volumes(db: &Db, m: &Machine, peer: &mut dyn Peer) -> Result<&'static str> {
    let answer = peer.call("volumes", json!({}), ASK);
    store_volumes(db, m.id, &answer)
}

/// What's kept of machine `id`'s storage: (state, its report, when it last answered, when it was
/// last asked, why not); None when it was never asked.
pub fn kept_volumes(db: &Db, id: i64) -> Result<Option<crate::swarm::Kept>> {
    let has: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'remote_volumes')",
        [],
        |r| r.get(0),
    )?;
    if !has {
        return Ok(None);
    }
    Ok(db
        .conn
        .query_row(
            "SELECT state, data, ok_at, asked_at, error FROM remote_volumes WHERE remote_id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?
        .map(|(state, data, ok_at, asked, error)| {
            let report = data.and_then(|d| serde_json::from_str(&d).ok());
            (state, report, ok_at, asked, error)
        }))
}

// ---------- drive health there

/// A drive there whose latest reading was this many days older than the sync that fetched it
/// isn't being read there any more (taken out, or that machine's BackDown isn't running): it's
/// listed, but not as current.
pub const HEALTH_STALE_DAYS: i64 = 7;
/// The most kept of one drive there (its summary, as JSON): past it, its changes, trend, note and
/// mount points are left out, and past that, the drive.
const MAX_DRIVE_JSON: usize = 16 * 1024;

/// The drive-health tables are there (an index this version has opened for writing at least once).
fn has_health_tables(db: &Db) -> bool {
    db.conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'remote_health_state')",
            [],
            |r| r.get::<_, bool>(0),
        )
        .unwrap_or(false)
}

/// The other end doesn't know request `op`: a BackDown from before it was added. Those say "bad
/// request: unknown variant `op`"; from this version on, "unknown request \"op\"".
pub fn unknown_request(e: &anyhow::Error, op: &str) -> bool {
    let s = format!("{e:#}");
    s.contains(&format!("unknown request {op:?}")) || s.contains(&format!("unknown variant `{op}`"))
}

fn clip(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut i = n;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    format!("{}…", &s[..i])
}

/// One drive as a `health` answer gives it, in this machine's clock, with its verdict and findings
/// as this BackDown's own rules see its reading; None when it isn't one.
fn drive_from(d: &Value, skew: i64) -> Option<(Health, Value)> {
    let mut h: Health = serde_json::from_value(d.get("health")?.clone()).ok()?;
    let id = d["id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map_or_else(|| h.id.clone(), str::to_string);
    if id.is_empty() || id.len() > 200 {
        return None;
    }
    h.id = id.clone();
    if h.read_at > 0 {
        h.read_at -= skew;
    }
    let mut changes: Vec<Change> = d["changes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| serde_json::from_value(c.clone()).ok())
        .collect();
    changes.drain(..changes.len().saturating_sub(serve::MAX_HEALTH_CHANGES));
    for c in &mut changes {
        c.at -= skew;
    }
    let trend: Vec<&str> = d["trend"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .take(20)
        .collect();
    let a = health::assess(&h);
    let mut s = json!({
        "id": id, "health": h, "verdict": a.verdict, "findings": a.findings, "changes": changes,
        "trend": trend, "first_at": d["first_at"].as_i64().map(|t| t - skew),
        "readings": d["readings"].as_u64().unwrap_or(0),
    });
    if s.to_string().len() > MAX_DRIVE_JSON {
        s["changes"] = json!([]);
        s["trend"] = json!([]);
        s["health"]["note"] = Value::Null;
        s["health"]["mounts"] = json!([]);
        if s.to_string().len() > MAX_DRIVE_JSON {
            return None;
        }
    }
    Some((h, s))
}

/// One of the other machine's own notifications, in this machine's clock.
fn told_from(v: &Value, skew: i64) -> Option<Value> {
    let text = |k: &str, n: usize| v[k].as_str().map(|s| clip(s, n));
    Some(json!({
        "at": v["at"].as_i64()? - skew, "level": text("level", 16)?, "title": text("title", 200)?,
        "body": text("body", 1000).unwrap_or_default(), "drive": text("drive", 200),
    }))
}

/// Ask machine `m` for its drive health (what its own index holds: nothing is read from a drive
/// there for this), and keep it in place of what was kept. A drive whose verdict got worse since
/// the last time becomes a notification here, but never on the first sight of that machine's
/// drives. A BackDown there that doesn't know the request (an older one), or an error there, is
/// noted, and the sync goes on. With `show`, a notification may go to the desktop (see
/// `SyncOptions::show`).
fn fetch_health(
    db: &Db,
    m: &Machine,
    peer: &mut dyn Peer,
    skew: i64,
    show: Option<&Mutex<Vec<Note>>>,
    st: &mut SyncStats,
) -> Result<()> {
    let at = now();
    let ans = match peer.call("health", json!({}), ASK) {
        Ok(a) => a,
        Err(e) => {
            let older = unknown_request(&e, "health");
            if older {
                // what it said before (if it ever did) isn't current; once it's updated, its
                // drives are a first sight again
                db.conn
                    .execute("DELETE FROM remote_health WHERE remote_id = ?1", [m.id])?;
            }
            db.conn.execute(
                "INSERT INTO remote_health_state(remote_id, asked_at, ok_at, state, error, notes)
                 VALUES (?1, ?2, NULL, ?3, ?4, NULL)
                 ON CONFLICT(remote_id) DO UPDATE SET asked_at = excluded.asked_at,
                   state = excluded.state, error = excluded.error,
                   ok_at = CASE WHEN excluded.state = 'unsupported' THEN NULL ELSE ok_at END,
                   notes = CASE WHEN excluded.state = 'unsupported' THEN NULL ELSE notes END",
                params![
                    m.id,
                    at,
                    if older { "unsupported" } else { "error" },
                    (!older).then(|| clip(&format!("{e:#}"), 500))
                ],
            )?;
            st.health = if older { "unsupported" } else { "error" };
            return Ok(());
        }
    };
    // whether its drives were seen before, and how each was then: its verdict, and the changes
    // already known (by their keys, which name the event whatever either clock says)
    let seen: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM remote_health_state WHERE remote_id = ?1 AND ok_at IS NOT NULL)",
        [m.id],
        |r| r.get(0),
    )?;
    let before: HashMap<String, (Verdict, Vec<String>)> = {
        let mut stmt = db
            .conn
            .prepare("SELECT drive, verdict, summary FROM remote_health WHERE remote_id = ?1")?;
        let rows = stmt.query_map([m.id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (drive, verdict, summary) = row?;
            let s: Value = serde_json::from_str(&summary).unwrap_or(Value::Null);
            let keys = s["changes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c["key"].as_str().map(str::to_string))
                .collect();
            out.insert(drive, (Verdict::parse(&verdict), keys));
        }
        out
    };
    let mut drives: Vec<(Health, Value)> = Vec::new();
    for d in ans["drives"]
        .as_array()
        .into_iter()
        .flatten()
        .take(serve::MAX_HEALTH_DRIVES)
    {
        if let Some((h, s)) = drive_from(d, skew)
            && !drives.iter().any(|(x, _)| x.id == h.id)
        {
            drives.push((h, s));
        }
    }
    let told: Vec<Value> = ans["notes"]
        .as_array()
        .into_iter()
        .flatten()
        .take(serve::MAX_HEALTH_NOTES)
        .filter_map(|n| told_from(n, skew))
        .collect();
    db.conn.execute_batch("BEGIN")?;
    let r = (|| -> Result<()> {
        db.conn
            .execute("DELETE FROM remote_health WHERE remote_id = ?1", [m.id])?;
        let mut put = db.conn.prepare_cached(
            "INSERT INTO remote_health(remote_id, drive, model, serial, verdict, summary, read_at, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for (h, s) in &drives {
            put.execute(params![
                m.id,
                h.id,
                h.model.as_deref().map(|x| clip(x, 200)),
                h.serial.as_deref().map(|x| clip(x, 200)),
                s["verdict"].as_str().unwrap_or("unknown"),
                s.to_string(),
                (h.read_at > 0).then_some(h.read_at),
                at
            ])?;
        }
        db.conn.execute(
            "INSERT INTO remote_health_state(remote_id, asked_at, ok_at, state, error, notes)
             VALUES (?1, ?2, ?2, 'ok', NULL, ?3)
             ON CONFLICT(remote_id) DO UPDATE SET asked_at = excluded.asked_at, ok_at = excluded.ok_at,
               state = 'ok', error = NULL, notes = excluded.notes",
            params![m.id, at, json!(told).to_string()],
        )?;
        Ok(())
    })();
    db.conn
        .execute_batch(if r.is_ok() { "COMMIT" } else { "ROLLBACK" })?;
    r?;
    st.health = "ok";
    st.health_drives = drives.len() as u64;
    if !seen {
        return Ok(()); // the first sight of its drives: nothing to compare with, so nothing to tell
    }
    for (h, s) in &drives {
        let now_v = Verdict::parse(s["verdict"].as_str().unwrap_or(""));
        let (was, known) = match before.get(&h.id) {
            Some((v, k)) => (Some(*v), k.as_slice()),
            None => (None, &[][..]),
        };
        if now_v.rank() <= was.map_or(0, Verdict::rank) {
            continue;
        }
        let changes: Vec<Change> = serde_json::from_value(s["changes"].clone()).unwrap_or_default();
        let new: Vec<&Change> = changes.iter().filter(|c| !known.contains(&c.key)).collect();
        let note = worse_note(m, h, &new, was, now_v);
        let added = alerts::add(db, &note, show.is_some(), at, &Limits::default())?;
        if let Some((_, now)) = added {
            st.health_notes += 1;
            if let (true, Some(s)) = (now, show) {
                s.lock().unwrap_or_else(|e| e.into_inner()).push(note);
            }
        }
    }
    Ok(())
}

/// The notification for drive `h` on machine `m` getting worse: what its readings say changed since
/// the last sync (`new`: its changes not known then), else why it's at its verdict now; and what
/// to do.
fn worse_note(
    m: &Machine,
    h: &Health,
    new: &[&Change],
    was: Option<Verdict>,
    now_v: Verdict,
) -> Note {
    let mut what: Vec<String> = new
        .iter()
        .filter(|c| c.level.rank() > 0 && c.code != "verdict")
        .map(|c| c.text.clone())
        .collect();
    if what.is_empty() {
        what = health::assess(h)
            .findings
            .into_iter()
            .filter(|f| f.level == now_v)
            .map(|f| f.text)
            .collect();
    }
    if what.is_empty() {
        what.push(format!("it's {} now", now_v.as_str()));
    }
    let drive = match &h.serial {
        Some(sn) => format!("{} (S/N {sn})", h.label()),
        None => h.label(),
    };
    let failing = now_v == Verdict::Failing;
    let mut body = format!("{drive}: {}.", what.join("; "));
    match was {
        Some(w) if w != Verdict::Unknown => {
            body.push_str(&format!(" It was {} at the last sync.", w.as_str()))
        }
        Some(_) => {}
        None => body.push_str(" It wasn't there at the last sync."),
    }
    body.push_str(if failing {
        " Copy what matters off it now: BackDown's Rescue, on that machine, can copy the whole drive."
    } else {
        " Make sure what's on it has another copy."
    });
    Note {
        kind: "health",
        key: format!("health@{}:{}:verdict:{}", m.name, h.id, now_v.as_str()),
        level: now_v.as_str(),
        title: format!(
            "A drive on {} {}",
            m.name,
            if failing {
                "is failing"
            } else {
                "needs watching"
            }
        ),
        body,
        drive: Some(format!("@{}:{}", m.name, h.id)),
        quiet_for: 7 * 86_400,
    }
}

/// A word for a machine whose drives can't be shown, or not as they are now.
fn health_note(
    m: &Machine,
    state: Option<&str>,
    error: Option<&str>,
    drives: usize,
) -> Option<String> {
    match state {
        None if m.last_sync.is_none() => Some(format!(
            "{} hasn't been synced yet: its drive health comes with its first sync",
            m.name
        )),
        None => Some(format!(
            "{}'s drive health comes with its next sync",
            m.name
        )),
        Some("unsupported") => Some(format!(
            "update BackDown on {} to see its drive health",
            m.name
        )),
        Some("error") => Some(format!(
            "{}'s drive health couldn't be fetched at the last sync: {}",
            m.name,
            error.unwrap_or("it didn't say why")
        )),
        Some(_) if drives == 0 => Some(format!(
            "no drive health on {} yet: its BackDown reads it when its service runs, or with `backdown health` there",
            m.name
        )),
        Some(_) => None,
    }
}

/// Every machine's drives as last fetched (the worst first), how asking went, a word for a machine
/// that can't say, and its own latest notifications: the browser's "Other machines" and `backdown
/// health --machines`.
pub fn health_overview(db: &Db) -> Result<Value> {
    type StateRow = (i64, Option<i64>, String, Option<String>, Option<String>);
    let has = has_health_tables(db);
    let rank = |v: &Value| Verdict::parse(v["verdict"].as_str().unwrap_or("")).rank();
    let mut out = Vec::new();
    let mut worst = Verdict::Unknown;
    for m in list(db)? {
        let state: Option<StateRow> = if has {
            db.conn
                .query_row(
                    "SELECT asked_at, ok_at, state, error, notes FROM remote_health_state WHERE remote_id = ?1",
                    [m.id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?
        } else {
            None
        };
        let mut drives: Vec<Value> = Vec::new();
        if has {
            let mut stmt = db.conn.prepare_cached(
                "SELECT summary, fetched_at FROM remote_health WHERE remote_id = ?1",
            )?;
            let rows = stmt.query_map([m.id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (s, fetched) = row?;
                let Ok(mut v) = serde_json::from_str::<Value>(&s) else {
                    continue;
                };
                let read = v["health"]["read_at"].as_i64().unwrap_or(0);
                v["fetched_at"] = json!(fetched);
                v["current"] = json!(read >= fetched - HEALTH_STALE_DAYS * 86_400);
                v["machine"] = json!(m.name);
                drives.push(v);
            }
        }
        drives.sort_by_key(|v| {
            (
                std::cmp::Reverse(rank(v)),
                v["current"] != true,
                std::cmp::Reverse(v["health"]["read_at"].as_i64().unwrap_or(0)),
            )
        });
        let mine = drives
            .iter()
            .filter(|v| v["current"] == true)
            .map(|v| Verdict::parse(v["verdict"].as_str().unwrap_or("")))
            .max_by_key(|v| v.rank())
            .unwrap_or(Verdict::Unknown);
        if mine.rank() > worst.rank() || (worst == Verdict::Unknown && mine == Verdict::Ok) {
            worst = mine;
        }
        let (asked_at, ok_at, st, error, notes) = match &state {
            Some((a, o, s, e, n)) => (Some(*a), *o, Some(s.as_str()), e.as_deref(), n.as_deref()),
            None => (None, None, None, None, None),
        };
        let notes: Value = notes
            .and_then(|n| serde_json::from_str(n).ok())
            .unwrap_or(json!([]));
        out.push(json!({
            "id": m.id, "name": m.name, "url": m.url, "last_sync": m.last_sync, "last_try": m.last_try,
            "last_error": m.last_error, "version": m.info["version"],
            "health": {
                "state": st.unwrap_or("never"), "asked_at": asked_at, "ok_at": ok_at, "error": error,
                "note": health_note(&m, st, error, drives.len()),
            },
            "worst": mine, "drives": drives, "notes": notes,
        }));
    }
    Ok(json!({ "machines": out, "worst": worst, "stale_days": HEALTH_STALE_DAYS }))
}

// ---------- proof for a wipe

/// For a wipe: a copy on another machine of content about to be deleted here, confirmed by that
/// machine just now. Conversations are opened once per machine; one that can't be reached is left
/// out for the rest of the wipe.
pub struct Verifier<'a> {
    connect: &'a mut dyn Connect,
    peers: HashMap<i64, Option<Box<dyn Peer>>>,
    /// what went wrong reaching machines, for the wipe's problems list
    pub problems: Vec<String>,
}

impl<'a> Verifier<'a> {
    pub fn new(connect: &'a mut dyn Connect) -> Self {
        Verifier {
            connect,
            peers: HashMap::new(),
            problems: Vec::new(),
        }
    }

    /// A counting copy of `sha` (`size` bytes) on another machine, re-read there now (or, with
    /// `trust_index`, checked to be there as scanned): where it is, and how it was confirmed.
    pub fn confirm(
        &mut self,
        db: &Db,
        sha: &[u8],
        size: u64,
        trust_index: bool,
    ) -> Result<Option<(String, &'static str)>> {
        for c in copies_of(db, sha, size)?
            .into_iter()
            .filter(|c| c.counts)
            .take(5)
        {
            let m = get(db, c.machine_id)?;
            let peer = match self.peers.entry(m.id) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let p = match self.connect.connect(&m) {
                        Ok(p) => Some(p),
                        Err(err) => {
                            self.problems.push(format!(
                                "{} couldn't be reached, so its copies weren't used: {err:#}",
                                m.label()
                            ));
                            None
                        }
                    };
                    e.insert(p)
                }
            };
            let Some(peer) = peer else { continue };
            let secs = (60 + size / 20_000_000).min(3600);
            let (op, args) = if trust_index {
                ("stat", json!({ "ids": [c.rid] }))
            } else {
                (
                    "hash",
                    json!({ "ids": [c.rid], "mode": "full", "fresh": true, "budget": secs }),
                )
            };
            let ans = match peer.call(op, args, Duration::from_secs(secs + 30)) {
                Ok(a) => a,
                Err(err) => {
                    self.problems.push(format!(
                        "{} stopped answering, so its copies weren't used: {err:#}",
                        m.label()
                    ));
                    self.peers.insert(m.id, None);
                    continue;
                }
            };
            let r = ans["results"]
                .as_array()
                .and_then(|a| a.iter().find(|r| r["id"].as_i64() == Some(c.rid)))
                .cloned()
                .unwrap_or(Value::Null);
            let same = r["sha256"].as_str().and_then(unhex).as_deref() == Some(sha);
            let there = if trust_index {
                r["ok"] == true
            } else {
                r["read"] == true
            };
            if same && there {
                if !trust_index {
                    db.conn.execute(
                        "UPDATE remote_files SET hashed_at = ?3 WHERE remote_id = ?1 AND rid = ?2",
                        params![m.id, c.rid, now()],
                    )?;
                }
                return Ok(Some((
                    format!(
                        "{}{}{}",
                        m.url,
                        if c.path.starts_with('/') { "" } else { "/" },
                        c.path
                    ),
                    if trust_index {
                        "remote index"
                    } else {
                        "remote re-read"
                    },
                )));
            }
        }
        Ok(None)
    }
}
