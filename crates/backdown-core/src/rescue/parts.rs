//! Lost partitions (like TestDisk's search): what the partition table says now, and what is really
//! on the disk, found by the filesystems' own signatures wherever they start.
//!
//! A wiped or damaged partition table (a drive re-initialised by mistake, a failed repartitioning,
//! a click on "this disk isn't readable, initialise it?") usually leaves the filesystems intact.
//!
//! - `table` reads the table as it is: an MBR with its chain of logical partitions, or a GPT with
//!   both of its copies checked against their CRCs (a damaged or disagreeing copy is itself worth
//!   knowing about: the other one is what to recover from).
//! - `search` finds the filesystems. Quick: at the likely starts (the first sector, every MiB, the
//!   old cylinder boundaries, wherever any copy of any table points). Deep: at every sector, with
//!   large sequential reads and cheap in-memory checks for each known signature at each sector.
//!   Every candidate is confirmed with `magic::probe_fs`, which also says what it's called and how
//!   big it is. When a filesystem's first sectors are what got overwritten, it's found through a
//!   backup: NTFS's copy of its boot sector in its last sector, FAT32's at sector 6, exFAT's boot
//!   region at sector 12, ext's superblocks in later block groups, HFS+'s alternate volume header,
//!   XFS's per-allocation-group superblocks, btrfs's superblock mirrors, LUKS2's second header.
//!
//! Everything here only reads. Rewriting a table is deliberately not offered: a found partition is
//! copied out as an image, or opened read-only, instead.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::magic::{self, Fs, be, field, has, le, read_at, utf16};

const MIB: u64 = 1 << 20;
/// Where the old 255-head, 63-sectors-per-track geometry put partitions: a track in (the first
/// partition, and a logical one after its EBR), and every cylinder.
const TRACK: u64 = 63 * 512;
const CYLINDER: u64 = 255 * TRACK;
/// The deep search reads this much at a time...
const CHUNK: usize = 4 << 20;
/// ...and keeps this much more, so a signature up to here past a sector is in memory too (btrfs's
/// superblock is 64 KiB in).
const REACH: usize = 72 << 10;
/// The quick search reads around each likely start, merging starts close together into one read
/// of at most this much.
const WINDOW: usize = 2 << 20;
/// Where, relative to a likely start, the quick search looks: the start itself; just before it,
/// where the partition ending there keeps its backup (NTFS's boot sector, HFS+'s alternate
/// header); and just after it, where the one starting there keeps its backups (FAT32's boot sector
/// at sector 6, exFAT's boot region at 12, LUKS2's second header), for 512- and 4096-byte sectors.
const AROUND: [i64; 9] = [0, -512, -2048, -4096, 3072, 6144, 16384, 24576, 49152];
/// Enough for any real disk; past this a search of garbage stops collecting.
const MAX_FOUND: usize = 100_000;

/// What to search, and how thoroughly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    /// Look at every sector, not only where partitions usually start.
    pub deep: bool,
    /// The deep search's step: 512, or 4096 (fewer places to look, though the disk rather than
    /// the looking is what usually limits the speed; it then only finds filesystems that start on
    /// a 4 KiB boundary, and their backups next to one).
    pub sector: u64,
    /// Where to look: from `start` to `end` (the end of the source when None).
    pub start: u64,
    pub end: Option<u64>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            deep: false,
            sector: 512,
            start: 0,
            end: None,
        }
    }
}

/// A partition the table lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// 1 to 4 for an MBR's primary partitions and 5 on for its logical ones; a GPT's slot, from 1.
    pub index: u32,
    pub start: u64,
    pub size: u64,
    /// The partition type by name ("Linux filesystem", "NTFS / exFAT", "Extended").
    pub kind: String,
    /// The raw type: "0x83", or the GPT type GUID.
    pub code: String,
    /// The GPT partition name.
    pub name: Option<String>,
    /// The GPT partition's own GUID.
    pub guid: Option<String>,
    pub bootable: bool,
}

/// The partition table as it is now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Table {
    /// "gpt", "mbr", or "none" (no table, or one filesystem on the whole disk).
    pub scheme: &'static str,
    /// The sector size the table counts in: 512, or 4096 on 4K-native drives and in some USB
    /// enclosures.
    pub sector: u64,
    pub entries: Vec<Entry>,
    /// What's wrong with it, in plain English ("the backup GPT at the end of the disk is damaged").
    pub problems: Vec<String>,
}

/// A filesystem found on the disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Found {
    pub start: u64,
    /// How much of the disk it spans (for NTFS, including the backup boot sector after it), when
    /// its superblock says.
    pub size: Option<u64>,
    /// What it is, as `magic` names it ("NTFS", "FAT32", "ext4", "LUKS2 (encrypted)").
    pub fs: String,
    pub label: Option<String>,
    /// What it was found by: "boot sector", "superblock", "volume header"…, or "backup boot
    /// sector", "backup superblock", "alternate volume header", "backup header" when its primary
    /// structure is gone.
    pub via: &'static str,
    /// The current table has a partition starting here that's big enough for it (a filesystem at
    /// the first sector of a disk without a table counts: that's how it's used).
    pub in_table: bool,
    /// It runs past the end of the source (a partial image, or a disk that shrank).
    pub truncated: bool,
    /// It lies wholly inside another filesystem found here: most likely a disk image stored as a
    /// file in that one, rather than a partition.
    pub nested: bool,
}

/// How far a search is: `pos` goes from `Options::start` to `size`, where it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Progress {
    pub pos: u64,
    pub size: u64,
    /// Filesystems found so far.
    pub found: u64,
}

/// What a search found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Search {
    /// The size of the source.
    pub size: u64,
    /// The deep search's step.
    pub sector: u64,
    pub table: Table,
    /// By where they start.
    pub found: Vec<Found>,
    /// Bytes that couldn't be read (bad sectors), searched as if they were zeros.
    pub unreadable: u64,
    /// The search was stopped before it finished; `found` is what it had found by then.
    pub stopped: bool,
}

/// The partition table of a disk (or an image of one) of `size` bytes.
pub fn table(f: &File, size: u64) -> Result<Table> {
    Ok(layout(f, size)?.0)
}

/// Finds the filesystems on `src` (a device node or an image file, opened read-only), reporting
/// progress at most about four times a second, and stopping early when `stop` is set.
pub fn search(
    src: &Path,
    opts: &Options,
    progress: &mut dyn FnMut(&Progress),
    stop: &AtomicBool,
) -> Result<Search> {
    if !opts.sector.is_power_of_two() || !(512..=65536).contains(&opts.sector) {
        bail!(
            "the sector size must be a power of two from 512 to 65536 bytes, not {}",
            opts.sector
        );
    }
    let f = super::open(src).with_context(|| format!("can't open {}", src.display()))?;
    let size = super::size_of(&f).with_context(|| format!("can't size {}", src.display()))?;
    let end = opts.end.unwrap_or(size).min(size);
    let start = opts.start.min(end);
    let (table, mut hints) = layout(&f, size).unwrap_or_else(|e| {
        let table = Table {
            scheme: "none",
            sector: 512,
            entries: Vec::new(),
            problems: vec![format!("the partition table can't be read: {e}")],
        };
        (table, Vec::new())
    });
    hints.sort_unstable();
    hints.dedup();
    let mut s = Scanner {
        f: &f,
        size,
        found: Vec::new(),
        index: HashMap::new(),
        tried: HashSet::new(),
        shown: 0,
        unreadable: 0,
        last: None,
    };
    let stopped = if opts.deep {
        s.deep(start, end, opts.sector, progress, stop)
    } else {
        s.quick(start, end, &hints, progress, stop)
    };
    if !stopped {
        progress(&Progress {
            pos: end,
            size: end,
            found: s.shown,
        });
    }
    let found = finish(s.found, &table, size);
    Ok(Search {
        size,
        sector: opts.sector,
        table,
        found,
        unreadable: s.unreadable,
        stopped,
    })
}

// ---- the partition table

/// CRC-32 (IEEE 802.3), as GPT uses it.
fn crc32(b: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut t = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                k += 1;
            }
            t[i] = c;
            i += 1;
        }
        t
    };
    !b.iter().fold(!0u32, |c, &x| {
        TABLE[((c ^ u32::from(x)) & 0xFF) as usize] ^ (c >> 8)
    })
}

/// A GUID the way it's written (mixed-endian, upper case).
fn guid(b: &[u8]) -> String {
    let tail: String = b
        .get(10..16)
        .unwrap_or_default()
        .iter()
        .map(|x| format!("{x:02X}"))
        .collect();
    format!(
        "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{tail}",
        le(b, 0, 4),
        le(b, 4, 2),
        le(b, 6, 2),
        b.get(8).copied().unwrap_or(0),
        b.get(9).copied().unwrap_or(0),
    )
}

fn named(name: &'static str, code: &str) -> String {
    if name == "other" {
        format!("other ({code})")
    } else {
        name.to_string()
    }
}

/// One copy of a GPT: the header and its partition entries, each checked against its CRC.
struct GptCopy {
    lba: u64,
    header_ok: bool,
    entries_ok: bool,
    alt: u64,
    disk: String,
    esize: usize,
    entries: Vec<u8>,
}

/// The GPT header at `lba` (None when there's no "EFI PART" there).
fn gpt_copy(f: &File, ss: u64, lba: u64, size: u64) -> io::Result<Option<GptCopy>> {
    let at = match lba.checked_mul(ss) {
        Some(at) if lba > 0 && at.saturating_add(ss) <= size => at,
        _ => return Ok(None),
    };
    let h = read_at(f, at, ss as usize)?;
    if !has(&h, 0, b"EFI PART") {
        return Ok(None);
    }
    let hs = le(&h, 12, 4) as usize;
    let mut header_ok = (92..=h.len()).contains(&hs) && le(&h, 24, 8) == lba;
    if header_ok {
        let mut c = h[..hs].to_vec();
        c[16..20].fill(0);
        header_ok = u64::from(crc32(&c)) == le(&h, 16, 4);
    }
    let (n, esize, first) = (le(&h, 80, 4), le(&h, 84, 4), le(&h, 72, 8));
    let len = n * esize;
    let fits = (128..=4096).contains(&esize)
        && esize.is_multiple_of(8)
        && len > 0
        && len <= 4 << 20
        && first
            .checked_mul(ss)
            .and_then(|o| o.checked_add(len))
            .is_some_and(|e| e <= size);
    let entries = if fits {
        read_at(f, first * ss, len as usize)?
    } else {
        Vec::new()
    };
    let entries_ok =
        fits && entries.len() as u64 == len && u64::from(crc32(&entries)) == le(&h, 88, 4);
    Ok(Some(GptCopy {
        lba,
        header_ok,
        entries_ok,
        alt: le(&h, 32, 8),
        disk: guid(&h[56..72]),
        esize: esize as usize,
        entries: if fits { entries } else { Vec::new() },
    }))
}

fn gpt_entries(c: &GptCopy, ss: u64) -> Vec<Entry> {
    if c.entries.is_empty() {
        return Vec::new();
    }
    c.entries
        .chunks_exact(c.esize)
        .enumerate()
        .filter(|(_, e)| e[..16].iter().any(|&b| b != 0))
        .map(|(i, e)| {
            let code = guid(&e[..16]);
            let (first, last) = (le(e, 32, 8), le(e, 40, 8));
            Entry {
                index: i as u32 + 1,
                start: first.saturating_mul(ss),
                size: last
                    .saturating_sub(first)
                    .saturating_add(1)
                    .saturating_mul(ss),
                kind: named(magic::gpt_type(&code), &code),
                name: Some(utf16(&e[56..128], false)).filter(|n| !n.is_empty()),
                guid: Some(guid(&e[16..32])),
                bootable: le(e, 48, 8) & 4 != 0,
                code,
            }
        })
        .collect()
}

/// A GPT, from whichever copy is intact.
struct Gpt {
    sector: u64,
    entries: Vec<Entry>,
    problems: Vec<String>,
    /// Where the partitions of either copy start and end.
    hints: Vec<u64>,
}

fn read_gpt(f: &File, size: u64) -> io::Result<Option<Gpt>> {
    for ss in [512u64, 4096] {
        if size < 3 * ss {
            continue;
        }
        let last = size / ss - 1;
        let p = gpt_copy(f, ss, 1, size)?;
        // the primary says where its backup is (not at the end of a disk that was copied to a
        // bigger one); without it, the end is where to look
        let named_alt = p.as_ref().filter(|c| c.header_ok).map(|c| c.alt);
        let b = match named_alt {
            Some(a) if a != 1 && a != last => match gpt_copy(f, ss, a, size)? {
                Some(c) => Some(c),
                None => gpt_copy(f, ss, last, size)?,
            },
            _ => gpt_copy(f, ss, last, size)?,
        };
        if p.is_none() && b.is_none() {
            continue;
        }
        let mut hints = Vec::new();
        for c in p.iter().chain(b.iter()) {
            for e in gpt_entries(c, ss) {
                hints.extend([e.start, e.start.saturating_add(e.size)]);
            }
        }
        let good =
            |c: &Option<GptCopy>| c.as_ref().filter(|c| c.header_ok && c.entries_ok).is_some();
        let state = |c: &Option<GptCopy>| match c {
            None => "missing",
            Some(c) if !c.header_ok => "damaged",
            Some(_) => "damaged (its list of partitions fails its checksum)",
        };
        let count = |c: &Option<GptCopy>| c.as_ref().map_or(0, |c| gpt_entries(c, ss).len());
        let mut problems = Vec::new();
        let chosen = match (good(&p), good(&b)) {
            (true, true) => {
                let (pc, bc) = (p.as_ref().unwrap(), b.as_ref().unwrap());
                if pc.entries != bc.entries || pc.disk != bc.disk {
                    problems.push(format!(
                        "the primary GPT and its backup at the end of the disk disagree ({} and \
                         {} partitions); the primary's are listed",
                        count(&p),
                        count(&b)
                    ));
                }
                if bc.lba != last {
                    problems.push(
                        "the backup GPT isn't at the end of the disk (was the disk copied to a \
                         bigger one?)"
                            .into(),
                    );
                }
                p.as_ref()
            }
            (true, false) => {
                let alt = p.as_ref().map_or(0, |c| c.alt);
                problems.push(if alt > last {
                    "the backup GPT would be past the end of the disk: the disk (or this image \
                     of it) is smaller than when it was partitioned"
                        .into()
                } else {
                    format!("the backup GPT at the end of the disk is {}", state(&b))
                });
                p.as_ref()
            }
            (false, true) => {
                problems.push(format!(
                    "the primary GPT at the start of the disk is {}; the partitions listed come \
                     from its backup at the end of the disk",
                    state(&p)
                ));
                b.as_ref()
            }
            (false, false) => {
                let usable = |c: &Option<GptCopy>| {
                    c.as_ref()
                        .filter(|c| c.header_ok && !c.entries.is_empty())
                        .is_some()
                };
                if usable(&p) || usable(&b) {
                    problems.push(
                        "both copies of the GPT are damaged; the partitions listed come from a \
                         list that fails its checksum, and may be wrong"
                            .into(),
                    );
                    if usable(&p) { p.as_ref() } else { b.as_ref() }
                } else {
                    problems.push(
                        "there's a GPT on the disk, but both of its copies are damaged".into(),
                    );
                    None
                }
            }
        };
        let entries = chosen.map(|c| gpt_entries(c, ss)).unwrap_or_default();
        return Ok(Some(Gpt {
            sector: ss,
            entries,
            problems,
            hints,
        }));
    }
    Ok(None)
}

/// An MBR (or EBR) partition slot, in sectors.
#[derive(Clone, Copy)]
struct Slot {
    boot: u8,
    kind: u8,
    start: u64,
    len: u64,
}

fn slot(b: &[u8], i: usize) -> Slot {
    let e = &b[446 + 16 * i..462 + 16 * i];
    Slot {
        boot: e[0],
        kind: e[4],
        start: le(e, 8, 4),
        len: le(e, 12, 4),
    }
}

/// The four slots of the MBR in `s0`, when it has one that makes sense.
fn mbr_slots(s0: &[u8]) -> Option<[Slot; 4]> {
    if !has(s0, 510, &[0x55, 0xAA]) {
        return None;
    }
    let s: [Slot; 4] = std::array::from_fn(|i| slot(s0, i));
    s.iter()
        .all(|s| matches!(s.boot, 0 | 0x80) && (s.kind == 0 || (s.start > 0 && s.len > 0)))
        .then_some(s)
}

fn extended(kind: u8) -> bool {
    matches!(kind, 0x05 | 0x0F | 0x85)
}

fn is_extended(e: &Entry) -> bool {
    matches!(e.code.as_str(), "0x05" | "0x0F" | "0x85")
}

fn mbr_entry(index: u32, s: &Slot, base: u64, ss: u64) -> Entry {
    let code = format!("0x{:02X}", s.kind);
    Entry {
        index,
        start: base + s.start * ss,
        size: s.len * ss,
        kind: named(magic::mbr_type(s.kind), &code),
        code,
        name: None,
        guid: None,
        bootable: s.boot == 0x80,
    }
}

/// The MBR's partitions, the logical ones included (a chain of EBRs, each one's partition
/// relative to itself, each link relative to the start of the extended partition).
fn mbr_entries(
    f: &File,
    slots: &[Slot; 4],
    ss: u64,
    size: u64,
    problems: &mut Vec<String>,
) -> io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for (i, s) in slots.iter().enumerate() {
        if s.kind == 0 || s.kind == 0xEE {
            continue;
        }
        out.push(mbr_entry(i as u32 + 1, s, 0, ss));
        if !extended(s.kind) {
            continue;
        }
        let ext = s.start * ss;
        let (mut ebr, mut n, mut seen) = (ext, 5u32, HashSet::new());
        loop {
            if !seen.insert(ebr) {
                problems.push("the chain of logical partitions loops back on itself".into());
                break;
            }
            if seen.len() > 256 {
                break;
            }
            let b = if ebr + 512 <= size {
                read_at(f, ebr, 512)?
            } else {
                Vec::new()
            };
            if !has(&b, 510, &[0x55, 0xAA]) {
                problems.push(if n == 5 {
                    "the extended partition's first logical partition record is missing".into()
                } else {
                    format!(
                        "the chain of logical partitions is broken after partition {}",
                        n - 1
                    )
                });
                break;
            }
            let (part, next) = (slot(&b, 0), slot(&b, 1));
            if part.kind != 0 && part.len > 0 {
                out.push(mbr_entry(n, &part, ebr, ss));
                n += 1;
            }
            if !extended(next.kind) || next.start == 0 {
                break;
            }
            ebr = ext + next.start * ss;
        }
    }
    Ok(out)
}

/// The sector size an MBR counts in: 512, unless nothing is where 512 says and something is where
/// 4096 says (a drive partitioned in a USB enclosure that reports 4K sectors).
fn mbr_sector(f: &File, slots: &[Slot; 4], size: u64) -> io::Result<u64> {
    let parts: Vec<&Slot> = slots
        .iter()
        .filter(|s| s.kind != 0 && s.kind != 0xEE && !extended(s.kind))
        .collect();
    let any_at = |ss: u64| -> io::Result<bool> {
        for p in &parts {
            let at = p.start * ss;
            if at < size && magic::probe_fs(f, at)?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    };
    let fit = slots
        .iter()
        .all(|s| s.kind == 0 || s.start.saturating_add(s.len).saturating_mul(4096) <= size);
    Ok(
        if !parts.is_empty() && fit && !any_at(512)? && any_at(4096)? {
            4096
        } else {
            512
        },
    )
}

/// Whether `s0` holds an MBR whose partitions have filesystems where it says: then it's a partition
/// table, even if a boot loader kept an old boot sector's BPB in front of it.
fn mbr_points_at_filesystems(f: &File, s0: &[u8], size: u64) -> io::Result<bool> {
    let Some(slots) = mbr_slots(s0) else {
        return Ok(false);
    };
    for s in slots
        .iter()
        .filter(|s| s.kind != 0 && s.kind != 0xEE && !extended(s.kind))
    {
        let at = s.start * 512;
        if at < size && magic::probe_fs(f, at)?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Partitions past the end of the disk, and partitions that overlap.
fn check(entries: &[Entry], size: u64, problems: &mut Vec<String>) {
    for e in entries {
        if e.start.saturating_add(e.size) > size {
            problems.push(format!(
                "partition {} runs past the end of the disk",
                e.index
            ));
        }
    }
    let mut v: Vec<&Entry> = entries.iter().filter(|e| !is_extended(e)).collect();
    v.sort_by_key(|e| e.start);
    let overlaps = v
        .windows(2)
        .filter(|w| w[0].start.saturating_add(w[0].size) > w[1].start);
    for w in overlaps.take(8) {
        problems.push(format!(
            "partitions {} and {} overlap",
            w[0].index, w[1].index
        ));
    }
}

fn listing(n: usize) -> String {
    match n {
        0 => "listing no partitions".into(),
        1 => "listing 1 partition".into(),
        n => format!("listing {n} partitions"),
    }
}

/// The current table, and every place any table on the disk (a stale or damaged one included)
/// says a partition starts or ends: where a quick search should look.
fn layout(f: &File, size: u64) -> io::Result<(Table, Vec<u64>)> {
    let s0 = read_at(f, 0, 512)?;
    let whole = magic::probe_fs(f, 0)?.filter(|fs| fs.id != "iso9660");
    // these put their own signature in the first sector, so there can't be a table there too
    let owns_first = whole.as_ref().is_some_and(|fs| {
        matches!(
            fs.id,
            "ntfs"
                | "vfat"
                | "exfat"
                | "BitLocker"
                | "refs"
                | "xfs"
                | "crypto_LUKS"
                | "squashfs"
                | "apfs"
        )
    }) && !mbr_points_at_filesystems(f, &s0, size)?;
    let gpt = read_gpt(f, size)?;
    let slots = if owns_first { None } else { mbr_slots(&s0) };
    let mut problems = Vec::new();
    let mut hints = gpt.as_ref().map(|g| g.hints.clone()).unwrap_or_default();
    let protective = slots.is_some_and(|s| s.iter().any(|s| s.kind == 0xEE));
    let (scheme, sector, entries) = if owns_first {
        if let Some(g) = &gpt {
            problems.push(format!(
                "the disk holds one filesystem from its first sector on, but a GPT from an \
                 earlier layout survives on it, {}",
                listing(g.entries.len())
            ));
        }
        ("none", 512, Vec::new())
    } else {
        match (gpt, slots) {
            (Some(g), Some(s)) if !protective => {
                let ss = mbr_sector(f, &s, size)?;
                let v = mbr_entries(f, &s, ss, size, &mut problems)?;
                problems.push(format!(
                    "there's also a GPT on the disk, from an earlier layout, {}",
                    listing(g.entries.len())
                ));
                ("mbr", ss, v)
            }
            (Some(g), s) => {
                if s.is_none() {
                    problems.push(if has(&s0, 510, &[0x55, 0xAA]) {
                        "the protective MBR in the first sector is damaged".into()
                    } else {
                        "the protective MBR in the first sector is missing".into()
                    });
                }
                problems.extend(g.problems);
                ("gpt", g.sector, g.entries)
            }
            (None, Some(_)) if protective => {
                problems.push(
                    "the first sector says the disk uses GPT, but neither copy of the GPT can be \
                     found"
                        .into(),
                );
                ("gpt", 512, Vec::new())
            }
            (None, Some(s)) => {
                let ss = mbr_sector(f, &s, size)?;
                if ss == 4096 {
                    problems.push(
                        "the partition table counts in 4096-byte sectors (it was probably written \
                         through a USB enclosure that reports 4K sectors)"
                            .into(),
                    );
                }
                ("mbr", ss, mbr_entries(f, &s, ss, size, &mut problems)?)
            }
            (None, None) => {
                if whole.is_none() && has(&s0, 510, &[0x55, 0xAA]) {
                    problems.push(
                        "the first sector ends in a boot signature, but the partition table in \
                         it doesn't make sense"
                            .into(),
                    );
                }
                ("none", 512, Vec::new())
            }
        }
    };
    if let Some(fs) = whole.as_ref().filter(|_| !owns_first && scheme != "none") {
        problems.push(format!(
            "there's also a filesystem ({}) starting at the disk's first sector, from when the \
             whole disk was formatted, before or after it was partitioned",
            fs.kind
        ));
    }
    check(&entries, size, &mut problems);
    // a hybrid or stale MBR's partitions are worth a look too
    if let Some(s) = mbr_slots(&s0).filter(|_| !owns_first) {
        let ss = if sector == 4096 { 4096 } else { 512 };
        for e in mbr_entries(f, &s, ss, size, &mut Vec::new())? {
            hints.extend([e.start, e.start.saturating_add(e.size)]);
        }
    }
    for e in &entries {
        hints.extend([e.start, e.start.saturating_add(e.size)]);
    }
    Ok((
        Table {
            scheme,
            sector,
            entries,
            problems,
        },
        hints,
    ))
}

// ---- finding filesystems by their signatures

/// A family of filesystems (one `magic::probe_fs` id, or a few that are one thing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Fam {
    Ntfs,
    Fat,
    Exfat,
    Ext,
    Xfs,
    Btrfs,
    Hfsp,
    Hfs,
    Apfs,
    Luks,
    Swap,
    Md,
    Squash,
    F2fs,
    Iso,
    Udf,
    BitLocker,
    Refs,
    Lvm,
}

fn fam_of(id: &str) -> Option<Fam> {
    Some(match id {
        "ntfs" => Fam::Ntfs,
        "vfat" => Fam::Fat,
        "exfat" => Fam::Exfat,
        "ext2" | "ext3" | "ext4" => Fam::Ext,
        "xfs" => Fam::Xfs,
        "btrfs" => Fam::Btrfs,
        "hfsplus" => Fam::Hfsp,
        "hfs" => Fam::Hfs,
        "apfs" => Fam::Apfs,
        "crypto_LUKS" => Fam::Luks,
        "swap" => Fam::Swap,
        "linux_raid_member" => Fam::Md,
        "squashfs" => Fam::Squash,
        "f2fs" => Fam::F2fs,
        "iso9660" => Fam::Iso,
        "udf" => Fam::Udf,
        "BitLocker" => Fam::BitLocker,
        "refs" => Fam::Refs,
        "LVM2_member" => Fam::Lvm,
        _ => return None,
    })
}

fn primary_via(fam: Fam) -> &'static str {
    match fam {
        Fam::Ntfs | Fam::Fat | Fam::Exfat | Fam::BitLocker | Fam::Refs => "boot sector",
        Fam::Hfsp => "volume header",
        Fam::Hfs => "master directory block",
        Fam::Apfs => "container superblock",
        Fam::Luks => "header",
        Fam::Swap => "swap header",
        Fam::Iso | Fam::Udf => "volume descriptor",
        Fam::Lvm => "physical volume label",
        _ => "superblock",
    }
}

/// A signature, seen in memory.
#[derive(Debug, Clone, Copy)]
enum Hit {
    /// A structure that says exactly where its filesystem starts; when it's a backup, what it's
    /// called and where it is.
    At {
        fam: Fam,
        start: u64,
        backup: Option<(&'static str, u64)>,
    },
    /// A boot sector or volume header at `at` that could be the primary one (the filesystem starts
    /// at `at`) or its backup, `back` bytes past the start: which one takes a look at the disk.
    Either { fam: Fam, at: u64, back: u64 },
}

/// The signatures of filesystems starting at `pos`, or of backups found there, looking only at
/// `b`, the bytes from `pos` on (up to `REACH` of them).
fn sniff(b: &[u8], pos: u64, out: &mut Vec<Hit>) {
    if b.len() < 512 {
        return;
    }
    let here = |fam| Hit::At {
        fam,
        start: pos,
        backup: None,
    };
    match &b[3..11] {
        b"NTFS    " => {
            let (bps, total) = (le(b, 11, 2), le(b, 40, 8));
            if matches!(bps, 512 | 1024 | 2048 | 4096)
                && total > 0
                && le(b, 48, 8) > 0
                && let Some(back) = total.checked_mul(bps)
            {
                out.push(Hit::Either {
                    fam: Fam::Ntfs,
                    at: pos,
                    back,
                });
            }
        }
        b"EXFAT   " => {
            let shift = b[108];
            let len = le(b, 72, 8);
            if (9..=12).contains(&shift)
                && b[109] <= 25 - shift
                && len > 0
                && len.leading_zeros() >= u32::from(shift)
                && le(b, 80, 4) >= 24
            {
                out.push(Hit::Either {
                    fam: Fam::Exfat,
                    at: pos,
                    back: 12 << shift,
                });
            }
        }
        b"-FVE-FS-" => out.push(here(Fam::BitLocker)),
        b"ReFS\0\0\0\0" => out.push(here(Fam::Refs)),
        _ => {}
    }
    if b[510] == 0x55 && b[511] == 0xAA {
        fat(b, pos, out);
    }
    match &b[..4] {
        b"XFSB" => xfs(b, pos, out),
        b"LUKS" if has(b, 4, b"\xba\xbe") && matches!(be(b, 6, 2), 1 | 2) => {
            out.push(here(Fam::Luks))
        }
        b"SKUL" if has(b, 4, b"\xba\xbe") && be(b, 6, 2) == 2 => {
            // LUKS2's second header says how far in it is
            let off = be(b, 256, 8);
            if off.is_power_of_two() && (0x4000..=0x40_0000).contains(&off) && off <= pos {
                out.push(Hit::At {
                    fam: Fam::Luks,
                    start: pos - off,
                    backup: Some(("backup header", pos)),
                });
            }
        }
        b"hsqs" => {
            let bs = le(b, 12, 4);
            if bs.is_power_of_two() && (4096..=1 << 20).contains(&bs) && le(b, 40, 8) > 0 {
                out.push(here(Fam::Squash));
            }
        }
        _ => {}
    }
    if has(b, 32, b"NXSB") {
        let bs = le(b, 36, 4);
        if le(b, 24, 2) == 1
            && bs.is_power_of_two()
            && (4096..=65536).contains(&bs)
            && le(b, 40, 8) > 0
            && le(b, 40, 8).checked_mul(bs).is_some()
        {
            out.push(here(Fam::Apfs));
        }
    }
    if has(b, 512, b"LABELONE") && le(b, 520, 8) == 1 && has(b, 536, b"LVM2 001") {
        out.push(here(Fam::Lvm));
    }
    // ext's superblock: here (a backup, at the start of a block of 2 KiB or more, or of a 1 KiB
    // block when the step is 512) or 1 KiB in (the primary; or a backup of 1 KiB blocks)
    for off in [0usize, 1024] {
        if le(b, off + 56, 2) == 0xEF53
            && let Some(h) = ext(&b[off..], pos + off as u64)
        {
            out.push(h);
        }
    }
    if b.len() >= 2048 {
        match &b[1024..1026] {
            b"H+" | b"HX" => {
                let (bs, total) = (be(b, 1064, 4), be(b, 1068, 4));
                if matches!(be(b, 1026, 2), 4 | 5)
                    && bs.is_power_of_two()
                    && (512..=1 << 20).contains(&bs)
                    && total > 0
                {
                    // the alternate header is 1 KiB before the end: this may be it
                    let len = total * bs;
                    out.push(if len > 2048 {
                        Hit::Either {
                            fam: Fam::Hfsp,
                            at: pos,
                            back: len - 2048,
                        }
                    } else {
                        here(Fam::Hfsp)
                    });
                }
            }
            b"BD" => {
                let (blocks, bs) = (be(b, 1042, 2), be(b, 1044, 4));
                if blocks > 0 && bs > 0 && bs.is_multiple_of(512) {
                    out.push(here(Fam::Hfs));
                }
            }
            _ => {}
        }
        if le(b, 1024, 4) == 0xF2F5_2010
            && le(b, 1040, 4) == 12
            && (9..=12).contains(&le(b, 1032, 4))
        {
            out.push(here(Fam::F2fs));
        }
    }
    for page in [4096usize, 8192, 16384, 65536] {
        if has(b, page - 10, b"SWAPSPACE2") && le(b, 1024, 4) == 1 && le(b, 1028, 4) > 0 {
            out.push(here(Fam::Swap));
            break;
        }
    }
    if has(b, 4086, b"SWAP-SPACE") {
        out.push(here(Fam::Swap));
    }
    // md RAID 1.2: 4 KiB in, and saying so
    if le(b, 4096, 4) == 0xA92B_4EFC && le(b, 4100, 4) == 1 && le(b, 4096 + 144, 8) == 8 {
        out.push(here(Fam::Md));
    }
    if b.get(32768) == Some(&1) && has(b, 32769, b"CD001") {
        out.push(here(Fam::Iso));
    }
    if has(b, 32769, b"BEA01") {
        out.push(here(Fam::Udf));
    }
    if has(b, 0x10040, b"_BHRfS_M") {
        // each copy of btrfs's superblock says where it is: 64 KiB in, or a mirror at 64 MiB or
        // 256 GiB
        let x = pos + 0x10000;
        match le(b, 0x10030, 8) {
            0x10000 => out.push(here(Fam::Btrfs)),
            n @ (0x400_0000 | 0x40_0000_0000) if x >= n => out.push(Hit::At {
                fam: Fam::Btrfs,
                start: x - n,
                backup: Some(("backup superblock", x)),
            }),
            _ => {}
        }
    }
}

/// A FAT boot sector (the caller saw its 55 AA).
fn fat(b: &[u8], pos: u64, out: &mut Vec<Hit>) {
    let (bps, reserved) = (le(b, 11, 2), le(b, 14, 2));
    if !(matches!(bps, 512 | 1024 | 2048 | 4096)
        && b[13].is_power_of_two()
        && reserved > 0
        && matches!(b[16], 1 | 2))
    {
        return;
    }
    if has(b, 82, b"FAT32   ") {
        // FAT32 keeps a copy of its boot sector a few sectors in (6, nearly always)
        let k = le(b, 50, 2);
        out.push(if k > 0 && k < reserved {
            Hit::Either {
                fam: Fam::Fat,
                at: pos,
                back: k * bps,
            }
        } else {
            Hit::At {
                fam: Fam::Fat,
                start: pos,
                backup: None,
            }
        });
    } else if has(b, 54, b"FAT12   ") || has(b, 54, b"FAT16   ") || has(b, 54, b"FAT     ") {
        out.push(Hit::At {
            fam: Fam::Fat,
            start: pos,
            backup: None,
        });
    }
}

/// An XFS superblock: the primary, or the copy at the start of a later allocation group, which
/// the group's header right after it numbers.
fn xfs(b: &[u8], pos: u64, out: &mut Vec<Hit>) {
    let (bs, dblocks, agblocks, agcount, ss) = (
        be(b, 4, 4),
        be(b, 8, 8),
        be(b, 84, 4),
        be(b, 88, 4),
        be(b, 102, 2),
    );
    if !(bs.is_power_of_two()
        && (512..=65536).contains(&bs)
        && dblocks > 0
        && dblocks.checked_mul(bs).is_some()
        && agblocks > 0
        && agcount > 0
        && ss.is_power_of_two()
        && (512..=32768).contains(&ss))
    {
        return;
    }
    let ss = ss as usize;
    let seqno = if has(b, ss, b"XAGF") {
        be(b, ss + 8, 4)
    } else {
        0
    };
    if seqno == 0 {
        out.push(Hit::At {
            fam: Fam::Xfs,
            start: pos,
            backup: None,
        });
    } else if let Some(start) = (seqno < agcount)
        .then(|| (seqno * agblocks).checked_mul(bs))
        .flatten()
        .and_then(|d| pos.checked_sub(d))
    {
        out.push(Hit::At {
            fam: Fam::Xfs,
            start,
            backup: Some(("backup superblock", pos)),
        });
    }
}

/// An ext2/3/4 superblock at `at` (`sb` holds it): the primary, 1 KiB into the filesystem, or a
/// backup at the start of a later block group, which it numbers.
fn ext(sb: &[u8], at: u64) -> Option<Hit> {
    if sb.len() < 0x160 {
        return None;
    }
    let log = le(sb, 24, 4);
    if log > 6 {
        return None;
    }
    let bs = 1024u64 << log;
    let (first, per_group, inodes, rev, group) = (
        le(sb, 20, 4),
        le(sb, 32, 4),
        le(sb, 40, 4),
        le(sb, 76, 4),
        le(sb, 90, 2),
    );
    let mut blocks = le(sb, 4, 4);
    if le(sb, 96, 4) & 0x80 != 0 {
        blocks |= le(sb, 0x150, 4) << 32;
    }
    // 1 KiB blocks number the superblock's block 1; bigger ones number it 0
    if first != u64::from(bs == 1024)
        || per_group == 0
        || per_group > 8 * bs
        || !per_group.is_multiple_of(8)
        || inodes == 0
        || rev > 1
        || blocks <= first
        || blocks.checked_mul(bs).is_none()
        || group >= (blocks - first).div_ceil(per_group)
    {
        return None;
    }
    if group == 0 {
        return Some(Hit::At {
            fam: Fam::Ext,
            start: at.checked_sub(1024)?,
            backup: None,
        });
    }
    let start = at.checked_sub((first + group * per_group).checked_mul(bs)?)?;
    Some(Hit::At {
        fam: Fam::Ext,
        start,
        backup: Some(("backup superblock", at)),
    })
}

/// Whether the filesystem whose boot sector (or volume header) is `b` plausibly starts at
/// `start`: whether what the boot sector says is at a given distance from the start is there.
fn home(f: &File, fam: Fam, start: u64, b: &[u8]) -> bool {
    let read = |off: Option<u64>, n: usize| off.map(|o| read_span(f, o, n)).unwrap_or_default();
    match fam {
        // the MFT (or its mirror) begins with a FILE record
        Fam::Ntfs => {
            let bps = le(b, 11, 2);
            let cluster = match b[13] {
                n if n > 0x80 => bps.checked_shl(256 - u32::from(n)),
                n => Some(bps * u64::from(n)),
            };
            [le(b, 48, 8), le(b, 56, 8)].into_iter().any(|lcn| {
                let at = cluster
                    .and_then(|c| lcn.checked_mul(c))
                    .and_then(|o| start.checked_add(o));
                read(at, 4) == b"FILE"
            })
        }
        // each FAT begins with the media byte and then all ones
        Fam::Fat => {
            let (bps, reserved, per_fat, media) = (le(b, 11, 2), le(b, 14, 2), le(b, 36, 4), b[21]);
            (0..u64::from(b[16])).any(|i| {
                let at = start.checked_add((reserved + i * per_fat) * bps);
                read(at, 3) == [media, 0xFF, 0xFF]
            })
        }
        Fam::Exfat => {
            let at = le(b, 80, 4)
                .checked_shl(u32::from(b[108]))
                .and_then(|o| start.checked_add(o));
            read(at, 8) == [0xF8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
        }
        // the catalog file's first node is its B-tree's header node
        Fam::Hfsp => {
            let bs = be(b, 1064, 4);
            let at = be(b, 1024 + 288, 4)
                .checked_mul(bs)
                .and_then(|o| start.checked_add(o));
            let n = read(at, 34);
            let node = be(&n, 32, 2);
            n.len() == 34
                && n[8] == 1
                && n[9] == 0
                && be(&n, 10, 2) == 3
                && node.is_power_of_two()
                && (512..=32768).contains(&node)
        }
        _ => false,
    }
}

/// Whether the backup of the boot sector (or HFS+ volume header) `b` at `at` is where it would be
/// if `at` were the start, `back` bytes on, saying the same (exFAT's backup may lag in its flags
/// and how full it is; HFS+'s alternate header in its dates and counts).
fn twin(f: &File, fam: Fam, at: u64, back: u64, b: &[u8]) -> bool {
    let Some(t) = at.checked_add(back) else {
        return false;
    };
    let t = read_span(f, t, 1536);
    match fam {
        Fam::Hfsp => {
            t.len() == 1536
                && b.len() >= 1536
                && t[1024..1028] == b[1024..1028]
                && t[1064..1072] == b[1064..1072]
        }
        _ => {
            let n = if fam == Fam::Exfat { 106 } else { 90 };
            t.len() >= n && b.len() >= n && t[..n] == b[..n]
        }
    }
}

/// A filesystem confirmed on the disk, before duplicates are weeded out.
struct Raw {
    fam: Fam,
    start: u64,
    size: Option<u64>,
    fs: String,
    label: Option<String>,
    via: &'static str,
    /// Found by its primary structure.
    primary: bool,
}

impl Raw {
    fn new(fam: Fam, start: u64, fs: Fs, via: &'static str, primary: bool) -> Raw {
        Raw {
            fam,
            start,
            size: fs.size,
            fs: fs.kind,
            label: fs.label,
            via,
            primary,
        }
    }

    fn end(&self) -> Option<u64> {
        self.size.map(|s| self.start.saturating_add(s))
    }

    /// The same filesystem's metadata again, further in (a backup read as if it were a primary, a
    /// copy in a journal, an APFS checkpoint): the same kind and size, starting inside `o`.
    fn copy_of(&self, o: &Raw) -> bool {
        self.fs == o.fs
            && self.size.is_some()
            && self.size == o.size
            && o.start < self.start
            && o.end().is_some_and(|e| self.start < e)
    }
}

struct Scanner<'a> {
    f: &'a File,
    size: u64,
    found: Vec<Raw>,
    /// Where each (family, start) is in `found`.
    index: HashMap<(Fam, u64), usize>,
    /// What's been looked at: (family, start, 0) for a primary structure, (family, where, 1) for a
    /// backup, (family, where, 2) for a boot sector that may be either.
    tried: HashSet<(Fam, u64, u8)>,
    /// Found so far, copies aside.
    shown: u64,
    unreadable: u64,
    last: Option<Instant>,
}

impl Scanner<'_> {
    /// What `magic` makes of `at`, if it's of the family expected.
    fn probe(&self, fam: Fam, at: u64) -> Option<Fs> {
        let fs = magic::probe_fs(self.f, at).ok()??;
        let got = fam_of(fs.id)?;
        (got == fam || (fam == Fam::Hfs && got == Fam::Hfsp)).then_some(fs)
    }

    fn hit(&mut self, h: Hit) {
        if self.tried.len() > 1 << 20 {
            self.tried.clear();
        }
        let raw = match h {
            Hit::At { fam, start, backup } => {
                if start >= self.size {
                    return;
                }
                if let Some(&i) = self.index.get(&(fam, start))
                    && (self.found[i].primary || backup.is_some())
                {
                    return;
                }
                // each backup is worth a try (one may be damaged where the next isn't)
                let key = match backup {
                    Some((_, at)) => (fam, at, 1),
                    None => (fam, start, 0),
                };
                if !self.tried.insert(key) {
                    return;
                }
                self.at(fam, start, backup)
            }
            Hit::Either { fam, at, back } => {
                if !self.tried.insert((fam, at, 2)) {
                    return;
                }
                self.either(fam, at, back)
            }
        };
        if let Some(r) = raw {
            self.add(r);
        }
    }

    fn at(&self, fam: Fam, start: u64, backup: Option<(&'static str, u64)>) -> Option<Raw> {
        if let Some(fs) = self.probe(fam, start) {
            // F2FS's second superblock is 4 KiB after its first
            if fam == Fam::F2fs && start >= 4096 && self.probe(fam, start - 4096).is_some() {
                return None;
            }
            return Some(Raw::new(fam, start, fs, primary_via(fam), true));
        }
        let (via, at) = backup?;
        let fs = match fam {
            // `magic` reads a superblock (or volume header) 1 KiB in
            Fam::Ext => self.probe(fam, at.checked_sub(1024)?)?,
            Fam::Xfs => self.probe(fam, at)?,
            Fam::Btrfs => {
                let sb = read_span(self.f, at, 0x22B);
                if !has(&sb, 0x40, b"_BHRfS_M") {
                    return None;
                }
                Fs {
                    kind: "btrfs".into(),
                    label: Some(field(&sb, 0x12B, 256)).filter(|l| !l.is_empty()),
                    size: Some(le(&sb, 0x70, 8)).filter(|&s| s > 0),
                    id: "btrfs",
                }
            }
            Fam::Luks => {
                let h = read_span(self.f, at, 512);
                if !has(&h, 0, b"SKUL\xba\xbe") {
                    return None;
                }
                Fs {
                    kind: "LUKS2 (encrypted)".into(),
                    label: Some(field(&h, 24, 48)).filter(|l| !l.is_empty()),
                    size: None,
                    id: "crypto_LUKS",
                }
            }
            _ => return None,
        };
        Some(Raw::new(fam, start, fs, via, false))
    }

    /// A boot sector (or HFS+ volume header) at `at`: the primary, if what it describes is there
    /// or its own backup is where it would be; else the backup, if what it describes is `back`
    /// bytes earlier; else (its filesystem too damaged to tell) taken for the primary.
    fn either(&self, fam: Fam, at: u64, back: u64) -> Option<Raw> {
        let b = read_span(self.f, at, 4096);
        let this = self.probe(fam, at)?;
        // NTFS counts all but the last sector, where the backup boot sector is
        let extent = |mut fs: Fs| {
            if fam == Fam::Ntfs {
                fs.size = fs.size.map(|s| s + le(&b, 11, 2));
            }
            fs
        };
        if home(self.f, fam, at, &b) || twin(self.f, fam, at, back, &b) {
            return Some(Raw::new(fam, at, extent(this), primary_via(fam), true));
        }
        // HFS+'s alternate header is 1 KiB before the end of the partition, which can be up to an
        // allocation block past the end of the volume
        let slack = match fam {
            Fam::Hfsp => (be(&b, 1064, 4) / 512).clamp(1, 128),
            _ => 1,
        };
        for q in (0..slack).filter_map(|k| at.checked_sub(back + 512 * k)) {
            if q == at {
                continue;
            }
            if let Some(fs) = self.probe(fam, q).filter(|fs| fs.size == this.size) {
                return Some(Raw::new(fam, q, extent(fs), primary_via(fam), true));
            }
            if home(self.f, fam, q, &b) {
                let label = match fam {
                    Fam::Ntfs => magic::ntfs_label(self.f, q, &b),
                    Fam::Exfat => magic::exfat_label(self.f, q, &b),
                    _ => this.label.clone(),
                };
                let via = match fam {
                    Fam::Hfsp => "alternate volume header",
                    _ => "backup boot sector",
                };
                let fs = Fs { label, ..this };
                return Some(Raw::new(fam, q, extent(fs), via, false));
            }
        }
        Some(Raw::new(fam, at, extent(this), primary_via(fam), true))
    }

    fn add(&mut self, r: Raw) {
        match self.index.get(&(r.fam, r.start)) {
            Some(&i) => {
                if r.primary && !self.found[i].primary {
                    self.found[i] = r;
                }
            }
            None if self.found.len() < MAX_FOUND => {
                if self.found.len() > 5000 || !self.found.iter().any(|o| r.copy_of(o)) {
                    self.shown += 1;
                }
                self.index.insert((r.fam, r.start), self.found.len());
                self.found.push(r);
            }
            None => {}
        }
    }

    /// Reports progress, at most about four times a second.
    fn tick(&mut self, pos: u64, end: u64, progress: &mut dyn FnMut(&Progress)) {
        if self
            .last
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(250))
        {
            progress(&Progress {
                pos: pos.min(end),
                size: end,
                found: self.shown,
            });
            self.last = Some(Instant::now());
        }
    }

    /// Fills `buf` from `off` on and says how much it filled (less at the end of the source); what
    /// can't be read (bad sectors) is left as zeros and counted.
    fn fill(&mut self, off: u64, buf: &mut [u8]) -> usize {
        let want = self.size.saturating_sub(off).min(buf.len() as u64) as usize;
        if want == 0 || read_exact_at(self.f, off, &mut buf[..want]).is_ok() {
            return want;
        }
        for (i, piece) in buf[..want].chunks_mut(64 << 10).enumerate() {
            if read_exact_at(self.f, off + (i as u64) * (64 << 10), piece).is_err() {
                piece.fill(0);
                self.unreadable += piece.len() as u64;
            }
        }
        want
    }

    /// Every `step` bytes from `start` to `end`, reading `CHUNK` at a time and keeping `REACH` more.
    fn deep(
        &mut self,
        start: u64,
        end: u64,
        step: u64,
        progress: &mut dyn FnMut(&Progress),
        stop: &AtomicBool,
    ) -> bool {
        // with bigger steps, also where backups sit next to a boundary: NTFS's boot sector and
        // HFS+'s alternate header just before one, FAT32's and exFAT's just after
        let mut extra: Vec<usize> = [step.saturating_sub(512), step.saturating_sub(2048), 3072]
            .into_iter()
            .filter(|&o| o > 0 && o < step)
            .map(|o| o as usize)
            .collect();
        extra.sort_unstable();
        extra.dedup();
        let mut base = start.div_ceil(step) * step;
        let mut buf = vec![0u8; CHUNK + REACH];
        let mut n = self.fill(base, &mut buf);
        let mut hits = Vec::new();
        while base < end {
            if stop.load(Ordering::Relaxed) {
                return true;
            }
            let last = (base + CHUNK as u64).min(end);
            let mut pos = base;
            while pos < last {
                let r = (pos - base) as usize;
                if r >= n {
                    break;
                }
                sniff(&buf[r..n], pos, &mut hits);
                for &o in &extra {
                    if r + o < n {
                        sniff(&buf[r + o..n], pos + o as u64, &mut hits);
                    }
                }
                pos += step;
            }
            for h in hits.drain(..) {
                self.hit(h);
            }
            self.tick(last, end, progress);
            if n <= CHUNK {
                break;
            }
            buf.copy_within(CHUNK..n, 0);
            let kept = n - CHUNK;
            base += CHUNK as u64;
            n = kept + self.fill(base + kept as u64, &mut buf[kept..]);
        }
        false
    }

    /// Around each likely start from `start` to `end`.
    fn quick(
        &mut self,
        start: u64,
        end: u64,
        hints: &[u64],
        progress: &mut dyn FnMut(&Progress),
        stop: &AtomicBool,
    ) -> bool {
        let mut buf = vec![0u8; WINDOW + REACH + 8192];
        let mut hits = Vec::new();
        let mut cands = Vec::new();
        let mut from = start;
        loop {
            if stop.load(Ordering::Relaxed) {
                return true;
            }
            let first = likely(from, hints);
            if first >= end {
                break;
            }
            // one read for the starts close together
            let lo = first.saturating_sub(4096) / 4096 * 4096;
            let mut hi = first + REACH as u64;
            cands.clear();
            cands.push(first);
            from = first + 1;
            loop {
                let c = likely(from, hints);
                if c >= end || c.saturating_sub(4096) > hi || c + REACH as u64 - lo > WINDOW as u64
                {
                    break;
                }
                cands.push(c);
                hi = c + REACH as u64;
                from = c + 1;
            }
            let len = (hi.div_ceil(4096) * 4096 - lo) as usize;
            let n = self.fill(lo, &mut buf[..len]);
            for &c in &cands {
                for d in AROUND {
                    let Some(p) = c.checked_add_signed(d).filter(|&p| p >= lo) else {
                        continue;
                    };
                    let r = (p - lo) as usize;
                    if r < n {
                        sniff(&buf[r..n], p, &mut hits);
                    }
                }
            }
            for h in hits.drain(..) {
                self.hit(h);
            }
            self.tick(from, end, progress);
        }
        false
    }
}

/// The first likely partition start at or after `x`: the first sector, the first track, every
/// MiB, a track either side of every cylinder, and wherever a table says.
fn likely(x: u64, hints: &[u64]) -> u64 {
    if x == 0 {
        return 0;
    }
    let mut best = x.div_ceil(MIB) * MIB;
    if x <= TRACK {
        best = best.min(TRACK);
    }
    for o in [CYLINDER - TRACK, CYLINDER, CYLINDER + TRACK] {
        best = best.min(x.saturating_sub(o).div_ceil(CYLINDER) * CYLINDER + o);
    }
    if let Some(&h) = hints.get(hints.partition_point(|&h| h < x)) {
        best = best.min(h);
    }
    best
}

/// Up to `len` bytes at `off`, read as whole 4 KiB blocks (raw disks on Windows take nothing else).
fn read_span(f: &File, off: u64, len: usize) -> Vec<u8> {
    let lo = off / 4096 * 4096;
    let Some(hi) = off.checked_add(len as u64).map(|e| e.div_ceil(4096) * 4096) else {
        return Vec::new();
    };
    let mut b = read_at(f, lo, (hi - lo) as usize).unwrap_or_default();
    let skip = ((off - lo) as usize).min(b.len());
    b.drain(..skip);
    b.truncate(len);
    b
}

fn read_exact_at(f: &File, off: u64, buf: &mut [u8]) -> io::Result<()> {
    let mut r = f;
    r.seek(SeekFrom::Start(off))?;
    r.read_exact(buf)
}

/// The filesystems found, by where they start: copies of another one's metadata dropped, and so
/// is what starts inside another one but runs off the disk; the rest marked as in the table,
/// running past the end, or inside another one.
fn finish(mut v: Vec<Raw>, table: &Table, size: u64) -> Vec<Found> {
    v.sort_by_key(|r| (r.start, r.fam));
    // copies: by kind and size, in order, each one starting inside the last one kept
    let mut keep = vec![true; v.len()];
    let mut last: HashMap<(&str, u64), usize> = HashMap::new();
    for (i, r) in v.iter().enumerate() {
        let Some(s) = r.size else { continue };
        match last.get(&(r.fs.as_str(), s)) {
            Some(&j) if r.copy_of(&v[j]) => keep[i] = false,
            _ => {
                last.insert((r.fs.as_str(), s), i);
            }
        }
    }
    let mut i = 0;
    v.retain(|_| {
        i += 1;
        keep[i - 1]
    });
    // what can hold another: one with a known size that fits on the disk, found by its primary
    // structure or alone at its start (not a backup's claim to a place another has taken since)
    let mut at_start: HashMap<u64, usize> = HashMap::new();
    for r in &v {
        *at_start.entry(r.start).or_default() += 1;
    }
    let holds =
        |r: &Raw| r.end().is_some_and(|e| e <= size) && (r.primary || at_start[&r.start] == 1);
    let mut out = Vec::with_capacity(v.len());
    let mut reach = 0u64; // the furthest end of the holders that start before this one
    let mut group_reach = 0u64;
    let mut group_start = None;
    for r in &v {
        if group_start != Some(r.start) {
            reach = reach.max(group_reach);
            group_start = Some(r.start);
        }
        let inside = r.start < reach;
        let nested = inside && r.end().is_none_or(|e| e <= reach);
        let truncated = r.end().is_some_and(|e| e > size);
        if holds(r) {
            group_reach = group_reach.max(r.end().unwrap_or(0));
        }
        // starting inside another filesystem and running off the disk: bytes in a file that
        // happen to look like a boot sector, not a partition
        if inside && truncated {
            continue;
        }
        let in_table = if table.scheme == "none" {
            r.start == 0 && r.primary
        } else {
            table.entries.iter().any(|e| {
                !is_extended(e) && e.start == r.start && r.size.is_none_or(|s| s <= e.size)
            })
        };
        out.push(Found {
            start: r.start,
            size: r.size,
            fs: r.fs.clone(),
            label: r.label.clone(),
            via: r.via,
            in_table,
            truncated,
            nested,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_of_the_standard_check_string() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn likely_starts_in_order() {
        let mut v = Vec::new();
        let mut x = 0;
        while x < 17 * MIB {
            let c = likely(x, &[5 * MIB + 7]);
            v.push(c);
            x = c + 1;
        }
        assert_eq!(&v[..3], &[0, TRACK, MIB]);
        assert!(v.contains(&(CYLINDER - TRACK)));
        assert!(v.contains(&CYLINDER));
        assert!(v.contains(&(CYLINDER + TRACK)));
        assert!(v.contains(&(2 * CYLINDER)));
        assert!(v.contains(&(5 * MIB + 7)));
        assert!(v.windows(2).all(|w| w[0] < w[1]));
    }
}
