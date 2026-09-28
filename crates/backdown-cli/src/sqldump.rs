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
    let token = crate::serve::new_token()?;
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

    // ---- statements

    fn statement(&mut self, st: &Stmt, len: u64) {
        self.f.statements += 1;
        let first = st.first();
        self.last_commit = first == b"commit";
        if matches!(first, b"insert" | b"replace") {
            return self.insert(st, len);
        }
        let text = String::from_utf8_lossy(&st.head);
        let t = tokenize(&text, self.backslashes());
        let Some(w) = t.first().filter(|x| x.k == K::Word) else {
            return;
        };
        match w.v.to_ascii_lowercase().as_str() {
            "create" => self.create(&t, &text),
            "alter" => self.alter(&t, &text),
            "copy" => self.copy_start(&t, len),
            "comment" => self.comment_on(&t),
            "use" => {
                self.hint(Dialect::Mysql);
                if let Some(x) = t.get(1).filter(|x| x.named()) {
                    self.db = x.v.clone();
                    push_unique(&mut self.f.databases, &x.v);
                }
            }
            "set" => self.set(&t),
            "pragma" => {
                self.hint(Dialect::Sqlite);
                if self.f.statements == 1 && self.dialect == Dialect::Sqlite {
                    self.f.tool.get_or_insert_with(|| "sqlite3 .dump".into());
                }
            }
            "lock" | "unlock" => self.hint(Dialect::Mysql),
            "select" if text.contains("pg_catalog.") => self.hint(Dialect::Postgres),
            _ => {}
        }
    }

    fn ident(&self, t: &Tok) -> String {
        if t.k == K::Word && self.dialect == Dialect::Postgres {
            t.v.to_lowercase()
        } else {
            t.v.clone()
        }
    }

    /// A possibly qualified name at `i`: (schema, name, the index after it).
    fn qname(&self, t: &[Tok], i: usize) -> Option<(Option<String>, String, usize)> {
        let mut parts = Vec::new();
        let mut j = i;
        while let Some(x) = t.get(j).filter(|x| x.named()) {
            parts.push(self.ident(x));
            j += 1;
            if pt(t, j, b'.') && t.get(j + 1).is_some_and(Tok::named) {
                j += 1;
            } else {
                break;
            }
        }
        let name = parts.pop()?;
        Some((parts.pop(), name, j))
    }

    fn schema_of(&self, given: Option<String>) -> String {
        given.unwrap_or_else(|| match self.dialect {
            Dialect::Postgres => self.search_path.clone().unwrap_or_else(|| "public".into()),
            Dialect::Sqlite => String::new(),
            Dialect::Mysql | Dialect::Unknown => {
                if self.db.is_empty() {
                    self.f.database.clone().unwrap_or_default()
                } else {
                    self.db.clone()
                }
            }
        })
    }

    /// The pg_dumpall database a table is in ("" for everything else).
    fn database(&self) -> String {
        if self.dialect == Dialect::Postgres {
            self.db.clone()
        } else {
            String::new()
        }
    }

    fn table(&mut self, schema: Option<String>, name: String) -> Option<usize> {
        let key = (self.database(), self.schema_of(schema), name);
        if let Some(&i) = self.f.by_key.get(&key) {
            return Some(i);
        }
        if self.f.tables.len() >= MAX_TABLES {
            self.f.more_tables += 1;
            return None;
        }
        let i = self.f.tables.len();
        self.f.tables.push(Table {
            database: key.0.clone(),
            schema: key.1.clone(),
            name: key.2.clone(),
            ..Table::default()
        });
        self.f.by_key.insert(key, i);
        Some(i)
    }

    fn existing(&self, schema: Option<String>, name: &str) -> Option<usize> {
        let key = (self.database(), self.schema_of(schema), name.to_string());
        self.f.by_key.get(&key).copied()
    }

    fn insert(&mut self, st: &Stmt, len: u64) {
        let cached = self
            .last_insert
            .as_ref()
            .filter(|(h, _)| *h == st.head)
            .map(|(_, i)| *i);
        let (idx, rows) = match cached {
            Some(i) => (Some(i), st.tuples),
            None => {
                let text = String::from_utf8_lossy(&st.head);
                let t = tokenize(&text, self.backslashes());
                let mut i = 1;
                while t.get(i).is_some_and(|x| {
                    ["low_priority", "delayed", "high_priority", "ignore"]
                        .iter()
                        .any(|k| x.is(k))
                }) {
                    i += 1;
                }
                if kw(&t, i, "or") {
                    i += 2; // SQLite: INSERT OR REPLACE
                }
                if kw(&t, i, "into") {
                    i += 1;
                }
                let Some((schema, name, mut j)) = self.qname(&t, i) else {
                    return;
                };
                if self.dialect == Dialect::Sqlite && name.starts_with("sqlite_") {
                    return; // sqlite_sequence and the like: SQLite's own bookkeeping
                }
                let idx = self.table(schema, name);
                let rows = if st.values {
                    if let Some(i) = idx {
                        self.last_insert = Some((st.head.clone(), i));
                    }
                    let default = t.len() >= 2 && t[t.len() - 2].is("default");
                    st.tuples.max(u64::from(default))
                } else {
                    if pt(&t, j, b'(') {
                        j = close(&t, j);
                    }
                    if kw(&t, j, "set") {
                        1
                    } else {
                        if let Some(tab) = idx.and_then(|i| self.f.tables.get_mut(i)) {
                            tab.uncounted = true;
                        }
                        0
                    }
                };
                (idx, rows)
            }
        };
        if let Some(tab) = idx.and_then(|i| self.f.tables.get_mut(i)) {
            tab.rows += rows;
            tab.bytes += len;
            tab.statements += 1;
        }
    }

    fn copy_start(&mut self, t: &[Tok], len: u64) {
        let Some((schema, name, mut j)) = self.qname(t, 1) else {
            return;
        };
        if pt(t, j, b'(') {
            j = close(t, j);
        }
        if !(kw(t, j, "from") && kw(t, j + 1, "stdin")) {
            return;
        }
        self.hint(Dialect::Postgres);
        let idx = self.table(schema, name);
        if let Some(tab) = idx.and_then(|i| self.f.tables.get_mut(i)) {
            tab.copy = true;
            tab.statements += 1;
            tab.bytes += len;
        }
        self.copy = idx;
        self.lex = Lex::Copy(false);
    }

    fn set(&mut self, t: &[Tok]) {
        let Some(what) = t.get(1) else {
            return;
        };
        if what.p(b'@') {
            return self.hint(Dialect::Mysql);
        }
        let value = |from: usize| -> Option<String> {
            let mut i = from;
            if pt(t, i, b'=') || kw(t, i, "to") {
                i += 1;
            }
            t.get(i).map(|x| x.v.clone())
        };
        match what.v.to_ascii_lowercase().as_str() {
            "names" => {
                self.hint(Dialect::Mysql);
                self.f.charset = value(2);
            }
            "character" if kw(t, 2, "set") => self.f.charset = value(3),
            "charset" => self.f.charset = value(2),
            "client_encoding" => {
                self.hint(Dialect::Postgres);
                self.f.charset = value(2);
            }
            "standard_conforming_strings" => {
                self.hint(Dialect::Postgres);
                self.pg_bs = value(2).is_some_and(|v| v.eq_ignore_ascii_case("off"));
            }
            "search_path" => {
                self.hint(Dialect::Postgres);
                let mut i = 2;
                if pt(t, i, b'=') || kw(t, i, "to") {
                    i += 1;
                }
                self.search_path = t
                    .get(i)
                    .filter(|x| {
                        x.named() && !x.v.is_empty() && !x.v.eq_ignore_ascii_case("pg_catalog")
                    })
                    .map(|x| self.ident(x));
            }
            "statement_timeout"
            | "lock_timeout"
            | "idle_in_transaction_session_timeout"
            | "transaction_timeout"
            | "check_function_bodies"
            | "xmloption"
            | "client_min_messages"
            | "row_security"
            | "default_tablespace"
            | "default_table_access_method"
            | "default_with_oids" => self.hint(Dialect::Postgres),
            "sql_mode" | "foreign_key_checks" | "unique_checks" | "time_zone" | "sql_notes"
            | "autocommit" => self.hint(Dialect::Mysql),
            _ => {}
        }
    }

    fn comment_on(&mut self, t: &[Tok]) {
        if !kw(t, 1, "on") {
            return;
        }
        let Some(is) = t.iter().position(|x| x.is("is")) else {
            return;
        };
        let Some(text) = t.get(is + 1).filter(|x| x.k == K::Str).map(|x| x.v.clone()) else {
            return;
        };
        let mut parts: Vec<String> = t[3..is]
            .iter()
            .filter(|x| x.named())
            .map(|x| self.ident(x))
            .collect();
        if kw(t, 2, "table") {
            let Some(name) = parts.pop() else {
                return;
            };
            if let Some(i) = self.existing(parts.pop(), &name) {
                self.f.tables[i].comment = Some(text);
            }
        } else if kw(t, 2, "column") && parts.len() >= 2 {
            let col = parts.pop().unwrap_or_default();
            let name = parts.pop().unwrap_or_default();
            if let Some(i) = self.existing(parts.pop(), &name)
                && let Some(c) = self.f.tables[i].columns.iter_mut().find(|c| c.name == col)
            {
                c.comment = Some(text);
            }
        }
    }

    fn create(&mut self, t: &[Tok], src: &str) {
        let mut i = 1;
        let mut kind = "index";
        let mut materialized = false;
        let what = loop {
            let Some(x) = t.get(i).filter(|_| i < 40) else {
                return;
            };
            if x.k == K::Word {
                let w = x.v.to_ascii_lowercase();
                match w.as_str() {
                    "unique" => kind = "unique",
                    "fulltext" => kind = "fulltext",
                    "spatial" => kind = "spatial",
                    "materialized" => materialized = true,
                    "table" | "view" | "index" | "trigger" | "procedure" | "function"
                    | "aggregate" | "event" | "sequence" | "schema" | "database" | "type"
                    | "domain" | "extension" => break w,
                    _ => {}
                }
            }
            i += 1;
        };
        i += 1;
        loop {
            if kw(t, i, "if") && kw(t, i + 1, "not") && kw(t, i + 2, "exists") {
                i += 3;
            } else if kw(t, i, "concurrently") {
                i += 1;
            } else {
                break;
            }
        }
        match what.as_str() {
            "table" => self.create_table(t, src, i),
            "index" => self.create_index(t, src, i, kind),
            "database" | "schema" => {
                if let Some(x) = t.get(i).filter(|x| x.named() && !x.is("authorization")) {
                    let name = self.ident(x);
                    if what == "database" || self.dialect == Dialect::Mysql {
                        push_unique(&mut self.f.databases, &name);
                    } else {
                        push_unique(&mut self.f.schemas, &name);
                    }
                }
            }
            _ => {
                let Some((schema, name, j)) = self.qname(t, i) else {
                    return;
                };
                let schema = self.schema_of(schema);
                let kind = if what == "view" && materialized {
                    "materialized view".to_string()
                } else {
                    what.clone()
                };
                if what == "view" {
                    // mysqldump first creates a stand-in (a table, or a view of constants)
                    let key = (self.database(), schema.clone(), name.clone());
                    if let Some(&ti) = self.f.by_key.get(&key) {
                        let tab = &mut self.f.tables[ti];
                        if tab.rows == 0 && tab.statements == 0 {
                            tab.hidden = true;
                        }
                    }
                }
                let on = if what == "trigger" {
                    t[j.min(t.len())..]
                        .iter()
                        .position(|x| x.is("on"))
                        .and_then(|k| self.qname(t, j + k + 1))
                        .map(|(s, n, _)| key_of(&self.database(), &self.schema_of(s), &n))
                } else {
                    None
                };
                let f = &mut self.f;
                let list = match what.as_str() {
                    "view" => &mut f.views,
                    "trigger" => &mut f.triggers,
                    "procedure" | "function" | "aggregate" => &mut f.routines,
                    "event" => &mut f.events,
                    "sequence" => &mut f.sequences,
                    _ => &mut f.other,
                };
                if list
                    .iter()
                    .any(|o| o.schema == schema && o.name == name && o.kind == kind)
                {
                    return;
                }
                if list.len() >= MAX_OBJECTS {
                    f.more_objects += 1;
                    return;
                }
                list.push(Obj {
                    schema,
                    name,
                    kind,
                    on,
                });
            }
        }
    }

    fn create_table(&mut self, t: &[Tok], src: &str, i: usize) {
        let Some((schema, name, mut i)) = self.qname(t, i) else {
            return;
        };
        let Some(ti) = self.table(schema, name) else {
            return;
        };
        let mut tab = std::mem::take(&mut self.f.tables[ti]);
        tab.defined = true;
        tab.hidden = false;
        tab.columns.clear();
        tab.pk.clear();
        tab.indexes.clear();
        tab.fks.clear();
        tab.checks = 0;
        tab.note = None;
        if kw(t, i, "partition")
            && kw(t, i + 1, "of")
            && let Some((s, n, j)) = self.qname(t, i + 2)
        {
            tab.note = Some(format!(
                "a partition of {}",
                key_of("", &self.schema_of(s), &n)
            ));
            i = j;
        }
        if pt(t, i, b'(') {
            let e = close(t, i);
            for (s, e) in commas(t, i + 1, inner_end(t, i, e)) {
                self.definition(&mut tab, t, src, s, e);
            }
            i = e;
            self.table_options(&mut tab, t, i);
        } else if kw(t, i, "as") {
            tab.note = Some("made from a query (CREATE TABLE … AS)".into());
        } else if kw(t, i, "like")
            && let Some((s, n, _)) = self.qname(t, i + 1)
        {
            tab.note = Some(format!("like {}", key_of("", &self.schema_of(s), &n)));
        }
        if self.f.sql_kept < SQL_TOTAL {
            let s = clip(src.trim(), SQL_CAP);
            self.f.sql_kept += s.len();
            tab.sql = Some(s);
        }
        self.f.tables[ti] = tab;
    }

    fn table_options(&self, tab: &mut Table, t: &[Tok], mut i: usize) {
        let value = |i: usize| -> (Option<String>, usize) {
            let j = if pt(t, i, b'=') { i + 1 } else { i };
            (t.get(j).map(|x| x.v.clone()), j + 1)
        };
        while i < t.len() {
            if kw(t, i, "engine") || kw(t, i, "type") {
                (tab.engine, i) = value(i + 1);
            } else if kw(t, i, "charset") {
                (tab.charset, i) = value(i + 1);
            } else if kw(t, i, "character") && kw(t, i + 1, "set") {
                (tab.charset, i) = value(i + 2);
            } else if kw(t, i, "comment") {
                (tab.comment, i) = value(i + 1);
            } else if kw(t, i, "inherits")
                && let Some((s, n, j)) = self.qname(t, i + 2)
            {
                tab.note = Some(format!("inherits {}", key_of("", &self.schema_of(s), &n)));
                i = j;
            } else if kw(t, i, "partition") && kw(t, i + 1, "by") {
                tab.note.get_or_insert_with(|| "partitioned".into());
                i += 2;
            } else {
                i += 1;
            }
        }
    }

    fn definition(&self, tab: &mut Table, t: &[Tok], src: &str, s: usize, e: usize) {
        let Some(x) = t.get(s) else {
            return;
        };
        let w = x.v.to_ascii_lowercase();
        let constraint = x.k == K::Word
            && match w.as_str() {
                "constraint" | "primary" | "foreign" | "check" | "exclude" | "unique" => true,
                // a column may be called key (SQLite); MySQL's KEY name (…) has its parenthesis
                "key" | "index" | "fulltext" | "spatial" => {
                    (s + 1..e.min(s + 4)).any(|j| pt(t, j, b'('))
                }
                _ => false,
            };
        if constraint {
            self.constraint(tab, t, src, s, e);
        } else if let Some(c) = self.column(tab, t, src, s, e) {
            tab.columns.push(c);
        }
    }

    fn column(&self, tab: &mut Table, t: &[Tok], src: &str, s: usize, e: usize) -> Option<Column> {
        let x = t.get(s).filter(|x| x.named() || x.k == K::Str)?;
        let mut c = Column {
            name: self.ident(x),
            nullable: true,
            ..Column::default()
        };
        let mut j = s + 1;
        while j < e {
            if t[j].p(b'(') {
                j = close(t, j).min(e);
                continue;
            }
            if is_attr(t, j) {
                break;
            }
            j += 1;
        }
        c.ty = squash(&span(src, t, s + 1, j));
        while j < e {
            let x = &t[j];
            if x.p(b'(') {
                j = close(t, j).min(e);
                continue;
            }
            if x.k != K::Word {
                j += 1;
                continue;
            }
            match x.v.to_ascii_lowercase().as_str() {
                "not" if kw(t, j + 1, "null") => {
                    c.nullable = false;
                    j += 2;
                }
                "default" => {
                    let (d, k) = expr(src, t, j + 1, e);
                    c.default = Some(d);
                    j = k;
                }
                "primary" => {
                    c.nullable = false;
                    push_unique(&mut tab.pk, &c.name);
                    j += 1;
                }
                "unique" => {
                    tab.indexes.push(Index {
                        name: None,
                        columns: vec![c.name.clone()],
                        kind: "unique",
                    });
                    j += 1;
                }
                "references" => {
                    let (fk, k) = self.references(t, j + 1, e, None, vec![c.name.clone()]);
                    tab.fks.extend(fk);
                    j = k;
                }
                "check" => {
                    tab.checks += 1;
                    j += 1;
                }
                "constraint" => j += 2,
                "collate" => {
                    if let Some(v) = t.get(j + 1) {
                        c.extra.push(format!("collate {}", v.v));
                    }
                    j += 2;
                }
                "character" if kw(t, j + 1, "set") => {
                    if let Some(v) = t.get(j + 2) {
                        c.extra.push(format!("charset {}", v.v));
                    }
                    j += 3;
                }
                "charset" => {
                    if let Some(v) = t.get(j + 1) {
                        c.extra.push(format!("charset {}", v.v));
                    }
                    j += 2;
                }
                "auto_increment" | "autoincrement" | "identity" => {
                    c.auto = true;
                    j += 1;
                }
                "generated" | "as" => {
                    let mut k = j + 1;
                    while k < e && !kw(t, k, "as") && !pt(t, k, b'(') {
                        k += 1;
                    }
                    if kw(t, k, "as") {
                        k += 1;
                    }
                    if kw(t, k, "identity") {
                        c.auto = true;
                        j = k + 1;
                    } else if pt(t, k, b'(') {
                        let ke = close(t, k).min(e);
                        c.extra
                            .push(format!("generated as {}", squash(&span(src, t, k, ke))));
                        j = ke;
                    } else {
                        j = k.max(j + 1);
                    }
                }
                "stored" | "virtual" | "persistent" => {
                    if let Some(g) = c.extra.last_mut().filter(|g| g.starts_with("generated")) {
                        g.push(' ');
                        g.push_str(&x.v.to_ascii_lowercase());
                    }
                    j += 1;
                }
                "comment" => {
                    c.comment = t.get(j + 1).map(|v| v.v.clone());
                    j += 2;
                }
                "on" if kw(t, j + 1, "update") => {
                    let (d, k) = expr(src, t, j + 2, e);
                    c.extra.push(format!("on update {d}"));
                    j = k;
                }
                _ => j += 1,
            }
        }
        let ty = c.ty.to_ascii_lowercase();
        if c.default
            .as_deref()
            .is_some_and(|d| d.to_ascii_lowercase().starts_with("nextval("))
            || matches!(ty.as_str(), "serial" | "bigserial" | "smallserial")
        {
            c.auto = true;
        }
        Some(c)
    }

    /// A table constraint (or MySQL key) from `s` to `e`.
    fn constraint(&self, tab: &mut Table, t: &[Tok], src: &str, s: usize, e: usize) {
        let mut i = s;
        let mut name = None;
        if kw(t, i, "constraint") {
            let named = t.get(i + 1).is_some_and(|x| {
                x.named()
                    && !["primary", "unique", "foreign", "check", "exclude"]
                        .iter()
                        .any(|k| x.is(k))
            });
            if named {
                name = t.get(i + 1).map(|x| self.ident(x));
                i += 2;
            } else {
                i += 1;
            }
        }
        let Some(x) = t.get(i) else {
            return;
        };
        let w = x.v.to_ascii_lowercase();
        i += 1;
        // [KEY|INDEX] [name] [USING method] (columns)
        let cols = |i: &mut usize, name: &mut Option<String>| -> Vec<String> {
            if kw(t, *i, "key") || kw(t, *i, "index") {
                *i += 1;
            }
            while *i < e && !pt(t, *i, b'(') {
                if kw(t, *i, "using") {
                    *i += 2;
                } else {
                    if name.is_none() && t[*i].named() {
                        *name = Some(self.ident(&t[*i]));
                    }
                    *i += 1;
                }
            }
            if pt(t, *i, b'(') {
                let (c, j) = self.col_list(src, t, *i);
                *i = j;
                c
            } else {
                Vec::new()
            }
        };
        match w.as_str() {
            "primary" => {
                let mut ignored = None;
                for c in cols(&mut i, &mut ignored) {
                    push_unique(&mut tab.pk, &c);
                }
            }
            "unique" | "key" | "index" | "fulltext" | "spatial" => {
                let kind = match w.as_str() {
                    "unique" => "unique",
                    "fulltext" => "fulltext",
                    "spatial" => "spatial",
                    _ => "index",
                };
                let columns = cols(&mut i, &mut name);
                tab.indexes.push(Index {
                    name,
                    columns,
                    kind,
                });
            }
            "foreign" => {
                let columns = cols(&mut i, &mut name);
                if kw(t, i, "references") {
                    let (fk, _) = self.references(t, i + 1, e, name, columns);
                    tab.fks.extend(fk);
                }
            }
            "check" => tab.checks += 1,
            _ => {}
        }
    }

    /// REFERENCES table [(columns)] [ON DELETE …] [ON UPDATE …] …, from `i` (after REFERENCES).
    fn references(
        &self,
        t: &[Tok],
        i: usize,
        e: usize,
        name: Option<String>,
        columns: Vec<String>,
    ) -> (Option<Fk>, usize) {
        let Some((schema, table, mut j)) = self.qname(t, i) else {
            return (None, i);
        };
        let mut ref_columns = Vec::new();
        if pt(t, j, b'(') {
            (ref_columns, j) = self.col_list("", t, j);
        }
        let mut fk = Fk {
            name,
            columns,
            schema,
            table,
            ref_columns,
            on_delete: None,
            on_update: None,
        };
        while j < e {
            if kw(t, j, "on") && (kw(t, j + 1, "delete") || kw(t, j + 1, "update")) {
                let del = kw(t, j + 1, "delete");
                let mut k = j + 2;
                let mut action = Vec::new();
                while k < e && t[k].k == K::Word && !t[k].is("on") && !t[k].is("match") {
                    let a = t[k].v.to_ascii_lowercase();
                    if ![
                        "cascade", "restrict", "no", "action", "set", "null", "default",
                    ]
                    .contains(&a.as_str())
                    {
                        break;
                    }
                    action.push(a);
                    k += 1;
                }
                let action = Some(action.join(" ")).filter(|a| !a.is_empty());
                if del {
                    fk.on_delete = action;
                } else {
                    fk.on_update = action;
                }
                j = k;
            } else if kw(t, j, "match") {
                j += 2;
            } else if t[j].k == K::Word
                && [
                    "deferrable",
                    "not",
                    "initially",
                    "deferred",
                    "immediate",
                    "enforced",
                    "valid",
                ]
                .iter()
                .any(|k| t[j].is(k))
            {
                j += 1;
            } else {
                break;
            }
        }
        (Some(fk), j)
    }

    /// `( a, b(10), c DESC, lower(d) )` at `i`: the column names (expressions as written).
    fn col_list(&self, src: &str, t: &[Tok], i: usize) -> (Vec<String>, usize) {
        let e = close(t, i);
        let mut out = Vec::new();
        for (s, e2) in commas(t, i + 1, inner_end(t, i, e)) {
            let x = &t[s];
            let one = e2 - s == 1
                || (e2 - s == 2 && (t[s + 1].is("asc") || t[s + 1].is("desc")))
                || (x.named() && pt(t, s + 1, b'(') && close(t, s + 1) >= e2);
            if x.named() && one {
                let mut name = self.ident(x);
                if pt(t, s + 1, b'(') && !src.is_empty() {
                    name.push_str(&squash(&span(src, t, s + 1, close(t, s + 1).min(e2))));
                }
                out.push(name);
            } else if !src.is_empty() {
                out.push(squash(&span(src, t, s, e2)));
            } else {
                out.push(
                    t[s..e2]
                        .iter()
                        .map(|x| x.v.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
        }
        (out, e)
    }

    fn create_index(&mut self, t: &[Tok], src: &str, mut i: usize, kind: &'static str) {
        let mut name = None;
        if !kw(t, i, "on")
            && let Some((_, n, j)) = self.qname(t, i)
        {
            name = Some(n);
            i = j;
        }
        while i < t.len() && !kw(t, i, "on") {
            i += 1;
        }
        i += 1;
        if kw(t, i, "only") {
            i += 1;
        }
        let Some((schema, table, mut j)) = self.qname(t, i) else {
            return;
        };
        if kw(t, j, "using") {
            j += 2;
        }
        let columns = if pt(t, j, b'(') {
            self.col_list(src, t, j).0
        } else {
            Vec::new()
        };
        if let Some(ti) = self.table(schema, table) {
            self.f.tables[ti].indexes.push(Index {
                name,
                columns,
                kind,
            });
        }
    }

    fn alter(&mut self, t: &[Tok], src: &str) {
        let mut i = 1;
        while t
            .get(i)
            .is_some_and(|x| x.is("online") || x.is("offline") || x.is("ignore"))
        {
            i += 1;
        }
        if !kw(t, i, "table") {
            return;
        }
        i += 1;
        loop {
            if kw(t, i, "if") && kw(t, i + 1, "exists") {
                i += 2;
            } else if kw(t, i, "only") {
                i += 1;
            } else {
                break;
            }
        }
        let Some((schema, name, i)) = self.qname(t, i) else {
            return;
        };
        let Some(ti) = self.table(schema, name) else {
            return;
        };
        let mut tab = std::mem::take(&mut self.f.tables[ti]);
        for (s, e) in commas(t, i, t.len()) {
            let Some(x) = t.get(s) else {
                continue;
            };
            let mut j = s + 1;
            if x.is("add") {
                if kw(t, j, "column") {
                    j += 1;
                }
                if kw(t, j, "if") && kw(t, j + 1, "not") && kw(t, j + 2, "exists") {
                    j += 3;
                }
                let starts_constraint = t.get(j).is_some_and(|y| {
                    y.k == K::Word
                        && [
                            "constraint",
                            "primary",
                            "unique",
                            "key",
                            "index",
                            "foreign",
                            "fulltext",
                            "spatial",
                            "check",
                        ]
                        .iter()
                        .any(|k| y.is(k))
                });
                if starts_constraint {
                    self.constraint(&mut tab, t, src, j, e);
                } else if let Some(c) = self.column(&mut tab, t, src, j, e) {
                    replace_column(&mut tab, None, c);
                }
            } else if x.is("modify") || x.is("change") {
                if kw(t, j, "column") {
                    j += 1;
                }
                let old = if x.is("change") {
                    j += 1;
                    t.get(j - 1).map(|y| self.ident(y))
                } else {
                    None
                };
                if let Some(c) = self.column(&mut tab, t, src, j, e) {
                    replace_column(&mut tab, old, c);
                }
            } else if x.is("alter") {
                if kw(t, j, "column") {
                    j += 1;
                }
                let Some(col) = t.get(j).map(|y| self.ident(y)) else {
                    continue;
                };
                let Some(c) = tab.columns.iter_mut().find(|c| c.name == col) else {
                    continue;
                };
                if kw(t, j + 1, "set") && kw(t, j + 2, "default") {
                    let d = expr(src, t, j + 3, e).0;
                    if d.to_ascii_lowercase().starts_with("nextval(") {
                        c.auto = true;
                    }
                    c.default = Some(d);
                } else if kw(t, j + 1, "drop") && kw(t, j + 2, "default") {
                    c.default = None;
                } else if kw(t, j + 1, "set") && kw(t, j + 2, "not") {
                    c.nullable = false;
                } else if kw(t, j + 1, "drop") && kw(t, j + 2, "not") {
                    c.nullable = true;
                } else if kw(t, j + 1, "add") && t[j..e].iter().any(|y| y.is("identity")) {
                    c.auto = true;
                }
            }
        }
        self.f.tables[ti] = tab;
    }

    // ---- the header's comments

    fn header_comment(&mut self) {
        let text = String::from_utf8_lossy(&self.note).trim().to_string();
        let at = self.note_at;
        if text.is_empty() {
            return;
        }
        let t = text.as_str();
        let val =
            |s: &str| Some(s.trim_start_matches(':').trim().to_string()).filter(|v| !v.is_empty());
        if let Some(d) = t.strip_prefix("Dump completed on") {
            self.f.completed = val(d);
            self.f.footer = true;
            return;
        }
        if t.starts_with("PostgreSQL database dump complete")
            || t.starts_with("PostgreSQL database cluster dump complete")
        {
            self.f.footer = true;
            return;
        }
        if let Some(d) = t.strip_prefix("Completed on") {
            self.f.completed = val(d);
            return;
        }
        if let Some(r) = t.strip_prefix("Database \"")
            && let Some((db, _)) = r.split_once('"')
        {
            push_unique(&mut self.f.databases, db);
            return;
        }
        if at > HEADER_ZONE {
            return;
        }
        let tool_line = |f: &mut Found, tool: &str, rest: &str| {
            // "10.13  Distrib 8.0.36, for Linux (x86_64)"
            f.tool = Some(tool.into());
            let mut w = rest.split_whitespace();
            f.version = w.next().map(str::to_string);
            if w.next() == Some("Distrib") {
                f.client = w.next().map(|v| v.trim_end_matches(',').to_string());
            }
        };
        if let Some(r) = t.strip_prefix("MySQL dump") {
            self.hint(Dialect::Mysql);
            tool_line(&mut self.f, "mysqldump", r);
        } else if let Some(r) = t.strip_prefix("MariaDB dump") {
            self.hint(Dialect::Mysql);
            tool_line(&mut self.f, "mariadb-dump", r);
        } else if let Some(r) = t.strip_prefix("Host:") {
            let (host, db) = r.split_once("Database:").unwrap_or((r, ""));
            self.f.host = val(host);
            if let Some(db) = val(db) {
                self.f.database = Some(db.trim_matches('`').to_string());
            }
        } else if let Some(r) = t.strip_prefix("Server version") {
            self.hint(Dialect::Mysql);
            self.f.server = val(r);
        } else if t.starts_with("phpMyAdmin SQL Dump") {
            self.hint(Dialect::Mysql);
            self.f.tool = Some("phpMyAdmin".into());
        } else if let Some(v) = t.strip_prefix("version ")
            && self.f.tool.as_deref() == Some("phpMyAdmin")
        {
            self.f.version = val(v);
        } else if let Some(v) = t.strip_prefix("Generation Time:") {
            self.f.dumped = val(v);
        } else if let Some(v) = t.strip_prefix("Database:") {
            if let Some(db) = val(v) {
                self.f.database = Some(db.trim_matches('`').to_string());
            }
        } else if let Some(r) = t.strip_prefix("Adminer ") {
            // "Adminer 4.8.1 MySQL 8.0.36 dump"
            let r = r.trim_end_matches("dump").trim();
            let (v, server) = r.split_once(' ').unwrap_or((r, ""));
            self.f.tool = Some("Adminer".into());
            self.f.version = Some(v.into());
            if server.starts_with("MySQL") || server.starts_with("MariaDB") {
                self.hint(Dialect::Mysql);
            } else if server.starts_with("PostgreSQL") {
                self.hint(Dialect::Postgres);
            }
            self.f.server = val(server);
        } else if let Some(v) = t.strip_prefix("HeidiSQL Version:") {
            self.f.tool = Some("HeidiSQL".into());
            self.f.version = val(v);
        } else if t == "PostgreSQL database dump" {
            self.hint(Dialect::Postgres);
            self.f.tool.get_or_insert_with(|| "pg_dump".into());
        } else if t == "PostgreSQL database cluster dump" {
            self.hint(Dialect::Postgres);
            self.f.tool = Some("pg_dumpall".into());
        } else if let Some(v) = t.strip_prefix("Dumped from database version") {
            self.f.server = val(v).map(|v| format!("PostgreSQL {v}"));
        } else if let Some(v) = t
            .strip_prefix("Dumped by pg_dump version")
            .or_else(|| t.strip_prefix("Dumped by pg_dumpall version"))
        {
            self.f.version = val(v);
        } else if let Some(v) = t.strip_prefix("Started on") {
            self.f.dumped = val(v);
        }
    }

    // ---- the answer

    fn json(&mut self, done: bool) -> Value {
        let read = self
            .raw
            .stream_position()
            .unwrap_or(self.size)
            .min(self.size);
        let f = &self.f;
        let mut facts: Vec<[String; 2]> = Vec::new();
        let mut fact = |k: &str, v: Option<String>| {
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                facts.push([k.into(), v]);
            }
        };
        let client = f.client.as_ref().map(|c| {
            let product = if f.tool.as_deref() == Some("mariadb-dump") || c.contains("MariaDB") {
                "MariaDB"
            } else {
                "MySQL"
            };
            format!(" (with {product} {})", c.trim_end_matches("-MariaDB"))
        });
        fact(
            "Made by",
            f.tool.as_ref().map(|t| {
                format!(
                    "{t}{}{}",
                    f.version
                        .as_ref()
                        .map(|v| format!(" {v}"))
                        .unwrap_or_default(),
                    client.unwrap_or_default()
                )
            }),
        );
        fact("Server", f.server.clone());
        fact("Host", f.host.clone());
        fact(
            if f.databases.len() > 1 {
                "Databases"
            } else {
                "Database"
            },
            if f.databases.is_empty() {
                f.database.clone()
            } else {
                Some(f.databases.join(", "))
            },
        );
        fact("Dumped", f.dumped.clone().or_else(|| f.completed.clone()));
        if f.dumped.is_some() && f.completed.is_some() {
            fact("Completed", f.completed.clone());
        }
        fact("Character set", f.charset.clone());

        let tables: Vec<Value> = f
            .tables
            .iter()
            .filter(|t| !t.hidden)
            .map(|t| self.table_json(t))
            .collect();
        let mut schemas: Vec<String> = f.schemas.clone();
        for t in f.tables.iter().filter(|t| !t.hidden) {
            push_unique(&mut schemas, &key_of(&t.database, &t.schema, ""));
        }
        schemas.retain(|s| !s.is_empty());
        let objs = |list: &[Obj]| -> Vec<Value> {
            list.iter()
                .map(|o| json!({ "schema": o.schema, "name": o.name, "kind": o.kind, "on": o.on }))
                .collect()
        };
        let mut notes = Vec::new();
        if f.more_tables > 0 {
            notes.push(format!(
                "{} more tables aren't listed (the first {MAX_TABLES} are).",
                f.more_tables
            ));
        }
        if f.more_objects > 0 {
            notes.push(format!(
                "{} more views, triggers or routines aren't listed.",
                f.more_objects
            ));
        }
        let mut unfinished = false;
        if done && self.truncated.is_none() && !f.footer {
            let tool = f.tool.as_deref().unwrap_or("");
            let line = match tool {
                "mysqldump" | "mariadb-dump" => Some("“Dump completed on …”"),
                "pg_dump" => Some("“PostgreSQL database dump complete”"),
                "pg_dumpall" => Some("“PostgreSQL database cluster dump complete”"),
                "sqlite3 .dump" if !self.last_commit => Some("COMMIT"),
                _ => None,
            };
            if let Some(line) = line {
                unfinished = true;
                notes.push(format!(
                    "There's no {line} at the end: {tool} didn't finish, or the file was cut short."
                ));
            }
        }
        json!({
            "format": "sql",
            "dialect": self.dialect.name(),
            "tool": f.tool,
            "compression": match self.codec {
                Codec::Plain => None,
                Codec::Gzip => Some("gzip"),
                Codec::Bzip2 => Some("bzip2"),
                Codec::Xz => Some("xz"),
                Codec::Zstd => Some("zstd"),
            },
            "facts": facts,
            "databases": f.databases,
            "schemas": schemas,
            "tables": tables,
            "views": objs(&f.views),
            "triggers": objs(&f.triggers),
            "routines": objs(&f.routines),
            "events": objs(&f.events),
            "sequences": objs(&f.sequences),
            "other": objs(&f.other),
            "statements": f.statements,
            "size": self.size,
            "read": if done { self.size } else { read },
            "pos": self.base + self.lo as u64,
            "complete": done,
            "truncated": self.truncated.is_some() || unfinished,
            "cut": self.truncated,
            "notes": notes,
            "elapsed_ms": self.elapsed.as_millis() as u64,
            "more": Value::Null,
        })
    }

    fn table_json(&self, t: &Table) -> Value {
        let columns: Vec<Value> = t
            .columns
            .iter()
            .map(|c| {
                json!({
                    "name": c.name, "type": c.ty, "null": c.nullable && !t.pk.contains(&c.name),
                    "default": c.default, "pk": t.pk.contains(&c.name), "auto": c.auto,
                    "extra": c.extra, "comment": c.comment,
                })
            })
            .collect();
        let fks: Vec<Value> = t
            .fks
            .iter()
            .map(|k| {
                let target = self.resolve(t, k);
                json!({
                    "name": k.name, "columns": k.columns, "ref": target,
                    "ref_columns": k.ref_columns, "on_delete": k.on_delete, "on_update": k.on_update,
                })
            })
            .collect();
        let indexes: Vec<Value> = t
            .indexes
            .iter()
            .map(|x| json!({ "name": x.name, "columns": x.columns, "kind": x.kind }))
            .collect();
        json!({
            "key": key_of(&t.database, &t.schema, &t.name),
            "schema": key_of(&t.database, &t.schema, ""),
            "name": t.name,
            "defined": t.defined,
            "columns": columns,
            "primary_key": t.pk,
            "indexes": indexes,
            "foreign_keys": fks,
            "checks": t.checks,
            "rows": t.rows,
            "uncounted": t.uncounted,
            "bytes": t.bytes,
            "statements": t.statements,
            "copy": t.copy,
            "engine": t.engine,
            "charset": t.charset,
            "comment": t.comment,
            "note": t.note,
            "sql": t.sql,
        })
    }

    /// A foreign key's table, as the key of a table in the dump when there is one.
    fn resolve(&self, from: &Table, k: &Fk) -> String {
        let schema = k.schema.clone().unwrap_or_else(|| from.schema.clone());
        let exact = (from.database.clone(), schema.clone(), k.table.clone());
        if self.f.by_key.contains_key(&exact) {
            return key_of(&exact.0, &exact.1, &exact.2);
        }
        let near = self.f.tables.iter().find(|t| {
            t.database == from.database
                && t.name.eq_ignore_ascii_case(&k.table)
                && (k.schema.is_none() || t.schema.eq_ignore_ascii_case(&schema))
        });
        match near {
            Some(t) => key_of(&t.database, &t.schema, &t.name),
            None => key_of(&from.database, &schema, &k.table),
        }
    }
}

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

// ---- tokens of one statement

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum K {
    Word,
    Ident,
    Str,
    Num,
    Punct,
}

#[derive(Debug)]
struct Tok {
    k: K,
    s: usize,
    e: usize,
    /// the value: a word as written, an identifier or string unquoted
    v: String,
}

impl Tok {
    fn is(&self, kw: &str) -> bool {
        self.k == K::Word && self.v.eq_ignore_ascii_case(kw)
    }

    fn p(&self, c: u8) -> bool {
        self.k == K::Punct && self.v.as_bytes().first() == Some(&c)
    }

    fn named(&self) -> bool {
        matches!(self.k, K::Word | K::Ident)
    }
}

fn kw(t: &[Tok], i: usize, k: &str) -> bool {
    t.get(i).is_some_and(|x| x.is(k))
}

fn pt(t: &[Tok], i: usize, c: u8) -> bool {
    t.get(i).is_some_and(|x| x.p(c))
}

fn tokenize(src: &str, bs: bool) -> Vec<Tok> {
    let b = src.as_bytes();
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let (k, e, v) = match c {
            b'\'' => {
                // E'…' (PostgreSQL) always takes backslash escapes
                let e_str = out
                    .last()
                    .is_some_and(|p| p.e == i && p.k == K::Word && p.v.eq_ignore_ascii_case("e"));
                let (v, e) = unquote(b, i, c, bs || e_str);
                (K::Str, e, v)
            }
            b'"' | b'`' => {
                let (v, e) = unquote(b, i, c, false);
                (K::Ident, e, v)
            }
            b'[' => match b[i..].iter().position(|&x| x == b']') {
                Some(k) => (K::Ident, i + k + 1, src[i + 1..i + k].to_string()),
                None => (K::Ident, b.len(), src[i + 1..].to_string()),
            },
            b'0'..=b'9' => {
                let mut e = i;
                while e < b.len() && (is_word(b[e]) || b[e] == b'.') {
                    e += 1;
                }
                (K::Num, e, src[i..e].to_string())
            }
            _ if is_word(c) => {
                let mut e = i;
                while e < b.len() && is_word(b[e]) {
                    e += 1;
                }
                (K::Word, e, src[i..e].to_string())
            }
            _ => (K::Punct, i + 1, (c as char).to_string()),
        };
        out.push(Tok { k, s: i, e, v });
        i = e;
    }
    out
}

fn unquote(b: &[u8], i: usize, q: u8, bs: bool) -> (String, usize) {
    let mut v = Vec::new();
    let mut j = i + 1;
    while j < b.len() {
        let c = b[j];
        if bs && c == b'\\' && j + 1 < b.len() {
            v.push(match b[j + 1] {
                b'n' => b'\n',
                b't' => b'\t',
                b'r' => b'\r',
                b'0' => 0,
                b'Z' => 26,
                x => x,
            });
            j += 2;
            continue;
        }
        if c == q {
            if b.get(j + 1) == Some(&q) {
                v.push(q);
                j += 2;
                continue;
            }
            return (String::from_utf8_lossy(&v).into_owned(), j + 1);
        }
        v.push(c);
        j += 1;
    }
    (String::from_utf8_lossy(&v).into_owned(), j)
}

/// Just past the parenthesis closing the one at `i`.
fn close(t: &[Tok], i: usize) -> usize {
    let mut d = 0usize;
    for (j, x) in t.iter().enumerate().skip(i) {
        if x.p(b'(') {
            d += 1;
        } else if x.p(b')') {
            d = d.saturating_sub(1);
            if d == 0 {
                return j + 1;
            }
        }
    }
    t.len()
}

/// The end of what's inside the parentheses opened at `i` and closed just before `e`.
fn inner_end(t: &[Tok], i: usize, e: usize) -> usize {
    if e > i + 1 && pt(t, e - 1, b')') {
        e - 1
    } else {
        e
    }
}

/// `t[s..e]` split at its top-level commas.
fn commas(t: &[Tok], s: usize, e: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut d, mut from) = (0usize, s);
    for (j, x) in t.iter().enumerate().take(e).skip(s) {
        if x.p(b'(') {
            d += 1;
        } else if x.p(b')') {
            d = d.saturating_sub(1);
        } else if d == 0 && x.p(b',') {
            if j > from {
                out.push((from, j));
            }
            from = j + 1;
        }
    }
    if e.min(t.len()) > from {
        out.push((from, e.min(t.len())));
    }
    out
}

fn span(src: &str, t: &[Tok], s: usize, e: usize) -> String {
    if s >= e || e > t.len() {
        return String::new();
    }
    src[t[s].s..t[e - 1].e].to_string()
}

/// An expression (a default, ON UPDATE …) from `i` up to the next column attribute.
fn expr(src: &str, t: &[Tok], i: usize, e: usize) -> (String, usize) {
    let mut j = i;
    while j < e {
        if t[j].p(b'(') {
            j = close(t, j).min(e);
            continue;
        }
        if j > i && is_attr(t, j) {
            break;
        }
        j += 1;
    }
    (squash(&span(src, t, i, j)), j)
}

/// Does a column's attributes (after its type) start at `j`?
fn is_attr(t: &[Tok], j: usize) -> bool {
    let x = &t[j];
    if x.k != K::Word {
        return false;
    }
    match x.v.to_ascii_lowercase().as_str() {
        "not" | "null" | "default" | "primary" | "unique" | "references" | "check"
        | "constraint" | "collate" | "charset" | "auto_increment" | "autoincrement"
        | "generated" | "as" | "comment" | "identity" | "visible" | "invisible" | "stored"
        | "virtual" | "persistent" | "srid" | "column_format" | "storage" => true,
        "character" => kw(t, j + 1, "set"),
        "on" => kw(t, j + 1, "update"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(bytes: &[u8], suffix: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new().suffix(suffix).tempfile().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    /// Read all of `sql` in one go.
    fn read(sql: &[u8]) -> Value {
        let f = tmp(sql, ".sql");
        let mut p = Parser::open(f.path()).unwrap();
        assert_eq!(p.not_sql(), None);
        assert!(p.run(Instant::now() + Duration::from_secs(60)));
        p.json(true)
    }

    fn table<'a>(v: &'a Value, key: &str) -> &'a Value {
        v["tables"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["key"] == key)
            .unwrap_or_else(|| panic!("no table {key} in {:#}", v["tables"]))
    }

    fn cols(t: &Value) -> Vec<String> {
        t["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn fact(v: &Value, k: &str) -> Option<String> {
        v["facts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f[0] == k)
            .map(|f| f[1].as_str().unwrap().to_string())
    }

    fn names(v: &Value, list: &str) -> Vec<String> {
        v[list]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["name"].as_str().unwrap().to_string())
            .collect()
    }

    const MYSQL: &str = r#"-- MySQL dump 10.13  Distrib 8.0.36, for Linux (x86_64)
--
-- Host: db.example.org    Database: shop
-- ------------------------------------------------------
-- Server version	8.0.36

/*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */;
/*!50503 SET NAMES utf8mb4 */;
/*!40103 SET @OLD_TIME_ZONE=@@TIME_ZONE */;
/*!40014 SET @OLD_FOREIGN_KEY_CHECKS=@@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS=0 */;
/*!40101 SET @OLD_SQL_MODE=@@SQL_MODE, SQL_MODE='NO_AUTO_VALUE_ON_ZERO' */;

--
-- Table structure for table `users`
--

DROP TABLE IF EXISTS `users`;
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!50503 SET character_set_client = utf8mb4 */;
CREATE TABLE `users` (
  `id` int unsigned NOT NULL AUTO_INCREMENT,
  `email` varchar(255) COLLATE utf8mb4_unicode_ci NOT NULL,
  `name` varchar(100) DEFAULT NULL COMMENT 'shown; to others',
  `created_at` timestamp NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  PRIMARY KEY (`id`),
  UNIQUE KEY `email` (`email`),
  KEY `idx_name` (`name`(20))
) ENGINE=InnoDB AUTO_INCREMENT=5 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci COMMENT='people';
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table `users`
--

LOCK TABLES `users` WRITE;
/*!40000 ALTER TABLE `users` DISABLE KEYS */;
INSERT INTO `users` VALUES (1,'ann@example.org','Ann (admin); \'the\' boss','2024-01-02 03:04:05'),(2,'bob@example.org','Bob \\','2024-01-02 03:04:05'),(3,'cy@example.org','it''s ) (( ;',NULL);
INSERT INTO `users` VALUES (4,'di@example.org',"double \" quoted ; )",NULL);
/*!40000 ALTER TABLE `users` ENABLE KEYS */;
UNLOCK TABLES;

DROP TABLE IF EXISTS `orders`;
CREATE TABLE `orders` (
  `id` bigint NOT NULL,
  `user_id` int unsigned NOT NULL,
  `total` decimal(10,2) NOT NULL DEFAULT '0.00',
  `note` text,
  PRIMARY KEY (`id`),
  KEY `fk_user` (`user_id`),
  CONSTRAINT `fk_user` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`) ON DELETE CASCADE ON UPDATE RESTRICT
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

LOCK TABLES `orders` WRITE;
INSERT INTO `orders` VALUES (10,1,9.99,_binary 'a\0b'),(11,2,0.00,'x\'),(y'),(12,1,1.50,NULL) ON DUPLICATE KEY UPDATE total=VALUES(total);
UNLOCK TABLES;

--
-- Temporary view structure for view `v_big`
--

DROP TABLE IF EXISTS `v_big`;
/*!50001 DROP VIEW IF EXISTS `v_big`*/;
/*!50001 CREATE VIEW `v_big` AS SELECT
 1 AS `id`*/;

/*!50003 SET @saved_cs_client      = @@character_set_client */ ;
DELIMITER ;;
/*!50003 CREATE*/ /*!50017 DEFINER=`root`@`localhost`*/ /*!50003 TRIGGER `orders_bi` BEFORE INSERT ON `orders` FOR EACH ROW BEGIN
  IF NEW.total < 0 THEN SET NEW.total = 0; END IF;
  INSERT INTO audit VALUES (1), (2);
END */;;
DELIMITER ;

DELIMITER ;;
CREATE DEFINER=`root`@`localhost` PROCEDURE `cleanup`(IN days INT)
BEGIN
  DELETE FROM orders WHERE id < days; -- not the end;
  SELECT 'done;';
END ;;
DELIMITER ;

/*!50001 DROP VIEW IF EXISTS `v_big`*/;
/*!50001 CREATE ALGORITHM=UNDEFINED */
/*!50013 DEFINER=`root`@`localhost` SQL SECURITY DEFINER */
/*!50001 VIEW `v_big` AS select `orders`.`id` AS `id` from `orders` where (`orders`.`total` > 5) */;

/*!40111 SET SQL_NOTES=@OLD_SQL_NOTES */;

-- Dump completed on 2024-01-15 10:22:33
"#;

    #[test]
    fn mysqldump_schema_rows_and_header() {
        let v = read(MYSQL.as_bytes());
        assert_eq!(v["dialect"], "mysql");
        assert_eq!(v["truncated"], false, "{:#}", v["notes"]);
        assert_eq!(
            fact(&v, "Made by").unwrap(),
            "mysqldump 10.13 (with MySQL 8.0.36)"
        );
        assert_eq!(fact(&v, "Host").unwrap(), "db.example.org");
        assert_eq!(fact(&v, "Database").unwrap(), "shop");
        assert_eq!(fact(&v, "Server").unwrap(), "8.0.36");
        assert_eq!(fact(&v, "Dumped").unwrap(), "2024-01-15 10:22:33");
        assert_eq!(fact(&v, "Character set").unwrap(), "utf8mb4");

        let users = table(&v, "shop.users");
        assert_eq!(cols(users), ["id", "email", "name", "created_at"]);
        assert_eq!(users["rows"], 4);
        assert_eq!(users["statements"], 2);
        assert_eq!(users["engine"], "InnoDB");
        assert_eq!(users["comment"], "people");
        assert_eq!(users["primary_key"], json!(["id"]));
        let c = &users["columns"];
        assert_eq!(c[0]["type"], "int unsigned");
        assert_eq!(c[0]["auto"], true);
        assert_eq!(c[0]["null"], false);
        assert_eq!(c[1]["type"], "varchar(255)");
        assert_eq!(c[2]["null"], true);
        assert_eq!(c[2]["default"], "NULL");
        assert_eq!(c[2]["comment"], "shown; to others");
        assert_eq!(c[3]["default"], "CURRENT_TIMESTAMP");
        assert_eq!(c[3]["extra"], json!(["on update CURRENT_TIMESTAMP"]));
        let idx = users["indexes"].as_array().unwrap();
        assert_eq!(idx.len(), 2);
        assert_eq!(idx[0]["kind"], "unique");
        assert_eq!(idx[1]["columns"], json!(["name(20)"]));

        let orders = table(&v, "shop.orders");
        assert_eq!(
            orders["rows"], 3,
            "ON DUPLICATE KEY UPDATE … VALUES(…) isn't a row"
        );
        let fk = &orders["foreign_keys"][0];
        assert_eq!(fk["ref"], "shop.users");
        assert_eq!(fk["columns"], json!(["user_id"]));
        assert_eq!(fk["ref_columns"], json!(["id"]));
        assert_eq!(fk["on_delete"], "cascade");
        assert_eq!(fk["on_update"], "restrict");
        assert_eq!(orders["columns"][2]["default"], "'0.00'");

        // the trigger body's INSERT isn't data, and there's no table called audit
        assert!(
            v["tables"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["name"] != "audit")
        );
        assert_eq!(names(&v, "triggers"), ["orders_bi"]);
        assert_eq!(v["triggers"][0]["on"], "shop.orders");
        assert_eq!(names(&v, "routines"), ["cleanup"]);
        assert_eq!(
            names(&v, "views"),
            ["v_big"],
            "the stand-in and the view are one view"
        );
        assert_eq!(v["tables"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn mariadb_sandbox_line_and_executable_comments() {
        let dump = "/*M!999999\\- enable the sandbox mode */ \n-- MariaDB dump 10.19  Distrib 10.11.6-MariaDB, for debian-linux-gnu (x86_64)\n--\n-- Host: localhost    Database: wiki\n\n/*!40101 SET NAMES utf8mb4 */;\nCREATE TABLE `page` (`id` int(10) unsigned NOT NULL, `title` varbinary(255) NOT NULL DEFAULT '', PRIMARY KEY (`id`)) ENGINE=InnoDB;\nINSERT INTO `page` VALUES (1,'Main_Page'),(2,'Talk:\\'x\\'');\n-- Dump completed on 2024-02-03 10:00:00\n";
        let v = read(dump.as_bytes());
        assert_eq!(
            fact(&v, "Made by").unwrap(),
            "mariadb-dump 10.19 (with MariaDB 10.11.6)"
        );
        let page = table(&v, "wiki.page");
        assert_eq!(page["rows"], 2);
        assert_eq!(page["columns"][1]["default"], "''");
        assert_eq!(v["truncated"], false);
    }

    const PG: &str = r#"--
-- PostgreSQL database dump
--

\restrict AbCdEf123

-- Dumped from database version 16.1 (Debian 16.1-1.pgdg120+1)
-- Dumped by pg_dump version 16.1 (Debian 16.1-1.pgdg120+1)

SET statement_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);

CREATE SCHEMA sales;

CREATE FUNCTION public.touch() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
  NEW.updated := now(); -- a ; inside
  RAISE NOTICE 'it''s done; really';
  RETURN NEW;
END;
$$;

CREATE FUNCTION public.answer() RETURNS integer
    LANGUAGE sql
    BEGIN ATOMIC
     SELECT 42;
    END;

CREATE TABLE public.users (
    id integer NOT NULL,
    email character varying(255) NOT NULL,
    bio text DEFAULT 'C:\'::text,
    created timestamp with time zone DEFAULT now()
);

COMMENT ON TABLE public.users IS 'People who can sign in';
COMMENT ON COLUMN public.users.email IS 'unique; lowercased';

CREATE SEQUENCE public.users_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    CACHE 1;

ALTER TABLE ONLY public.users ALTER COLUMN id SET DEFAULT nextval('public.users_id_seq'::regclass);

CREATE TABLE sales.orders (
    id bigint NOT NULL,
    user_id integer,
    amount numeric(10,2)
);

CREATE VIEW public.big_orders AS
 SELECT id FROM sales.orders WHERE (amount > (100)::numeric);

COPY public.users (id, email, bio, created) FROM stdin;
1	ann@example.org	line one\nline two	2024-01-02 03:04:05+00
2	bob@example.org	\N	2024-01-02 03:04:05+00
3	cy@example.org	tab\there; not a statement	\N
\.

COPY sales.orders (id, user_id, amount) FROM stdin;
10	1	5.00
11	2	150.00
\.

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_pkey PRIMARY KEY (id);

ALTER TABLE ONLY sales.orders
    ADD CONSTRAINT orders_pkey PRIMARY KEY (id);

CREATE UNIQUE INDEX users_email_key ON public.users USING btree (lower((email)::text));
CREATE INDEX orders_user ON sales.orders USING btree (user_id);

CREATE TRIGGER users_touch BEFORE UPDATE ON public.users FOR EACH ROW EXECUTE FUNCTION public.touch();

ALTER TABLE ONLY sales.orders
    ADD CONSTRAINT orders_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE SET NULL;

\unrestrict AbCdEf123

--
-- PostgreSQL database dump complete
--

"#;

    #[test]
    fn pg_dump_copy_blocks_and_late_constraints() {
        let v = read(PG.as_bytes());
        assert_eq!(v["dialect"], "postgres");
        assert_eq!(v["truncated"], false, "{:#}", v["notes"]);
        assert_eq!(
            fact(&v, "Made by").unwrap(),
            "pg_dump 16.1 (Debian 16.1-1.pgdg120+1)"
        );
        assert_eq!(
            fact(&v, "Server").unwrap(),
            "PostgreSQL 16.1 (Debian 16.1-1.pgdg120+1)"
        );
        assert_eq!(fact(&v, "Character set").unwrap(), "UTF8");
        let users = table(&v, "public.users");
        assert_eq!(users["rows"], 3);
        assert_eq!(users["copy"], true);
        assert_eq!(cols(users), ["id", "email", "bio", "created"]);
        assert_eq!(users["columns"][0]["auto"], true);
        assert_eq!(users["columns"][0]["pk"], true);
        assert_eq!(users["columns"][1]["type"], "character varying(255)");
        assert_eq!(users["columns"][1]["comment"], "unique; lowercased");
        assert_eq!(users["columns"][2]["default"], "'C:\\'::text");
        assert_eq!(users["columns"][3]["type"], "timestamp with time zone");
        assert_eq!(users["comment"], "People who can sign in");
        assert_eq!(users["indexes"][0]["kind"], "unique");
        assert_eq!(
            users["indexes"][0]["columns"],
            json!(["lower((email)::text)"])
        );
        let orders = table(&v, "sales.orders");
        assert_eq!(orders["rows"], 2);
        assert_eq!(orders["primary_key"], json!(["id"]));
        assert_eq!(orders["foreign_keys"][0]["ref"], "public.users");
        assert_eq!(orders["foreign_keys"][0]["on_delete"], "set null");
        assert_eq!(v["schemas"], json!(["sales", "public"]));
        assert_eq!(names(&v, "routines"), ["touch", "answer"]);
        assert_eq!(names(&v, "views"), ["big_orders"]);
        assert_eq!(names(&v, "sequences"), ["users_id_seq"]);
        assert_eq!(v["triggers"][0]["on"], "public.users");
        assert_eq!(v["tables"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn pg_inserts_with_every_kind_of_quoting() {
        let dump = r#"--
-- PostgreSQL database dump
--
SET standard_conforming_strings = on;
CREATE TABLE public.t (id integer, s text);
INSERT INTO public.t VALUES (1, 'plain \ backslash');
INSERT INTO public.t VALUES (2, E'escaped \' quote; (');
INSERT INTO public.t VALUES (3, 'x'), (4, $$dollar ' ; ( quoted$$), (5, /* a ) comment; */ 'y'), (6, $q$ $$ ; $q$);
SET standard_conforming_strings = off;
INSERT INTO public.t (id, s) VALUES (7, 'now \' escapes ;');
INSERT INTO public.t VALUES (8, 'a') ON CONFLICT (id) DO NOTHING;
--
-- PostgreSQL database dump complete
--
"#;
        let v = read(dump.as_bytes());
        assert_eq!(v["truncated"], false, "{:#}", v["notes"]);
        assert_eq!(table(&v, "public.t")["rows"], 8);
        assert_eq!(table(&v, "public.t")["statements"], 5);
    }

    #[test]
    fn sqlite_dump_with_standard_strings_and_a_trigger() {
        let dump = r#"PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE orgs(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE);
INSERT INTO orgs VALUES(1,'Acme ''Tools''');
INSERT INTO orgs VALUES(2,'C:\');
INSERT INTO orgs VALUES(3,'back\''slash');
CREATE TABLE kv(key TEXT, value TEXT, org INTEGER REFERENCES orgs(id), PRIMARY KEY(key, org));
INSERT INTO kv VALUES('a','(1);',1);
INSERT INTO "kv" VALUES('b','2',2);
DELETE FROM sqlite_sequence;
INSERT INTO sqlite_sequence VALUES('orgs',3);
CREATE INDEX kv_org ON kv(org);
CREATE TRIGGER orgs_log AFTER INSERT ON orgs BEGIN
  INSERT INTO kv VALUES('log', CASE WHEN new.id > 1 THEN 'x' ELSE 'y' END, new.id);
  UPDATE kv SET value = 'z';
END;
CREATE VIEW names AS SELECT name FROM orgs;
COMMIT;
"#;
        let v = read(dump.as_bytes());
        assert_eq!(v["dialect"], "sqlite");
        assert_eq!(fact(&v, "Made by").unwrap(), "sqlite3 .dump");
        assert_eq!(v["truncated"], false, "{:#}", v["notes"]);
        let orgs = table(&v, "orgs");
        assert_eq!(orgs["rows"], 3, "'C:\\' ends its string in SQLite");
        assert_eq!(orgs["columns"][0]["auto"], true);
        assert_eq!(orgs["columns"][1]["null"], false);
        let kv = table(&v, "kv");
        assert_eq!(kv["rows"], 2, "the trigger's INSERT isn't a row");
        assert_eq!(cols(kv), ["key", "value", "org"]);
        assert_eq!(kv["primary_key"], json!(["key", "org"]));
        assert_eq!(kv["foreign_keys"][0]["ref"], "orgs");
        assert_eq!(kv["indexes"][0]["name"], "kv_org");
        assert_eq!(names(&v, "triggers"), ["orgs_log"]);
        assert_eq!(names(&v, "views"), ["names"]);
    }

    #[test]
    fn generic_script_without_a_header() {
        let dump = "create table t (id int primary key, label varchar(20) default 'a,b', parent int, foreign key (parent) references t(id));\n\
            insert into t (id, label) values (1, 'one (1)'), (2, 'two, 2'),\n  (3, 'three');\n\
            insert into t select * from t;\n\
            INSERT INTO t SET id = 9, label = 'x';\n\
            insert into t values (4, 'no semicolon at the end')";
        let v = read(dump.as_bytes());
        let t = table(&v, "t");
        assert_eq!(t["rows"], 5);
        assert_eq!(t["uncounted"], true);
        assert_eq!(t["columns"][1]["default"], "'a,b'");
        assert_eq!(t["primary_key"], json!(["id"]));
        assert_eq!(t["foreign_keys"][0]["ref"], "t");
        assert_eq!(v["tool"], Value::Null);
    }

    #[test]
    fn phpmyadmin_keys_come_in_alter_statements() {
        let dump = "-- phpMyAdmin SQL Dump\n-- version 5.2.1\n-- https://www.phpmyadmin.net/\n--\n-- Host: 127.0.0.1\n-- Generation Time: Jan 15, 2024 at 10:22 AM\n-- Server version: 10.4.32-MariaDB\n-- PHP Version: 8.2.12\n\nSET SQL_MODE = \"NO_AUTO_VALUE_ON_ZERO\";\n\n--\n-- Database: `shop`\n--\n\nCREATE TABLE `users` (\n  `id` int(11) NOT NULL,\n  `email` varchar(255) NOT NULL\n) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n\nINSERT INTO `users` (`id`, `email`) VALUES\n(1, 'a@example.org'),\n(2, 'b@example.org');\n\nCREATE TABLE `posts` (`id` int(11) NOT NULL, `user_id` int(11) NOT NULL) ENGINE=InnoDB;\n\nALTER TABLE `users`\n  ADD PRIMARY KEY (`id`),\n  ADD UNIQUE KEY `email` (`email`);\n\nALTER TABLE `posts`\n  ADD PRIMARY KEY (`id`),\n  ADD KEY `user_id` (`user_id`);\n\nALTER TABLE `users`\n  MODIFY `id` int(11) NOT NULL AUTO_INCREMENT, AUTO_INCREMENT=3;\n\nALTER TABLE `posts`\n  ADD CONSTRAINT `posts_ibfk_1` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`);\nCOMMIT;\n";
        let v = read(dump.as_bytes());
        assert_eq!(fact(&v, "Made by").unwrap(), "phpMyAdmin 5.2.1");
        assert_eq!(fact(&v, "Server").unwrap(), "10.4.32-MariaDB");
        assert_eq!(fact(&v, "Dumped").unwrap(), "Jan 15, 2024 at 10:22 AM");
        let users = table(&v, "shop.users");
        assert_eq!(users["rows"], 2);
        assert_eq!(users["primary_key"], json!(["id"]));
        assert_eq!(users["columns"][0]["auto"], true);
        assert_eq!(users["indexes"][0]["name"], "email");
        let posts = table(&v, "shop.posts");
        assert_eq!(posts["indexes"][0]["name"], "user_id");
        assert_eq!(posts["foreign_keys"][0]["name"], "posts_ibfk_1");
        assert_eq!(posts["foreign_keys"][0]["ref"], "shop.users");
    }

    #[test]
    fn a_dump_cut_off_mid_insert_counts_whole_rows_and_says_so() {
        let cut = MYSQL.find("(3,'cy@").unwrap() + 12;
        let v = read(&MYSQL.as_bytes()[..cut]);
        assert_eq!(v["complete"], true);
        assert_eq!(v["truncated"], true);
        assert!(v["cut"].as_str().unwrap().contains("quoted string"));
        assert_eq!(table(&v, "shop.users")["rows"], 2);
        // cut between statements: the missing footer gives it away
        let cut = MYSQL.find("DROP TABLE IF EXISTS `orders`").unwrap();
        let v = read(&MYSQL.as_bytes()[..cut]);
        assert_eq!(v["truncated"], true);
        assert!(v["notes"][0].as_str().unwrap().contains("Dump completed"));
        assert_eq!(table(&v, "shop.users")["rows"], 4);
    }

    #[test]
    fn gzip_and_a_damaged_gzip() {
        use flate2::{Compression, write::GzEncoder};
        let mut z = GzEncoder::new(Vec::new(), Compression::default());
        z.write_all(PG.as_bytes()).unwrap();
        let gz = z.finish().unwrap();
        let f = tmp(&gz, ".sql.gz");
        let v = schema(1, f.path(), None).unwrap();
        assert_eq!(v["compression"], "gzip");
        assert_eq!(v["complete"], true);
        assert_eq!(table(&v, "public.users")["rows"], 3);
        let f = tmp(&gz[..gz.len() * 2 / 3], ".sql.gz");
        let v = schema(1, f.path(), None).unwrap();
        assert_eq!(v["complete"], true);
        assert_eq!(v["truncated"], true);
    }

    #[test]
    fn not_a_dump() {
        let f = tmp(b"PGDMP\x01\x0e\x00\x04\x08\x01\x01\x01", ".dump");
        assert_eq!(
            schema(1, f.path(), None).unwrap()["format"],
            "pg_dump-custom"
        );
        let f = tmp(b"\x00\x01\x02binary", ".sql");
        assert_eq!(schema(1, f.path(), None).unwrap()["format"], "binary");
    }

    /// A read in many short slices adds up to the same as one long one, and parks in between.
    #[test]
    fn reading_in_slices_matches_reading_at_once() {
        let mut dump = String::from(MYSQL.split("-- Dump completed").next().unwrap());
        for i in 0..20_000 {
            dump.push_str(&format!(
                "INSERT INTO `orders` VALUES ({i},1,{i}.5,'note; with ''quotes'' and (parens) {}'),({},2,0,NULL);\n",
                "x".repeat(i % 300),
                i + 100_000
            ));
        }
        dump.push_str("-- Dump completed on 2024-01-15 10:22:33\n");
        let whole = read(dump.as_bytes());
        let f = tmp(dump.as_bytes(), ".sql");
        let mut p = Box::new(Parser::open(f.path()).unwrap());
        let mut slices = 0;
        let v = loop {
            slices += 1;
            if p.run(Instant::now()) {
                break p.json(true);
            }
            let token = park(7, p).unwrap();
            assert!(resume(8, &token).is_none(), "another file's token");
            p = resume(7, &token).unwrap();
        };
        assert!(slices > 2, "{slices}");
        for key in ["shop.users", "shop.orders"] {
            assert_eq!(table(&v, key)["rows"], table(&whole, key)["rows"]);
            assert_eq!(table(&v, key)["bytes"], table(&whole, key)["bytes"]);
        }
        assert_eq!(table(&v, "shop.orders")["rows"], 3 + 40_000);
        assert_eq!(v["truncated"], false);
    }
}
