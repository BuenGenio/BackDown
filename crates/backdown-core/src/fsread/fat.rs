//! FAT12, FAT16 and FAT32: the boot sector's geometry, cluster chains through the first FAT, and
//! folders of 32-byte entries with their long (VFAT, UTF-16) names. Deleted entries are skipped.

use std::collections::HashMap;
use std::io::{Read, Seek};

use anyhow::{Context, Result, bail};

use super::{
    Data, Entry, Info, Node, Region, Run, clean_name, dos_time, each_chunk, looped, push_run,
    utf16le,
};
use crate::magic::{field, le};

/// A FAT folder holds at most 65,536 entries.
const DIR_MAX: u64 = 65_536 * 32;
/// The FAT is read in windows of this size, at most `WINDOWS` of them kept.
const WINDOW: u64 = 64 << 10;
const WINDOWS: usize = 64;
/// Where the 13 UTF-16 characters of a long-name entry are.
const LFN_AT: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];

pub(crate) struct Fat {
    bits: u32,
    cluster: u64,
    fat_at: u64,
    fat_len: u64,
    /// FAT12/16: the fixed root folder.
    root_at: u64,
    root_len: u64,
    /// FAT32: the root folder's first cluster.
    root_cluster: u32,
    /// Where cluster 2 starts.
    data_at: u64,
    /// The highest valid cluster number.
    max: u32,
    cache: HashMap<u64, Vec<u8>>,
}

enum Next {
    To(u32),
    End,
    Broken(u32),
}

/// A plausible BIOS parameter block (NTFS and exFAT are told apart before this is asked).
pub(crate) fn detect(b: &[u8]) -> bool {
    b.len() >= 512
        && matches!(le(b, 11, 2), 512 | 1024 | 2048 | 4096)
        && b[13].is_power_of_two()
        && le(b, 14, 2) > 0
        && (1..=4).contains(&b[16])
        && (le(b, 19, 2) > 0 || le(b, 32, 4) > 0)
        && (le(b, 22, 2) > 0 || le(b, 36, 4) > 0)
        && (b[21] == 0xF0 || b[21] >= 0xF8)
}

impl Fat {
    pub(crate) fn open<R: Read + Seek>(r: &mut Region<R>, b: &[u8]) -> Result<(Fat, Info)> {
        if !detect(b) {
            bail!("not a FAT boot sector");
        }
        let bps = le(b, 11, 2);
        let spc = u64::from(b[13]);
        let reserved = le(b, 14, 2);
        let fats = u64::from(b[16]);
        let root_entries = le(b, 17, 2);
        let total = match le(b, 19, 2) {
            0 => le(b, 32, 4),
            n => n,
        };
        // FAT32 is the one whose 16-bit FAT size is zero (as Linux decides), whatever its size
        let fat32 = le(b, 22, 2) == 0;
        let fat_sectors = if fat32 { le(b, 36, 4) } else { le(b, 22, 2) };
        let root_sectors = (root_entries * 32).div_ceil(bps);
        let meta = reserved + fats * fat_sectors + root_sectors;
        let data_sectors = total
            .checked_sub(meta)
            .context("the FAT boot sector's numbers don't add up (damaged)")?;
        let bits = if fat32 {
            32
        } else if data_sectors / spc < 4085 {
            12
        } else {
            16
        };
        let fat_len = fat_sectors * bps;
        // no more clusters than the FAT can describe, nor than the values meaning "bad" allow
        let top: u64 = match bits {
            12 => 0xFF6,
            16 => 0xFFF6,
            _ => 0x0FFF_FFF6,
        };
        let max = (data_sectors / spc + 1)
            .min((fat_len * 8 / u64::from(bits)).saturating_sub(1))
            .min(top);
        if max < 2 {
            bail!("the FAT volume has no data clusters (damaged)");
        }
        let mut fat = Fat {
            bits,
            cluster: bps * spc,
            fat_at: reserved * bps,
            fat_len,
            root_at: (reserved + fats * fat_sectors) * bps,
            root_len: root_sectors * bps,
            root_cluster: 0,
            data_at: meta * bps,
            max: max as u32,
            cache: HashMap::new(),
        };
        if fat32 {
            let rc = le(b, 44, 4) as u32;
            if !(2..=fat.max).contains(&rc) {
                bail!("the FAT32 root folder is at cluster {rc}, outside the volume (damaged)");
            }
            fat.root_cluster = rc;
        }
        let (sig, at) = if fat32 { (66, 71) } else { (38, 43) };
        let mut label = if b[sig] == 0x29 {
            Some(field(b, at, 11)).filter(|l| !l.is_empty() && l != "NO NAME")
        } else {
            None
        };
        if let Ok(Some(l)) = fat.root_label(r) {
            label = Some(l);
        }
        let mut notes = Vec::new();
        let size = total * bps;
        if size > r.len() {
            notes.push(format!(
                "the filesystem says it's {size} bytes but only {} are here: files near its end may not open",
                r.len()
            ));
        }
        let info = Info {
            kind: format!("FAT{bits}"),
            label,
            size,
            block: fat.cluster,
            notes,
        };
        Ok((fat, info))
    }

    /// The volume label entry in the root folder (it wins over the boot sector's copy, as on
    /// Windows).
    fn root_label<R: Read + Seek>(&mut self, r: &mut Region<R>) -> Result<Option<String>> {
        let runs = self.dir_runs(r, 0)?;
        let mut label = None;
        each_chunk(r, &runs, self.cluster.max(4096), |chunk| {
            for e in chunk.as_chunks::<32>().0 {
                match e[0] {
                    0 => return Ok(false),
                    0xE5 => continue,
                    _ => {}
                }
                if e[11] & 0x3F != 0x0F && e[11] & 0x08 != 0 {
                    label = Some(field(e, 0, 11)).filter(|l| !l.is_empty());
                    return Ok(false);
                }
            }
            Ok(true)
        })?;
        Ok(label)
    }

    fn byte<R: Read + Seek>(&mut self, r: &mut Region<R>, off: u64) -> Result<u8> {
        if off >= self.fat_len {
            bail!("a cluster number beyond the FAT (damaged)");
        }
        let w = off / WINDOW;
        if !self.cache.contains_key(&w) {
            if self.cache.len() >= WINDOWS {
                self.cache.clear();
            }
            let start = w * WINDOW;
            let bytes = r.read_vec(self.fat_at + start, WINDOW.min(self.fat_len - start))?;
            self.cache.insert(w, bytes);
        }
        Ok(self.cache[&w][(off % WINDOW) as usize])
    }

    fn next<R: Read + Seek>(&mut self, r: &mut Region<R>, n: u32) -> Result<Next> {
        let n64 = u64::from(n);
        let (off, width) = match self.bits {
            12 => (n64 + n64 / 2, 2),
            16 => (n64 * 2, 2),
            _ => (n64 * 4, 4),
        };
        let mut v = 0u32;
        for i in 0..width {
            v |= u32::from(self.byte(r, off + i)?) << (8 * i);
        }
        let (v, eoc) = match self.bits {
            12 => (if n & 1 == 1 { v >> 4 } else { v & 0xFFF }, 0xFF8),
            16 => (v, 0xFFF8),
            _ => (v & 0x0FFF_FFFF, 0x0FFF_FFF8),
        };
        Ok(if v >= eoc {
            Next::End
        } else if (2..=self.max).contains(&v) {
            Next::To(v)
        } else {
            Next::Broken(v)
        })
    }

    /// The runs of the chain from `first`, until it ends or covers `want` bytes; and, if it broke
    /// off before either, why.
    fn chain<R: Read + Seek>(
        &mut self,
        r: &mut Region<R>,
        first: u32,
        want: u64,
    ) -> Result<(Vec<Run>, u64, Option<String>)> {
        if !(2..=self.max).contains(&first) {
            return Ok((
                Vec::new(),
                0,
                Some(format!("it starts at cluster {first}, outside the volume")),
            ));
        }
        let (mut runs, mut at, mut c) = (Vec::new(), 0u64, first);
        for _ in 0..=self.max {
            let phys = self.data_at + u64::from(c - 2) * self.cluster;
            push_run(
                &mut runs,
                Run {
                    at,
                    len: self.cluster,
                    phys: Some(phys),
                },
            )?;
            at += self.cluster;
            if at >= want {
                return Ok(looped(runs, at, None));
            }
            match self.next(r, c)? {
                Next::To(n) => c = n,
                Next::End => return Ok(looped(runs, at, None)),
                Next::Broken(v) => {
                    return Ok((
                        runs,
                        at,
                        Some(format!(
                            "its cluster chain breaks off (a FAT entry of {v:#x})"
                        )),
                    ));
                }
            }
        }
        Ok((runs, at, Some("its cluster chain loops".into())))
    }

    /// A folder's runs: the fixed root region, or its cluster chain (as much as is readable).
    fn dir_runs<R: Read + Seek>(&mut self, r: &mut Region<R>, cluster: u32) -> Result<Vec<Run>> {
        if cluster == 0 && self.bits != 32 {
            return Ok(vec![Run {
                at: 0,
                len: self.root_len,
                phys: Some(self.root_at),
            }]);
        }
        let first = if cluster == 0 {
            self.root_cluster
        } else {
            cluster
        };
        Ok(self.chain(r, first, DIR_MAX)?.0)
    }

    pub(crate) fn read_dir<R: Read + Seek>(
        &mut self,
        r: &mut Region<R>,
        node: &Node,
        want: Option<&str>,
        each: &mut dyn FnMut(Entry) -> bool,
    ) -> Result<()> {
        let Node::Fat { cluster, .. } = *node else {
            bail!("not a FAT folder");
        };
        let runs = self.dir_runs(r, cluster)?;
        let fat32 = self.bits == 32;
        let mut lfn = Lfn::default();
        each_chunk(r, &runs, DIR_MAX, |chunk| {
            for e in chunk.as_chunks::<32>().0 {
                match e[0] {
                    0 => return Ok(false), // the end of the folder
                    0xE5 => {
                        lfn = Lfn::default(); // deleted
                        continue;
                    }
                    _ => {}
                }
                let attr = e[11];
                if attr & 0x3F == 0x0F {
                    lfn.add(e);
                    continue;
                }
                let long = std::mem::take(&mut lfn).name(e);
                if attr & 0x08 != 0 {
                    continue; // the volume label
                }
                let Some(name) = clean_name(long.unwrap_or_else(|| short_name(e))) else {
                    continue;
                };
                if want.is_some_and(|w| w != name) {
                    continue;
                }
                let hi = if fat32 { le(e, 20, 2) << 16 } else { 0 };
                let dir = attr & 0x10 != 0;
                let size = le(e, 28, 4) as u32;
                let mut flags = Vec::new();
                if attr & 0x02 != 0 {
                    flags.push("hidden");
                }
                if attr & 0x04 != 0 {
                    flags.push("system");
                }
                let entry = Entry {
                    name,
                    dir,
                    size: if dir { 0 } else { u64::from(size) },
                    mtime: dos_time(le(e, 24, 2), le(e, 22, 2)),
                    link: None,
                    unreadable: None,
                    flags,
                    node: Node::Fat {
                        cluster: (hi | le(e, 26, 2)) as u32,
                        size,
                    },
                };
                // a subfolder at cluster 0 would be the root again: damaged, so it's left out
                if dir && entry.node == (Node::Fat { cluster: 0, size }) {
                    continue;
                }
                if !each(entry) {
                    return Ok(false);
                }
            }
            Ok(true)
        })
    }

    pub(crate) fn data<R: Read + Seek>(&mut self, r: &mut Region<R>, node: &Node) -> Result<Data> {
        let Node::Fat { cluster, size } = *node else {
            bail!("not a FAT file");
        };
        let size = u64::from(size);
        if size == 0 {
            return Ok(Data::Bytes(Vec::new()));
        }
        let (runs, got, why) = self.chain(r, cluster, size)?;
        if got < size {
            bail!(
                "only {got} of its {size} bytes can be found: {} (damaged)",
                why.unwrap_or_else(|| "its cluster chain ends early".into())
            );
        }
        Ok(Data::Runs {
            runs,
            size,
            valid: size,
        })
    }
}

/// A long name being put together from the entries before its short entry (last part first).
#[derive(Default)]
struct Lfn {
    units: Vec<u16>,
    /// The part number the next entry should carry.
    expect: usize,
    sum: u8,
}

impl Lfn {
    fn add(&mut self, e: &[u8; 32]) {
        let ord = usize::from(e[0] & 0x1F);
        if e[0] & 0x40 != 0 {
            if ord == 0 || ord > 20 {
                *self = Lfn::default();
                return;
            }
            self.units = vec![0xFFFF; 13 * ord];
            self.expect = ord;
            self.sum = e[13];
        }
        if ord == 0 || ord != self.expect || e[13] != self.sum {
            *self = Lfn::default();
            return;
        }
        let at = (ord - 1) * 13;
        for (i, &p) in LFN_AT.iter().enumerate() {
            self.units[at + i] = u16::from_le_bytes([e[p], e[p + 1]]);
        }
        self.expect -= 1;
    }

    /// The long name, if it's complete and belongs to the short entry `e`.
    fn name(self, e: &[u8; 32]) -> Option<String> {
        if self.units.is_empty() || self.expect != 0 || checksum(&e[..11]) != self.sum {
            return None;
        }
        let end = self
            .units
            .iter()
            .position(|&u| u == 0 || u == 0xFFFF)
            .unwrap_or(self.units.len());
        let bytes: Vec<u8> = self.units[..end]
            .iter()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        Some(utf16le(&bytes)).filter(|n| !n.is_empty())
    }
}

/// The checksum of a short name that its long-name entries carry.
fn checksum(short: &[u8]) -> u8 {
    short
        .iter()
        .fold(0u8, |s, &c| s.rotate_right(1).wrapping_add(c))
}

/// An 8.3 name, in the DOS code page, with the lower-case flags Windows NT sets.
fn short_name(e: &[u8; 32]) -> String {
    let mut base = e[..8].to_vec();
    if base[0] == 0x05 {
        base[0] = 0xE5; // a name starting with E5 is stored as 05
    }
    let trim = |b: &[u8]| {
        let n = b.iter().rposition(|&c| c != b' ').map_or(0, |i| i + 1);
        cp437(&b[..n])
    };
    let (mut base, mut ext) = (trim(&base), trim(&e[8..11]));
    if e[12] & 0x08 != 0 {
        base = base.to_lowercase();
    }
    if e[12] & 0x10 != 0 {
        ext = ext.to_lowercase();
    }
    if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    }
}

/// Short names are in the DOS code page (437 is the common one).
fn cp437(b: &[u8]) -> String {
    const HIGH: &str = concat!(
        "ÇüéâäàåçêëèïîìÄÅ",
        "ÉæÆôöòûùÿÖÜ¢£¥₧ƒ",
        "áíóúñÑªº¿⌐¬½¼¡«»",
        "░▒▓│┤╡╢╖╕╣║╗╝╜╛┐",
        "└┴┬├─┼╞╟╚╔╩╦╠═╬╧",
        "╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀",
        "αßΓπΣσµτΦΘΩδ∞φε∩",
        "≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}",
    );
    b.iter()
        .map(|&c| {
            if c < 128 {
                c as char
            } else {
                HIGH.chars().nth(usize::from(c) - 128).unwrap_or('?')
            }
        })
        .collect()
}
