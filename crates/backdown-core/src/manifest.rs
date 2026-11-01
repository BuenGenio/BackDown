//! Signed, tamper-evident manifests: a statement, signed with this installation's key
//! (signing.rs), of exactly what one drive (an indexed root) held at one finished scan. Every
//! file's path, size and content hash go into one Merkle root; the document with that root is
//! signed; and each drive's manifests are chained, each naming the one before it, so leaving one
//! out or reordering them shows. Checking one needs only the document, its listing and this
//! description: nothing of BackDown.
//!
//! ## The listing
//!
//! JSON Lines, one file per line, in leaf order (below):
//!
//! ```text
//! {"path":"photos/2019/beach.jpg","size":2481152,"sha256":"9f86d081…"}
//! {"path":"notes.txt","size":120,"sha256":null}
//! ```
//!
//! `path` is relative to the drive, with the separator of the system that made the manifest (`/`;
//! `\` on Windows). A name that isn't valid Unicode (an old drive's Latin-1 names, say) can't be a
//! JSON string exactly, so such a line also has `path_b64`, the path's bytes in base64url without
//! padding, and its `path` is then only a readable approximation. `sha256` is the content's
//! SHA-256 in hex, or null for a file that wasn't hashed: BackDown reads only what could be a
//! duplicate, so a file whose size nothing else shares is attested by its path and size alone. An
//! empty file's hash is the SHA-256 of nothing.
//!
//! ## The Merkle root (`backdown-merkle-v1`)
//!
//! - A file's *path bytes* are the UTF-8 bytes of `path`, or the bytes `path_b64` decodes to.
//!   (For a name that isn't valid Unicode they're the bytes the system stores: the name's bytes on
//!   Linux and macOS, its UTF-16LE on Windows.)
//! - Leaves are sorted by path bytes, compared as unsigned bytes; each path appears once.
//! - A leaf is `SHA-256(0x00 || len || path bytes || size || hash)`: `len` is the path's length in
//!   bytes as 4 bytes big-endian, `size` the file's size in bytes as 8 bytes big-endian, `hash`
//!   the 32 bytes of its SHA-256, or 32 zero bytes when it isn't hashed.
//! - An inner node is `SHA-256(0x01 || left || right)`. Each level pairs its nodes from the left;
//!   an odd node at the end of a level is carried up to the next level unchanged. The root is the
//!   one node left. A drive with no files has the SHA-256 of nothing as its root.
//!
//! ## The document
//!
//! ```text
//! { "v": 1, "kind": "backdown-manifest", "algorithm": "backdown-merkle-v1",
//!   "drive": { "path": "/media/old-drive", "root_id": 3 },
//!   "scan": { "id": 17, "finished_at": "2026-10-01T09:12:44Z" },
//!   "created": "2026-10-01T09:20:03Z",
//!   "counts": { "files": 12345, "bytes": 98765432, "hashed": 9876, "unhashed": 2469 },
//!   "merkle": "<hex>", "prev": "<hex>" or null, "key": "<hex>", "sig": "<base64url>" }
//! ```
//!
//! `drive.path` gets a `path_b64` beside it too when it isn't valid Unicode. `key` is the Ed25519
//! public key that signed it and `sig` the signature of the document's canonical form without
//! `sig` (signing.rs: keys sorted, no spaces). `prev` is the SHA-256, in hex, of the canonical
//! form of the same drive's previous manifest, whole (its `sig` included); null for a drive's
//! first.
//!
//! The index keeps every manifest (`manifests`), and for each which leaves had no hash when it
//! was signed, so its listing can be made again later from the index's history although hashes
//! have been found since (`mask`).

use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::clock::now;
use crate::db::{Lock, Tx};
use crate::fmt::{hex, unhex32};
use crate::signing::{self, Signer};
use crate::{Db, paths};

pub const KIND: &str = "backdown-manifest";
pub const ALGORITHM: &str = "backdown-merkle-v1";

/// The table (made by `Db::open`): every manifest signed here, by drive.
pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS manifests (
    id         INTEGER PRIMARY KEY,
    root_id    INTEGER NOT NULL,      -- roots.id (no foreign key: a signed statement outlives its drive's record)
    scan_id    INTEGER NOT NULL,
    created_at INTEGER NOT NULL,      -- unix seconds
    merkle     TEXT NOT NULL,         -- hex
    body       TEXT NOT NULL,         -- the signed document, JSON
    unhashed   BLOB                   -- which leaves had no hash when it was signed (manifest.rs `mask`)
);
CREATE INDEX IF NOT EXISTS manifests_root ON manifests(root_id, id);
";

/// SHA-256 of nothing: an empty drive's root, and an empty file's content hash.
pub fn empty_hash() -> [u8; 32] {
    Sha256::digest(b"").into()
}

// ---------- leaves and the tree

/// One leaf: `SHA-256(0x00 || u32be(len) || path || u64be(size) || hash or 32 zero bytes)`.
pub fn leaf(path: &[u8], size: u64, sha256: Option<&[u8; 32]>) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update([0u8]);
    h.update((path.len() as u32).to_be_bytes());
    h.update(path);
    h.update(size.to_be_bytes());
    h.update(sha256.unwrap_or(&[0u8; 32]));
    h.finalize().into()
}

/// An inner node: `SHA-256(0x01 || left || right)`.
pub fn node(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update([1u8]);
    h.update(left);
    h.update(right);
    h.finalize().into()
}

/// The Merkle root of leaves pushed in order, in memory proportional to the tree's height. Pairing
/// each level from the left and carrying an odd node up unchanged makes the tree a row of perfect
/// subtrees, largest first (one per bit of the count), joined from the right: the stack holds them.
#[derive(Default)]
pub struct Merkle {
    stack: Vec<(u32, [u8; 32])>,
    leaves: u64,
}

impl Merkle {
    pub fn push(&mut self, leaf: [u8; 32]) {
        self.leaves += 1;
        let mut cur = (0u32, leaf);
        while let Some(&(level, left)) = self.stack.last()
            && level == cur.0
        {
            self.stack.pop();
            cur = (level + 1, node(&left, &cur.1));
        }
        self.stack.push(cur);
    }

    pub fn leaves(&self) -> u64 {
        self.leaves
    }

    pub fn root(&self) -> [u8; 32] {
        let mut it = self.stack.iter().rev();
        match it.next() {
            None => empty_hash(),
            Some(&(_, last)) => it.fold(last, |acc, (_, left)| node(left, &acc)),
        }
    }
}

// ---------- entries: one file as the manifest sees it

/// One file of a manifest: its path bytes as the leaf hashes them, its size and its hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: Vec<u8>,
    /// a readable form, when the path bytes aren't UTF-8 (the listing's `path` then)
    pub lossy: Option<String>,
    pub size: u64,
    pub sha256: Option<[u8; 32]>,
}

impl Entry {
    pub fn leaf(&self) -> [u8; 32] {
        leaf(&self.path, self.size, self.sha256.as_ref())
    }

    /// The path for people.
    pub fn display(&self) -> String {
        match &self.lossy {
            Some(l) => l.clone(),
            None => String::from_utf8_lossy(&self.path).into_owned(),
        }
    }

    /// `path_b64`, for a path that isn't UTF-8.
    pub fn path_b64(&self) -> Option<String> {
        self.lossy
            .as_ref()
            .map(|_| URL_SAFE_NO_PAD.encode(&self.path))
    }

    /// Its line in a listing (without the newline): path, path_b64 if needed, size, sha256.
    pub fn line(&self) -> String {
        let text = |s: &str| serde_json::to_string(s).expect("a string");
        let mut out = format!("{{\"path\":{}", text(&self.display()));
        if let Some(b) = self.path_b64() {
            out += &format!(",\"path_b64\":{}", text(&b));
        }
        let sha = self
            .sha256
            .map_or_else(|| "null".to_string(), |h| format!("\"{}\"", hex(&h)));
        out += &format!(",\"size\":{},\"sha256\":{sha}}}", self.size);
        out
    }

    /// Read a listing's line.
    pub fn parse(line: &str) -> Result<Entry> {
        let v: Value = serde_json::from_str(line).context("not JSON")?;
        let o = v.as_object().context("not a JSON object")?;
        let text = o
            .get("path")
            .and_then(Value::as_str)
            .context("no \"path\"")?;
        let (path, lossy) = match o.get("path_b64") {
            None | Some(Value::Null) => (text.as_bytes().to_vec(), None),
            Some(b) => {
                let raw = b
                    .as_str()
                    .and_then(|s| URL_SAFE_NO_PAD.decode(s).ok())
                    .context("\"path_b64\" isn't base64url")?;
                (raw, Some(text.to_string()))
            }
        };
        let size = o
            .get("size")
            .and_then(Value::as_u64)
            .context("no \"size\" (a whole number of bytes)")?;
        let sha256 = match o.get("sha256") {
            None => bail!("no \"sha256\" (a hex hash, or null)"),
            Some(Value::Null) => None,
            Some(s) => Some(
                s.as_str()
                    .and_then(unhex32)
                    .context("\"sha256\" isn't 64 hex digits")?,
            ),
        };
        Ok(Entry {
            path,
            lossy,
            size,
            sha256,
        })
    }
}

/// A path below the drive, as a leaf hashes it: UTF-8 when it's valid Unicode, else the bytes
/// the system stores (and a readable form beside them).
#[cfg(not(windows))]
fn leaf_path(rel: &[u8]) -> (Vec<u8>, Option<String>) {
    match std::str::from_utf8(rel) {
        Ok(_) => (rel.to_vec(), None),
        Err(_) => (
            rel.to_vec(),
            Some(String::from_utf8_lossy(rel).into_owned()),
        ),
    }
}

#[cfg(windows)]
fn leaf_path(rel: &[u8]) -> (Vec<u8>, Option<String>) {
    let wide: Vec<u16> = rel
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    match String::from_utf16(&wide) {
        Ok(s) => (s.into_bytes(), None),
        Err(_) => (rel.to_vec(), Some(String::from_utf16_lossy(&wide))),
    }
}

/// The files of `root_id` at `scan`, in leaf order.
const AT_SCAN: &str = "SELECT path, size, sha256 FROM files
    WHERE root_id = ?1 AND first_scan <= ?2 AND last_scan >= ?2 ORDER BY path";

/// The files of a drive at one scan, as entries in leaf order. The index stores paths as the
/// system's bytes, which SQLite orders bytewise: on Linux and macOS those are the leaves' path
/// bytes, so they stream in order straight from the index. On Windows (UTF-16 in the index,
/// UTF-8 in the leaves) they're sorted here instead.
enum Files<'s> {
    Streamed {
        rows: rusqlite::Rows<'s>,
        prefix: Vec<u8>,
        last: Option<Vec<u8>>,
    },
    Sorted(std::vec::IntoIter<Entry>),
}

impl<'s> Files<'s> {
    fn open(
        stmt: &'s mut rusqlite::Statement,
        root_id: i64,
        scan: i64,
        prefix: Vec<u8>,
        sort: bool,
    ) -> Result<Files<'s>> {
        let rows = stmt.query(params![root_id, scan])?;
        let mut f = Files::Streamed {
            rows,
            prefix,
            last: None,
        };
        if !sort {
            return Ok(f);
        }
        let mut all = Vec::new();
        if let Files::Streamed { rows, prefix, .. } = &mut f {
            while let Some(r) = rows.next()? {
                all.push(entry(r, prefix)?);
            }
        }
        all.sort_by(|a, b| a.path.cmp(&b.path));
        if let Some(w) = all.windows(2).find(|w| w[0].path == w[1].path) {
            bail!(
                "two of this drive's files have the same path bytes ({}); a manifest needs each once",
                w[0].display()
            );
        }
        Ok(Files::Sorted(all.into_iter()))
    }

    fn next(&mut self) -> Result<Option<Entry>> {
        match self {
            Files::Sorted(it) => Ok(it.next()),
            Files::Streamed { rows, prefix, last } => {
                let Some(r) = rows.next()? else {
                    return Ok(None);
                };
                let e = entry(r, prefix)?;
                if last.as_ref().is_some_and(|l| *l >= e.path) {
                    bail!(
                        "the index lists this drive's files out of order (at {}); a manifest needs them sorted",
                        e.display()
                    );
                }
                *last = Some(e.path.clone());
                Ok(Some(e))
            }
        }
    }
}

fn entry(r: &rusqlite::Row, prefix: &[u8]) -> Result<Entry> {
    let path: Vec<u8> = r.get(0)?;
    let size = r.get::<_, i64>(1)? as u64;
    let sha: Option<Vec<u8>> = r.get(2)?;
    let rel = path.strip_prefix(prefix).unwrap_or(&path);
    let (path, lossy) = leaf_path(rel);
    let sha256 = if size == 0 {
        Some(empty_hash()) // nothing to read: its hash is known
    } else {
        sha.and_then(|h| <[u8; 32]>::try_from(h.as_slice()).ok())
    };
    Ok(Entry {
        path,
        lossy,
        size,
        sha256,
    })
}

// ---------- the unhashed mask

/// Which leaves had no hash when a manifest was signed: run lengths, alternately hashed and
/// unhashed (hashed first, possibly 0), each LEB128. Hashes are only ever added to the index, so
/// with this the listing of an old manifest can be made again exactly from the index's history.
#[derive(Default)]
struct MaskWriter {
    out: Vec<u8>,
    unhashed: bool,
    run: u64,
}

impl MaskWriter {
    fn push(&mut self, unhashed: bool) {
        if unhashed != self.unhashed {
            varint(&mut self.out, self.run);
            self.unhashed = unhashed;
            self.run = 0;
        }
        self.run += 1;
    }

    fn finish(mut self) -> Vec<u8> {
        varint(&mut self.out, self.run);
        self.out
    }
}

struct MaskReader<'a> {
    data: &'a [u8],
    unhashed: bool,
    left: u64,
}

impl<'a> MaskReader<'a> {
    fn new(data: &'a [u8]) -> MaskReader<'a> {
        MaskReader {
            data,
            unhashed: true, // flipped to "hashed" by the first run
            left: 0,
        }
    }

    /// Whether the next leaf was unhashed; None past the end.
    fn next(&mut self) -> Option<bool> {
        while self.left == 0 {
            self.left = read_varint(&mut self.data)?;
            self.unhashed = !self.unhashed;
        }
        self.left -= 1;
        Some(self.unhashed)
    }
}

fn varint(out: &mut Vec<u8>, mut n: u64) {
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn read_varint(data: &mut &[u8]) -> Option<u64> {
    let mut n = 0u64;
    for shift in (0..64).step_by(7) {
        let (&b, rest) = data.split_first()?;
        *data = rest;
        n |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Some(n);
        }
    }
    None
}

// ---------- making one

/// A manifest just signed and kept.
#[derive(Debug, Clone)]
pub struct Made {
    pub id: i64,
    pub doc: Value,
    pub files: u64,
    pub bytes: u64,
    pub hashed: u64,
    pub scan: i64,
    /// the stored manifest it follows
    pub prev_id: Option<i64>,
}

struct Root {
    path: std::path::PathBuf,
    last_scan: Option<i64>,
}

fn root(db: &Db, root_id: i64) -> Result<Root> {
    let (path, last_scan): (Vec<u8>, Option<i64>) = db
        .conn
        .query_row(
            "SELECT path, last_scan FROM roots WHERE id = ?1",
            [root_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .with_context(|| format!("there's no drive {root_id} in the index"))?;
    Ok(Root {
        path: paths::from_bytes(&path),
        last_scan,
    })
}

/// A path in a document: the exact string, and its bytes in base64url when it isn't valid Unicode.
fn path_json(p: &Path) -> Value {
    match p.to_str() {
        Some(s) => json!({ "path": s }),
        None => json!({
            "path": p.to_string_lossy(),
            "path_b64": URL_SAFE_NO_PAD.encode(paths::to_bytes(p)),
        }),
    }
}

/// Sign a manifest of drive `root_id` at `scan` (default: its latest finished scan), chained to
/// the drive's previous one, and keep it. With `listing`, the listing is written there as it's
/// made. `progress` hears the files done so far now and then.
pub fn create(
    db: &Db,
    signer: &Signer,
    root_id: i64,
    scan: Option<i64>,
    mut listing: Option<&mut dyn Write>,
    progress: &mut dyn FnMut(u64),
) -> Result<Made> {
    let r = root(db, root_id)?;
    let show = paths::show(&r.path);
    let scan = match scan {
        Some(s) => s,
        None => r
            .last_scan
            .with_context(|| format!("{show} has no finished scan yet: index it first"))?,
    };
    let finished_at: i64 = db
        .conn
        .query_row(
            "SELECT finished_at FROM scans WHERE id = ?1 AND root_id = ?2",
            params![scan, root_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten()
        .with_context(|| format!("scan {scan} isn't a finished scan of {show}"))?;

    let mut tree = Merkle::default();
    let mut mask = MaskWriter::default();
    let (mut bytes, mut hashed) = (0u64, 0u64);
    {
        let mut stmt = db.conn.prepare(AT_SCAN)?;
        let mut files = Files::open(
            &mut stmt,
            root_id,
            scan,
            paths::dir_prefix(&r.path),
            cfg!(windows),
        )?;
        while let Some(e) = files.next()? {
            tree.push(e.leaf());
            mask.push(e.sha256.is_none());
            bytes += e.size;
            hashed += u64::from(e.sha256.is_some());
            if let Some(w) = listing.as_mut() {
                writeln!(w, "{}", e.line()).context("writing the listing")?;
            }
            if tree.leaves() % 20_000 == 0 {
                progress(tree.leaves());
            }
        }
    }
    let files = tree.leaves();
    progress(files);
    if let Some(w) = listing.as_mut() {
        w.flush().context("writing the listing")?;
    }
    let merkle = hex(&tree.root());
    let mask = mask.finish();

    // read the drive's newest manifest and add this one after it in one transaction, so that two
    // signers at once (the service and a command) can't both follow the same one
    let tx = Tx::begin(&db.conn, Lock::Immediate)?;
    let made = (|| -> Result<Made> {
        let mut drive = path_json(&r.path);
        drive["root_id"] = json!(root_id);
        let last: Option<(i64, String)> = db
            .conn
            .query_row(
                "SELECT id, body FROM manifests WHERE root_id = ?1 ORDER BY id DESC LIMIT 1",
                [root_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        // a root id reused for another drive (one forgotten and indexed anew) starts a new chain
        let prev = last
            .and_then(|(id, body)| serde_json::from_str::<Value>(&body).ok().map(|b| (id, b)))
            .filter(|(_, b)| b["drive"]["path"] == drive["path"]);
        let t = now();
        let mut doc = json!({
            "v": 1,
            "kind": KIND,
            "algorithm": ALGORITHM,
            "drive": drive,
            "scan": { "id": scan, "finished_at": rfc3339(finished_at) },
            "created": rfc3339(t),
            "counts": { "files": files, "bytes": bytes, "hashed": hashed, "unhashed": files - hashed },
            "merkle": merkle,
            "prev": prev.as_ref().map(|(_, b)| chain_hash(b)),
        });
        signer.sign_json(&mut doc)?;
        db.conn.execute(
            "INSERT INTO manifests (root_id, scan_id, created_at, merkle, body, unhashed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![root_id, scan, t, merkle, doc.to_string(), mask],
        )?;
        Ok(Made {
            id: db.conn.last_insert_rowid(),
            doc,
            files,
            bytes,
            hashed,
            scan,
            prev_id: prev.map(|(id, _)| id),
        })
    })()?;
    tx.commit()?;
    Ok(made)
}

// ---------- the stored ones

/// A manifest kept in the index.
#[derive(Debug, Clone, Serialize)]
pub struct Stored {
    pub id: i64,
    pub root_id: i64,
    pub scan_id: i64,
    pub created_at: i64,
    pub merkle: String,
    pub drive: String,
    pub files: u64,
    pub bytes: u64,
    pub hashed: u64,
    pub fingerprint: String,
    #[serde(skip)]
    pub doc: Value,
}

fn has_table(db: &Db) -> Result<bool> {
    Ok(db.conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'manifests')",
        [],
        |r| r.get(0),
    )?)
}

fn stored(
    id: i64,
    root_id: i64,
    scan_id: i64,
    created_at: i64,
    merkle: String,
    body: &str,
) -> Stored {
    let doc: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let n = |k: &str| doc["counts"][k].as_u64().unwrap_or(0);
    Stored {
        id,
        root_id,
        scan_id,
        created_at,
        merkle,
        drive: doc["drive"]["path"].as_str().unwrap_or("").to_string(),
        files: n("files"),
        bytes: n("bytes"),
        hashed: n("hashed"),
        fingerprint: doc["key"]
            .as_str()
            .map(signing::fingerprint)
            .unwrap_or_default(),
        doc,
    }
}

/// The manifests kept here, oldest first: one drive's, or every drive's.
pub fn list(db: &Db, root_id: Option<i64>) -> Result<Vec<Stored>> {
    if !has_table(db)? {
        return Ok(Vec::new());
    }
    let mut stmt = db.conn.prepare(
        "SELECT id, root_id, scan_id, created_at, merkle, body FROM manifests
         WHERE ?1 IS NULL OR root_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([root_id], |r| {
        Ok(stored(
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            &r.get::<_, String>(5)?,
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One kept manifest.
pub fn get(db: &Db, id: i64) -> Result<Stored> {
    let none = || format!("there's no manifest {id} in this index");
    if !has_table(db)? {
        bail!(none());
    }
    db.conn
        .query_row(
            "SELECT id, root_id, scan_id, created_at, merkle, body FROM manifests WHERE id = ?1",
            [id],
            |r| {
                Ok(stored(
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    &r.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?
        .with_context(none)
}

/// The kept manifest that is this document, if it's kept here.
pub fn find(db: &Db, doc: &Value) -> Result<Option<Stored>> {
    let (Some(root_id), Some(merkle)) = (doc["drive"]["root_id"].as_i64(), doc["merkle"].as_str())
    else {
        return Ok(None);
    };
    let h = chain_hash(doc);
    Ok(list(db, Some(root_id))?
        .into_iter()
        .find(|s| s.merkle == merkle && chain_hash(&s.doc) == h))
}

/// Make a kept manifest's listing again from the index's record of its scan, with the hashes it
/// had then; `each` hears every entry in leaf order. The Merkle root that gives (the signed one,
/// unless the index's history changed since).
pub fn replay(db: &Db, s: &Stored, each: &mut dyn FnMut(&Entry) -> Result<()>) -> Result<[u8; 32]> {
    let mask: Option<Vec<u8>> = db.conn.query_row(
        "SELECT unhashed FROM manifests WHERE id = ?1",
        [s.id],
        |r| r.get(0),
    )?;
    let mut mask = mask.as_deref().map(MaskReader::new);
    let r = root(db, s.root_id)?;
    let mut tree = Merkle::default();
    let mut stmt = db.conn.prepare(AT_SCAN)?;
    let mut files = Files::open(
        &mut stmt,
        s.root_id,
        s.scan_id,
        paths::dir_prefix(&r.path),
        cfg!(windows),
    )?;
    while let Some(mut e) = files.next()? {
        if mask.as_mut().and_then(MaskReader::next) == Some(true) {
            e.sha256 = None; // hashed since it was signed
        }
        tree.push(e.leaf());
        each(&e)?;
    }
    Ok(tree.root())
}

// ---------- checking one

/// What a document says, once its signature and shape check out.
#[derive(Debug, Clone, Serialize)]
pub struct Checked {
    pub key: String,
    pub fingerprint: String,
    pub drive: String,
    pub root_id: i64,
    pub scan_id: i64,
    pub scan_finished: String,
    pub created: String,
    pub files: u64,
    pub bytes: u64,
    pub hashed: u64,
    pub unhashed: u64,
    pub merkle: String,
    pub prev: Option<String>,
}

/// Check a manifest's signature and shape (not yet its listing or its chain).
pub fn check(doc: &Value) -> Result<Checked> {
    if doc["kind"] != KIND {
        bail!(
            "not a BackDown manifest (its kind is {})",
            doc.get("kind").map_or("missing".into(), Value::to_string)
        );
    }
    if doc["v"] != 1 {
        bail!(
            "a version {} manifest: this BackDown reads version 1",
            doc["v"]
        );
    }
    if doc["algorithm"] != ALGORITHM {
        bail!(
            "made with {}, which this BackDown doesn't know (it knows {ALGORITHM})",
            doc["algorithm"]
        );
    }
    let key = signing::verify_json(doc)?;
    let merkle = doc["merkle"]
        .as_str()
        .filter(|m| unhex32(m).is_some())
        .context("its Merkle root isn't 64 hex digits")?
        .to_string();
    let prev = match &doc["prev"] {
        Value::Null => None,
        Value::String(p) if unhex32(p).is_some() => Some(p.clone()),
        _ => bail!("its \"prev\" isn't 64 hex digits or null"),
    };
    let n = |k: &str| -> Result<u64> {
        doc["counts"][k]
            .as_u64()
            .with_context(|| format!("no count of {k}"))
    };
    let (files, hashed, unhashed) = (n("files")?, n("hashed")?, n("unhashed")?);
    if hashed + unhashed != files {
        bail!("its counts don't add up: {hashed} hashed and {unhashed} not, of {files} files");
    }
    Ok(Checked {
        fingerprint: signing::fingerprint(&key),
        key,
        drive: doc["drive"]["path"].as_str().unwrap_or("").to_string(),
        root_id: doc["drive"]["root_id"].as_i64().unwrap_or(0),
        scan_id: doc["scan"]["id"].as_i64().context("no scan id")?,
        scan_finished: doc["scan"]["finished_at"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        created: doc["created"].as_str().unwrap_or("").to_string(),
        files,
        bytes: n("bytes")?,
        hashed,
        unhashed,
        merkle,
        prev,
    })
}

/// How a listing compares with its manifest.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ListingCheck {
    pub ok: bool,
    pub files: u64,
    pub bytes: u64,
    pub hashed: u64,
    pub merkle: String,
    pub problems: Vec<String>,
}

/// A listing's lines in order, checked as they're read: each one parses, and comes after the one
/// before it.
struct Lines<'r> {
    reader: &'r mut dyn BufRead,
    line: String,
    n: u64,
    last: Option<Vec<u8>>,
}

impl<'r> Lines<'r> {
    fn new(reader: &'r mut dyn BufRead) -> Lines<'r> {
        Lines {
            reader,
            line: String::new(),
            n: 0,
            last: None,
        }
    }

    /// The next entry, or why the listing isn't valid (an outer error is a read that failed).
    fn next(&mut self) -> Result<Result<Option<Entry>, String>> {
        loop {
            self.line.clear();
            if self.reader.read_line(&mut self.line)? == 0 {
                return Ok(Ok(None));
            }
            self.n += 1;
            let t = self.line.trim();
            if t.is_empty() {
                continue;
            }
            let e = match Entry::parse(t) {
                Ok(e) => e,
                Err(e) => return Ok(Err(format!("line {}: {e:#}", self.n))),
            };
            if self.last.as_ref().is_some_and(|l| *l >= e.path) {
                return Ok(Err(format!(
                    "line {} ({}) is out of order or repeats a path: a listing is sorted by path bytes",
                    self.n,
                    e.display()
                )));
            }
            self.last = Some(e.path.clone());
            return Ok(Ok(Some(e)));
        }
    }
}

/// Check a listing against its (checked) manifest: it must give the signed Merkle root, with the
/// same counts.
pub fn verify_listing(c: &Checked, reader: &mut dyn BufRead) -> Result<ListingCheck> {
    let mut lines = Lines::new(reader);
    let mut tree = Merkle::default();
    let mut out = ListingCheck::default();
    loop {
        match lines.next()? {
            Ok(Some(e)) => {
                tree.push(e.leaf());
                out.bytes += e.size;
                out.hashed += u64::from(e.sha256.is_some());
            }
            Ok(None) => break,
            Err(why) => {
                out.problems.push(why);
                return Ok(out);
            }
        }
    }
    out.files = tree.leaves();
    out.merkle = hex(&tree.root());
    if out.files != c.files {
        out.problems.push(format!(
            "it lists {} files; the manifest says {}",
            out.files, c.files
        ));
    }
    if out.bytes != c.bytes {
        out.problems.push(format!(
            "its files add up to {} bytes; the manifest says {}",
            out.bytes, c.bytes
        ));
    }
    if out.hashed != c.hashed {
        out.problems.push(format!(
            "{} of its files have a hash; the manifest says {}",
            out.hashed, c.hashed
        ));
    }
    if out.merkle != c.merkle {
        out.problems.push(format!(
            "it gives the Merkle root {}, not the signed {}: a path, size or hash in it isn't what was signed",
            out.merkle, c.merkle
        ));
    }
    out.ok = out.problems.is_empty();
    Ok(out)
}

// ---------- the chain

/// What the next manifest of the same drive names as its `prev`: the SHA-256 of this one's
/// canonical form, whole.
pub fn chain_hash(doc: &Value) -> String {
    signing::sha256_hex(&signing::canonical(doc))
}

/// Something wrong with a chain: which manifest (its place in the list given), and what.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChainProblem {
    pub at: usize,
    pub what: String,
}

/// Check manifests that should form one drive's chain, oldest first: each names the one before
/// it as its `prev`, and wasn't signed before it. A manifest left out shows as a gap, two swapped
/// as being out of order.
pub fn check_chain(docs: &[Value]) -> Vec<ChainProblem> {
    let hashes: Vec<String> = docs.iter().map(chain_hash).collect();
    let mut out = Vec::new();
    for i in 1..docs.len() {
        let prev = docs[i]["prev"].as_str();
        // one listed before it follows it
        let ahead = (0..i).find(|&j| docs[j]["prev"].as_str() == Some(hashes[i].as_str()));
        let what = match prev {
            Some(p) if p == hashes[i - 1] => {
                if docs[i]["created"].as_str() < docs[i - 1]["created"].as_str() {
                    Some("it was signed before the one it follows".to_string())
                } else {
                    None
                }
            }
            _ if ahead.is_some() => Some(format!(
                "out of order: #{}, listed before it, follows it",
                ahead.expect("checked") + 1
            )),
            None => Some(format!(
                "it starts a chain of its own: nothing links it to #{i} before it"
            )),
            Some(p) => match hashes.iter().position(|h| h == p) {
                Some(j) if j > i => Some(format!(
                    "out of order: it follows #{}, which comes after it",
                    j + 1
                )),
                Some(j) => Some(format!(
                    "out of order: it follows #{}, not #{} just before it",
                    j + 1,
                    i
                )),
                None => Some(format!(
                    "a gap: the manifest it follows isn't here (between #{} and it)",
                    i
                )),
            },
        };
        if let Some(what) = what {
            out.push(ChainProblem { at: i, what });
        }
    }
    out
}

/// Put manifests of one drive in their chain's order: each after the one it names as `prev`,
/// pieces that don't link to each other by when they were signed. For files given in any order.
pub fn chain_order(docs: &[Value]) -> Vec<usize> {
    let hashes: Vec<String> = docs.iter().map(chain_hash).collect();
    let created = |i: usize| docs[i]["created"].as_str().unwrap_or("").to_string();
    let mut followers: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, d) in docs.iter().enumerate() {
        if let Some(p) = d["prev"].as_str() {
            followers.entry(p).or_default().push(i);
        }
    }
    for f in followers.values_mut() {
        f.sort_by_key(|&i| created(i));
    }
    let mut starts: Vec<usize> = (0..docs.len())
        .filter(|&i| {
            docs[i]["prev"]
                .as_str()
                .is_none_or(|p| !hashes.contains(&p.to_string()))
        })
        .collect();
    starts.sort_by_key(|&i| created(i));
    let mut seen = vec![false; docs.len()];
    let mut out = Vec::new();
    for s in starts {
        let mut cur = Some(s);
        while let Some(i) = cur.filter(|&i| !seen[i]) {
            seen[i] = true;
            out.push(i);
            cur = followers
                .get(hashes[i].as_str())
                .and_then(|f| f.iter().copied().find(|&j| !seen[j]));
        }
    }
    // a second manifest following the same one (a fork) goes last, where check_chain names it
    let mut rest: Vec<usize> = (0..docs.len()).filter(|&i| !seen[i]).collect();
    rest.sort_by_key(|&i| created(i));
    out.extend(rest);
    out
}

// ---------- comparing with the index now

/// One side of a difference: a file's size and hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Side {
    pub size: u64,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diff {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_b64: Option<String>,
    /// in the manifest
    pub was: Option<Side>,
    /// in the index now
    pub now: Option<Side>,
}

/// Files of one kind of difference: how many, their bytes (as they are now, or as they were if
/// gone), and the first few.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Tally {
    pub files: u64,
    pub bytes: u64,
    pub examples: Vec<Diff>,
}

impl Tally {
    fn add(&mut self, e: &Entry, was: Option<&Entry>, now: Option<&Entry>, limit: usize) {
        self.files += 1;
        self.bytes += e.size;
        if self.examples.len() < limit {
            let side = |x: &Entry| Side {
                size: x.size,
                sha256: x.sha256.map(|h| hex(&h)),
            };
            self.examples.push(Diff {
                path: e.display(),
                path_b64: e.path_b64(),
                was: was.map(side),
                now: now.map(side),
            });
        }
    }
}

/// A manifest beside what the index holds for its drive now.
#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    pub root_id: i64,
    /// the drive's latest finished scan, and when it finished
    pub now_scan: i64,
    pub now_finished_at: i64,
    /// where the manifest's side came from: "listing" (given) or "index" (its record of the scan)
    pub from: &'static str,
    /// that side gave the signed Merkle root
    pub reproduced: bool,
    /// it came from the index with the hashes the manifest had (a manifest kept here), so a
    /// root that isn't reproduced means the index's history changed
    pub exact: bool,
    pub added: Tally,
    pub removed: Tally,
    /// a different size, or a different hash where both sides have one
    pub changed: Tally,
    pub unchanged: u64,
    /// of the unchanged: the same size, but one side has no hash to compare the contents by
    pub unverified: u64,
}

/// The root holding a manifest's drive in this index: the one with its path (root ids differ
/// from one index to another; a drive's folder is what says it's the same).
fn root_for(db: &Db, c: &Checked) -> Result<i64> {
    let mut stmt = db.conn.prepare("SELECT id, path FROM roots")?;
    let rows: Vec<(i64, Vec<u8>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    rows.into_iter()
        .find(|(_, p)| paths::from_bytes(p).to_string_lossy() == c.drive)
        .map(|(id, _)| id)
        .with_context(|| format!("{} isn't a drive in this index", c.drive))
}

/// Compare a (checked) manifest with what the index holds for its drive at its latest scan: what
/// was added, removed and changed since. The manifest's side is its listing when given, else the
/// index's record of the manifest's scan (when this index has that scan).
pub fn compare(
    db: &Db,
    doc: &Value,
    c: &Checked,
    listing: Option<&mut dyn BufRead>,
    limit: usize,
) -> Result<Comparison> {
    let root_id = root_for(db, c)?;
    let r = root(db, root_id)?;
    let show = paths::show(&r.path);
    let now_scan = r
        .last_scan
        .with_context(|| format!("{show} has no finished scan in this index"))?;
    let now_finished_at: i64 = db.conn.query_row(
        "SELECT COALESCE(finished_at, 0) FROM scans WHERE id = ?1",
        [now_scan],
        |row| row.get(0),
    )?;
    let prefix = paths::dir_prefix(&r.path);
    let mut now_stmt = db.conn.prepare(AT_SCAN)?;
    let mut now = Files::open(
        &mut now_stmt,
        root_id,
        now_scan,
        prefix.clone(),
        cfg!(windows),
    )?;

    // the manifest's side
    let mut then_stmt = db.conn.prepare(AT_SCAN)?;
    let kept = find(db, doc)?;
    let mut mask_data: Option<Vec<u8>> = None;
    enum Then<'a, 'r> {
        Listing(Lines<'r>),
        Index(Files<'a>),
    }
    let (mut then, from) = match listing {
        Some(reader) => (Then::Listing(Lines::new(reader)), "listing"),
        None => {
            let ours: bool = db
                .conn
                .query_row(
                    "SELECT 1 FROM scans WHERE id = ?1 AND root_id = ?2 AND finished_at IS NOT NULL",
                    params![c.scan_id, root_id],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);
            if !ours {
                bail!(
                    "this index has no record of scan {} of {show}: give the manifest's listing (--list) to compare it",
                    c.scan_id
                );
            }
            if let Some(k) = &kept {
                mask_data = db.conn.query_row(
                    "SELECT unhashed FROM manifests WHERE id = ?1",
                    [k.id],
                    |row| row.get(0),
                )?;
            }
            (
                Then::Index(Files::open(
                    &mut then_stmt,
                    root_id,
                    c.scan_id,
                    prefix,
                    cfg!(windows),
                )?),
                "index",
            )
        }
    };
    let mut mask = mask_data.as_deref().map(MaskReader::new);
    let mut tree = Merkle::default();
    let mut next_then = |then: &mut Then| -> Result<Option<Entry>> {
        let e = match then {
            Then::Listing(l) => l
                .next()?
                .map_err(|why| anyhow::anyhow!("the listing: {why}"))?,
            Then::Index(f) => f.next()?.map(|mut e| {
                if mask.as_mut().and_then(MaskReader::next) == Some(true) {
                    e.sha256 = None;
                }
                e
            }),
        };
        if let Some(e) = &e {
            tree.push(e.leaf());
        }
        Ok(e)
    };

    let mut out = Comparison {
        root_id,
        now_scan,
        now_finished_at,
        from,
        reproduced: false,
        exact: from == "index" && kept.is_some(),
        added: Tally::default(),
        removed: Tally::default(),
        changed: Tally::default(),
        unchanged: 0,
        unverified: 0,
    };
    let mut a = next_then(&mut then)?;
    let mut b = now.next()?;
    loop {
        let ord = match (&a, &b) {
            (None, None) => break,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some(x), Some(y)) => x.path.cmp(&y.path),
        };
        match ord {
            Ordering::Less => {
                let x = a.as_ref().expect("there");
                out.removed.add(x, Some(x), None, limit);
                a = next_then(&mut then)?;
            }
            Ordering::Greater => {
                let y = b.as_ref().expect("there");
                out.added.add(y, None, Some(y), limit);
                b = now.next()?;
            }
            Ordering::Equal => {
                let (x, y) = (a.as_ref().expect("there"), b.as_ref().expect("there"));
                let differs = x.size != y.size
                    || matches!((x.sha256, y.sha256), (Some(p), Some(q)) if p != q);
                if differs {
                    out.changed.add(y, Some(x), Some(y), limit);
                } else {
                    out.unchanged += 1;
                    if x.sha256.is_none() || y.sha256.is_none() {
                        out.unverified += 1;
                    }
                }
                a = next_then(&mut then)?;
                b = now.next()?;
            }
        }
    }
    out.reproduced = hex(&tree.root()) == c.merkle && tree.leaves() == c.files;
    Ok(out)
}

// ---------- helpers

/// Unix seconds as RFC 3339 in UTC: 2026-10-01T09:12:44Z.
pub fn rfc3339(t: i64) -> String {
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    // civil from days (Howard Hinnant), as fmt::day
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(b: &[u8]) -> [u8; 32] {
        Sha256::digest(b).into()
    }

    /// The tree as the module doc describes it, level by level.
    fn reference_root(leaves: &[[u8; 32]]) -> [u8; 32] {
        if leaves.is_empty() {
            return sha(b"");
        }
        let mut level = leaves.to_vec();
        while level.len() > 1 {
            let mut up = Vec::new();
            for pair in level.chunks(2) {
                up.push(match pair {
                    [l, r] => node(l, r),
                    [odd] => *odd,
                    _ => unreachable!(),
                });
            }
            level = up;
        }
        level[0]
    }

    #[test]
    fn the_streamed_tree_is_the_level_by_level_tree() {
        let leaves: Vec<[u8; 32]> = (0u32..70).map(|i| sha(&i.to_be_bytes())).collect();
        for n in 0..leaves.len() {
            let mut m = Merkle::default();
            for l in &leaves[..n] {
                m.push(*l);
            }
            assert_eq!(m.root(), reference_root(&leaves[..n]), "{n} leaves");
        }
    }

    #[test]
    fn a_small_tree_by_hand() {
        // three files: a.txt (hashed), b/c (empty), d (not hashed)
        let leaf_bytes = |path: &[u8], size: u64, h: [u8; 32]| {
            let mut v = vec![0u8];
            v.extend((path.len() as u32).to_be_bytes());
            v.extend(path);
            v.extend(size.to_be_bytes());
            v.extend(h);
            sha(&v)
        };
        let la = leaf_bytes(b"a.txt", 3, sha(b"abc"));
        let lb = leaf_bytes(b"b/c", 0, sha(b""));
        let ld = leaf_bytes(b"d", 5, [0; 32]);
        assert_eq!(leaf(b"a.txt", 3, Some(&sha(b"abc"))), la);
        assert_eq!(leaf(b"d", 5, None), ld);
        let inner = |l: [u8; 32], r: [u8; 32]| {
            let mut v = vec![1u8];
            v.extend(l);
            v.extend(r);
            sha(&v)
        };
        // a and b pair; d, odd at the end of its level, is carried up and joins them
        let want = inner(inner(la, lb), ld);
        let mut m = Merkle::default();
        for l in [la, lb, ld] {
            m.push(l);
        }
        assert_eq!(m.root(), want);
        // the same, worked out independently of this code (with the Python in
        // marketing/docs/guides/manifests-and-attestations.md)
        assert_eq!(
            hex(&want),
            "6dc17965ade625c342c4ac85bf48b5a5a844e335ad00f3f452dd6a7b8ba8d675"
        );
        assert_eq!(hex(&Merkle::default().root()), hex(&sha(b"")));
        // deterministic
        let mut again = Merkle::default();
        for l in [la, lb, ld] {
            again.push(l);
        }
        assert_eq!(again.root(), m.root());
    }

    #[test]
    fn sorting_the_leaves_here_gives_the_order_they_stream_in() {
        // Windows sorts the leaves itself; elsewhere they come in the index's own order
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("drive");
        for name in [
            "b.txt", "a/z.txt", "a b/c", "A.txt", "ä.txt", "a.txt", "a/b/c/d",
        ] {
            let p = d.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, name).unwrap();
        }
        let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
        let opts = crate::index::Options {
            one_file_system: true,
            allow_empty: false,
        };
        let scan = crate::index::index(&mut db, &d, &opts, &mut |_| {})
            .unwrap()
            .scan;
        let (root_id, root): (i64, Vec<u8>) = db
            .conn
            .query_row("SELECT id, path FROM roots", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        let prefix = paths::dir_prefix(&paths::from_bytes(&root));
        let read = |sort: bool| {
            let mut stmt = db.conn.prepare(AT_SCAN).unwrap();
            let mut f = Files::open(&mut stmt, root_id, scan, prefix.clone(), sort).unwrap();
            let mut v = Vec::new();
            while let Some(e) = f.next().unwrap() {
                v.push(e);
            }
            v
        };
        let streamed = read(false);
        assert_eq!(streamed.len(), 7);
        assert_eq!(read(true), streamed);
        let names: Vec<String> = streamed.iter().map(Entry::display).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "by their bytes: {names:?}");
        assert_eq!(names[0], "A.txt");
    }

    #[test]
    fn masks_round_trip() {
        for pattern in [
            vec![],
            vec![true],
            vec![false],
            vec![true, true, false, true, false, false, false],
            (0..1000).map(|i| i % 7 == 0 || i > 900).collect(),
        ] {
            let mut w = MaskWriter::default();
            for &u in &pattern {
                w.push(u);
            }
            let data = w.finish();
            let mut r = MaskReader::new(&data);
            let back: Vec<bool> = std::iter::from_fn(|| r.next()).collect();
            assert_eq!(back, pattern);
        }
        let mut w = MaskWriter::default();
        for _ in 0..100_000 {
            w.push(true);
        }
        assert!(w.finish().len() <= 4, "a long run is a few bytes");
    }

    #[test]
    fn lines_round_trip() {
        let e = Entry {
            path: b"photos/beach.jpg".to_vec(),
            lossy: None,
            size: 2048,
            sha256: Some(sha(b"x")),
        };
        assert_eq!(Entry::parse(&e.line()).unwrap(), e);
        assert!(e.line().starts_with(r#"{"path":"photos/beach.jpg","#));
        let odd = Entry {
            path: b"caf\xe9.txt".to_vec(),
            lossy: Some("caf\u{fffd}.txt".into()),
            size: 1,
            sha256: None,
        };
        let line = odd.line();
        assert!(line.contains(r#""path_b64":"Y2Fm6S50eHQ""#), "{line}");
        assert!(line.contains(r#""sha256":null"#), "{line}");
        assert_eq!(Entry::parse(&line).unwrap(), odd);
        for bad in [
            r#"{"path":"a","size":1}"#,
            r#"{"path":"a","sha256":null}"#,
            r#"{"size":1,"sha256":null}"#,
            r#"{"path":"a","size":1,"sha256":"abc"}"#,
            r#"{"path":"a","path_b64":"%%","size":1,"sha256":null}"#,
            "not json",
        ] {
            assert!(Entry::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn dates() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_790_000_000), "2026-09-21T14:13:20Z");
    }

    #[test]
    fn chains_show_gaps_and_reordering() {
        let dir = tempfile::tempdir().unwrap();
        let s = Signer::load_or_create(&dir.path().join("k")).unwrap();
        let mut docs: Vec<Value> = Vec::new();
        for i in 0..4 {
            let mut d = json!({
                "kind": KIND, "n": i, "created": format!("2026-10-0{}T00:00:00Z", i + 1),
                "prev": docs.last().map(chain_hash),
            });
            s.sign_json(&mut d).unwrap();
            docs.push(d);
        }
        assert!(check_chain(&docs).is_empty());
        assert_eq!(chain_order(&docs), vec![0, 1, 2, 3]);
        // one left out
        let gap = [docs[0].clone(), docs[1].clone(), docs[3].clone()];
        let p = check_chain(&gap);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].what.contains("a gap"), "{p:?}");
        assert_eq!(p[0].at, 2);
        // two swapped
        let swapped = [
            docs[0].clone(),
            docs[2].clone(),
            docs[1].clone(),
            docs[3].clone(),
        ];
        let p = check_chain(&swapped);
        assert!(
            !p.is_empty() && p.iter().all(|x| x.what.contains("out of order")),
            "{p:?}"
        );
        // given in any order, they're put back in the chain's; with one missing, that shows
        let shuffled = [
            docs[3].clone(),
            docs[0].clone(),
            docs[2].clone(),
            docs[1].clone(),
        ];
        let order = chain_order(&shuffled);
        let ordered: Vec<Value> = order.iter().map(|&i| shuffled[i].clone()).collect();
        assert!(check_chain(&ordered).is_empty());
        let holey = [docs[3].clone(), docs[0].clone(), docs[1].clone()];
        let ordered: Vec<Value> = chain_order(&holey)
            .iter()
            .map(|&i| holey[i].clone())
            .collect();
        let p = check_chain(&ordered);
        assert!(p.len() == 1 && p[0].what.contains("a gap"), "{p:?}");
    }
}
