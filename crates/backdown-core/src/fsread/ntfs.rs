//! NTFS: the boot sector, the MFT (found through its own record 0), records with their update
//! sequence fixups and attribute lists, file data (resident, non-resident, sparse, LZNT1
//! compressed; encrypted files are recognised and refused), folders by walking their $I30 index
//! B-tree ($INDEX_ROOT and the INDX blocks of $INDEX_ALLOCATION), and reparse points (symbolic
//! links and junctions shown, not followed). The metadata files ($MFT, $LogFile…) are left out of
//! listings, as Windows does. $LogFile isn't replayed.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek};

use anyhow::{Context, Result, bail};

use super::undelete::{Deleted, Spans, Verdict};
use super::{
    DIR_CAP, Data, Entry, Info, MAX_RUNS, META_CAP, Node, Region, Run, clean_name, filetime,
    push_run, read_all, read_mapped, utf16le,
};
use crate::magic::le;

/// The root folder's MFT record.
pub(crate) const ROOT: u64 = 5;
/// Records below this are the filesystem's own metadata.
const FIRST_USER: u64 = 16;

const AT_STANDARD_INFORMATION: u32 = 0x10;
const AT_ATTRIBUTE_LIST: u32 = 0x20;
const AT_FILE_NAME: u32 = 0x30;
const AT_VOLUME_NAME: u32 = 0x60;
const AT_VOLUME_INFORMATION: u32 = 0x70;
const AT_DATA: u32 = 0x80;
const AT_INDEX_ROOT: u32 = 0x90;
const AT_INDEX_ALLOCATION: u32 = 0xA0;
const AT_REPARSE_POINT: u32 = 0xC0;

const ATTR_COMPRESSED: u16 = 0x0001;
const ATTR_ENCRYPTED: u16 = 0x4000;

const FILE_HIDDEN: u64 = 0x2;
const FILE_SYSTEM: u64 = 0x4;
const FILE_SPARSE: u64 = 0x200;
const FILE_REPARSE: u64 = 0x400;
const FILE_COMPRESSED: u64 = 0x800;
const FILE_ENCRYPTED: u64 = 0x4000;
const FILE_DIRECTORY: u64 = 0x1000_0000;

/// A file's records: at most this many extension records through an attribute list.
const MAX_EXTENSIONS: usize = 4096;
/// A folder index deeper than this is damaged.
const MAX_INDEX_DEPTH: u32 = 32;

pub(crate) struct Ntfs {
    cluster: u64,
    clusters: u64,
    rec: u64,
    /// Where the MFT's own bytes are.
    mft: Vec<Run>,
    records: u64,
    /// $Bitmap's map and length (which clusters are in use), once it's needed; None inside when
    /// it can't be read.
    bitmap: Option<Option<(Vec<Run>, u64)>>,
    bits: HashMap<u64, Vec<u8>>,
    /// The records not in use, by folder, once they're needed.
    scan: Option<Scan>,
}

/// What a look through the MFT for records no longer in use found.
struct Scan {
    by_parent: HashMap<u64, Vec<Gone>>,
    /// For every record looked at: its sequence number, and whether it's in use and a folder
    /// (`HEAD_*`); 0 for one that isn't a valid record.
    heads: Vec<u32>,
    note: Option<String>,
}

const HEAD_VALID: u32 = 1 << 16;
const HEAD_IN_USE: u32 = 1 << 17;
const HEAD_DIR: u32 = 1 << 18;

/// A deleted file or folder's record, as the MFT scan found it.
#[derive(Debug, Clone)]
struct Gone {
    record: u64,
    parent_seq: u16,
    name: String,
    dir: bool,
    size: u64,
    mtime: Option<i64>,
    attr: u64,
}

/// The most MFT records a look for deleted files goes through (4 GiB of MFT).
const MAX_SCAN: u64 = 4 << 20;
/// ...and the most deleted records it keeps.
const MAX_GONE: usize = 2_000_000;

pub(crate) fn detect(b: &[u8]) -> bool {
    b.len() >= 512 && &b[3..11] == b"NTFS    "
}

/// One attribute of a record.
struct Attr {
    kind: u32,
    name: String,
    flags: u16,
    body: Body,
}

enum Body {
    Resident(Vec<u8>),
    NonResident {
        start: u64,
        /// (first VCN, clusters, LCN or None for a hole)
        runs: Vec<(u64, u64, Option<u64>)>,
        unit: u8,
        size: u64,
        init: u64,
    },
}

/// A stream (an attribute's value, across the records it's split over).
enum Stream {
    Resident(Vec<u8>),
    NonResident {
        runs: Vec<Run>,
        size: u64,
        init: u64,
        flags: u16,
        unit: u8,
    },
}

/// A record's size as the boot sector gives it: clusters, or (negative) a power of two in bytes.
fn record_size(v: u8, cluster: u64) -> u64 {
    match v as i8 {
        n if n < 0 => 1u64.checked_shl(u32::from(n.unsigned_abs())).unwrap_or(0),
        n => n as u64 * cluster,
    }
}

/// Undoes the update sequence (the last two bytes of each 512-byte stride stand in for what's
/// kept in an array), after checking the record's signature.
fn fixup(b: &mut [u8], magic: &[u8; 4], what: &dyn Fn() -> String) -> Result<()> {
    if &b[..4] != magic {
        if &b[..4] == b"BAAD" {
            bail!("{} is marked damaged", what());
        }
        bail!("{} isn't a valid record (damaged)", what());
    }
    let (usa, count) = (le(b, 4, 2) as usize, le(b, 6, 2) as usize);
    if count == 0 || usa + 2 * count > b.len() || (count - 1) * 512 > b.len() {
        bail!("{} has a damaged update sequence", what());
    }
    let usn = [b[usa], b[usa + 1]];
    for i in 1..count {
        let end = i * 512;
        if b[end - 2..end] != usn {
            bail!("{} was torn by an interrupted write (damaged)", what());
        }
        b[end - 2] = b[usa + 2 * i];
        b[end - 1] = b[usa + 2 * i + 1];
    }
    Ok(())
}

/// A non-resident attribute's run list: (first VCN, clusters, LCN or None for a hole).
pub(crate) fn decode_runs(b: &[u8], start: u64) -> Result<Vec<(u64, u64, Option<u64>)>> {
    let (mut out, mut i, mut vcn, mut lcn) = (Vec::new(), 0usize, start, 0i64);
    while i < b.len() && b[i] != 0 {
        let (ln, on) = (usize::from(b[i] & 0xF), usize::from(b[i] >> 4));
        if ln == 0 || ln > 8 || on > 8 || i + 1 + ln + on > b.len() {
            bail!("a damaged run list");
        }
        let len = le(b, i + 1, ln);
        if len == 0 {
            bail!("a damaged run list (an empty run)");
        }
        let phys = if on == 0 {
            None // sparse
        } else {
            let shift = 64 - 8 * on as u32;
            let delta = ((le(b, i + 1 + ln, on) << shift) as i64) >> shift;
            lcn = lcn
                .checked_add(delta)
                .filter(|&l| l >= 0)
                .context("a damaged run list (a run before the volume's start)")?;
            Some(lcn as u64)
        };
        if out.len() >= MAX_RUNS {
            bail!("the file is in more than {MAX_RUNS} pieces: too fragmented to read here");
        }
        out.push((vcn, len, phys));
        vcn = vcn
            .checked_add(len)
            .context("a damaged run list (runs past the largest file)")?;
        i += 1 + ln + on;
    }
    Ok(out)
}

/// The attributes of one (fixed-up) record.
fn attrs_of(rec: &[u8], n: u64) -> Result<Vec<Attr>> {
    let used = (le(rec, 24, 4) as usize).min(rec.len());
    let (mut a, mut out) = (le(rec, 20, 2) as usize, Vec::new());
    while a + 16 <= used {
        let kind = le(rec, a, 4) as u32;
        if kind == 0xFFFF_FFFF {
            break;
        }
        let len = le(rec, a + 4, 4) as usize;
        if len < 16 || a + len > used {
            bail!("MFT record {n} has a damaged attribute");
        }
        let x = &rec[a..a + len];
        let (nlen, noff) = (usize::from(x[9]), le(x, 10, 2) as usize);
        let name = x
            .get(noff..noff + 2 * nlen)
            .map(utf16le)
            .unwrap_or_default();
        let body = if x[8] == 0 {
            let (vlen, voff) = (le(x, 16, 4) as usize, le(x, 20, 2) as usize);
            let v = x
                .get(voff..voff + vlen)
                .with_context(|| format!("MFT record {n} has a damaged attribute"))?;
            Body::Resident(v.to_vec())
        } else {
            if len < 64 {
                bail!("MFT record {n} has a damaged attribute");
            }
            let start = le(x, 16, 8);
            let runs = x
                .get(le(x, 32, 2) as usize..)
                .map(|b| decode_runs(b, start))
                .transpose()?
                .unwrap_or_default();
            Body::NonResident {
                start,
                runs,
                unit: x[34],
                size: le(x, 48, 8),
                init: le(x, 56, 8),
            }
        };
        out.push(Attr {
            kind,
            name,
            flags: le(x, 12, 2) as u16,
            body,
        });
        a += len;
    }
    Ok(out)
}

/// A symbolic link's or junction's target, or why a reparse point makes the file unreadable here.
enum Reparse {
    Link(String),
    Unreadable(&'static str),
    Other,
}

fn reparse(v: &[u8]) -> Reparse {
    let tag = le(v, 0, 4);
    let names = |buf: usize| {
        let (so, sl, po, pl) = (
            le(v, 8, 2) as usize,
            le(v, 10, 2) as usize,
            le(v, 12, 2) as usize,
            le(v, 14, 2) as usize,
        );
        let get = |o: usize, l: usize| v.get(buf + o..buf + o + l).map(utf16le);
        let t = get(po, pl)
            .filter(|s| !s.is_empty())
            .or_else(|| get(so, sl))
            .unwrap_or_default();
        let t = t.strip_prefix("\\??\\").map(str::to_string).unwrap_or(t);
        if t.is_empty() {
            Reparse::Other
        } else {
            Reparse::Link(t)
        }
    };
    match tag {
        0xA000_000C => names(20),
        0xA000_0003 => names(16),
        0x8000_0017 => Reparse::Unreadable(
            "compressed by Windows (CompactOS / WOF): that kind of compression can't be read here",
        ),
        0x8000_0013 => Reparse::Unreadable(
            "deduplicated by Windows Server: its contents are kept in a store elsewhere on the volume",
        ),
        _ => Reparse::Other,
    }
}

/// LZNT1: `out_len` bytes from compressed chunks of up to 4 KiB each.
pub(crate) fn lznt1(input: &[u8], out_len: usize) -> Result<Vec<u8>> {
    const CHUNK: usize = 4096;
    let mut out = Vec::with_capacity(out_len + CHUNK);
    let mut i = 0;
    while i + 2 <= input.len() && out.len() < out_len {
        let h = le(input, i, 2) as usize;
        if h == 0 {
            break;
        }
        let (start, end) = (i + 2, i + 2 + (h & 0xFFF) + 1);
        if end > input.len() {
            bail!("damaged compressed data (a chunk runs past its unit)");
        }
        let base = out.len();
        if h & 0x8000 == 0 {
            out.extend_from_slice(&input[start..end]); // stored as is
        } else {
            let mut p = start;
            'chunk: while p < end {
                let tag = input[p];
                p += 1;
                for bit in 0..8 {
                    if p >= end {
                        break 'chunk;
                    }
                    if (tag >> bit) & 1 == 0 {
                        out.push(input[p]);
                        p += 1;
                        continue;
                    }
                    if p + 2 > end {
                        bail!("damaged compressed data (a cut-off back reference)");
                    }
                    let pos = out.len() - base;
                    if pos == 0 {
                        bail!("damaged compressed data (a back reference at a chunk's start)");
                    }
                    // the split between offset and length bits moves as the chunk fills up
                    let mut lg = 0;
                    let mut k = pos - 1;
                    while k >= 0x10 {
                        lg += 1;
                        k >>= 1;
                    }
                    let t = le(input, p, 2) as usize;
                    p += 2;
                    let (back, len) = ((t >> (12 - lg)) + 1, (t & (0xFFF >> lg)) + 3);
                    if back > pos || pos + len > CHUNK {
                        bail!("damaged compressed data (a back reference out of range)");
                    }
                    for _ in 0..len {
                        out.push(out[out.len() - back]);
                    }
                }
            }
        }
        if out.len() - base > CHUNK {
            bail!("damaged compressed data (a chunk larger than 4 KiB)");
        }
        out.resize(base + CHUNK, 0); // a short chunk is followed by zeros
        i = end;
    }
    out.resize(out_len, 0);
    Ok(out)
}

/// A folder walk's state: the index blocks, what's been visited, and where entries go.
struct Walk<'a, 'b> {
    alloc: Option<Vec<Run>>,
    block: u64,
    vcn_unit: u64,
    seen: HashSet<u64>,
    want: Option<&'a str>,
    each: &'b mut dyn FnMut(Entry) -> bool,
}

impl Ntfs {
    pub(crate) fn open<R: Read + Seek>(r: &mut Region<R>, b: &[u8]) -> Result<(Ntfs, Info)> {
        let bps = le(b, 11, 2);
        if !matches!(bps, 256 | 512 | 1024 | 2048 | 4096) {
            bail!("the NTFS boot sector's sector size ({bps}) is out of range (damaged)");
        }
        let spc = match b[13] {
            n @ 1..=0x80 => u64::from(n),
            n => 1u64.checked_shl(256 - u32::from(n)).unwrap_or(0),
        };
        let cluster = bps * spc;
        if !cluster.is_power_of_two() || cluster > 2 << 20 {
            bail!("the NTFS boot sector's cluster size is out of range (damaged)");
        }
        let rec = record_size(b[64], cluster);
        if !rec.is_power_of_two() || !(512..=65536).contains(&rec) {
            bail!("the NTFS boot sector's MFT record size is out of range (damaged)");
        }
        let size = le(b, 40, 8).saturating_mul(bps);
        let mft_at = le(b, 48, 8)
            .checked_mul(cluster)
            .filter(|&a| a < r.len())
            .context("the NTFS boot sector puts the MFT outside the volume (damaged)")?;
        let mut rec0 = r
            .read_vec(mft_at, rec)
            .context("can't read the MFT's first record")?;
        fixup(&mut rec0, b"FILE", &|| "the MFT's first record".into())?;
        let mut ntfs = Ntfs {
            cluster,
            clusters: size / cluster + 1,
            rec,
            mft: Vec::new(),
            records: 1,
            bitmap: None,
            bits: HashMap::new(),
            scan: None,
        };
        // the MFT's own map: record 0's $DATA, and (on a very fragmented MFT) the rest of it in
        // the extension records its attribute list names, which the first part of the map reaches
        let attrs = attrs_of(&rec0, 0)?;
        let Some(Stream::NonResident {
            runs, size: bytes, ..
        }) = ntfs.stream(&attrs, AT_DATA, "")?
        else {
            bail!("the MFT has no map of itself (damaged)");
        };
        ntfs.mft = runs;
        ntfs.records = (bytes / rec).max(1);
        if attrs.iter().any(|a| a.kind == AT_ATTRIBUTE_LIST) {
            let all = ntfs.extended(r, 0, attrs)?;
            if let Some(Stream::NonResident { runs, .. }) = ntfs.stream(&all, AT_DATA, "")? {
                ntfs.mft = runs;
            }
        }
        let mut notes = Vec::new();
        let mut label = None;
        if let Ok((_, attrs)) = ntfs.attrs(r, 3) {
            for a in &attrs {
                match (a.kind, &a.body) {
                    (AT_VOLUME_NAME, Body::Resident(v)) => {
                        label = Some(utf16le(v)).filter(|l| !l.is_empty());
                    }
                    (AT_VOLUME_INFORMATION, Body::Resident(v)) if le(v, 10, 2) & 1 != 0 => {
                        notes.push("it wasn't shut down cleanly (marked dirty): the newest changes may be missing (its log isn't replayed here)".into());
                    }
                    _ => {}
                }
            }
        }
        if size > r.len() {
            notes.push(format!(
                "the filesystem says it's {size} bytes but only {} are here: files near its end may not open",
                r.len()
            ));
        }
        let info = Info {
            kind: "NTFS".into(),
            label,
            size,
            block: cluster,
            notes,
        };
        Ok((ntfs, info))
    }

    /// MFT record `n`, fixed up.
    fn record<R: Read + Seek>(&self, r: &mut Region<R>, n: u64) -> Result<Vec<u8>> {
        if n >= self.records {
            bail!("MFT record {n} is out of range (damaged)");
        }
        let mut b = vec![0u8; self.rec as usize];
        read_mapped(r, &self.mft, n * self.rec, &mut b)?;
        fixup(&mut b, b"FILE", &|| format!("MFT record {n}"))?;
        Ok(b)
    }

    /// Record `n` and all its attributes, including those an attribute list puts in other records.
    fn attrs<R: Read + Seek>(&self, r: &mut Region<R>, n: u64) -> Result<(Vec<u8>, Vec<Attr>)> {
        let rec = self.record(r, n)?;
        let attrs = attrs_of(&rec, n)?;
        if !attrs.iter().any(|a| a.kind == AT_ATTRIBUTE_LIST) {
            return Ok((rec, attrs));
        }
        let all = self.extended(r, n, attrs)?;
        Ok((rec, all))
    }

    fn extended<R: Read + Seek>(
        &self,
        r: &mut Region<R>,
        n: u64,
        mut attrs: Vec<Attr>,
    ) -> Result<Vec<Attr>> {
        let list = match self.stream(&attrs, AT_ATTRIBUTE_LIST, "")? {
            Some(Stream::Resident(v)) => v,
            Some(Stream::NonResident { runs, size, .. }) => {
                let data = Data::Runs {
                    runs,
                    size,
                    valid: size,
                };
                read_all(r, data, META_CAP)?
            }
            None => return Ok(attrs),
        };
        let mut others = Vec::new();
        let mut at = 0;
        while at + 26 <= list.len() {
            let len = le(&list, at + 4, 2) as usize;
            if len < 26 {
                break;
            }
            let m = le(&list, at + 16, 8) & 0xFFFF_FFFF_FFFF;
            if m != n && !others.contains(&m) {
                if others.len() >= MAX_EXTENSIONS {
                    bail!("MFT record {n}'s attribute list is implausibly long (damaged)");
                }
                others.push(m);
            }
            at += len;
        }
        for m in others {
            let x = self.record(r, m)?;
            if le(&x, 32, 8) & 0xFFFF_FFFF_FFFF != n {
                bail!("MFT record {m} doesn't belong to record {n} (damaged)");
            }
            attrs.extend(attrs_of(&x, m)?);
        }
        Ok(attrs)
    }

    /// The stream of attribute `kind` named `name`, put together from its parts.
    fn stream(&self, attrs: &[Attr], kind: u32, name: &str) -> Result<Option<Stream>> {
        let mut parts: Vec<&Attr> = attrs
            .iter()
            .filter(|a| a.kind == kind && a.name == name)
            .collect();
        let Some(first) = parts.first() else {
            return Ok(None);
        };
        if let Body::Resident(v) = &first.body {
            return Ok(Some(Stream::Resident(v.clone())));
        }
        parts.retain(|a| matches!(a.body, Body::NonResident { .. }));
        parts.sort_by_key(|a| match a.body {
            Body::NonResident { start, .. } => start,
            Body::Resident(_) => 0,
        });
        let head = parts[0];
        let Body::NonResident {
            start: 0,
            size,
            init,
            unit,
            ..
        } = head.body
        else {
            bail!("a stream's first part is missing (damaged)");
        };
        let mut runs = Vec::new();
        let mut next = 0u64;
        for p in &parts {
            let Body::NonResident { runs: pr, .. } = &p.body else {
                continue;
            };
            for &(vcn, len, lcn) in pr {
                if vcn < next {
                    bail!("a stream's parts overlap (damaged)");
                }
                if vcn.saturating_add(len) > u64::MAX / self.cluster {
                    bail!("a stream is implausibly large (damaged)");
                }
                if lcn.is_some_and(|l| l.saturating_add(len) > self.clusters) {
                    bail!("a stream runs outside the volume (damaged)");
                }
                let bytes = |c: u64| {
                    c.checked_mul(self.cluster)
                        .context("a stream is implausibly large (damaged)")
                };
                push_run(
                    &mut runs,
                    Run {
                        at: bytes(vcn)?,
                        len: bytes(len)?,
                        phys: lcn.map(bytes).transpose()?,
                    },
                )?;
                next = vcn + len;
            }
        }
        Ok(Some(Stream::NonResident {
            runs,
            size,
            init: init.min(size),
            flags: head.flags,
            unit,
        }))
    }

    /// A reparse point's meaning, from the record's $REPARSE_POINT.
    fn reparse_of<R: Read + Seek>(&self, r: &mut Region<R>, n: u64) -> Result<Reparse> {
        let (_, attrs) = self.attrs(r, n)?;
        Ok(match self.stream(&attrs, AT_REPARSE_POINT, "")? {
            Some(Stream::Resident(v)) => reparse(&v),
            Some(Stream::NonResident { runs, size, .. }) => {
                let data = Data::Runs {
                    runs,
                    size,
                    valid: size,
                };
                reparse(&read_all(r, data, 64 << 10)?)
            }
            None => Reparse::Other,
        })
    }

    fn visit<R: Read + Seek>(
        &self,
        r: &mut Region<R>,
        ie: &[u8],
        key: &[u8],
        w: &mut Walk,
    ) -> bool {
        let fref = le(ie, 0, 8);
        let record = fref & 0xFFFF_FFFF_FFFF;
        let nl = usize::from(key[64]);
        if key[65] == 2 || record < FIRST_USER {
            return true; // a DOS 8.3 alias, or a metadata file
        }
        let Some(name) = key.get(66..66 + 2 * nl).map(utf16le).and_then(clean_name) else {
            return true;
        };
        if w.want.is_some_and(|x| x != name) {
            return true;
        }
        let attr = le(key, 56, 4);
        let dir = attr & FILE_DIRECTORY != 0;
        let mut e = Entry {
            name,
            dir,
            size: if dir { 0 } else { le(key, 48, 8) },
            mtime: filetime(le(key, 16, 8)),
            link: None,
            unreadable: None,
            flags: Vec::new(),
            deleted: None,
            node: Node::Ntfs {
                record,
                seq: (fref >> 48) as u16,
            },
        };
        for (bit, flag) in [
            (FILE_HIDDEN, "hidden"),
            (FILE_SYSTEM, "system"),
            (FILE_COMPRESSED, "compressed"),
            (FILE_SPARSE, "sparse"),
            (FILE_ENCRYPTED, "encrypted"),
        ] {
            if attr & bit != 0 {
                e.flags.push(flag);
            }
        }
        if attr & FILE_ENCRYPTED != 0 && !dir {
            e.unreadable =
                Some("encrypted (EFS): it can't be read without the Windows account's keys".into());
        }
        // the index keeps a copy of the size and times that can lag behind; the record has the
        // real ones ($STANDARD_INFORMATION, and $DATA when it's in the base record)
        if let Ok(rec) = self.record(r, record) {
            let seq = (fref >> 48) as u16;
            if le(&rec, 22, 2) & 1 == 0 || (seq != 0 && le(&rec, 16, 2) != u64::from(seq)) {
                // an index entry left behind by a delete (its record is free, or reused): the
                // file shows among the deleted ones instead
                return true;
            }
            let attrs = attrs_of(&rec, record).unwrap_or_default();
            for a in attrs.iter().filter(|a| a.name.is_empty()) {
                match (a.kind, &a.body) {
                    (AT_STANDARD_INFORMATION, Body::Resident(v)) => {
                        e.mtime = filetime(le(v, 8, 8)).or(e.mtime);
                    }
                    (AT_DATA, Body::Resident(v)) if !dir => e.size = v.len() as u64,
                    (AT_DATA, Body::NonResident { start: 0, size, .. }) if !dir => e.size = *size,
                    _ => {}
                }
            }
        }
        if attr & FILE_REPARSE != 0 {
            match self.reparse_of(r, record) {
                Ok(Reparse::Link(t)) => {
                    e.link = Some(t);
                    e.dir = false;
                    e.size = 0;
                }
                Ok(Reparse::Unreadable(why)) => e.unreadable = Some(why.into()),
                Ok(Reparse::Other) => {}
                Err(err) => e.unreadable = Some(format!("its record can't be read: {err:#}")),
            }
        }
        (w.each)(e)
    }

    /// The entries of one index node (the root's, or an INDX block's) in order, with the
    /// sub-nodes before each entry walked first.
    fn walk_node<R: Read + Seek>(
        &self,
        r: &mut Region<R>,
        buf: &[u8],
        hdr: usize,
        w: &mut Walk,
        depth: u32,
    ) -> Result<bool> {
        let end = (hdr + le(buf, hdr + 4, 4) as usize).min(buf.len());
        let mut p = hdr + le(buf, hdr, 4) as usize;
        while p + 16 <= end {
            let (len, klen, fl) = (
                le(buf, p + 8, 2) as usize,
                le(buf, p + 10, 2) as usize,
                le(buf, p + 12, 2),
            );
            if len < 16 || p + len > end {
                break; // damaged: the rest of this node is skipped
            }
            if fl & 1 != 0 && len >= 24 {
                let vcn = le(buf, p + len - 8, 8);
                if !self.walk_vcn(r, vcn, w, depth + 1)? {
                    return Ok(false);
                }
            }
            if fl & 2 != 0 {
                break; // the last entry carries no name
            }
            if klen >= 66
                && 16 + klen <= len
                && !self.visit(r, &buf[p..p + len], &buf[p + 16..p + 16 + klen], w)
            {
                return Ok(false);
            }
            p += len;
        }
        Ok(true)
    }

    fn walk_vcn<R: Read + Seek>(
        &self,
        r: &mut Region<R>,
        vcn: u64,
        w: &mut Walk,
        depth: u32,
    ) -> Result<bool> {
        if depth > MAX_INDEX_DEPTH {
            bail!("the folder's index is too deep (damaged)");
        }
        if !w.seen.insert(vcn) {
            return Ok(true); // a loop: each block is read once
        }
        if w.seen.len() as u64 > DIR_CAP / w.block {
            bail!("the folder's index is implausibly large (damaged)");
        }
        let alloc = w
            .alloc
            .take()
            .context("the folder's index refers to blocks it doesn't have (damaged)")?;
        let mut b = vec![0u8; w.block as usize];
        let read = vcn
            .checked_mul(w.vcn_unit)
            .context("a damaged folder index")
            .and_then(|off| read_mapped(r, &alloc, off, &mut b));
        w.alloc = Some(alloc);
        read?;
        fixup(&mut b, b"INDX", &|| format!("index block {vcn}"))?;
        self.walk_node(r, &b, 24, w, depth)
    }

    pub(crate) fn read_dir<R: Read + Seek>(
        &mut self,
        r: &mut Region<R>,
        node: &Node,
        want: Option<&str>,
        each: &mut dyn FnMut(Entry) -> bool,
    ) -> Result<()> {
        let Node::Ntfs { record, .. } = *node else {
            bail!("not an NTFS folder");
        };
        let (rec, attrs) = self.attrs(r, record)?;
        if le(&rec, 22, 2) & 3 != 3 {
            bail!("MFT record {record} isn't a folder in use (damaged, or the listing is stale)");
        }
        let Some(Stream::Resident(root)) = self.stream(&attrs, AT_INDEX_ROOT, "$I30")? else {
            bail!("the folder has no index (damaged)");
        };
        let block = le(&root, 8, 4);
        if !block.is_power_of_two() || !(512..=65536).contains(&block) {
            bail!("the folder's index block size is out of range (damaged)");
        }
        let alloc = match self.stream(&attrs, AT_INDEX_ALLOCATION, "$I30")? {
            Some(Stream::NonResident { runs, .. }) => Some(runs),
            _ => None,
        };
        let mut w = Walk {
            alloc,
            block,
            vcn_unit: if block >= self.cluster {
                self.cluster
            } else {
                512
            },
            seen: HashSet::new(),
            want,
            each,
        };
        self.walk_node(r, &root, 16, &mut w, 0)?;
        Ok(())
    }

    pub(crate) fn data<R: Read + Seek>(&mut self, r: &mut Region<R>, node: &Node) -> Result<Data> {
        if let Node::NtfsDel { record } = *node {
            let (data, v) = self.gone_data(r, record)?;
            return data.with_context(|| format!("it can't be recovered: {}", v.why));
        }
        let Node::Ntfs { record, seq } = *node else {
            bail!("not an NTFS file");
        };
        let (rec, attrs) = self.attrs(r, record)?;
        if le(&rec, 22, 2) & 1 == 0 || (seq != 0 && le(&rec, 16, 2) != u64::from(seq)) {
            bail!("the folder lists a file whose record has since been reused (a stale index)");
        }
        match self.stream(&attrs, AT_DATA, "")? {
            None => Ok(Data::Bytes(Vec::new())),
            Some(Stream::Resident(v)) => Ok(Data::Bytes(v)),
            Some(Stream::NonResident {
                runs,
                size,
                init,
                flags,
                unit,
            }) => {
                if flags & ATTR_ENCRYPTED != 0 {
                    bail!("encrypted (EFS): it can't be read without the Windows account's keys");
                }
                if flags & ATTR_COMPRESSED != 0 && unit > 0 {
                    if unit > 8 || self.cluster << unit > 1 << 20 {
                        bail!("compressed with an unusual unit size: it can't be read here");
                    }
                    return Ok(Data::Lznt1 {
                        runs,
                        size,
                        valid: init,
                        unit: self.cluster << unit,
                    });
                }
                Ok(Data::Runs {
                    runs,
                    size,
                    valid: init,
                })
            }
        }
    }

    // ---- deleted files: records no longer in use

    /// Loads $Bitmap's map once; whether it could be.
    fn load_bitmap<R: Read + Seek>(&mut self, r: &mut Region<R>) -> bool {
        if self.bitmap.is_none() {
            let loaded = self.attrs(r, 6).ok().and_then(|(_, attrs)| {
                match self.stream(&attrs, AT_DATA, "").ok()?? {
                    Stream::NonResident { runs, size, .. } if size > 0 => Some((runs, size)),
                    _ => None,
                }
            });
            self.bitmap = Some(loaded);
        }
        matches!(self.bitmap, Some(Some(_)))
    }

    /// Byte `off` of $Bitmap (clusters past its end count as in use).
    fn bitmap_byte<R: Read + Seek>(&mut self, r: &mut Region<R>, off: u64) -> Result<u8> {
        const WINDOW: u64 = 64 << 10;
        let Some(Some((runs, len))) = &self.bitmap else {
            return Ok(0);
        };
        if off >= *len {
            return Ok(0xFF);
        }
        let w = off / WINDOW;
        if !self.bits.contains_key(&w) {
            if self.bits.len() >= 64 {
                self.bits.clear();
            }
            let start = w * WINDOW;
            let mut b = vec![0u8; WINDOW.min(len - start) as usize];
            read_mapped(r, runs, start, &mut b)?;
            self.bits.insert(w, b);
        }
        Ok(self.bits[&w][(off % WINDOW) as usize])
    }

    /// The clusters `lcn..lcn + count` in stretches, each in use now or free.
    fn stretches<R: Read + Seek>(
        &mut self,
        r: &mut Region<R>,
        lcn: u64,
        count: u64,
    ) -> Result<Vec<(u64, u64, bool)>> {
        if !self.load_bitmap(r) {
            return Ok(vec![(lcn, count, false)]);
        }
        let mut out: Vec<(u64, u64, bool)> = Vec::new();
        let mut i = 0;
        while i < count {
            if i % (1 << 20) == 0 {
                r.in_time()?;
            }
            let c = lcn + i;
            let byte = self.bitmap_byte(r, c / 8)?;
            let (used, step) =
                if c.is_multiple_of(8) && count - i >= 8 && (byte == 0 || byte == 0xFF) {
                    (byte == 0xFF, 8)
                } else {
                    ((byte >> (c % 8)) & 1 == 1, 1)
                };
            match out.last_mut() {
                Some(l) if l.2 == used => l.1 += step,
                _ => {
                    if out.len() >= MAX_RUNS {
                        bail!("its clusters are in too many pieces to check");
                    }
                    out.push((c, step, used));
                }
            }
            i += step;
        }
        Ok(out)
    }

    /// A deleted record's data, with the clusters in use now as holes (None when nothing can be
    /// read back), and how likely it's the file as it was.
    fn gone_data<R: Read + Seek>(
        &mut self,
        r: &mut Region<R>,
        record: u64,
    ) -> Result<(Option<Data>, Verdict)> {
        let rec = match self.record(r, record) {
            Ok(rec) => rec,
            Err(e) => return Ok((None, Verdict::lost(format!("{e:#}")))),
        };
        if le(&rec, 22, 2) & 1 != 0 {
            return Ok((
                None,
                Verdict::lost("its MFT record has been reused by another file"),
            ));
        }
        let mut attrs = attrs_of(&rec, record)?;
        if attrs.iter().any(|a| a.kind == AT_ATTRIBUTE_LIST) {
            match self.extended(r, record, attrs) {
                Ok(all) => attrs = all,
                Err(_) => {
                    return Ok((
                        None,
                        Verdict::lost("the other MFT records it was spread over have been reused"),
                    ));
                }
            }
        }
        let stream = match self.stream(&attrs, AT_DATA, "") {
            Ok(s) => s,
            Err(e) => return Ok((None, Verdict::lost(format!("its map is damaged ({e:#})")))),
        };
        match stream {
            None => Ok((Some(Data::Bytes(Vec::new())), Verdict::sure("it was empty"))),
            Some(Stream::Resident(v)) => Ok((
                Some(Data::Bytes(v)),
                Verdict::sure(
                    "it was small enough to be kept in its MFT record, which is still there",
                ),
            )),
            Some(Stream::NonResident {
                runs,
                size,
                init,
                flags,
                unit,
            }) => {
                if flags & ATTR_ENCRYPTED != 0 {
                    return Ok((
                        None,
                        Verdict {
                            state: "can't be read",
                            confidence: 0,
                            why: "encrypted (EFS): it can't be read without the Windows account's keys".into(),
                        },
                    ));
                }
                let known = self.load_bitmap(r);
                let mut spans = Spans::default();
                for run in &runs {
                    if run.at > spans.at {
                        spans.hole(run.at - spans.at)?;
                    }
                    match run.phys {
                        None => spans.hole(run.len)?,
                        Some(p) => {
                            let s =
                                match self.stretches(r, p / self.cluster, run.len / self.cluster) {
                                    Ok(s) => s,
                                    Err(e) => {
                                        let why = format!("its clusters can't be checked ({e:#})");
                                        return Ok((None, Verdict::lost(why)));
                                    }
                                };
                            for (c, n, used) in s {
                                spans.push(c * self.cluster, n * self.cluster, used)?;
                            }
                        }
                    }
                }
                let mut v = spans.verdict(
                    95,
                    "its MFT record still says where its data is, and no other file has used that space since",
                );
                if !known {
                    v.confidence = v.confidence.min(40);
                    v.why = format!(
                        "{}; but $Bitmap can't be read, so whether other files have used its space since is unknown",
                        v.why
                    );
                }
                if v.state == "overwritten" {
                    return Ok((None, v));
                }
                if flags & ATTR_COMPRESSED != 0 && unit > 0 {
                    if spans.used > 0 || unit > 8 || self.cluster << unit > 1 << 20 {
                        return Ok((
                            None,
                            Verdict::lost(
                                "it was compressed, and part of it has been reused: it can't be put back together",
                            ),
                        ));
                    }
                    let data = Data::Lznt1 {
                        runs,
                        size,
                        valid: init,
                        unit: self.cluster << unit,
                    };
                    return Ok((Some(data), v));
                }
                let data = Data::Runs {
                    runs: spans.runs,
                    size,
                    valid: init,
                };
                Ok((Some(data), v))
            }
        }
    }

    /// Looks through the MFT once for records no longer in use, noting every record's header on
    /// the way, until it's done, `MAX_SCAN` records, or the deadline.
    fn scan<R: Read + Seek>(&mut self, r: &mut Region<R>) -> Result<()> {
        if self.scan.is_some() {
            return Ok(());
        }
        let limit = self.records.min(MAX_SCAN);
        let per = ((1u64 << 20) / self.rec).max(1);
        let mut heads: Vec<u32> = Vec::with_capacity(limit.min(1 << 16) as usize);
        let mut by_parent: HashMap<u64, Vec<Gone>> = HashMap::new();
        let (mut kept, mut note) = (0usize, None);
        if self.records > limit {
            note = Some(format!(
                "only the first {limit} of its {} MFT records were looked through",
                self.records
            ));
        }
        let mut buf = vec![0u8; (per * self.rec) as usize];
        let mut n = 0u64;
        while n < limit {
            if r.late() {
                note = Some(format!(
                    "only the first {n} of its {} MFT records could be looked through in the time allowed",
                    self.records
                ));
                break;
            }
            let k = per.min(limit - n);
            let chunk = &mut buf[..(k * self.rec) as usize];
            if let Err(e) = read_mapped(r, &self.mft, n * self.rec, chunk) {
                note = Some(format!("the MFT couldn't be read past record {n} ({e:#})"));
                break;
            }
            for (i, b) in chunk.chunks_exact_mut(self.rec as usize).enumerate() {
                let m = n + i as u64;
                if &b[..4] != b"FILE" {
                    heads.push(0);
                    continue;
                }
                let flags = le(b, 22, 2);
                let mut h = HEAD_VALID | le(b, 16, 2) as u32;
                if flags & 1 != 0 {
                    h |= HEAD_IN_USE;
                }
                if flags & 2 != 0 {
                    h |= HEAD_DIR;
                }
                heads.push(h);
                if flags & 1 != 0 || m < FIRST_USER || le(b, 32, 8) != 0 || kept >= MAX_GONE {
                    continue;
                }
                if fixup(b, b"FILE", &String::new).is_err() {
                    continue;
                }
                let Ok(attrs) = attrs_of(b, m) else {
                    continue;
                };
                if let Some((parent, g)) = gone_of(&attrs, m, flags & 2 != 0) {
                    by_parent.entry(parent).or_default().push(g);
                    kept += 1;
                }
            }
            n += k;
        }
        if kept >= MAX_GONE {
            note = Some(format!(
                "only the first {MAX_GONE} deleted files and folders are listed"
            ));
        }
        self.scan = Some(Scan {
            by_parent,
            heads,
            note,
        });
        Ok(())
    }

    /// Calls `each` for the deleted files and folders whose records name folder `node` as theirs
    /// (and, in the root, those whose folder is gone too), with how likely each can be recovered.
    pub(crate) fn deleted<R: Read + Seek>(
        &mut self,
        r: &mut Region<R>,
        node: &Node,
        each: &mut dyn FnMut(Entry) -> bool,
    ) -> Result<Option<String>> {
        let (dir, live) = match *node {
            Node::Ntfs { record, .. } => (record, true),
            Node::NtfsDel { record } => (record, false),
            _ => bail!("not an NTFS folder"),
        };
        self.scan(r)?;
        let scan = self.scan.as_ref().expect("just scanned");
        let head = |n: u64| scan.heads.get(n as usize).copied().unwrap_or(0);
        // a folder's children name it by record and sequence number; a record's sequence number
        // goes up by one when it's freed
        let fits = |g: &Gone, h: u32, deleted_parent: bool| {
            let seq = (h & 0xFFFF) as u16;
            h & HEAD_VALID != 0
                && h & HEAD_DIR != 0
                && (g.parent_seq == seq || deleted_parent && g.parent_seq.wrapping_add(1) == seq)
        };
        let mut kids: Vec<(Gone, bool)> = scan
            .by_parent
            .get(&dir)
            .into_iter()
            .flatten()
            .filter(|g| head(dir) == 0 || fits(g, head(dir), !live))
            .map(|g| (g.clone(), false))
            .collect();
        if dir == ROOT {
            // deleted files whose folder is gone as well: shown in the root
            for (p, list) in &scan.by_parent {
                if *p == ROOT {
                    continue;
                }
                let h = head(*p);
                for g in list {
                    if !fits(g, h, h & HEAD_IN_USE == 0) {
                        kids.push((g.clone(), true));
                    }
                }
            }
        }
        let note = scan.note.clone();
        kids.sort_by(|a, b| (a.0.name.as_str(), a.0.record).cmp(&(b.0.name.as_str(), b.0.record)));
        for (g, orphan) in kids {
            if r.late() {
                return Ok(Some(super::undelete::LATE.into()));
            }
            let mut v = if g.dir {
                Verdict {
                    state: "recoverable",
                    confidence: 90,
                    why: "its MFT record is still there, and so are those of what was in it".into(),
                }
            } else {
                self.gone_data(r, g.record)?.1
            };
            if orphan {
                v.why = format!("{}. Its folder was deleted too, so it's shown here", v.why);
            }
            let mut flags = Vec::new();
            for (bit, flag) in [
                (FILE_HIDDEN, "hidden"),
                (FILE_SYSTEM, "system"),
                (FILE_COMPRESSED, "compressed"),
                (FILE_ENCRYPTED, "encrypted"),
            ] {
                if g.attr & bit != 0 {
                    flags.push(flag);
                }
            }
            let bad = !g.dir && matches!(v.state, "overwritten" | "can't be read");
            let e = Entry {
                unreadable: bad.then(|| v.why.clone()),
                deleted: Some(Deleted::new(&g.name, g.record, v)),
                name: g.name,
                dir: g.dir,
                size: if g.dir { 0 } else { g.size },
                mtime: g.mtime,
                link: None,
                flags,
                node: Node::NtfsDel { record: g.record },
            };
            if !each(e) {
                break;
            }
        }
        Ok(note)
    }
}

/// A deleted record's name, folder, size and time, from its attributes: its parent folder's
/// record number, and what's worth listing.
fn gone_of(attrs: &[Attr], record: u64, dir: bool) -> Option<(u64, Gone)> {
    // the Win32 or POSIX name rather than the DOS 8.3 alias
    let names = attrs.iter().filter_map(|a| match (&a.body, a.kind) {
        (Body::Resident(v), AT_FILE_NAME) if v.len() >= 66 => Some(v),
        _ => None,
    });
    let v = names
        .clone()
        .find(|v| v[65] != 2)
        .or(names.clone().next())?;
    let nl = usize::from(v[64]);
    let name = v.get(66..66 + 2 * nl).map(utf16le).and_then(clean_name)?;
    let pref = le(v, 0, 8);
    let mut g = Gone {
        record,
        parent_seq: (pref >> 48) as u16,
        name,
        dir,
        size: le(v, 48, 8),
        mtime: filetime(le(v, 16, 8)),
        attr: le(v, 56, 4),
    };
    for a in attrs.iter().filter(|a| a.name.is_empty()) {
        match (a.kind, &a.body) {
            (AT_STANDARD_INFORMATION, Body::Resident(s)) if s.len() >= 16 => {
                g.mtime = filetime(le(s, 8, 8)).or(g.mtime);
            }
            (AT_DATA, Body::Resident(d)) => g.size = d.len() as u64,
            (AT_DATA, Body::NonResident { start: 0, size, .. }) => g.size = *size,
            _ => {}
        }
    }
    Some((pref & 0xFFFF_FFFF_FFFF, g))
}
