//! One statement as tokens: words, quoted names and strings, numbers and punctuation, with what
//! the statement parsers need to walk them.

use super::*;

// ---- tokens of one statement

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum K {
    Word,
    Ident,
    Str,
    Num,
    Punct,
}

#[derive(Debug)]
pub(super) struct Tok {
    pub(super) k: K,
    pub(super) s: usize,
    pub(super) e: usize,
    /// the value: a word as written, an identifier or string unquoted
    pub(super) v: String,
}

impl Tok {
    pub(super) fn is(&self, kw: &str) -> bool {
        self.k == K::Word && self.v.eq_ignore_ascii_case(kw)
    }

    pub(super) fn p(&self, c: u8) -> bool {
        self.k == K::Punct && self.v.as_bytes().first() == Some(&c)
    }

    pub(super) fn named(&self) -> bool {
        matches!(self.k, K::Word | K::Ident)
    }
}

pub(super) fn kw(t: &[Tok], i: usize, k: &str) -> bool {
    t.get(i).is_some_and(|x| x.is(k))
}

pub(super) fn pt(t: &[Tok], i: usize, c: u8) -> bool {
    t.get(i).is_some_and(|x| x.p(c))
}

pub(super) fn tokenize(src: &str, bs: bool) -> Vec<Tok> {
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

pub(super) fn unquote(b: &[u8], i: usize, q: u8, bs: bool) -> (String, usize) {
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
pub(super) fn close(t: &[Tok], i: usize) -> usize {
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
pub(super) fn inner_end(t: &[Tok], i: usize, e: usize) -> usize {
    if e > i + 1 && pt(t, e - 1, b')') {
        e - 1
    } else {
        e
    }
}

/// `t[s..e]` split at its top-level commas.
pub(super) fn commas(t: &[Tok], s: usize, e: usize) -> Vec<(usize, usize)> {
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

pub(super) fn span(src: &str, t: &[Tok], s: usize, e: usize) -> String {
    if s >= e || e > t.len() {
        return String::new();
    }
    src[t[s].s..t[e - 1].e].to_string()
}

/// An expression (a default, ON UPDATE …) from `i` up to the next column attribute.
pub(super) fn expr(src: &str, t: &[Tok], i: usize, e: usize) -> (String, usize) {
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
pub(super) fn is_attr(t: &[Tok], j: usize) -> bool {
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
