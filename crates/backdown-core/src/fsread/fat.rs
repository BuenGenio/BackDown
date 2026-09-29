//! FAT12, FAT16 and FAT32: the boot sector's geometry, cluster chains through the first FAT, and
//! folders of 32-byte entries with their long (VFAT, UTF-16) names. Deleted entries are left out
//! of listings; `deleted` lists them (see `undelete` for how sure a recovery can be): a deleted
//! entry has its first byte replaced by E5 (its long-name entries too, so their order is lost but
//! they stay in place, checksummed against the short name) and its cluster chain freed.

use std::collections::HashMap;
use std::io::{Read, Seek};

use anyhow::{Context, Result, bail};

use super::undelete::{Deleted, Spans, Verdict};
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

    /// Cluster `n`'s FAT entry as it's stored (0: free).
    fn raw<R: Read + Seek>(&mut self, r: &mut Region<R>, n: u32) -> Result<u32> {
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
        Ok(match self.bits {
            12 => {
                if n & 1 == 1 {
                    v >> 4
                } else {
                    v & 0xFFF
                }
            }
            16 => v,
            _ => v & 0x0FFF_FFFF,
        })
    }

    fn next<R: Read + Seek>(&mut self, r: &mut Region<R>, n: u32) -> Result<Next> {
        let v = self.raw(r, n)?;
        let eoc = match self.bits {
            12 => 0xFF8,
            16 => 0xFFF8,
            _ => 0x0FFF_FFF8,
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
                    deleted: None,
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
        if let Node::FatDel {
            cluster,
            size,
            dir: false,
        } = *node
        {
            let size = u64::from(size);
            let (spans, v) = self.assess(r, cluster, size)?;
            if v.state == "overwritten" {
                bail!("it can't be recovered: {}", v.why);
            }
            return Ok(Data::Runs {
                runs: spans.runs,
                size,
                valid: size,
            });
        }
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

    // ---- deleted entries

    /// Where a deleted file's `size` bytes from cluster `first` would be (the clusters one after
    /// another), with the ones in use now as holes, and how likely that's the file.
    fn assess<R: Read + Seek>(
        &mut self,
        r: &mut Region<R>,
        first: u32,
        size: u64,
    ) -> Result<(Spans, Verdict)> {
        let mut spans = Spans::default();
        if size == 0 {
            return Ok((spans, Verdict::sure("it was empty")));
        }
        if !(2..=self.max).contains(&first) {
            spans.gone(size)?;
            return Ok((
                spans,
                Verdict::lost("its first cluster is outside the volume"),
            ));
        }
        let n = size.div_ceil(self.cluster);
        for i in 0..n {
            if i % 65_536 == 0 {
                r.in_time()?;
            }
            let at = i * self.cluster;
            let len = self.cluster.min(size - at);
            let c = u64::from(first) + i;
            if c > u64::from(self.max) {
                spans.gone(size - at)?;
                break;
            }
            let used = self.raw(r, c as u32)? != 0;
            spans.push(self.data_at + (c - 2) * self.cluster, len, used)?;
        }
        let v = if n == 1 {
            spans.verdict(
                95,
                "it fitted in one cluster, and no other file has used it since",
            )
        } else {
            spans.verdict(
                70,
                "its clusters are all still free; they're taken to follow one another, as most files are written (if it was stored in pieces, parts may come back wrong)",
            )
        };
        Ok((spans, v))
    }

    /// A deleted folder's clusters: the first, if it's still free and still starts with the
    /// folder's own "." entry, then the free ones after it that still look like folder entries.
    fn deleted_dir_runs<R: Read + Seek>(
        &mut self,
        r: &mut Region<R>,
        first: u32,
    ) -> Result<Vec<Run>> {
        let reused = "what was in it has been overwritten since it was deleted";
        if !(2..=self.max).contains(&first) {
            bail!("its first cluster is outside the volume");
        }
        if self.raw(r, first)? != 0 {
            bail!("{reused}");
        }
        let (data_at, cl) = (self.data_at, self.cluster);
        let phys = move |c: u32| data_at + u64::from(c - 2) * cl;
        let head = r.read_vec(phys(first), self.cluster)?;
        let dot = &head[..32];
        let hi = if self.bits == 32 {
            le(dot, 20, 2) << 16
        } else {
            0
        };
        if &dot[..11] != b".          "
            || dot[11] & 0x10 == 0
            || (hi | le(dot, 26, 2)) as u32 != first
        {
            bail!("{reused}");
        }
        let mut runs = Vec::new();
        push_run(
            &mut runs,
            Run {
                at: 0,
                len: self.cluster,
                phys: Some(phys(first)),
            },
        )?;
        let (mut at, mut c, mut ended) = (self.cluster, first, dir_end(&head));
        while !ended && at < DIR_MAX && c < self.max {
            c += 1;
            if self.raw(r, c)? != 0 {
                break;
            }
            let b = r.read_vec(phys(c), self.cluster)?;
            if b.iter().all(|&x| x == 0) || !looks_like_dir(&b, self.max) {
                break;
            }
            ended = dir_end(&b);
            push_run(
                &mut runs,
                Run {
                    at,
                    len: self.cluster,
                    phys: Some(phys(c)),
                },
            )?;
            at += self.cluster;
        }
        Ok(runs)
    }

    /// Calls `each` for the deleted entries of a folder (every entry of a deleted one), with how
    /// likely each can be recovered, until it returns false.
    pub(crate) fn deleted<R: Read + Seek>(
        &mut self,
        r: &mut Region<R>,
        node: &Node,
        each: &mut dyn FnMut(Entry) -> bool,
    ) -> Result<Option<String>> {
        let (runs, all) = match *node {
            Node::Fat { cluster, .. } => (self.dir_runs(r, cluster)?, false),
            Node::FatDel {
                cluster, dir: true, ..
            } => (self.deleted_dir_runs(r, cluster)?, true),
            _ => bail!("not a FAT folder"),
        };
        // the entries first (a folder is at most 2 MiB), then each one looked at more closely
        let mut found: Vec<(u64, [u8; 32], Option<String>)> = Vec::new();
        let mut gone: Vec<[u8; 32]> = Vec::new(); // deleted long-name parts, as they lie
        let mut lfn = Lfn::default(); // a live long name, in a deleted folder
        let mut off = 0u64;
        let max = self.max;
        each_chunk(r, &runs, DIR_MAX, |chunk| {
            for e in chunk.as_chunks::<32>().0 {
                let at = off;
                off += 32;
                if e[0] == 0 {
                    return Ok(false);
                }
                let long_part = e[11] & 0x3F == 0x0F;
                if e[0] == 0xE5 {
                    lfn = Lfn::default();
                    if long_part {
                        if gone.len() >= 20 {
                            gone.remove(0);
                        }
                        gone.push(*e);
                        continue;
                    }
                    let parts = std::mem::take(&mut gone);
                    if plausible(e, max, true) {
                        found.push((at, *e, deleted_long_name(&parts, e)));
                    }
                    continue;
                }
                gone.clear();
                if !all {
                    continue;
                }
                // a deleted folder's entries that weren't themselves marked deleted
                if long_part {
                    lfn.add(e);
                    continue;
                }
                let long = std::mem::take(&mut lfn).name(e);
                if e[..11] == *b".          " || e[..11] == *b"..         " {
                    continue;
                }
                if plausible(e, max, false) {
                    found.push((at, *e, long));
                }
            }
            Ok(true)
        })?;
        for (at, e, long) in found {
            if r.late() {
                return Ok(Some(super::undelete::LATE.into()));
            }
            let entry = self.deleted_entry(r, &e, at, long)?;
            if !each(entry) {
                break;
            }
        }
        Ok(None)
    }

    fn deleted_entry<R: Read + Seek>(
        &mut self,
        r: &mut Region<R>,
        e: &[u8; 32],
        at: u64,
        long: Option<String>,
    ) -> Result<Entry> {
        let lost_letter = e[0] == 0xE5 && long.is_none();
        let name = long.unwrap_or_else(|| {
            let mut s = *e;
            if s[0] == 0xE5 {
                s[0] = b'_';
            }
            short_name(&s)
        });
        let name = clean_name(name).unwrap_or_else(|| "_".into());
        let dir = e[11] & 0x10 != 0;
        let hi = if self.bits == 32 {
            le(e, 20, 2) << 16
        } else {
            0
        };
        let cluster = (hi | le(e, 26, 2)) as u32;
        let size = if dir { 0 } else { le(e, 28, 4) as u32 };
        let mut v = if dir {
            match self.deleted_dir_runs(r, cluster) {
                Ok(_) => Verdict {
                    state: "recoverable",
                    confidence: 80,
                    why: "the list of what was in it is still there".into(),
                },
                Err(err) => Verdict::lost(format!("{err:#}")),
            }
        } else {
            match self.assess(r, cluster, u64::from(size)) {
                Ok((_, v)) => v,
                Err(err) => Verdict::lost(format!("its clusters can't be checked ({err:#})")),
            }
        };
        if lost_letter {
            v.why = format!(
                "{}. The first letter of its name was lost when it was deleted (shown as _)",
                v.why
            );
        }
        let mut flags = Vec::new();
        if e[11] & 0x02 != 0 {
            flags.push("hidden");
        }
        Ok(Entry {
            unreadable: (v.state == "overwritten" && !dir).then(|| v.why.clone()),
            deleted: Some(Deleted::new(&name, at, v)),
            name,
            dir,
            size: u64::from(size),
            mtime: dos_time(le(e, 24, 2), le(e, 22, 2)),
            link: None,
            flags,
            node: Node::FatDel { cluster, size, dir },
        })
    }
}

/// Whether a folder cluster has the end-of-folder mark.
fn dir_end(b: &[u8]) -> bool {
    b.as_chunks::<32>().0.iter().any(|e| e[0] == 0)
}

/// Whether a cluster looks like a folder's entries (up to its end mark).
fn looks_like_dir(b: &[u8], max: u32) -> bool {
    for e in b.as_chunks::<32>().0 {
        match e[0] {
            0 => return true,
            0xE5 => continue,
            _ if e[11] & 0x3F == 0x0F => continue,
            _ if e[..11] == *b".          " || e[..11] == *b"..         " => continue,
            _ if plausible(e, max, false) => continue,
            _ => return false,
        }
    }
    true
}

/// Whether a (deleted, or in a deleted folder) short entry looks like a real file or folder, not
/// leftover bytes: a valid 8.3 name (its first byte aside when it's deleted), sane attributes, and
/// a first cluster in the volume.
fn plausible(e: &[u8; 32], max: u32, deleted: bool) -> bool {
    let attr = e[11];
    if attr & 0xC8 != 0 {
        return false; // reserved bits, or the volume label
    }
    let ok = |c: u8| {
        c >= 0x80
            || c.is_ascii_uppercase()
            || c.is_ascii_digit()
            || b" !#$%&'()-@^_`{}~".contains(&c)
    };
    let from = usize::from(deleted);
    if !e[from..11].iter().all(|&c| ok(c)) || (!deleted && (e[0] == b' ' || !ok(e[0]))) {
        return false;
    }
    // spaces only pad the end of the name and of the extension
    let padded = |f: &[u8]| {
        let mut space = false;
        f.iter().all(|&c| {
            space |= c == b' ';
            !space || c == b' '
        })
    };
    if !padded(&e[1..8]) || !padded(&e[8..11]) {
        return false;
    }
    let cluster = ((le(e, 20, 2) << 16) | le(e, 26, 2)) as u32;
    let cluster16 = le(e, 26, 2) as u32;
    let size = le(e, 28, 4);
    let in_vol = |c: u32| (2..=max).contains(&c);
    if attr & 0x10 != 0 {
        in_vol(cluster) || in_vol(cluster16)
    } else {
        size == 0 || in_vol(cluster) || in_vol(cluster16)
    }
}

/// A deleted file's long name, from the deleted long-name entries just before its short entry
/// (their order numbers are gone, so they're taken as they lie: the nearest is the first part).
/// They must agree with each other, end properly, and fit the short name: the checksum they carry
/// fixes the short name's lost first letter, which must be what that name would start with.
fn deleted_long_name(parts: &[[u8; 32]], e: &[u8; 32]) -> Option<String> {
    if parts.is_empty() {
        return None;
    }
    let sum = parts[0][13];
    if parts
        .iter()
        .any(|p| p[13] != sum || p[12] != 0 || le(p, 26, 2) != 0)
    {
        return None;
    }
    let mut units = Vec::with_capacity(13 * parts.len());
    for p in parts.iter().rev() {
        units.extend(LFN_AT.iter().map(|&i| u16::from_le_bytes([p[i], p[i + 1]])));
    }
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    // after the end, only padding; before it, no padding; and the end is in the last part
    if units[..end].contains(&0xFFFF)
        || units
            .get(end + 1..)
            .is_some_and(|t| t.iter().any(|&u| u != 0xFFFF))
        || end + 13 <= units.len() && end != units.len()
        || end == 0
    {
        return None;
    }
    let bytes: Vec<u8> = units[..end].iter().flat_map(|u| u.to_le_bytes()).collect();
    let long = utf16le(&bytes);
    // the one first byte that makes the short name's checksum match
    let first = (0..=255u8).find(|&c| {
        let mut s = [0u8; 11];
        s[0] = c;
        s[1..].copy_from_slice(&e[1..11]);
        checksum(&s) == sum
    })?;
    let lead = long.trim_start_matches(['.', ' ']).chars().next()?;
    let fits = if lead.is_ascii() {
        let u = lead.to_ascii_uppercase() as u8;
        let valid =
            u.is_ascii_uppercase() || u.is_ascii_digit() || b"!#$%&'()-@^_`{}~".contains(&u);
        first == if valid { u } else { b'_' }
    } else {
        first >= 0x80 || first == b'_' || first.is_ascii_uppercase() || first.is_ascii_digit()
    };
    fits.then_some(long)
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
