//! SQL dumps as schemas, for the browser's SQL viewer: the tables a dump creates (columns, types,
//! keys, indexes, foreign keys), how many rows and how much data it holds for each, its views,
//! triggers and routines, and what its header says (tool, versions, host, database, date, charset).
//! It reads mysqldump and MariaDB dumps, phpMyAdmin/Adminer/HeidiSQL exports, pg_dump and
//! pg_dumpall plain output (COPY blocks included), SQLite `.dump`, and plain CREATE/INSERT scripts,
//! as they are or compressed (gzip, bzip2, xz, zstd: the archive viewer's decoders).
//!
//! Like peek.rs, this reads the file an index id resolved to and nothing else, and writes and runs
//! nothing. The dump streams through a lexer a buffer at a time (quotes and escapes in each
//! dialect, comments, MySQL's executable comments and DELIMITER, dollar quoting, COPY data), so
//! memory stays bounded whatever its size: only the text of statements that define things is kept
//! (capped), and rows are counted as they go by. A read stops on a time budget; the parser is then
//! parked in memory under a random token, and the next request carries on exactly where it stopped
//! (a parked read is dropped after a couple of idle minutes, and reading then starts over).

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use backdown_core::magic::read_at;
use serde_json::{Value, json};

use crate::peek::Codec;

mod answer;
mod statements;
mod tokens;

use tokens::*;

/// Bytes decoded per read, and how far the lexer may look past where it is.
const BUF: usize = 1 << 20;
const LOOK: usize = 64;
/// Text kept of one statement: all of a CREATE TABLE, an INSERT up to its VALUES.
const HEAD_CAP: usize = 1 << 20;
/// A table's CREATE statement is sent with it, up to this much, and this much for all of them.
const SQL_CAP: usize = 16 << 10;
const SQL_TOTAL: usize = 8 << 20;
/// The header comments (tool, host, versions) are looked for this far in.
const HEADER_ZONE: u64 = 64 << 10;
const MAX_TABLES: usize = 20_000;
const MAX_OBJECTS: usize = 10_000;
/// The first answer comes quickly; carrying on reads longer each time.
const FIRST_BUDGET: Duration = Duration::from_millis(2500);
const MORE_BUDGET: Duration = Duration::from_secs(6);
/// Parked reads: at most this many, each kept this long unused.
const PARKED_MAX: usize = 4;
const PARKED_IDLE: Duration = Duration::from_secs(120);

/// The schema of the dump `path` (index id `id`), or more of it: `more` is the token the previous
/// answer gave, to carry on reading where it stopped.
pub fn schema(id: i64, path: &Path, more: Option<&str>) -> Result<Value> {
    let t0 = Instant::now();
    let (mut p, budget) = match more.and_then(|token| resume(id, token)) {
        Some(p) => (p, MORE_BUDGET),
        None => (Box::new(Parser::open(path)?), FIRST_BUDGET),
    };
    if let Some(kind) = p.not_sql() {
        return Ok(json!({ "format": kind, "size": p.size }));
    }
    let done = p.run(t0 + budget);
    p.elapsed += t0.elapsed();
    let mut v = p.json(done);
    if !done {
        v["more"] = json!(park(id, p)?);
    }
    Ok(v)
}

// ---- parked reads

struct Parked {
    token: String,
    id: i64,
    at: Instant,
    p: Box<Parser>,
}

static PARKED: Mutex<Vec<Parked>> = Mutex::new(Vec::new());

fn resume(id: i64, token: &str) -> Option<Box<Parser>> {
    let mut parked = PARKED.lock().unwrap_or_else(|e| e.into_inner());
    parked.retain(|x| x.at.elapsed() < PARKED_IDLE);
    let i = parked.iter().position(|x| x.token == token && x.id == id)?;
    Some(parked.swap_remove(i).p)
}

fn park(id: i64, p: Box<Parser>) -> Result<String> {
    let token = crate::new_token()?;
    let mut parked = PARKED.lock().unwrap_or_else(|e| e.into_inner());
    parked.retain(|x| x.at.elapsed() < PARKED_IDLE);
    while parked.len() >= PARKED_MAX {
        let oldest = (0..parked.len()).min_by_key(|&i| parked[i].at).unwrap_or(0);
        parked.swap_remove(oldest);
    }
    parked.push(Parked {
        token: token.clone(),
        id,
        at: Instant::now(),
        p,
    });
    Ok(token)
}

// ---- what's found

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dialect {
    Unknown,
    Mysql,
    Postgres,
    Sqlite,
}

impl Dialect {
    fn name(self) -> &'static str {
        match self {
            Dialect::Unknown => "sql",
            Dialect::Mysql => "mysql",
            Dialect::Postgres => "postgres",
            Dialect::Sqlite => "sqlite",
        }
    }
}

#[derive(Default, Debug)]
struct Column {
    name: String,
    ty: String,
    nullable: bool,
    default: Option<String>,
    auto: bool,
    extra: Vec<String>,
    comment: Option<String>,
}

#[derive(Debug)]
struct Index {
    name: Option<String>,
    columns: Vec<String>,
    /// unique | index | fulltext | spatial
    kind: &'static str,
}

#[derive(Debug)]
struct Fk {
    name: Option<String>,
    columns: Vec<String>,
    schema: Option<String>,
    table: String,
    ref_columns: Vec<String>,
    on_delete: Option<String>,
    on_update: Option<String>,
}

#[derive(Default, Debug)]
struct Table {
    /// pg_dumpall: the database a `\connect` chose ("" otherwise)
    database: String,
    /// the MySQL database or PostgreSQL schema ("" when the dump doesn't say)
    schema: String,
    name: String,
    /// a CREATE TABLE for it was read (not just data)
    defined: bool,
    /// a stand-in mysqldump creates for a view before the view itself
    hidden: bool,
    columns: Vec<Column>,
    pk: Vec<String>,
    indexes: Vec<Index>,
    fks: Vec<Fk>,
    checks: u32,
    rows: u64,
    /// some rows come from INSERT … SELECT, which can't be counted from the dump
    uncounted: bool,
    bytes: u64,
    statements: u64,
    copy: bool,
    engine: Option<String>,
    charset: Option<String>,
    comment: Option<String>,
    note: Option<String>,
    sql: Option<String>,
}

#[derive(Debug)]
struct Obj {
    schema: String,
    name: String,
    kind: String,
    /// a trigger's table
    on: Option<String>,
}

#[derive(Default)]
struct Found {
    tool: Option<String>,
    version: Option<String>,
    client: Option<String>,
    server: Option<String>,
    host: Option<String>,
    database: Option<String>,
    dumped: Option<String>,
    completed: Option<String>,
    charset: Option<String>,
    /// the tool's own "dump complete" line (or SQLite's final COMMIT)
    footer: bool,
    tables: Vec<Table>,
    by_key: HashMap<(String, String, String), usize>,
    more_tables: u64,
    views: Vec<Obj>,
    triggers: Vec<Obj>,
    routines: Vec<Obj>,
    sequences: Vec<Obj>,
    events: Vec<Obj>,
    other: Vec<Obj>,
    more_objects: u64,
    databases: Vec<String>,
    schemas: Vec<String>,
    statements: u64,
    sql_kept: usize,
}

// ---- the lexer

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lex {
    Normal,
    /// inside '…', "…" or `…`; whether a backslash escapes the next byte
    Quote(u8, bool),
    /// a -- or # comment, to the end of the line
    Line,
    Block,
    /// PostgreSQL's $tag$ … $tag$ (the tag is `Parser::tag`)
    Dollar,
    /// the data lines of COPY … FROM stdin; whether at the start of a line
    Copy(bool),
    /// a client command line: psql's \connect (false) or MySQL's DELIMITER (true)
    Meta(bool),
}

/// The statement being read.
struct Stmt {
    /// where it starts (its first byte that isn't space or a comment)
    start: u64,
    empty: bool,
    head: Vec<u8>,
    /// still keeping its text
    open: bool,
    depth: u32,
    /// the bare word being read at parenthesis depth 0, lowercased
    word: [u8; 12],
    wlen: usize,
    fw: [u8; 8],
    flen: usize,
    words: u32,
    /// CREATE TRIGGER/PROCEDURE/FUNCTION: a BEGIN … END body may hold the delimiter
    body: bool,
    block: u32,
    /// inside MySQL /*!NNNNN … */ executable comments
    cond: u32,
    values: bool,
    in_values: bool,
    tuples: u64,
}

impl Stmt {
    fn new() -> Stmt {
        Stmt {
            start: 0,
            empty: true,
            head: Vec::new(),
            open: true,
            depth: 0,
            word: [0; 12],
            wlen: 0,
            fw: [0; 8],
            flen: 0,
            words: 0,
            body: false,
            block: 0,
            cond: 0,
            values: false,
            in_values: false,
            tuples: 0,
        }
    }

    fn first(&self) -> &[u8] {
        &self.fw[..self.flen]
    }
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

/// The bytes that can change anything inside a row's parentheses (with the delimiter's first).
const SPECIAL: [bool; 256] = {
    let mut t = [false; 256];
    let s = b"'\"`()-/#*$\\;";
    let mut i = 0;
    while i < s.len() {
        t[s[i] as usize] = true;
        i += 1;
    }
    t
};

const LO: u64 = 0x0101_0101_0101_0101;
const HI: u64 = 0x8080_8080_8080_8080;

/// Nonzero where a byte of `w` is zero (the lowest flagged byte is the first zero).
fn zeros(w: u64) -> u64 {
    w.wrapping_sub(LO) & !w & HI
}

/// Where `a` or `b` first occurs in `h`, eight bytes at a time.
fn find2(h: &[u8], a: u8, b: u8) -> Option<usize> {
    let (ra, rb) = (LO * u64::from(a), LO * u64::from(b));
    let mut i = 0;
    while i + 8 <= h.len() {
        let w = u64::from_le_bytes(h[i..i + 8].try_into().unwrap_or([0; 8]));
        let hit = zeros(w ^ ra) | zeros(w ^ rb);
        if hit != 0 {
            return Some(i + (hit.trailing_zeros() / 8) as usize);
        }
        i += 8;
    }
    h[i..].iter().position(|&x| x == a || x == b).map(|k| i + k)
}

fn find1(h: &[u8], a: u8) -> Option<usize> {
    find2(h, a, a)
}

struct Parser {
    src: Box<dyn Read + Send>,
    /// the file handle the decoder reads from (sharing its offset): how far into the file it got
    raw: File,
    size: u64,
    codec: Codec,
    buf: Vec<u8>,
    lo: usize,
    hi: usize,
    /// stream offset of buf[0]
    base: u64,
    eof: bool,
    lex: Lex,
    tag: Vec<u8>,
    st: Stmt,
    delim: Vec<u8>,
    dialect: Dialect,
    /// PostgreSQL with standard_conforming_strings off: backslashes escape in '…'
    pg_bs: bool,
    /// MySQL: the database USE chose; PostgreSQL: the one \connect chose
    db: String,
    search_path: Option<String>,
    prev: [u8; 2],
    note: Vec<u8>,
    note_at: u64,
    copy: Option<usize>,
    last_insert: Option<(Vec<u8>, usize)>,
    last_commit: bool,
    f: Found,
    truncated: Option<String>,
    broken: Option<String>,
    elapsed: Duration,
}

impl Parser {
    fn open(path: &Path) -> Result<Parser> {
        let f = File::open(path)?;
        let size = f.metadata()?.len();
        let codec = Codec::sniff(&read_at(&f, 0, 8)?);
        (&f).seek(io::SeekFrom::Start(0))?;
        let raw = f.try_clone()?;
        let src = codec.open(f)?;
        Ok(Parser {
            src,
            raw,
            size,
            codec,
            buf: vec![0; BUF],
            lo: 0,
            hi: 0,
            base: 0,
            eof: false,
            lex: Lex::Normal,
            tag: Vec::new(),
            st: Stmt::new(),
            delim: b";".to_vec(),
            dialect: Dialect::Unknown,
            pg_bs: false,
            db: String::new(),
            search_path: None,
            prev: *b"  ",
            note: Vec::new(),
            note_at: 0,
            copy: None,
            last_insert: None,
            last_commit: false,
            f: Found::default(),
            truncated: None,
            broken: None,
            elapsed: Duration::ZERO,
        })
    }

    /// Not a dump at all: a pg_dump archive in its custom format, a database file, binary data.
    fn not_sql(&mut self) -> Option<&'static str> {
        if self.base > 0 || self.lo > 0 {
            return None;
        }
        if self.hi == 0 {
            self.refill();
            if self.buf[..self.hi].starts_with(b"\xEF\xBB\xBF") {
                self.lo = 3; // a UTF-8 byte order mark
            }
        }
        let head = &self.buf[..self.hi.min(4096)];
        if head.starts_with(b"PGDMP") {
            Some("pg_dump-custom")
        } else if head.starts_with(b"SQLite format 3\0") {
            Some("sqlite-database")
        } else if head.contains(&0) {
            Some("binary")
        } else {
            None
        }
    }

    fn refill(&mut self) {
        if self.lo > 0 {
            self.buf.copy_within(self.lo..self.hi, 0);
            self.hi -= self.lo;
            self.base += self.lo as u64;
            self.lo = 0;
        }
        while self.hi < self.buf.len() && !self.eof {
            match self.src.read(&mut self.buf[self.hi..]) {
                Ok(0) => self.eof = true,
                Ok(n) => self.hi += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.eof = true;
                    self.broken = Some(e.to_string());
                }
            }
        }
    }

    /// Read until the end or `until`; true once the whole dump has been read.
    fn run(&mut self, until: Instant) -> bool {
        loop {
            if !self.eof && self.hi - self.lo < 2 * LOOK {
                self.refill();
            }
            let end = if self.eof { self.hi } else { self.hi - LOOK };
            if self.lo >= end {
                if self.eof {
                    self.finish();
                    return true;
                }
                continue;
            }
            self.step(end);
            if Instant::now() >= until {
                return false;
            }
        }
    }

    fn step(&mut self, end: usize) {
        let buf = std::mem::take(&mut self.buf);
        let b = &buf[..self.hi];
        let mut i = self.lo;
        while i < end {
            i = match self.lex {
                Lex::Normal => self.normal(b, i, end),
                Lex::Quote(q, bs) => self.quoted(b, i, end, q, bs),
                Lex::Line => self.line(b, i, end),
                Lex::Block => block(b, i, end).map_or(end, |j| {
                    self.lex = Lex::Normal;
                    j
                }),
                Lex::Dollar => self.dollar(b, i, end),
                Lex::Copy(at_start) => self.copy_data(b, i, end, at_start),
                Lex::Meta(delimiter) => self.meta(b, i, end, delimiter),
            };
        }
        self.lo = i;
        self.buf = buf;
    }

    fn push(&mut self, c: u8) {
        if self.st.open {
            if self.st.head.len() < HEAD_CAP {
                self.st.head.push(c);
            } else {
                self.st.open = false;
            }
        }
    }

    fn push_all(&mut self, s: &[u8]) {
        if self.st.open {
            let room = HEAD_CAP.saturating_sub(self.st.head.len());
            self.st.head.extend_from_slice(&s[..s.len().min(room)]);
            if s.len() > room {
                self.st.open = false;
            }
        }
    }

    fn backslashes(&self) -> bool {
        match self.dialect {
            Dialect::Mysql | Dialect::Unknown => true,
            Dialect::Postgres => self.pg_bs,
            Dialect::Sqlite => false,
        }
    }

    fn hint(&mut self, d: Dialect) {
        if self.dialect == Dialect::Unknown {
            self.dialect = d;
        }
    }

    fn normal(&mut self, b: &[u8], mut i: usize, end: usize) -> usize {
        while i < end {
            if self.st.depth > 0
                && !self.st.open
                && self.st.wlen == 0
                && !self.st.empty
                && SPECIAL[usize::from(self.delim[0])]
            {
                // inside a row's parentheses with nothing to keep: skip to the next byte that matters
                let k = b[i..end]
                    .iter()
                    .position(|&x| SPECIAL[usize::from(x)])
                    .unwrap_or(end - i);
                if k > 0 {
                    self.prev = if k >= 2 {
                        [b[i + k - 2], b[i + k - 1]]
                    } else {
                        [self.prev[1], b[i]]
                    };
                    i += k;
                    continue;
                }
            }
            let c = b[i];
            let wordy = is_word(c);
            if !wordy && self.st.wlen > 0 {
                self.word_end();
            }
            if c == self.delim[0] && self.st.block == 0 && b[i..].starts_with(&self.delim) {
                let next = i + self.delim.len();
                self.end_stmt(self.base + next as u64);
                self.prev = *b"  ";
                return next;
            }
            if self.st.empty {
                if c.is_ascii_whitespace() {
                    i += 1;
                    continue;
                }
                if c == b'\\' && self.dialect != Dialect::Mysql {
                    self.lex = Lex::Meta(false);
                    self.note.clear();
                    return i + 1;
                }
                if (c | 0x20) == b'd'
                    && b.len() > i + 9
                    && b[i..i + 9].eq_ignore_ascii_case(b"delimiter")
                    && matches!(b[i + 9], b' ' | b'\t')
                {
                    self.lex = Lex::Meta(true);
                    self.note.clear();
                    return i + 10;
                }
            }
            match c {
                b'\'' | b'"' | b'`' => {
                    let bs = match c {
                        b'`' => false,
                        b'"' => matches!(self.dialect, Dialect::Mysql | Dialect::Unknown),
                        _ => {
                            self.backslashes()
                                || ((self.prev[1] | 0x20) == b'e' && !is_word(self.prev[0]))
                        }
                    };
                    self.began(i);
                    self.push(c);
                    self.lex = Lex::Quote(c, bs);
                    self.prev = [self.prev[1], c];
                    return i + 1;
                }
                b'-' if b.get(i + 1) == Some(&b'-')
                    && (self.dialect != Dialect::Mysql
                        || b.get(i + 2).is_none_or(|x| x.is_ascii_whitespace())) =>
                {
                    return self.comment(i, 2);
                }
                b'#' if self.dialect == Dialect::Mysql
                    || (self.dialect == Dialect::Unknown && self.st.empty) =>
                {
                    return self.comment(i, 1);
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    self.push(b' ');
                    if b.get(i + 2) == Some(&b'!') {
                        // MySQL's executable comment: what it holds is SQL
                        self.hint(Dialect::Mysql);
                        self.st.cond += 1;
                        let mut j = i + 3;
                        while j < b.len() && j < i + 12 && b[j].is_ascii_digit() {
                            j += 1;
                        }
                        return j;
                    }
                    self.lex = Lex::Block;
                    return i + 2;
                }
                b'*' if self.st.cond > 0 && b.get(i + 1) == Some(&b'/') => {
                    self.st.cond -= 1;
                    self.push(b' ');
                    return i + 2;
                }
                b'$' if self.dialect != Dialect::Mysql && !is_word(self.prev[1]) => {
                    if let Some(n) = dollar_tag(&b[i..]) {
                        self.began(i);
                        self.tag = b[i..i + n].to_vec();
                        self.push_all(&b[i..i + n]);
                        self.lex = Lex::Dollar;
                        self.prev = *b"$$";
                        return i + n;
                    }
                }
                b'(' => self.st.depth += 1,
                b')' => {
                    if self.st.depth == 1 && self.st.in_values {
                        self.st.tuples += 1;
                    }
                    self.st.depth = self.st.depth.saturating_sub(1);
                }
                _ => {}
            }
            if wordy && self.st.depth == 0 {
                if self.st.wlen < self.st.word.len() {
                    self.st.word[self.st.wlen] = c.to_ascii_lowercase();
                }
                self.st.wlen += 1;
            }
            if !c.is_ascii_whitespace() {
                self.began(i);
            }
            self.push(c);
            self.prev = [self.prev[1], c];
            i += 1;
        }
        i
    }

    fn began(&mut self, i: usize) {
        if self.st.empty {
            self.st.empty = false;
            self.st.start = self.base + i as u64;
        }
    }

    fn comment(&mut self, i: usize, skip: usize) -> usize {
        self.push(b' ');
        self.lex = Lex::Line;
        self.note.clear();
        self.note_at = self.base + i as u64;
        i + skip
    }

    fn word_end(&mut self) {
        let n = self.st.wlen;
        self.st.wlen = 0;
        self.st.words += 1;
        if n > self.st.word.len() {
            if self.st.in_values {
                self.st.in_values = false;
            }
            return;
        }
        let w = self.st.word;
        let w = &w[..n];
        if self.st.words == 1 {
            let k = n.min(self.st.fw.len());
            self.st.fw[..k].copy_from_slice(&w[..k]);
            self.st.flen = k;
        }
        match w {
            b"values" | b"value"
                if !self.st.values && matches!(self.st.first(), b"insert" | b"replace") =>
            {
                self.st.values = true;
                self.st.in_values = true;
                self.st.open = false;
            }
            // after the rows: ON DUPLICATE KEY UPDATE, ON CONFLICT, AS new, RETURNING…
            _ if self.st.in_values => self.st.in_values = w == b"row",
            b"trigger" | b"procedure" | b"function" | b"event"
                if self.st.first() == b"create" && self.st.words < 24 =>
            {
                self.st.body = true
            }
            b"begin" if self.st.body && self.dialect != Dialect::Mysql => self.st.block += 1,
            b"case" if self.st.block > 0 => self.st.block += 1,
            b"end" if self.st.block > 0 => self.st.block -= 1,
            _ => {}
        }
    }

    fn quoted(&mut self, b: &[u8], mut i: usize, end: usize, q: u8, bs: bool) -> usize {
        while i < end {
            let rest = &b[i..end];
            let k = if bs {
                find2(rest, q, b'\\')
            } else {
                find1(rest, q)
            };
            let Some(k) = k else {
                self.push_all(rest);
                return end;
            };
            let j = i + k;
            if b[j] == b'\\' {
                let to = (j + 2).min(b.len());
                self.push_all(&b[i..to]);
                i = to;
                continue;
            }
            if b.get(j + 1) == Some(&q) {
                self.push_all(&b[i..j + 2]);
                i = j + 2;
                continue;
            }
            self.push_all(&b[i..=j]);
            self.lex = Lex::Normal;
            self.prev = [b'x', q];
            return j + 1;
        }
        i
    }

    fn line(&mut self, b: &[u8], i: usize, end: usize) -> usize {
        match find1(&b[i..end], b'\n') {
            Some(k) => {
                self.keep_note(&b[i..i + k]);
                self.lex = Lex::Normal;
                self.header_comment();
                i + k + 1
            }
            None => {
                self.keep_note(&b[i..end]);
                end
            }
        }
    }

    fn keep_note(&mut self, s: &[u8]) {
        let room = 400usize.saturating_sub(self.note.len());
        self.note.extend_from_slice(&s[..s.len().min(room)]);
    }

    fn dollar(&mut self, b: &[u8], mut i: usize, end: usize) -> usize {
        while i < end {
            let Some(k) = find1(&b[i..end], b'$') else {
                self.push_all(&b[i..end]);
                return end;
            };
            let j = i + k;
            if b[j..].starts_with(&self.tag) {
                let to = j + self.tag.len();
                self.push_all(&b[i..to]);
                self.lex = Lex::Normal;
                return to;
            }
            self.push_all(&b[i..=j]);
            i = j + 1;
        }
        i
    }

    fn copy_data(&mut self, b: &[u8], mut i: usize, end: usize, mut at_start: bool) -> usize {
        let from = i;
        let mut rows = 0;
        while i < end {
            if at_start {
                if b[i] == b'\\'
                    && b.get(i + 1) == Some(&b'.')
                    && matches!(b.get(i + 2), None | Some(b'\n' | b'\r'))
                {
                    self.add_copy(rows, (i - from) as u64);
                    self.lex = Lex::Normal;
                    self.copy = None;
                    return b[i..]
                        .iter()
                        .position(|&x| x == b'\n')
                        .map_or(b.len(), |k| i + k + 1);
                }
                rows += 1;
                at_start = false;
            }
            match find1(&b[i..end], b'\n') {
                Some(k) => {
                    i += k + 1;
                    at_start = true;
                }
                None => i = end,
            }
        }
        self.add_copy(rows, (i - from) as u64);
        self.lex = Lex::Copy(at_start);
        i
    }

    fn add_copy(&mut self, rows: u64, bytes: u64) {
        if let Some(t) = self.copy.and_then(|t| self.f.tables.get_mut(t)) {
            t.rows += rows;
            t.bytes += bytes;
        }
    }

    fn meta(&mut self, b: &[u8], i: usize, end: usize, delimiter: bool) -> usize {
        let Some(k) = find1(&b[i..end], b'\n') else {
            self.keep_note(&b[i..end]);
            return end;
        };
        self.keep_note(&b[i..i + k]);
        self.lex = Lex::Normal;
        let line = String::from_utf8_lossy(&self.note).trim().to_string();
        if delimiter {
            self.hint(Dialect::Mysql);
            if let Some(d) = line.split_whitespace().next()
                && d.len() <= 16
            {
                self.delim = d.as_bytes().to_vec();
            }
        } else {
            let mut words = line.split_whitespace();
            if matches!(words.next(), Some("connect" | "c")) {
                self.hint(Dialect::Postgres);
                if let Some(arg) = words.last() {
                    let arg = arg.trim_matches(|c| c == '"' || c == '\'');
                    let db = arg
                        .strip_prefix("dbname=")
                        .unwrap_or(arg)
                        .trim_matches(|c| c == '"' || c == '\'');
                    if !db.is_empty() {
                        self.db = db.to_string();
                        push_unique(&mut self.f.databases, db);
                    }
                }
            }
        }
        i + k + 1
    }

    fn end_stmt(&mut self, at: u64) {
        if self.st.wlen > 0 {
            self.word_end();
        }
        let st = std::mem::replace(&mut self.st, Stmt::new());
        if !st.empty {
            self.statement(&st, at.saturating_sub(st.start));
        }
    }

    /// The end of the dump: say if it stops somewhere a complete dump wouldn't.
    fn finish(&mut self) {
        let cut = match self.lex {
            Lex::Line => {
                self.header_comment();
                None
            }
            Lex::Quote(..) | Lex::Dollar => Some("it ends inside a quoted string"),
            Lex::Block => Some("it ends inside a comment"),
            Lex::Copy(_) => Some("it ends inside a COPY block's data (no \\. line)"),
            Lex::Normal | Lex::Meta(_) => None,
        };
        self.lex = Lex::Normal;
        if self.st.wlen > 0 {
            self.word_end();
        }
        let st = std::mem::replace(&mut self.st, Stmt::new());
        let open_insert = !st.empty && st.values && (st.depth > 0 || st.tuples == 0);
        if !st.empty {
            let at = self.base + self.hi as u64;
            self.statement(&st, at.saturating_sub(st.start));
        }
        if let Some(e) = &self.broken {
            self.truncated = Some(format!("the compressed data stops making sense ({e})"));
        } else if let Some(why) = cut {
            self.truncated = Some(why.into());
        } else if open_insert {
            self.truncated = Some("its last INSERT has no end".into());
        }
    }
}

// ---- helpers

fn key_of(database: &str, schema: &str, name: &str) -> String {
    [database, schema, name]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(".")
}

fn push_unique(list: &mut Vec<String>, v: &str) {
    if !list.iter().any(|x| x == v) {
        list.push(v.to_string());
    }
}

fn replace_column(tab: &mut Table, old: Option<String>, c: Column) {
    let name = old.unwrap_or_else(|| c.name.clone());
    match tab.columns.iter_mut().find(|x| x.name == name) {
        Some(x) => {
            let comment = x.comment.take();
            *x = c;
            if x.comment.is_none() {
                x.comment = comment;
            }
        }
        None => tab.columns.push(c),
    }
}

/// Where a block comment ends (just past its */), if in `b[i..end]`.
fn block(b: &[u8], mut i: usize, end: usize) -> Option<usize> {
    while i < end {
        let j = i + find1(&b[i..end], b'*')?;
        if b.get(j + 1) == Some(&b'/') {
            return Some(j + 2);
        }
        i = j + 1;
    }
    None
}

/// The length of a $tag$ opening a dollar-quoted string at the start of `b`.
fn dollar_tag(b: &[u8]) -> Option<usize> {
    let n = b.iter().skip(1).take(LOOK - 1).position(|&c| c == b'$')? + 2;
    let tag = &b[1..n - 1];
    let ok = tag.is_empty()
        || (!tag[0].is_ascii_digit()
            && tag
                .iter()
                .all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80));
    ok.then_some(n)
}

fn clip(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}\n…", &s[..s.floor_char_boundary(max)])
    } else {
        s.to_string()
    }
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests;
