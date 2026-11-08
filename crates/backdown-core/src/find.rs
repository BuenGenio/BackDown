//! Finding files and folders by name, on every indexed drive at once or under one folder, ranked:
//! the exact name first (with or without its extension), then names that start with it, then
//! names with a word that starts with it, then names that merely contain it; newer first within
//! each. Several words must all be in the name. Files that were at a path in earlier scans but
//! aren't now can be found too, with when they were last seen.
//!
//! Names are compared folded: lowercase and without accents, whichever Unicode form they were
//! written in (`fold`). `files.lname` and `dirs.lname` hold each row's folded own name, written by
//! the indexer; `add_names` adds and fills them in an index made before they existed, and indexes
//! them together with `current`, the time and the drive, so a search reads that index rather than
//! the table. On 3 million files that took 8 s once and costs about 42 bytes a file (column and
//! index); a search of every drive then takes 0.13 s, or 0.45 s for a name half of them match.
//! Rows without a name (written by an older BackDown since) are matched the slow way, from paths.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, ToSql, params, params_from_iter};
use serde::Serialize;

use crate::db::{Lock, Tx};
use crate::name_index::{self, DIRS, FILES};
use crate::{Db, browse, paths};

/// Rows named per transaction while filling in names.
const FILL_BATCH: i64 = 50_000;
/// A folder with fewer files than this is searched through its paths, a bigger one through names.
const SMALL_FOLDER: i64 = 200_000;
const MAX_TERMS: usize = 8;
/// How long a search through the names index may spend looking up the rows of the names it found:
/// past it, it answers with the best of what it has and says there's more (`Found::more`). A
/// word in hundreds of thousands of names ("jpg") would otherwise take a lookup for each.
const BUDGET: Duration = Duration::from_millis(3000);
/// A little longer when nothing's been found yet (a single letter's names take a while to rank),
/// so that it answers with its best matches rather than none.
pub const GRACE: Duration = Duration::from_millis(1000);

/// A name as search compares it: lowercase, without accents (é and e + a combining accent both
/// become e, ß becomes ss).
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if ('\u{300}'..='\u{36f}').contains(&c) {
            continue; // combining accents: names written decomposed, as older Macs store them
        }
        match plain(c) {
            Some(p) => out.push_str(p),
            None => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// The folded own name of a path.
pub fn lname(p: &Path) -> String {
    fold(
        &p.file_name()
            .map_or_else(|| p.to_string_lossy(), |n| n.to_string_lossy()),
    )
}

// Latin-1 and Latin Extended-A letters without their accents ('*': several letters, '.': itself)
const LATIN1: &[u8; 64] = b"aaaaaa*ceeeeiiiidnooooo.ouuuuy**aaaaaa*ceeeeiiiidnooooo.ouuuuy*y";
const LATIN_A: &[u8; 128] = b"aaaaaaccccccccddddeeeeeeeeeegggggggghhhhiiiiiiiiii**jjkkkllllllllllnnnnnnnnnoooooo**rrrrrrssssssssttttttuuuuuuuuuuuuwwyyyzzzzzzs";

fn plain(c: char) -> Option<&'static str> {
    const LETTERS: [&str; 26] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y", "z",
    ];
    let one = |b: u8| (b.is_ascii_lowercase()).then(|| LETTERS[usize::from(b - b'a')]);
    let u = c as u32;
    match u {
        0xC0..=0xFF => match c {
            'Æ' | 'æ' => Some("ae"),
            'Þ' | 'þ' => Some("th"),
            'ß' => Some("ss"),
            _ => one(LATIN1[(u - 0xC0) as usize]),
        },
        0x100..=0x17F => match c {
            'Ĳ' | 'ĳ' => Some("ij"),
            'Œ' | 'œ' => Some("oe"),
            _ => one(LATIN_A[(u - 0x100) as usize]),
        },
        // Cyrillic and Greek letters that decompose into a base letter and an accent
        _ => Some(match c {
            'й' | 'Й' => "и",
            'ё' | 'Ё' => "е",
            'ї' | 'Ї' => "і",
            'ў' | 'Ў' => "у",
            'ά' | 'Ά' => "α",
            'έ' | 'Έ' => "ε",
            'ή' | 'Ή' => "η",
            'ί' | 'Ί' | 'ϊ' | 'Ϊ' | 'ΐ' => "ι",
            'ό' | 'Ό' => "ο",
            'ύ' | 'Ύ' | 'ϋ' | 'Ϋ' | 'ΰ' => "υ",
            'ώ' | 'Ώ' => "ω",
            _ => return None,
        }),
    }
}

/// Add `lname` to `files` and `dirs` where it's missing, fill it in for the rows that lack it (all
/// of them once, on an index made before it existed; afterwards only rows an older BackDown wrote),
/// and index it. Each batch is committed, so an interrupted run carries on the next time.
pub(crate) fn add_names(conn: &Connection) -> Result<()> {
    for (table, time) in [("files", "mtime"), ("dirs", "newest")] {
        let has: bool = conn.query_row(
            &format!("SELECT COUNT(*) > 0 FROM pragma_table_info('{table}') WHERE name = 'lname'"),
            [],
            |r| r.get(0),
        )?;
        if !has {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN lname TEXT"))?;
        }
        let indexed = index_exists(conn, &format!("{table}_lname"))?;
        let missing = !indexed
            || conn.query_row(
                &format!(
                    "SELECT EXISTS (SELECT 1 FROM {table} INDEXED BY {table}_lname WHERE lname IS NULL)"
                ),
                [],
                |r| r.get::<_, bool>(0),
            )?;
        if missing {
            fill_names(conn, table, indexed)?;
        }
        conn.execute_batch(&format!(
            "CREATE INDEX IF NOT EXISTS {table}_lname ON {table}(lname, current, {time}, root_id)"
        ))?;
    }
    Ok(())
}

fn index_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'index' AND name = ?1",
        [name],
        |r| r.get(0),
    )?)
}

fn fill_names(conn: &Connection, table: &str, indexed: bool) -> Result<()> {
    let via = if indexed {
        format!("INDEXED BY {table}_lname")
    } else {
        String::new()
    };
    let mut read = conn.prepare(&format!(
        "SELECT id, path FROM {table} {via} WHERE lname IS NULL AND id > ?1 ORDER BY id LIMIT ?2"
    ))?;
    let mut last = 0i64;
    loop {
        let rows: Vec<(i64, Vec<u8>)> = read
            .query_map(params![last, FILL_BATCH], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let Some(&(end, _)) = rows.last() else {
            return Ok(());
        };
        let tx = Tx::begin(conn, Lock::Immediate)?;
        {
            let mut write =
                conn.prepare_cached(&format!("UPDATE {table} SET lname = ?2 WHERE id = ?1"))?;
            for (id, path) in &rows {
                write.execute(params![id, lname(&paths::from_bytes(path))])?;
            }
        }
        tx.commit()?;
        last = end;
    }
}

pub struct Query<'a> {
    pub q: &'a str,
    /// only under this folder (default: every indexed drive)
    pub within: Option<&'a Path>,
    /// files that aren't there any more, too
    pub earlier: bool,
    pub limit: usize,
}

#[derive(Debug, Serialize)]
pub struct Hit {
    pub kind: &'static str, // "dir" | "file"
    pub id: Option<i64>,
    pub name: String,
    pub path: String,
    pub key: String,
    pub dir_key: String,
    pub size: u64,
    pub mtime: i64,
    pub root: i64,
    /// the drive's name, and the folder as a path inside it ("" at its top)
    pub drive: String,
    pub folder: String,
    /// 4 the name itself, 3 starts with it, 2 a word in it starts with it, 1 has it somewhere
    pub rank: u8,
    /// not there any more: last seen at `scan`, which finished at `seen`
    pub gone: bool,
    pub seen: Option<i64>,
    pub scan: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct Found {
    pub hits: Vec<Hit>,
    /// all matches (hits are the best `limit` of them), and how many of those are gone
    pub total: u64,
    pub gone: u64,
    /// there were more matches than there was time to count: `total` is how many were counted
    pub more: bool,
    pub ms: u64,
}

/// One match before it's looked up: which row, and how it ranks.
struct Cand {
    dir: bool,
    id: i64,
    rank: u8,
    current: bool,
    time: i64,
}

pub fn find(db: &Db, q: &Query) -> Result<Found> {
    let t0 = Instant::now();
    let folded = fold(q.q);
    let terms: Vec<&str> = folded.split_whitespace().take(MAX_TERMS).collect();
    if terms.is_empty() {
        return Ok(Found {
            hits: Vec::new(),
            total: 0,
            gone: 0,
            more: false,
            ms: 0,
        });
    }
    let phrase = terms.join(" ");
    let limit = q.limit.clamp(1, 5000);
    let range = q.within.map(|d| {
        let lo = paths::dir_prefix(d);
        let mut hi = lo.clone();
        if let Some(b) = hi.last_mut() {
            *b += 1; // a separator is never 0xFF
        }
        (lo, hi)
    });
    let conn = &db.conn;
    let named = index_exists(conn, "files_lname")? && index_exists(conn, "dirs_lname")?;
    let mut cands = Vec::new();
    let (mut total, mut gone, mut more) = (0u64, 0u64, false);
    // through the names index when it's ready: every drive, one drive, or a big folder (a small
    // folder is quicker through its paths)
    if named && !small_folder(conn, q)? && name_index::ready(conn, name_index::LOCAL)? {
        (cands, total, gone, more) =
            by_names(conn, &terms, &phrase, &range, q, limit, t0 + BUDGET)?;
    } else if named {
        for dir in [false, true] {
            let (c, n, g) = ranked(conn, dir, &terms, &phrase, &range, q, limit)?;
            cands.extend(c);
            total += n;
            gone += g;
        }
    }
    let (c, n) = unnamed(conn, &terms, &phrase, &range, named, limit)?;
    cands.extend(c);
    total += n;
    best_first(&mut cands);
    cands.truncate(limit);
    let hits = describe(conn, &cands)?;
    Ok(Found {
        hits,
        total,
        gone,
        more,
        ms: t0.elapsed().as_millis() as u64,
    })
}

/// Best rank first; there, what's still there first, then the newest.
fn best_first(cands: &mut [Cand]) {
    cands.sort_by(|a, b| {
        b.rank
            .cmp(&a.rank)
            .then(b.current.cmp(&a.current))
            .then(b.time.cmp(&a.time))
            .then(a.dir.cmp(&b.dir))
            .then(a.id.cmp(&b.id))
    });
}

/// Whether the search is of a folder (not a whole drive) small enough to read through its paths.
fn small_folder(conn: &Connection, q: &Query) -> Result<bool> {
    let Some(w) = q.within else {
        return Ok(false);
    };
    let at = paths::to_bytes(w);
    let drive: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM roots WHERE path = ?1)",
        [&at],
        |r| r.get(0),
    )?;
    if drive {
        return Ok(false);
    }
    Ok(conn
        .query_row(
            "SELECT files FROM dirs WHERE path = ?1 AND current = 1",
            [&at],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_none_or(|n| n < SMALL_FOLDER))
}

/// The rows of a file that's gone count too when no current row has its path and no later one
/// does either (the same file isn't listed twice).
const ALIVE_EARLIER: &str =
    "(f.current = 1 OR (NOT EXISTS (SELECT 1 FROM files c WHERE c.path = f.path AND c.current = 1)
       AND NOT EXISTS (SELECT 1 FROM files n WHERE n.path = f.path AND n.last_scan > f.last_scan)))";

/// The best matches through the names index (name_index.rs): the names that match, ranked, then
/// each one's rows from its table's own name index, best names first, until `limit` are found and
/// the rest are counted, or `until` passes. (candidates, matches, gone matches, more than counted)
fn by_names(
    conn: &Connection,
    terms: &[&str],
    phrase: &str,
    range: &Option<(Vec<u8>, Vec<u8>)>,
    q: &Query,
    limit: usize,
    until: Instant,
) -> Result<(Vec<Cand>, u64, u64, bool)> {
    let mut tiers: [Vec<(String, i64)>; 5] = Default::default();
    // reading every name (a term too short for trigrams) gets a third of the time, the rows the rest
    let part = Instant::now() + until.saturating_duration_since(Instant::now()) / 3;
    let (names, all_names) = name_index::matching(conn, terms, FILES | DIRS, part)?;
    for (name, has) in names {
        tiers[usize::from(rank_of(&name, phrase))].push((name, has));
    }
    let drive: Option<i64> = match q.within {
        Some(w) => conn
            .query_row(
                "SELECT id FROM roots WHERE path = ?1",
                [paths::to_bytes(w)],
                |r| r.get(0),
            )
            .optional()?,
        None => None,
    };
    // per table: whether it's folders, its bit in names.has, the rows of a name (?1, then the
    // scope, then the limit), how many there are, and the scope's values
    let mut tables = Vec::new();
    for (dir, table, time, bit) in [
        (false, "files", "mtime", FILES),
        (true, "dirs", "newest", DIRS),
    ] {
        let alive = if q.earlier && !dir {
            ALIVE_EARLIER
        } else {
            "f.current = 1"
        };
        let mut scope: Vec<Box<dyn ToSql>> = Vec::new();
        let within = if let Some(root) = drive {
            scope.push(Box::new(root));
            " AND f.root_id = ?2"
        } else if let Some((lo, hi)) = range {
            scope.push(Box::new(lo.clone()));
            scope.push(Box::new(hi.clone()));
            " AND f.path >= ?2 AND f.path < ?3"
        } else {
            ""
        };
        let rows = conn.prepare(&format!(
            "SELECT id, current, {time} FROM {table} f INDEXED BY {table}_lname
             WHERE f.lname = ?1 AND {alive}{within} ORDER BY current DESC, {time} DESC LIMIT ?{}",
            scope.len() + 2
        ))?;
        let count = conn.prepare(&format!(
            "SELECT COUNT(*), COALESCE(SUM(current = 0), 0) FROM {table} f INDEXED BY {table}_lname
             WHERE f.lname = ?1 AND {alive}{within}"
        ))?;
        tables.push((dir, bit, rows, count, scope));
    }
    let keep = limit as i64 + 1;
    let (mut cands, mut total, mut gone, mut more) = (Vec::new(), 0u64, 0u64, !all_names);
    'tiers: for rank in (1..=4u8).rev() {
        // a worse rank only adds to the count once there are enough of the better ones
        let collect = cands.len() < limit;
        for (name, has) in &tiers[usize::from(rank)] {
            let now = Instant::now();
            if now > until && (!cands.is_empty() || now > until + GRACE) {
                more = true;
                break 'tiers;
            }
            for (dir, bit, rows, count, scope) in &mut tables {
                if *has & *bit == 0 {
                    continue;
                }
                let mut args: Vec<&dyn ToSql> = vec![name];
                args.extend(scope.iter().map(|a| a.as_ref() as &dyn ToSql));
                let mut counted = |args: &[&dyn ToSql]| -> Result<()> {
                    let (n, g): (i64, i64) =
                        count.query_row(params_from_iter(args), |r| Ok((r.get(0)?, r.get(1)?)))?;
                    total += n as u64;
                    gone += g as u64;
                    Ok(())
                };
                if !collect {
                    counted(&args)?;
                    continue;
                }
                let mut with_limit = args.clone();
                with_limit.push(&keep);
                let found: Vec<Cand> = rows
                    .query_map(params_from_iter(with_limit), |r| {
                        Ok(Cand {
                            dir: *dir,
                            id: r.get(0)?,
                            current: r.get(1)?,
                            time: r.get(2)?,
                            rank,
                        })
                    })?
                    .collect::<rusqlite::Result<_>>()?;
                if found.len() as i64 >= keep {
                    counted(&args)?; // more of this name than are kept: count them all
                } else {
                    total += found.len() as u64;
                    gone += found.iter().filter(|c| !c.current).count() as u64;
                }
                cands.extend(found);
            }
            if cands.len() > limit * 4 {
                best_first(&mut cands);
                cands.truncate(limit);
            }
        }
    }
    Ok((cands, total, gone, more))
}

/// The rank of a folded name, as `ranked` computes it in SQL (for rows matched outside SQL).
fn rank_of(name: &str, phrase: &str) -> u8 {
    if name == phrase
        || name
            .strip_prefix(phrase)
            .is_some_and(|rest| rest.starts_with('.'))
    {
        4
    } else if name.starts_with(phrase) {
        3
    } else if name
        .match_indices(phrase)
        .any(|(i, _)| i > 0 && matches!(name.as_bytes()[i - 1], b' ' | b'_' | b'-' | b'.'))
    {
        2
    } else {
        1
    }
}

const RANK: &str = "CASE WHEN lname = ?1 OR substr(lname, 1, ?2 + 1) = ?1 || '.' THEN 4
     WHEN substr(lname, 1, ?2) = ?1 THEN 3
     WHEN instr(lname, ' ' || ?1) > 0 OR instr(lname, '_' || ?1) > 0 OR instr(lname, '-' || ?1) > 0
       OR instr(lname, '.' || ?1) > 0 THEN 2
     ELSE 1 END";

/// The best matches of one table through the names index: (candidates, matches, gone matches).
fn ranked(
    conn: &Connection,
    dir: bool,
    terms: &[&str],
    phrase: &str,
    range: &Option<(Vec<u8>, Vec<u8>)>,
    q: &Query,
    limit: usize,
) -> Result<(Vec<Cand>, u64, u64)> {
    let (table, time) = if dir {
        ("dirs", "newest")
    } else {
        ("files", "mtime")
    };
    let earlier = q.earlier && !dir;
    // ?1 the phrase, ?2 its length in characters, ?3 the limit, then the terms, then the range
    let mut args: Vec<Box<dyn ToSql>> = vec![
        Box::new(phrase.to_string()),
        Box::new(phrase.chars().count() as i64),
        Box::new(limit as i64 + 1),
    ];
    let mut cond = Vec::new();
    for t in terms {
        args.push(Box::new(t.to_string()));
        cond.push(format!("instr(lname, ?{}) > 0", args.len()));
    }
    // a whole drive is read through the names (its root id is in that index too), a small folder
    // through its paths, and a big one through the names, checking the path of each match
    let mut via = format!("INDEXED BY {table}_lname");
    let drive: Option<i64> = match q.within {
        Some(w) => conn
            .query_row(
                "SELECT id FROM roots WHERE path = ?1",
                [paths::to_bytes(w)],
                |r| r.get(0),
            )
            .optional()?,
        None => None,
    };
    if let Some(root) = drive {
        args.push(Box::new(root));
        cond.push(format!("root_id = ?{}", args.len()));
    } else if let Some((lo, hi)) = range {
        args.push(Box::new(lo.clone()));
        args.push(Box::new(hi.clone()));
        cond.push(format!(
            "path >= ?{} AND path < ?{}",
            args.len() - 1,
            args.len()
        ));
        let small = !dir
            && q.within.is_some_and(|w| {
                conn.query_row(
                    "SELECT files FROM dirs WHERE path = ?1 AND current = 1",
                    [paths::to_bytes(w)],
                    |r| r.get::<_, i64>(0),
                )
                .optional()
                .ok()
                .flatten()
                .is_some_and(|n| n < SMALL_FOLDER)
            });
        if small {
            via = if earlier {
                "INDEXED BY files_path_history".into()
            } else {
                "INDEXED BY files_current_path".into()
            };
        }
    }
    let alive = if earlier {
        ALIVE_EARLIER
    } else {
        "f.current = 1"
    };
    let filter = format!("{} AND {alive}", cond.join(" AND "));
    let sql = format!(
        "SELECT id, current, {time}, {RANK} AS rank FROM {table} f {via} WHERE {filter}
         ORDER BY rank DESC, current DESC, {time} DESC LIMIT ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter()), |r| {
        Ok(Cand {
            dir,
            id: r.get(0)?,
            current: r.get(1)?,
            time: r.get(2)?,
            rank: r.get::<_, i64>(3)? as u8,
        })
    })?;
    let cands: Vec<Cand> = rows.collect::<rusqlite::Result<_>>()?;
    if cands.len() <= limit {
        let (n, gone) = (cands.len(), cands.iter().filter(|c| !c.current).count());
        return Ok((cands, n as u64, gone as u64));
    }
    // more than fit: count them all (the rank isn't needed for that)
    let count = format!(
        "SELECT COUNT(*), COALESCE(SUM(current = 0), 0) FROM {table} f {via} WHERE {filter}"
    );
    let sql_args: Vec<&dyn ToSql> = args.iter().skip(3).map(|a| a.as_ref()).collect();
    // the count has no ?1..?3: renumber the rest from ?1
    let count = renumber(&count, 3);
    let (n, g): (i64, i64) = conn.query_row(&count, params_from_iter(sql_args), |r| {
        Ok((r.get(0)?, r.get(1)?))
    })?;
    Ok((cands, n as u64, g as u64))
}

/// `?N` → `?(N - by)` in `sql`.
fn renumber(sql: &str, by: usize) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '?' {
            let mut n = String::new();
            while let Some(d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                n.push(*d);
                chars.next();
            }
            if let Ok(v) = n.parse::<usize>() {
                out.push_str(&(v - by).to_string());
            }
        }
    }
    out
}

/// Current rows with no name yet (with the names index), or all of them (an index still without
/// it), matched from their paths: (candidates, matches).
fn unnamed(
    conn: &Connection,
    terms: &[&str],
    phrase: &str,
    range: &Option<(Vec<u8>, Vec<u8>)>,
    named: bool,
    limit: usize,
) -> Result<(Vec<Cand>, u64)> {
    let mut cands = Vec::new();
    let mut total = 0u64;
    for (dir, table, time) in [(false, "files", "mtime"), (true, "dirs", "newest")] {
        let which = if named {
            format!("INDEXED BY {table}_lname WHERE lname IS NULL AND")
        } else {
            "WHERE".to_string()
        };
        let mut sql = format!("SELECT id, path, {time} FROM {table} {which} current = 1");
        let mut args: Vec<Box<dyn ToSql>> = Vec::new();
        if let Some((lo, hi)) = range {
            args.push(Box::new(lo.clone()));
            args.push(Box::new(hi.clone()));
            sql.push_str(&format!(
                " AND path >= ?{} AND path < ?{}",
                args.len() - 1,
                args.len()
            ));
        }
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter()), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (id, path, t) = row?;
            let name = lname(&paths::from_bytes(&path));
            if !terms.iter().all(|x| name.contains(x)) {
                continue;
            }
            total += 1;
            cands.push(Cand {
                dir,
                id,
                rank: rank_of(&name, phrase),
                current: true,
                time: t,
            });
        }
    }
    cands.sort_by_key(|c| std::cmp::Reverse((c.rank, c.time)));
    cands.truncate(limit);
    Ok((cands, total))
}

/// The hits for the chosen candidates: paths, drives, and when gone ones were last seen.
fn describe(conn: &Connection, cands: &[Cand]) -> Result<Vec<Hit>> {
    let mut roots: HashMap<i64, PathBuf> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT id, path FROM roots")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
        for row in rows {
            let (id, p) = row?;
            roots.insert(id, paths::from_bytes(&p));
        }
    }
    let mut file = conn.prepare(
        "SELECT path, size, mtime, root_id, first_scan, last_scan FROM files WHERE id = ?1",
    )?;
    let mut dir = conn.prepare("SELECT path, bytes, newest, root_id FROM dirs WHERE id = ?1")?;
    let mut seen = conn.prepare(
        "SELECT id, finished_at FROM scans WHERE root_id = ?1 AND finished_at IS NOT NULL
         AND id BETWEEN ?2 AND ?3 ORDER BY id DESC LIMIT 1",
    )?;
    let mut hits = Vec::with_capacity(cands.len());
    for c in cands {
        let (path, size, mtime, root, span) = if c.dir {
            let (p, b, n, r): (Vec<u8>, i64, i64, i64) =
                dir.query_row([c.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            (p, b, n, r, None)
        } else {
            let (p, s, m, r, a, b): (Vec<u8>, i64, i64, i64, i64, i64) =
                file.query_row([c.id], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })?;
            (p, s, m, r, Some((a, b)))
        };
        let last = match span.filter(|_| !c.current) {
            Some((a, b)) => seen
                .query_row(params![root, a, b], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                })
                .optional()?,
            None => None,
        };
        let (scan, when) = (last.map(|x| x.0), last.map(|x| x.1));
        let p = paths::from_bytes(&path);
        let parent = p.parent().unwrap_or(&p);
        let top = roots.get(&root);
        let drive = top
            .and_then(|t| t.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .or_else(|| top.map(|t| t.display().to_string()))
            .unwrap_or_default();
        let folder = top
            .and_then(|t| parent.strip_prefix(t).ok())
            .map(|r| r.display().to_string())
            .unwrap_or_else(|| parent.display().to_string());
        hits.push(Hit {
            kind: if c.dir { "dir" } else { "file" },
            id: (!c.dir).then_some(c.id),
            name: p.file_name().map_or_else(
                || p.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
            path: p.display().to_string(),
            key: browse::key(&p),
            dir_key: browse::key(parent),
            size: size as u64,
            mtime,
            root,
            drive,
            folder,
            rank: c.rank,
            gone: !c.current,
            seen: when,
            scan,
        });
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding() {
        assert_eq!(fold("Résumé.PDF"), "resume.pdf");
        assert_eq!(fold("Re\u{301}sume\u{301}.pdf"), "resume.pdf");
        assert_eq!(fold("Straße Łódź Ærø"), "strasse lodz aero");
        assert_eq!(fold("Účtenky Šťastný"), "uctenky stastny");
        assert_eq!(fold("Йогурт"), fold("И\u{306}огурт"));
        assert_eq!(fold("ΆΛΦΑ"), "αλφα");
        assert_eq!(fold("日本語"), "日本語");
        assert_eq!(lname(Path::new("/a/B/Ça Va.TXT")), "ca va.txt");
    }

    #[test]
    fn ranks() {
        assert_eq!(rank_of("report", "report"), 4);
        assert_eq!(rank_of("report.pdf", "report"), 4);
        assert_eq!(rank_of("reports 2019", "report"), 3);
        assert_eq!(rank_of("annual report.docx", "report"), 2);
        assert_eq!(rank_of("unreported.txt", "report"), 1);
    }

    #[test]
    fn renumbering() {
        assert_eq!(renumber("a ?4 AND ?12 ?", 3), "a ?1 AND ?9 ?");
    }
}
