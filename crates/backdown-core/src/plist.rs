//! Property lists, the files Apple software keeps its metadata in: an iPhone backup's
//! `Info.plist` and `Manifest.plist`, Time Machine's machine and snapshot records. Both forms:
//! binary (`bplist00`, what current iOS and macOS mostly write) and XML.
//!
//! Only reading, only what provenance needs: strings, numbers, dates, booleans, arrays and
//! dictionaries (data is kept as its length). Every offset and count is checked, so a damaged or
//! hostile file gives `None`, never a panic or a huge allocation.

use std::cell::Cell;
use std::collections::BTreeMap;

use quick_xml::Reader;
use quick_xml::events::Event;

#[derive(Debug, Clone, PartialEq)]
pub enum Plist {
    Str(String),
    Int(i64),
    Real(f64),
    Bool(bool),
    /// seconds since 1970 (both forms are converted)
    Date(i64),
    /// raw bytes aren't kept, only how many
    Data(usize),
    Array(Vec<Plist>),
    Dict(BTreeMap<String, Plist>),
}

impl Plist {
    /// `dict["a"]["b"]…`
    pub fn get(&self, path: &[&str]) -> Option<&Plist> {
        let mut v = self;
        for k in path {
            match v {
                Plist::Dict(d) => v = d.get(*k)?,
                _ => return None,
            }
        }
        Some(v)
    }

    /// A value as text: strings as they are, numbers and dates written out.
    pub fn text(&self, path: &[&str]) -> Option<String> {
        match self.get(path)? {
            Plist::Str(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Plist::Int(n) => Some(n.to_string()),
            Plist::Date(t) => Some(crate::fmt::day(*t)),
            _ => None,
        }
    }

    pub fn date(&self, path: &[&str]) -> Option<i64> {
        match self.get(path)? {
            Plist::Date(t) => Some(*t),
            _ => None,
        }
    }
}

/// Parse either form.
pub fn parse(bytes: &[u8]) -> Option<Plist> {
    if bytes.starts_with(b"bplist00") {
        Binary::new(bytes)?.top()
    } else {
        xml(bytes)
    }
}

// Apple's epoch (1 Jan 2001) in Unix seconds
const APPLE_EPOCH: i64 = 978_307_200;
// objects and nesting: far beyond any real metadata file
const MAX_OBJECTS: u64 = 1 << 20;
const MAX_DEPTH: usize = 64;
// objects visited while reading one file: shared references can't make it blow up
const MAX_VISITS: u64 = 1 << 20;

struct Binary<'a> {
    b: &'a [u8],
    offsets: Vec<usize>,
    ref_size: usize,
    top: u64,
    visits: Cell<u64>,
}

impl<'a> Binary<'a> {
    fn new(b: &'a [u8]) -> Option<Binary<'a>> {
        if b.len() < 8 + 32 {
            return None;
        }
        let t = &b[b.len() - 32..];
        let off_size = t[6] as usize;
        let ref_size = t[7] as usize;
        let n = be(&t[8..16]);
        let top = be(&t[16..24]);
        let table = be(&t[24..32]) as usize;
        if !(1..=8).contains(&off_size) || !(1..=8).contains(&ref_size) || n == 0 || n > MAX_OBJECTS
        {
            return None;
        }
        let end = table.checked_add(off_size.checked_mul(n as usize)?)?;
        if end > b.len() - 32 || top >= n {
            return None;
        }
        let offsets = (0..n as usize)
            .map(|i| be(&b[table + i * off_size..table + (i + 1) * off_size]) as usize)
            .collect();
        Some(Binary {
            b,
            offsets,
            ref_size,
            top,
            visits: Cell::new(0),
        })
    }

    fn top(&self) -> Option<Plist> {
        self.object(self.top, 0)
    }

    /// The length that follows a marker: its low nibble, or an int object after it when that's 0xF.
    fn len(&self, at: usize, marker: u8) -> Option<(usize, usize)> {
        let low = (marker & 0x0F) as usize;
        if low != 0x0F {
            return Some((low, at + 1));
        }
        let m = *self.b.get(at + 1)?;
        if m >> 4 != 0x1 {
            return None;
        }
        let n = 1usize << (m & 0x0F);
        let v = be(self.b.get(at + 2..at + 2 + n)?) as usize;
        Some((v, at + 2 + n))
    }

    fn object(&self, idx: u64, depth: usize) -> Option<Plist> {
        self.visits.set(self.visits.get() + 1);
        if depth > MAX_DEPTH || self.visits.get() > MAX_VISITS {
            return None;
        }
        let at = *self.offsets.get(idx as usize)?;
        let marker = *self.b.get(at)?;
        Some(match marker >> 4 {
            0x0 => match marker {
                0x08 => Plist::Bool(false),
                0x09 => Plist::Bool(true),
                _ => return None,
            },
            0x1 => {
                let n = 1usize << (marker & 0x0F);
                let raw = self.b.get(at + 1..at + 1 + n)?;
                // 1, 2 and 4 bytes are unsigned; 8 bytes signed; 16 bytes: the low 8
                Plist::Int(if n >= 8 {
                    be(&raw[n - 8..]) as i64
                } else {
                    be(raw) as i64
                })
            }
            0x2 => {
                let n = 1usize << (marker & 0x0F);
                let raw = self.b.get(at + 1..at + 1 + n)?;
                Plist::Real(match n {
                    4 => f32::from_be_bytes(raw.try_into().ok()?) as f64,
                    8 => f64::from_be_bytes(raw.try_into().ok()?),
                    _ => return None,
                })
            }
            0x3 => {
                let raw = self.b.get(at + 1..at + 9)?;
                let secs = f64::from_be_bytes(raw.try_into().ok()?);
                if !secs.is_finite() {
                    return None;
                }
                Plist::Date(APPLE_EPOCH + secs as i64)
            }
            0x4 => {
                let (n, start) = self.len(at, marker)?;
                self.b.get(start..start.checked_add(n)?)?;
                Plist::Data(n)
            }
            0x5 => {
                let (n, start) = self.len(at, marker)?;
                let raw = self.b.get(start..start.checked_add(n)?)?;
                Plist::Str(raw.iter().map(|&c| c as char).collect())
            }
            0x6 => {
                let (n, start) = self.len(at, marker)?;
                let raw = self.b.get(start..start.checked_add(n.checked_mul(2)?)?)?;
                let units: Vec<u16> = raw
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_be_bytes(*c))
                    .collect();
                Plist::Str(String::from_utf16_lossy(&units))
            }
            0x8 => {
                let n = (marker & 0x0F) as usize + 1;
                Plist::Int(be(self.b.get(at + 1..at + 1 + n)?) as i64)
            }
            0xA | 0xC => {
                let (n, start) = self.len(at, marker)?;
                let mut out = Vec::with_capacity(n.min(4096));
                for i in 0..n {
                    out.push(self.object(self.reference(start, i)?, depth + 1)?);
                }
                Plist::Array(out)
            }
            0xD => {
                let (n, start) = self.len(at, marker)?;
                let mut out = BTreeMap::new();
                for i in 0..n {
                    let key = match self.object(self.reference(start, i)?, depth + 1)? {
                        Plist::Str(s) => s,
                        _ => return None,
                    };
                    let val = self.object(self.reference(start, n.checked_add(i)?)?, depth + 1)?;
                    out.insert(key, val);
                }
                Plist::Dict(out)
            }
            _ => return None,
        })
    }

    fn reference(&self, start: usize, i: usize) -> Option<u64> {
        let at = start.checked_add(i.checked_mul(self.ref_size)?)?;
        Some(be(self.b.get(at..at + self.ref_size)?))
    }
}

fn be(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |a, &x| a << 8 | u64::from(x))
}

/// The XML form, into the same tree.
fn xml(bytes: &[u8]) -> Option<Plist> {
    let text = std::str::from_utf8(bytes).ok()?;
    // not trimmed per piece: text around a reference ("a &amp; b") keeps its spaces; each
    // value is trimmed once it's whole
    let mut r = Reader::from_str(text);
    // containers being filled, and each dictionary's pending key
    let mut stack: Vec<(Plist, Option<String>)> = Vec::new();
    let mut done: Option<Plist> = None;
    let mut leaf: Option<String> = None;
    let mut buf = String::new();
    loop {
        match r.read_event().ok()? {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                match name.as_str() {
                    "plist" => {}
                    "dict" => stack.push((Plist::Dict(BTreeMap::new()), None)),
                    "array" => stack.push((Plist::Array(Vec::new()), None)),
                    _ => {
                        leaf = Some(name);
                        buf.clear();
                    }
                }
                if stack.len() > MAX_DEPTH {
                    return None;
                }
            }
            Event::Empty(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let v = match name.as_str() {
                    "true" => Plist::Bool(true),
                    "false" => Plist::Bool(false),
                    "dict" => Plist::Dict(BTreeMap::new()),
                    "array" => Plist::Array(Vec::new()),
                    "string" => Plist::Str(String::new()),
                    _ => continue,
                };
                add(&mut stack, &mut done, v)?;
            }
            Event::Text(t) => {
                if leaf.is_some() {
                    buf.push_str(&t.decode().ok()?);
                }
            }
            // &amp; &#8217; …: quick-xml hands references over separately
            Event::GeneralRef(g) => {
                if leaf.is_some() {
                    let c = match g.resolve_char_ref() {
                        Ok(Some(c)) => c,
                        _ => match g.decode().ok()?.as_ref() {
                            "amp" => '&',
                            "lt" => '<',
                            "gt" => '>',
                            "quot" => '"',
                            "apos" => '\'',
                            _ => continue,
                        },
                    };
                    buf.push(c);
                }
            }
            Event::CData(t) => {
                if leaf.is_some() {
                    buf.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::End(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                match name.as_str() {
                    "plist" => {}
                    "dict" | "array" => {
                        let (v, _) = stack.pop()?;
                        add(&mut stack, &mut done, v)?;
                    }
                    _ => {
                        let kind = leaf.take()?;
                        let s = buf.trim().to_string();
                        if kind == "key" {
                            match stack.last_mut()? {
                                (Plist::Dict(_), k) => *k = Some(s),
                                _ => return None,
                            }
                            continue;
                        }
                        let v = match kind.as_str() {
                            "string" => Plist::Str(s),
                            "integer" => Plist::Int(s.parse().ok()?),
                            "real" => Plist::Real(s.parse().ok()?),
                            "date" => Plist::Date(iso_date(&s)?),
                            "data" => Plist::Data(s.len() * 3 / 4),
                            _ => continue,
                        };
                        add(&mut stack, &mut done, v)?;
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    done
}

fn add(stack: &mut [(Plist, Option<String>)], done: &mut Option<Plist>, v: Plist) -> Option<()> {
    match stack.last_mut() {
        None => *done = Some(v),
        Some((Plist::Array(a), _)) => a.push(v),
        Some((Plist::Dict(d), k)) => {
            d.insert(k.take()?, v);
        }
        Some(_) => return None,
    }
    Some(())
}

/// `2020-05-31T10:00:00Z` (or just the day) → Unix seconds (UTC), without a date library.
pub fn iso_date(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> { s.get(r)?.parse().ok() };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = if b.len() >= 19 {
        (num(11..13)?, num(14..16)?, num(17..19)?)
    } else {
        (0, 0, 0)
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // days from civil (Howard Hinnant)
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A binary plist, written the way Apple's tools write one: objects, offset table, trailer.
    fn bplist(objects: &[Vec<u8>], top: u64) -> Vec<u8> {
        let mut out = b"bplist00".to_vec();
        let mut offs = Vec::new();
        for o in objects {
            offs.push(out.len() as u16);
            out.extend_from_slice(o);
        }
        let table = out.len() as u64;
        for o in offs {
            out.extend_from_slice(&o.to_be_bytes());
        }
        out.extend_from_slice(&[0; 6]);
        out.push(2); // offset size
        out.push(1); // ref size
        out.extend_from_slice(&(objects.len() as u64).to_be_bytes());
        out.extend_from_slice(&top.to_be_bytes());
        out.extend_from_slice(&table.to_be_bytes());
        out
    }

    fn ascii(s: &str) -> Vec<u8> {
        let mut v = vec![0x50 | s.len() as u8];
        v.extend_from_slice(s.as_bytes());
        v
    }

    #[test]
    fn a_binary_manifest() {
        // { "Date": <2020-05-31 12:00 UTC>, "Lockdown": { "DeviceName": "Office iPad", "ProductType": "iPad7,5" }, "IsEncrypted": false }
        let date = (1_590_926_400i64 - APPLE_EPOCH) as f64;
        let mut d = vec![0x33];
        d.extend_from_slice(&date.to_be_bytes());
        let mut utf16 = vec![0x6B]; // "Office iPad" as UTF-16, 11 units
        for u in "Office iPad".encode_utf16() {
            utf16.extend_from_slice(&u.to_be_bytes());
        }
        let objects = vec![
            vec![0xD3, 1, 2, 3, 4, 5, 6], // 0: top dict, keys 1,2,3 values 4,5,6
            ascii("Date"),                // 1
            ascii("Lockdown"),            // 2
            ascii("IsEncrypted"),         // 3
            d,                            // 4
            vec![0xD2, 7, 8, 9, 10],      // 5: Lockdown dict
            vec![0x08],                   // 6: false
            ascii("DeviceName"),          // 7
            ascii("ProductType"),         // 8
            utf16,                        // 9
            ascii("iPad7,5"),             // 10
        ];
        let p = parse(&bplist(&objects, 0)).unwrap();
        assert_eq!(p.date(&["Date"]), Some(1_590_926_400));
        assert_eq!(
            p.text(&["Lockdown", "DeviceName"]).as_deref(),
            Some("Office iPad")
        );
        assert_eq!(
            p.text(&["Lockdown", "ProductType"]).as_deref(),
            Some("iPad7,5")
        );
        assert_eq!(p.get(&["IsEncrypted"]), Some(&Plist::Bool(false)));
        assert_eq!(p.text(&["Date"]).as_deref(), Some("31 May 2020"));
    }

    #[test]
    fn damaged_binary_plists_are_refused() {
        let objects = vec![vec![0xD1, 1, 2], ascii("k"), ascii("v")];
        let good = bplist(&objects, 0);
        assert!(parse(&good).is_some());
        // every truncation, and every single-byte change, fails or parses: never panics
        for n in 0..good.len() {
            let _ = parse(&good[..n]);
        }
        for i in 0..good.len() {
            for x in [0x00, 0x0F, 0xDF, 0xFF] {
                let mut bad = good.clone();
                bad[i] = x;
                let _ = parse(&bad);
            }
        }
        // a dictionary that refers to itself
        let looped = bplist(&[vec![0xD1, 1, 0], ascii("k")], 0);
        assert!(parse(&looped).is_none());
    }

    #[test]
    fn xml_plists() {
        let x = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>Device Name</key><string>Alex&#8217;s iPhone</string>
  <key>Last Backup Date</key><date>2016-05-01T10:00:00Z</date>
  <key>Nested</key><dict><key>Count</key><integer>3</integer><key>On</key><true/></dict>
  <key>List</key><array><string>a</string><string>b</string></array>
  <key>Owner</key><string>Smith &amp; Sons</string>
</dict></plist>"#;
        let p = parse(x).unwrap();
        assert_eq!(
            p.text(&["Device Name"]).as_deref(),
            Some("Alex\u{2019}s iPhone")
        );
        assert_eq!(p.date(&["Last Backup Date"]), Some(1_462_096_800));
        assert_eq!(p.text(&["Nested", "Count"]).as_deref(), Some("3"));
        assert_eq!(p.get(&["Nested", "On"]), Some(&Plist::Bool(true)));
        assert_eq!(
            p.get(&["List"]),
            Some(&Plist::Array(vec![
                Plist::Str("a".into()),
                Plist::Str("b".into())
            ]))
        );
        assert_eq!(p.text(&["Owner"]).as_deref(), Some("Smith & Sons"));
        assert!(parse(b"<plist><dict><key>k</key>").is_none());
    }
}
