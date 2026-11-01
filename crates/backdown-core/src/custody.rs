//! The chain of custody: an append-only, hash-chained log of what BackDown does to files, and
//! signed seals of it, for evidence handling.
//!
//! **What's recorded, and when.** One event per thing done, written in the same transaction as
//! the journal row of the module that did it, so an event is there exactly when the action is:
//!
//! - `copy`: a verified copy (filing a plan, apply.rs; a mirror sync, mirror.rs);
//! - `move`: the file at `src` is now at `dest` and no longer at `src` (a wipe's `--move-to`:
//!   copied, read back, then deleted, wipe.rs; a mirror's old copy set aside in
//!   `.backdown-versions`, mirror.rs, not re-read);
//! - `delete`: a wipe deleted `src`, with an identical copy kept at `dest` (wipe.rs);
//! - `link`: `src` now shares its storage with the identical `dest`, as a hardlink or a clone
//!   (dedup.rs);
//! - `merge`: a mail message from `src` written to `dest` by a mailbox merge (mailmerge.rs; in an
//!   mbox, `src` is the mbox file and the hash is the message's);
//! - `rescue-copy`: a file read out of a filesystem on a drive or image and copied to `dest`
//!   (rescue/files.rs); `src` is `<drive or image>@<filesystem's first byte>:/<path inside>`.
//!
//! `ref` names the job: `plan:N`, `wipe:N`, `dedup:N`, `merge:N`, `rescue:N` or `mirror:N`.
//! `src_sha256` and `dest_sha256` are the content's SHA-256 where BackDown read it or had it
//! confirmed: a wipe with `--trust-index` relies on the index for the copy, so there's no
//! `dest_sha256`. History from before this log was kept isn't in it, and nothing is backfilled:
//! `custody_start` says when it began.
//!
//! **The chain.** Each event's fields (everything but `prev` and `hash`) as canonical JSON
//! (`signing::canonical`): the `digest` is its SHA-256, and
//! `hash = SHA-256(prev + digest)`, the two as 64 lowercase hex digits each, joined, with the
//! first event's `prev` 64 zeros. Changing, removing, adding or reordering an event breaks the
//! chain from there on. Paths are the OS's raw bytes (paths.rs): in JSON a UTF-8 one is a string
//! (`src`, `dest`), any other is `src_base64` / `dest_base64`, standard base64 of those bytes
//! (UTF-16LE on Windows), so nothing is lost and the hash covers what was stored.
//!
//! **Seals.** `seal` signs the head `{kind, seq, hash, at}` with this installation's key
//! (signing.rs) and keeps it in `custody_seals`; the daemon seals at the end of every job that
//! added events, and an export seals first. A seal proves this key saw the log, intact, up to
//! there. It can't prove nothing happened outside BackDown, and the events after the last seal
//! can be removed without a trace until the next one.
//!
//! **Exports.** `export` writes `{v: 1, kind: "backdown-custody", created, log_starts, key,
//! filter, note, events, seals, sig}`, signed like `Signer::sign_json` (the canonical form of
//! all but `sig`). With a filter, events that don't match are kept *elided*, as
//! `{seq, digest, hash, elided: true}`: no paths, no hashes of files, but enough to check the
//! chain end to end. `verify` checks a bundle, and `check` the log in the index, the same way:
//! every event against its hash and the one before, every seal's signature against the event it
//! names, that the seals reach the last event, and an export's own signature.

use std::collections::BTreeSet;
use std::fmt;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::clock::now;
use crate::fmt::{hex, unhex32};
use crate::paths;
use crate::signing::{self, Signer, canonical};

/// The tables (made by `Db::open`): the log, its seals, and when it began. Added in 1.0 without
/// a schema bump (older versions ignore them).
pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS custody (
    seq         INTEGER PRIMARY KEY,  -- 1, 2, 3… in the order recorded
    at          INTEGER NOT NULL,     -- when it was done (unix seconds)
    action      TEXT NOT NULL,        -- copy | move | delete | link | merge | rescue-copy
    src         BLOB NOT NULL,        -- raw OS bytes (paths.rs)
    src_sha256  BLOB,
    dest        BLOB,
    dest_sha256 BLOB,
    size        INTEGER NOT NULL,
    ref         TEXT NOT NULL,        -- the job: plan:N | wipe:N | dedup:N | merge:N | rescue:N | mirror:N
    prev        BLOB NOT NULL,        -- the event before's hash (32 zero bytes for the first)
    hash        BLOB NOT NULL         -- SHA-256(prev + SHA-256(canonical JSON)), see custody.rs
);
CREATE TABLE IF NOT EXISTS custody_seals (
    id  INTEGER PRIMARY KEY,
    seq INTEGER NOT NULL,             -- the last event it covers
    at  INTEGER NOT NULL,
    doc TEXT NOT NULL                 -- the signed seal, JSON
);
CREATE TABLE IF NOT EXISTS custody_start (at INTEGER NOT NULL);
INSERT INTO custody_start(at) SELECT CAST(strftime('%s', 'now') AS INTEGER)
    WHERE NOT EXISTS (SELECT 1 FROM custody_start);
";

pub const BUNDLE_KIND: &str = "backdown-custody";
pub const SEAL_KIND: &str = "backdown-custody-seal";
/// The jobs an event can belong to (`ref` is `<kind>:<id>`).
pub const JOBS: &[&str] = &["plan", "wipe", "dedup", "merge", "rescue", "mirror"];
/// Problems listed at most; the rest are counted.
const MAX_PROBLEMS: usize = 50;

/// What was done to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Copy,
    Move,
    Delete,
    Link,
    Merge,
    RescueCopy,
}

impl Action {
    pub fn name(self) -> &'static str {
        match self {
            Action::Copy => "copy",
            Action::Move => "move",
            Action::Delete => "delete",
            Action::Link => "link",
            Action::Merge => "merge",
            Action::RescueCopy => "rescue-copy",
        }
    }
}

/// One thing done, to record (`append`).
#[derive(Debug, Clone)]
pub struct Event {
    pub at: i64,
    pub action: Action,
    /// the job, `plan:3`
    pub job: String,
    pub src: Vec<u8>,
    pub src_sha256: Option<Vec<u8>>,
    pub dest: Option<Vec<u8>>,
    pub dest_sha256: Option<Vec<u8>>,
    pub size: u64,
}

impl Event {
    /// Done now, by job `(kind, id)` (one of `JOBS`), to `src` (raw path bytes).
    pub fn new(
        action: Action,
        job: (&str, i64),
        src: &[u8],
        src_sha256: Option<&[u8]>,
        size: u64,
    ) -> Event {
        Event {
            at: now(),
            action,
            job: format!("{}:{}", job.0, job.1),
            src: src.to_vec(),
            src_sha256: src_sha256.map(<[u8]>::to_vec),
            dest: None,
            dest_sha256: None,
            size,
        }
    }

    /// Where it went (or, for a deletion, the copy kept), and that copy's hash as read.
    pub fn to(mut self, dest: &[u8], dest_sha256: Option<&[u8]>) -> Event {
        self.dest = Some(dest.to_vec());
        self.dest_sha256 = dest_sha256.map(<[u8]>::to_vec);
        self
    }
}

/// A row of the log.
struct Row {
    seq: i64,
    at: i64,
    action: String,
    src: Vec<u8>,
    src_sha256: Option<Vec<u8>>,
    dest: Option<Vec<u8>>,
    dest_sha256: Option<Vec<u8>>,
    size: i64,
    job: String,
    prev: Vec<u8>,
    hash: Vec<u8>,
}

const ROW_COLS: &str = "seq, at, action, src, src_sha256, dest, dest_sha256, size, ref, prev, hash";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        seq: r.get(0)?,
        at: r.get(1)?,
        action: r.get(2)?,
        src: r.get(3)?,
        src_sha256: r.get(4)?,
        dest: r.get(5)?,
        dest_sha256: r.get(6)?,
        size: r.get(7)?,
        job: r.get(8)?,
        prev: r.get(9)?,
        hash: r.get(10)?,
    })
}

/// A path as JSON: `key` as a string when it's UTF-8, else `key_base64` (its raw bytes).
fn put_path(m: &mut Map<String, Value>, key: &str, b: &[u8]) {
    match paths::from_bytes(b).to_str() {
        Some(s) => m.insert(key.into(), Value::String(s.into())),
        None => m.insert(format!("{key}_base64"), Value::String(STANDARD.encode(b))),
    };
}

impl Row {
    /// The hashed fields.
    fn fields(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("seq".into(), json!(self.seq));
        m.insert("at".into(), json!(self.at));
        m.insert("action".into(), json!(self.action));
        put_path(&mut m, "src", &self.src);
        if let Some(h) = &self.src_sha256 {
            m.insert("src_sha256".into(), json!(hex(h)));
        }
        if let Some(d) = &self.dest {
            put_path(&mut m, "dest", d);
        }
        if let Some(h) = &self.dest_sha256 {
            m.insert("dest_sha256".into(), json!(hex(h)));
        }
        m.insert("size".into(), json!(self.size));
        m.insert("ref".into(), json!(self.job));
        m
    }

    /// As exported: its fields, `prev` and `hash`.
    fn json(&self) -> Value {
        let mut m = self.fields();
        m.insert("prev".into(), json!(hex(&self.prev)));
        m.insert("hash".into(), json!(hex(&self.hash)));
        Value::Object(m)
    }

    /// As exported when it doesn't match the filter: what checking the chain needs, nothing more.
    fn elided(&self) -> Value {
        json!({
            "seq": self.seq,
            "digest": hex(&digest(&self.fields())),
            "hash": hex(&self.hash),
            "elided": true,
        })
    }
}

fn hex32(v: &Value) -> Option<[u8; 32]> {
    v.as_str().and_then(unhex32)
}

/// SHA-256 of an event's fields in canonical form.
fn digest(fields: &Map<String, Value>) -> [u8; 32] {
    Sha256::digest(canonical(&Value::Object(fields.clone()))).into()
}

/// `hash = SHA-256(prev + digest)`, both as lowercase hex.
fn chain(prev: &[u8], digest: &[u8; 32]) -> [u8; 32] {
    Sha256::digest(format!("{}{}", hex(prev), hex(digest)).as_bytes()).into()
}

/// Run `f` in a transaction: the caller's, if one is open, else one of its own (taking the write
/// lock first, so the head can't move between reading it and adding to it).
fn in_tx<T>(conn: &Connection, f: impl FnOnce() -> Result<T>) -> Result<T> {
    if !conn.is_autocommit() {
        return f();
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    match f() {
        Ok(t) => match conn.execute_batch("COMMIT") {
            Ok(()) => Ok(t),
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e.into())
            }
        },
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

fn append_now(conn: &Connection, e: &Event) -> Result<i64> {
    let (last, prev) = conn
        .query_row(
            "SELECT seq, hash FROM custody ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?
        .unwrap_or((0, vec![0; 32]));
    let mut r = Row {
        seq: last + 1,
        at: e.at,
        action: e.action.name().into(),
        src: e.src.clone(),
        src_sha256: e.src_sha256.clone(),
        dest: e.dest.clone(),
        dest_sha256: e.dest_sha256.clone(),
        size: e.size as i64,
        job: e.job.clone(),
        prev,
        hash: Vec::new(),
    };
    r.hash = chain(&r.prev, &digest(&r.fields())).to_vec();
    conn.prepare_cached(&format!(
        "INSERT INTO custody({ROW_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
    ))?
    .execute(params![
        r.seq,
        r.at,
        r.action,
        r.src,
        r.src_sha256,
        r.dest,
        r.dest_sha256,
        r.size,
        r.job,
        r.prev,
        r.hash
    ])?;
    Ok(r.seq)
}

/// Record `e` at the end of the chain; its number. Inside the caller's transaction when one is
/// open (so it's kept exactly when the caller's own journal row is), else in one of its own.
pub fn append(conn: &Connection, e: &Event) -> Result<i64> {
    in_tx(conn, || append_now(conn, e))
}

/// Record several, in order, in one transaction.
pub fn append_all(conn: &Connection, events: &[Event]) -> Result<()> {
    in_tx(conn, || {
        for e in events {
            append_now(conn, e)?;
        }
        Ok(())
    })
}

/// Write a journal row (`write`) and record `e`, in one transaction: both are kept, or neither.
pub fn journaled<T>(
    conn: &Connection,
    e: &Event,
    write: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    in_tx(conn, || {
        let t = write()?;
        append_now(conn, e)?;
        Ok(t)
    })
}

/// The last event: its number and hash.
pub fn head(conn: &Connection) -> Result<Option<(i64, Vec<u8>)>> {
    Ok(conn
        .query_row(
            "SELECT seq, hash FROM custody ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

/// When the log began here (this version of BackDown first opened the index).
pub fn started(conn: &Connection) -> Result<Option<i64>> {
    Ok(conn
        .query_row("SELECT MIN(at) FROM custody_start", [], |r| r.get(0))
        .optional()?
        .flatten())
}

/// The seals kept, oldest event first.
fn seals(conn: &Connection) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare("SELECT doc FROM custody_seals ORDER BY seq, id")?;
    let docs = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // a row that isn't JSON any more is kept as what it is, so checking it says so
    Ok(docs
        .into_iter()
        .map(|d| serde_json::from_str(&d).unwrap_or(Value::String(d)))
        .collect())
}

fn sign_head(signer: &Signer, seq: i64, hash: &[u8]) -> Result<Value> {
    let mut doc = json!({ "kind": SEAL_KIND, "seq": seq, "hash": hex(hash), "at": now() });
    signer.sign_json(&mut doc)?;
    Ok(doc)
}

/// Whether `s` is a good seal by `key` of event `seq` with `hash`.
fn seals_head(s: &Value, key: &str, seq: i64, hash: &[u8]) -> bool {
    s["seq"].as_i64() == Some(seq)
        && s["hash"].as_str() == Some(&hex(hash))
        && s["key"].as_str() == Some(key)
        && s["kind"] == SEAL_KIND
        && signing::verify_json(s).is_ok()
}

/// Sign the log's head with `signer` and keep the seal: the seal, or None when there's nothing
/// new to seal (no events, or the head is sealed by this key already). Refused when the log
/// doesn't check out since the last good seal, or a seal was tampered with: a seal mustn't
/// vouch for a change.
pub fn seal(conn: &Connection, signer: &Signer) -> Result<Option<Value>> {
    let Some((seq, hash)) = head(conn)? else {
        return Ok(None);
    };
    let kept = seals(conn)?;
    let key = signer.public_hex();
    if kept.iter().any(|s| seals_head(s, &key, seq, &hash)) {
        return Ok(None);
    }
    let problems = since_last_seal(conn, &kept)?;
    if let Some(p) = problems.first() {
        bail!(
            "the log doesn't check out ({p}{}): see `backdown custody check`",
            if problems.len() > 1 {
                format!("; and {} more", problems.len() - 1)
            } else {
                String::new()
            }
        );
    }
    let doc = sign_head(signer, seq, &hash)?;
    conn.execute(
        "INSERT INTO custody_seals(seq, at, doc) VALUES (?1, ?2, ?3)",
        params![seq, doc["at"].as_i64(), doc.to_string()],
    )?;
    Ok(Some(doc))
}

/// Whether the head is sealed by `signer` already.
pub fn sealed(conn: &Connection, signer: &Signer) -> Result<bool> {
    let Some((seq, hash)) = head(conn)? else {
        return Ok(true);
    };
    let key = signer.public_hex();
    Ok(seals(conn)?.iter().any(|s| seals_head(s, &key, seq, &hash)))
}

/// What's wrong with the log since its newest good seal (or since the start), and with the seals.
fn since_last_seal(conn: &Connection, kept: &[Value]) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    let mut start: Option<(i64, [u8; 32])> = None;
    for s in kept.iter().rev() {
        let good = s["kind"] == SEAL_KIND && signing::verify_json(s).is_ok();
        let (Some(seq), Some(h)) = (s["seq"].as_i64(), hex32(&s["hash"])) else {
            problems.push(format!("{} is damaged", seal_name(s)));
            continue;
        };
        if !good {
            problems.push(format!("{} doesn't verify", seal_name(s)));
            continue;
        }
        if start.is_some() {
            continue;
        }
        let there: Option<Vec<u8>> = conn
            .query_row(
                "SELECT hash FROM custody WHERE seq = ?1",
                params![seq],
                |r| r.get(0),
            )
            .optional()?;
        if there.as_deref() != Some(h.as_slice()) {
            problems.push(format!(
                "event {seq} isn't what was sealed on {}",
                when(s["at"].as_i64())
            ));
        }
        start = Some((seq, h));
    }
    if !problems.is_empty() {
        return Ok(problems);
    }
    let mut walk = match start {
        Some((seq, h)) => Walk::after(seq, h),
        None => Walk::new(),
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {ROW_COLS} FROM custody WHERE seq > ?1 ORDER BY seq"
    ))?;
    for r in stmt.query_map(params![start.map_or(0, |s| s.0)], row)? {
        walk.event(&r?.json());
    }
    Ok(walk.rep.problems)
}

fn when(at: Option<i64>) -> String {
    at.map_or_else(|| "an unknown day".into(), crate::fmt::day)
}

fn seal_name(s: &Value) -> String {
    match s["seq"].as_i64() {
        Some(seq) => format!("the seal of {} (up to event {seq})", when(s["at"].as_i64())),
        None => "a seal".into(),
    }
}

// ---------- checking

/// What checking a chain found.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    /// nothing's wrong
    pub ok: bool,
    pub events: u64,
    /// of them, left out of an export (only their digest and hash are there)
    pub elided: u64,
    /// when the first and last events shown were done
    pub first_at: Option<i64>,
    pub last_at: Option<i64>,
    pub last_seq: i64,
    /// the last event a good seal covers
    pub sealed_to: i64,
    /// good seals
    pub seals: u64,
    /// the keys that made them (fingerprints)
    pub seal_keys: Vec<String>,
    /// the log in the index: events after the last seal (sealed when the job adding them ends)
    pub unsealed: u64,
    /// when the log began
    pub log_starts: Option<i64>,
    /// an export's: when it was made and the key that signed it (its fingerprint)
    pub created: Option<i64>,
    pub key: Option<String>,
    /// what doesn't check out (the first ones; `more_problems` counts the rest)
    pub problems: Vec<String>,
    pub more_problems: u64,
}

/// Goes along a chain event by event, from the log or a bundle, noting what doesn't check out.
struct Walk {
    next: i64,
    /// the hash the last event gave
    prev: [u8; 32],
    hashes: Vec<(i64, [u8; 32])>,
    rep: Report,
}

impl Walk {
    fn new() -> Walk {
        Walk {
            next: 1,
            prev: [0; 32],
            hashes: Vec::new(),
            rep: Report::default(),
        }
    }

    /// Carrying on after event `seq`, whose hash is `hash`.
    fn after(seq: i64, hash: [u8; 32]) -> Walk {
        Walk {
            next: seq + 1,
            prev: hash,
            hashes: vec![(seq, hash)],
            rep: Report {
                last_seq: seq,
                ..Report::default()
            },
        }
    }

    fn problem(&mut self, p: String) {
        if self.rep.problems.len() < MAX_PROBLEMS {
            self.rep.problems.push(p);
        } else {
            self.rep.more_problems += 1;
        }
    }

    fn event(&mut self, ev: &Value) {
        let Some(seq) = ev["seq"].as_i64() else {
            self.problem(format!(
                "an event after event {} has no number (seq)",
                self.next - 1
            ));
            return;
        };
        let gap = seq != self.next;
        if seq == self.next + 1 {
            self.problem(format!("event {} is missing", self.next));
        } else if seq > self.next {
            self.problem(format!("events {} to {} are missing", self.next, seq - 1));
        } else if seq < self.next {
            self.problem(format!(
                "event {seq} is out of place (after event {})",
                self.next - 1
            ));
        }
        self.next = seq + 1;
        self.rep.last_seq = seq;
        let Some(hash) = hex32(&ev["hash"]) else {
            self.problem(format!("event {seq} has no hash, or a damaged one"));
            return;
        };
        let elided = ev["elided"] == true;
        let digest = if elided {
            match hex32(&ev["digest"]) {
                Some(d) => d,
                None => {
                    self.problem(format!(
                        "event {seq} is left out without its digest, so the chain can't be checked through it"
                    ));
                    self.prev = hash;
                    self.hashes.push((seq, hash));
                    return;
                }
            }
        } else {
            let mut fields = ev.as_object().cloned().unwrap_or_default();
            fields.remove("prev");
            fields.remove("hash");
            digest(&fields)
        };
        let base = match ev.get("prev") {
            None => self.prev,
            Some(p) => match hex32(p) {
                Some(p) => {
                    if p != self.prev && !gap {
                        self.problem(format!(
                            "event {seq} doesn't follow event {}: an event was taken out, added or moved between them",
                            seq - 1
                        ));
                    }
                    p
                }
                None => {
                    self.problem(format!("event {seq}'s link to the one before is damaged"));
                    self.prev
                }
            },
        };
        if chain(&base, &digest) != hash {
            self.problem(format!("event {seq} was changed after it was recorded"));
        }
        self.rep.events += 1;
        if elided {
            self.rep.elided += 1;
        } else if let Some(at) = ev["at"].as_i64() {
            self.rep.first_at.get_or_insert(at);
            self.rep.last_at = Some(at);
        }
        self.prev = hash;
        self.hashes.push((seq, hash));
    }

    /// Check `seals` against the events gone through. `tail_is_problem`: events no seal covers
    /// are a problem (an export, sealed when made), not just unsealed yet (the log in the index).
    /// Also the keys that made a good seal of the last event (public, hex).
    fn finish(mut self, seals: &[Value], tail_is_problem: bool) -> (Report, Vec<String>) {
        self.hashes.sort_by_key(|h| h.0);
        let last = self.rep.last_seq;
        let mut covered = 0i64;
        let mut keys = BTreeSet::new();
        let mut head_keys = Vec::new();
        for s in seals {
            let name = seal_name(s);
            let key = match signing::verify_json(s) {
                Ok(k) if s["kind"] == SEAL_KIND => k,
                Ok(_) => {
                    self.problem(format!("{name} isn't a chain-of-custody seal"));
                    continue;
                }
                Err(_) => {
                    self.problem(format!(
                        "{name} doesn't verify: it was changed, or isn't BackDown's"
                    ));
                    continue;
                }
            };
            let (Some(seq), Some(h)) = (s["seq"].as_i64(), hex32(&s["hash"])) else {
                self.problem(format!("{name} is damaged"));
                continue;
            };
            match self.hashes.binary_search_by_key(&seq, |x| x.0) {
                Ok(i) if self.hashes[i].1 == h => {
                    covered = covered.max(seq);
                    keys.insert(signing::fingerprint(&key));
                    self.rep.seals += 1;
                    if seq == last {
                        head_keys.push(key);
                    }
                }
                Ok(_) => self.problem(format!(
                    "event {seq} isn't what was sealed on {}: the log up to there was changed after it was sealed",
                    when(s["at"].as_i64())
                )),
                Err(_) if seq > last => self.problem(format!(
                    "{name} goes further than the log, which ends at event {last}: events after it were removed"
                )),
                Err(_) => self.problem(format!("{name} is for an event that isn't there")),
            }
        }
        if last > covered {
            if !tail_is_problem {
                self.rep.unsealed = (last - covered) as u64;
            } else if covered == 0 {
                self.problem("no event is covered by a seal".into());
            } else {
                self.problem(format!(
                    "events after seq {covered} aren't covered by a seal"
                ));
            }
        }
        self.rep.sealed_to = covered;
        self.rep.seal_keys = keys.into_iter().collect();
        self.rep.ok = self.rep.problems.is_empty() && self.rep.more_problems == 0;
        (self.rep, head_keys)
    }
}

/// Check the log in the index: every event and seal. Events after the last seal are counted
/// (`unsealed`), not a problem: they're sealed when the job adding them ends.
pub fn check(conn: &Connection) -> Result<Report> {
    snapshot(conn, || {
        let kept = seals(conn)?;
        let mut walk = Walk::new();
        let mut stmt = conn.prepare(&format!("SELECT {ROW_COLS} FROM custody ORDER BY seq"))?;
        for r in stmt.query_map([], row)? {
            walk.event(&r?.json());
        }
        let (mut rep, _) = walk.finish(&kept, false);
        rep.log_starts = started(conn)?;
        Ok(rep)
    })
}

/// Read in one snapshot, so the events and seals are of the same moment.
fn snapshot<T>(conn: &Connection, f: impl FnOnce() -> Result<T>) -> Result<T> {
    if !conn.is_autocommit() {
        return f();
    }
    conn.execute_batch("BEGIN")?;
    let r = f();
    let _ = conn.execute_batch(if r.is_ok() { "COMMIT" } else { "ROLLBACK" });
    r
}

// ---------- exporting

/// Which events an export shows in full; the others are elided. Every condition given must hold.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// done at or after (unix seconds)
    pub since: Option<i64>,
    /// the job, `plan:3`
    pub job: Option<String>,
    /// `src` or `dest` is this path or inside it (raw bytes)
    pub path: Option<Vec<u8>>,
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        self.since.is_none() && self.job.is_none() && self.path.is_none()
    }

    fn matches(&self, r: &Row) -> bool {
        self.since.is_none_or(|t| r.at >= t)
            && self.job.as_ref().is_none_or(|j| *j == r.job)
            && self.path.as_ref().is_none_or(|p| {
                let dir = paths::dir_prefix(&paths::from_bytes(p));
                let within = |b: &[u8]| b == p.as_slice() || b.starts_with(&dir);
                within(&r.src) || r.dest.as_deref().is_some_and(within)
            })
    }

    fn json(&self) -> Value {
        let mut m = Map::new();
        if let Some(t) = self.since {
            m.insert("since".into(), json!(t));
        }
        if let Some(j) = &self.job {
            m.insert("ref".into(), json!(j));
        }
        if let Some(p) = &self.path {
            put_path(&mut m, "path", p);
        }
        Value::Object(m)
    }

    /// For people: "done since 1 Oct 2026, in plan:3".
    pub fn describe(&self) -> String {
        let mut v = Vec::new();
        if let Some(t) = self.since {
            v.push(format!("done since {}", crate::fmt::day(t)));
        }
        if let Some(j) = &self.job {
            v.push(format!("in {j}"));
        }
        if let Some(p) = &self.path {
            v.push(format!("in {}", paths::show(&paths::from_bytes(p))));
        }
        v.join(", ")
    }
}

/// `plan:3`, `plan 3` or `plan3` as a job's `ref` (`plan:3`).
pub fn parse_ref(s: &str) -> Result<String> {
    let s = s.trim();
    for kind in JOBS {
        if let Some(rest) = s.strip_prefix(kind) {
            let id = rest.trim_start_matches([':', ' ', '=']);
            if let Ok(n) = id.parse::<u64>() {
                return Ok(format!("{kind}:{n}"));
            }
        }
    }
    bail!(
        "{s:?} isn't a job: use plan:N, wipe:N, dedup:N, merge:N, rescue:N or mirror:N (N is its number)"
    )
}

/// A date for `--since`, in UTC: `2026-10-01`, `2026-10-01T14:30`, `2026-10-01 14:30:05`, or
/// unix seconds (`@1790000000`).
pub fn parse_date(s: &str) -> Result<i64> {
    let s = s.trim();
    if let Some(n) = s.strip_prefix('@') {
        return n.parse().context("@ is followed by unix seconds");
    }
    let bad = || anyhow::anyhow!("{s:?} isn't a date: use 2026-10-01 or 2026-10-01T14:30 (UTC)");
    let (date, time) = match s.split_once(['T', ' ']) {
        Some((d, t)) => (d, t.trim_end_matches('Z')),
        None => (s, ""),
    };
    let ymd: Vec<i64> = date
        .split('-')
        .map(|x| x.parse().map_err(|_| bad()))
        .collect::<Result<_>>()?;
    let [y, m, d] = ymd[..] else {
        return Err(bad());
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(bad());
    }
    let hms: Vec<i64> = if time.is_empty() {
        Vec::new()
    } else {
        time.split(':')
            .map(|x| x.parse().map_err(|_| bad()))
            .collect::<Result<_>>()?
    };
    let (h, mi, sec) = match hms[..] {
        [] => (0, 0, 0),
        [h, mi] => (h, mi, 0),
        [h, mi, sec] => (h, mi, sec),
        _ => return Err(bad()),
    };
    if h > 23 || mi > 59 || sec > 60 {
        return Err(bad());
    }
    // days from civil (Howard Hinnant)
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Ok(days * 86_400 + h * 3600 + mi * 60 + sec)
}

/// A path for `Filter::path`, as the log has it (absolute; a drive that's gone is fine).
pub fn path_filter(p: &Path) -> Vec<u8> {
    let p = paths::canonical(p)
        .or_else(|_| std::path::absolute(p))
        .unwrap_or_else(|_| p.to_path_buf());
    let mut b = paths::to_bytes(&p);
    let sep = paths::to_bytes(Path::new(std::path::MAIN_SEPARATOR_STR));
    while b.len() > sep.len() && b.ends_with(&sep) {
        b.truncate(b.len() - sep.len());
    }
    b
}

/// What an export holds.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Exported {
    pub events: u64,
    /// shown in full
    pub shown: u64,
    pub elided: u64,
    pub last_seq: i64,
    /// the last event a seal covers
    pub sealed_to: i64,
    /// a seal was made for this export (the head wasn't sealed by this key yet)
    pub sealed_now: bool,
    pub log_starts: Option<i64>,
    pub first_at: Option<i64>,
    pub last_at: Option<i64>,
    /// the key that signed it (fingerprint)
    pub key: String,
    /// what doesn't check out in the log (then no new seal is made: verifying says what's wrong)
    pub problems: Vec<String>,
}

/// The sentence an export (and the CLI) says about where the log begins.
pub fn start_note(log_starts: Option<i64>) -> String {
    match log_starts {
        Some(t) => format!(
            "The log starts on {}, when this BackDown was installed (or updated to a version that keeps it): what happened before isn't in it.",
            crate::fmt::day(t)
        ),
        None => "What happened before this BackDown kept the log isn't in it.".into(),
    }
}

/// Write the log as a signed bundle to `out` (see the module doc), showing what `filter`
/// matches and eliding the rest. When the head isn't sealed by `signer` yet and the log checks
/// out, the bundle gets a seal made now (not kept: `seal` keeps one). Reads only.
pub fn export(
    conn: &Connection,
    signer: &Signer,
    filter: &Filter,
    out: &mut dyn Write,
) -> Result<Exported> {
    snapshot(conn, || {
        let log_starts = started(conn)?;
        let mut seals = seals(conn)?;
        let mut top = Map::new();
        top.insert("v".into(), json!(1));
        top.insert("kind".into(), json!(BUNDLE_KIND));
        top.insert("created".into(), json!(now()));
        top.insert("log_starts".into(), json!(log_starts));
        top.insert("key".into(), json!(signer.public_hex()));
        let mut note = start_note(log_starts);
        if !filter.is_empty() {
            top.insert("filter".into(), filter.json());
            note.push_str(&format!(
                " Only events {} are shown; the others are elided: just their number, digest and hash, so the chain still checks from end to end.",
                filter.describe()
            ));
        }
        top.insert("note".into(), json!(note));
        // the head of the file, then each event on a line of its own
        for k in [
            "v",
            "kind",
            "created",
            "log_starts",
            "key",
            "filter",
            "note",
        ] {
            if let Some(v) = top.get(k) {
                write!(
                    out,
                    "{}{}:{}",
                    if k == "v" { "{" } else { "," },
                    serde_json::to_string(k)?,
                    serde_json::to_string(v)?
                )?;
            }
        }
        out.write_all(b",\n\"events\":[")?;
        let mut events: Vec<u8> = Vec::new();
        let mut walk = Walk::new();
        let mut ex = Exported {
            log_starts,
            key: signer.fingerprint(),
            ..Exported::default()
        };
        let mut stmt = conn.prepare(&format!("SELECT {ROW_COLS} FROM custody ORDER BY seq"))?;
        for r in stmt.query_map([], row)? {
            let r = r?;
            let full = r.json();
            walk.event(&full);
            let show = filter.matches(&r);
            let ev = if show { full } else { r.elided() };
            let bytes = canonical(&ev);
            if ex.events > 0 {
                events.push(b',');
                out.write_all(b",")?;
            }
            out.write_all(b"\n")?;
            out.write_all(&bytes)?;
            events.extend_from_slice(&bytes);
            ex.events += 1;
            if show {
                ex.shown += 1;
                ex.first_at.get_or_insert(r.at);
                ex.last_at = Some(r.at);
            } else {
                ex.elided += 1;
            }
        }
        let last_hash = walk.prev;
        let (rep, head_keys) = walk.finish(&seals, false);
        ex.last_seq = rep.last_seq;
        ex.sealed_to = rep.sealed_to;
        ex.problems = rep.problems;
        if rep.more_problems > 0 {
            ex.problems
                .push(format!("and {} more problems", rep.more_problems));
        }
        if ex.problems.is_empty() && rep.last_seq > 0 && !head_keys.contains(&signer.public_hex()) {
            seals.push(sign_head(signer, rep.last_seq, &last_hash)?);
            ex.sealed_to = rep.last_seq;
            ex.sealed_now = true;
        }
        top.insert("seals".into(), Value::Array(seals));
        let sig = signer.sign(&message(&top, &events));
        write!(
            out,
            "\n],\n\"seals\":{},\n\"sig\":{}}}\n",
            serde_json::to_string(&top["seals"])?,
            serde_json::to_string(&sig)?
        )?;
        out.flush()?;
        Ok(ex)
    })
}

/// The canonical form of a bundle without its `sig`: `top`'s fields with `events` (the events'
/// canonical forms, comma-separated) put in their place. The same bytes as
/// `canonical(bundle without sig)`, without holding every event as a JSON value.
fn message(top: &Map<String, Value>, events: &[u8]) -> Vec<u8> {
    let mut keys: Vec<&str> = top
        .keys()
        .map(String::as_str)
        .filter(|k| *k != "events" && *k != "sig")
        .chain(std::iter::once("events"))
        .collect();
    keys.sort_unstable();
    let mut out = vec![b'{'];
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        out.extend_from_slice(serde_json::to_string(k).expect("a string").as_bytes());
        out.push(b':');
        if *k == "events" {
            out.push(b'[');
            out.extend_from_slice(events);
            out.push(b']');
        } else {
            out.extend_from_slice(&canonical(&top[*k]));
        }
    }
    out.push(b'}');
    out
}

// ---------- verifying an export

/// Check an exported bundle: its chain, its seals, and its own signature. Reads it as it goes,
/// so a big export isn't held as JSON values.
pub fn verify(input: impl Read) -> Result<Report> {
    let mut walk = Walk::new();
    let mut events = Vec::new();
    let mut de = serde_json::Deserializer::from_reader(std::io::BufReader::new(input));
    let top = BundleSeed {
        walk: &mut walk,
        events: &mut events,
    }
    .deserialize(&mut de)
    .context("this isn't a BackDown chain-of-custody export, or it's damaged")?;
    de.end()
        .context("this isn't a BackDown chain-of-custody export: more follows it")?;
    if top.get("kind") != Some(&json!(BUNDLE_KIND)) {
        bail!("this isn't a BackDown chain-of-custody export (its kind isn't {BUNDLE_KIND})");
    }
    if top.get("v") != Some(&json!(1)) {
        bail!("this export is of a version this BackDown doesn't know: update BackDown");
    }
    let key = top.get("key").and_then(Value::as_str);
    let mut first = Vec::new();
    match (key, top.get("sig").and_then(Value::as_str)) {
        (Some(k), Some(sig)) => {
            if signing::verify(k, &message(&top, &events), sig).is_err() {
                first.push(
                    "the export itself was changed after it was made: its signature doesn't match"
                        .to_string(),
                );
            }
        }
        _ => first.push("the export isn't signed".to_string()),
    }
    let seals = top
        .get("seals")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (mut rep, _) = walk.finish(&seals, true);
    if !first.is_empty() {
        first.append(&mut rep.problems);
        rep.problems = first;
        rep.ok = false;
    }
    rep.created = top.get("created").and_then(Value::as_i64);
    rep.key = key.map(signing::fingerprint);
    rep.log_starts = top.get("log_starts").and_then(Value::as_i64);
    Ok(rep)
}

/// Reads a bundle's top level, handing each event to the walk as it's read and keeping its
/// canonical bytes for the signature; everything else is returned.
struct BundleSeed<'a> {
    walk: &'a mut Walk,
    events: &'a mut Vec<u8>,
}

impl<'de> DeserializeSeed<'de> for BundleSeed<'_> {
    type Value = Map<String, Value>;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for BundleSeed<'_> {
    type Value = Map<String, Value>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a chain-of-custody export (a JSON object)")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut top = Map::new();
        let mut seen = false;
        while let Some(k) = map.next_key::<String>()? {
            if k == "events" {
                if seen {
                    return Err(de::Error::duplicate_field("events"));
                }
                seen = true;
                map.next_value_seed(EventsSeed {
                    walk: &mut *self.walk,
                    events: &mut *self.events,
                })?;
            } else {
                let v: Value = map.next_value()?;
                if top.insert(k.clone(), v).is_some() {
                    return Err(de::Error::custom(format!("{k} is there twice")));
                }
            }
        }
        if !seen {
            return Err(de::Error::missing_field("events"));
        }
        Ok(top)
    }
}

struct EventsSeed<'a> {
    walk: &'a mut Walk,
    events: &'a mut Vec<u8>,
}

impl<'de> DeserializeSeed<'de> for EventsSeed<'_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for EventsSeed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a list of events")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut first = true;
        while let Some(ev) = seq.next_element::<Value>()? {
            if !first {
                self.events.push(b',');
            }
            first = false;
            self.events.extend_from_slice(&canonical(&ev));
            self.walk.event(&ev);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_is_as_documented() {
        // the chain's formula, spelled out, so the guide's recipe stays true
        let fields: Map<String, Value> = serde_json::from_str(
            r#"{"seq":1,"at":1790000000,"action":"copy","src":"/media/old-drive/a.jpg","size":3,"ref":"plan:1"}"#,
        )
        .unwrap();
        let text = String::from_utf8(canonical(&Value::Object(fields.clone()))).unwrap();
        assert_eq!(
            text,
            r#"{"action":"copy","at":1790000000,"ref":"plan:1","seq":1,"size":3,"src":"/media/old-drive/a.jpg"}"#
        );
        let d = signing::sha256_hex(text.as_bytes());
        assert_eq!(hex(&digest(&fields)), d);
        let h = signing::sha256_hex(format!("{}{d}", "0".repeat(64)).as_bytes());
        assert_eq!(hex(&chain(&[0; 32], &digest(&fields))), h);
    }

    #[test]
    fn dates_and_jobs() {
        assert_eq!(parse_date("1970-01-01").unwrap(), 0);
        assert_eq!(parse_date("2026-09-21T14:13:20Z").unwrap(), 1_790_000_000);
        assert_eq!(parse_date("2026-09-21 14:13:20").unwrap(), 1_790_000_000);
        assert_eq!(parse_date("2026-09-21T14:13").unwrap(), 1_789_999_980);
        assert_eq!(parse_date("2000-03-01").unwrap(), 951_868_800);
        assert_eq!(parse_date("@123").unwrap(), 123);
        for bad in ["yesterday", "2026-13-01", "2026-10", "2026-10-01T25:00"] {
            assert!(parse_date(bad).is_err(), "{bad}");
        }
        assert_eq!(parse_ref("plan:3").unwrap(), "plan:3");
        assert_eq!(parse_ref("wipe 12").unwrap(), "wipe:12");
        assert_eq!(parse_ref("mirror2").unwrap(), "mirror:2");
        assert!(parse_ref("scan:3").is_err());
        assert!(parse_ref("plan:x").is_err());
    }

    #[test]
    fn the_signed_message_is_the_canonical_form() {
        let mut top = Map::new();
        top.insert("v".into(), json!(1));
        top.insert("kind".into(), json!(BUNDLE_KIND));
        top.insert("seals".into(), json!([{ "b": 1, "a": 2 }]));
        top.insert("zz".into(), json!("é"));
        let evs = [
            json!({ "seq": 1, "hash": "x" }),
            json!({ "seq": 2, "a": [1] }),
        ];
        let bytes: Vec<u8> = evs
            .iter()
            .map(canonical)
            .collect::<Vec<_>>()
            .join(&b","[..]);
        let mut whole = top.clone();
        whole.insert("events".into(), Value::Array(evs.to_vec()));
        assert_eq!(message(&top, &bytes), canonical(&Value::Object(whole)));
        assert_eq!(
            message(&Map::new(), b""),
            canonical(&json!({ "events": [] }))
        );
    }
}
