//! ext2, ext3 and ext4: the superblock and group descriptors (32- or 64-bit, meta_bg), inodes,
//! file data by extent tree or by block map (direct and indirect blocks), inline data, and folders
//! read block by block (an htree's index blocks look like empty entries, so hashed folders read
//! the same way). The journal isn't replayed: a volume that wasn't unmounted cleanly is read as
//! its main structures stand, with a note saying so.

use std::collections::HashMap;
use std::io::{Read, Seek};

use anyhow::{Result, bail};

use super::{DIR_CAP, Data, Entry, Info, Node, Region, Run, clean_name, push_run, read_all};
use crate::magic::{field, le};

/// The root folder's inode.
pub(crate) const ROOT: u32 = 2;

const COMPAT_JOURNAL: u64 = 0x4;
const COMPAT_SPARSE_SUPER2: u64 = 0x200;
const RO_SPARSE_SUPER: u64 = 0x1;
const INCOMPAT_COMPRESSION: u64 = 0x1;
const INCOMPAT_FILETYPE: u64 = 0x2;
const INCOMPAT_RECOVER: u64 = 0x4;
const INCOMPAT_JOURNAL_DEV: u64 = 0x8;
const INCOMPAT_META_BG: u64 = 0x10;
const INCOMPAT_EXTENTS: u64 = 0x40;
const INCOMPAT_64BIT: u64 = 0x80;
const INCOMPAT_FLEX_BG: u64 = 0x200;
const INCOMPAT_LARGEDIR: u64 = 0x4000;
/// Every incompatible feature this reader knows what to do with (or can safely ignore).
const INCOMPAT_KNOWN: u64 = 0x3_F7D6;

const FL_ENCRYPT: u64 = 0x800;
const FL_EXTENTS: u64 = 0x8_0000;
const FL_INLINE: u64 = 0x1000_0000;

const S_IFMT: u64 = 0xF000;
const S_IFDIR: u64 = 0x4000;
const S_IFREG: u64 = 0x8000;
const S_IFLNK: u64 = 0xA000;

/// Inode-table blocks kept in memory.
const CACHED_BLOCKS: usize = 256;

pub(crate) struct Ext {
    block: u64,
    blocks: u64,
    inodes: u64,
    ipg: u64,
    bpg: u64,
    first_data_block: u64,
    inode_size: u64,
    desc_size: u64,
    groups: u64,
    compat: u64,
    incompat: u64,
    ro_compat: u64,
    first_meta_bg: u64,
    backup_bgs: [u64; 2],
    tables: HashMap<u64, u64>,
    cache: HashMap<u64, Vec<u8>>,
}

pub(crate) fn detect(b: &[u8]) -> bool {
    b.len() >= 2048 && le(b, 1080, 2) == 0xEF53
}

/// The parts of an inode this reader uses.
struct Inode {
    raw: Vec<u8>,
    mode: u64,
    size: u64,
    flags: u64,
    mtime: Option<i64>,
}

impl Inode {
    fn kind(&self) -> u64 {
        self.mode & S_IFMT
    }

    fn iblock(&self) -> &[u8] {
        &self.raw[0x28..0x28 + 60]
    }
}

impl Ext {
    pub(crate) fn open<R: Read + Seek>(r: &mut Region<R>, b: &[u8]) -> Result<(Ext, Info)> {
        let sb = &b[1024..2048];
        let log = le(sb, 24, 4);
        if log > 6 {
            bail!("the ext superblock's block size is out of range (damaged)");
        }
        let block = 1024u64 << log;
        let (compat, incompat, ro_compat) = (le(sb, 92, 4), le(sb, 96, 4), le(sb, 100, 4));
        if incompat & INCOMPAT_JOURNAL_DEV != 0 {
            bail!("this is an ext3/ext4 external journal: it holds no files");
        }
        if incompat & INCOMPAT_COMPRESSION != 0 {
            bail!("this ext2 volume uses the old compression patches, which can't be read here");
        }
        let inode_size = if le(sb, 76, 4) == 0 {
            128
        } else {
            le(sb, 88, 2)
        };
        if !inode_size.is_power_of_two() || !(128..=block).contains(&inode_size) {
            bail!("the ext superblock's inode size ({inode_size}) is out of range (damaged)");
        }
        let mut blocks = le(sb, 4, 4);
        let big = incompat & INCOMPAT_64BIT != 0;
        if big {
            blocks |= le(sb, 0x150, 4) << 32;
        }
        let (inodes, bpg, ipg) = (le(sb, 0, 4), le(sb, 32, 4), le(sb, 40, 4));
        let first_data_block = le(sb, 20, 4);
        if bpg == 0 || ipg == 0 || ipg > 8 * block || first_data_block >= blocks {
            bail!("the ext superblock's group sizes are out of range (damaged)");
        }
        if blocks.checked_mul(block).is_none() {
            bail!("the ext superblock's block count is implausibly large (damaged)");
        }
        let desc_size = if big { le(sb, 254, 2) } else { 32 };
        if !desc_size.is_power_of_two() || !(32..=block).contains(&desc_size) {
            bail!("the ext superblock's descriptor size ({desc_size}) is out of range (damaged)");
        }
        let ext = Ext {
            block,
            blocks,
            inodes,
            ipg,
            bpg,
            first_data_block,
            inode_size,
            desc_size,
            groups: (blocks - first_data_block).div_ceil(bpg),
            compat,
            incompat,
            ro_compat,
            first_meta_bg: le(sb, 260, 4),
            backup_bgs: [le(sb, 0x24C, 4), le(sb, 0x250, 4)],
            tables: HashMap::new(),
            cache: HashMap::new(),
        };
        let kind = if incompat & (INCOMPAT_EXTENTS | INCOMPAT_64BIT | INCOMPAT_FLEX_BG) != 0 {
            "ext4"
        } else if compat & COMPAT_JOURNAL != 0 {
            "ext3"
        } else {
            "ext2"
        };
        let mut notes = Vec::new();
        if incompat & INCOMPAT_RECOVER != 0 {
            notes.push(
                "it wasn't unmounted cleanly: changes still in its journal aren't shown (the journal isn't replayed here)"
                    .into(),
            );
        }
        if le(sb, 58, 2) & 2 != 0 {
            notes.push("it has recorded errors that haven't been repaired since".into());
        }
        if incompat & !INCOMPAT_KNOWN != 0 {
            notes.push(format!(
                "it uses features this reader doesn't know ({:#x}): some files may not read correctly",
                incompat & !INCOMPAT_KNOWN
            ));
        }
        let size = blocks * block;
        if size > r.len() {
            notes.push(format!(
                "the filesystem says it's {size} bytes but only {} are here: files near its end may not open",
                r.len()
            ));
        }
        let info = Info {
            kind: kind.into(),
            label: Some(field(sb, 120, 16)).filter(|l| !l.is_empty()),
            size,
            block,
            notes,
        };
        Ok((ext, info))
    }

    fn group_start(&self, g: u64) -> u64 {
        self.first_data_block + g * self.bpg
    }

    /// Whether group `g` keeps a copy of the superblock (and the descriptors after it).
    fn has_super(&self, g: u64) -> bool {
        if g == 0 {
            return true;
        }
        if self.compat & COMPAT_SPARSE_SUPER2 != 0 {
            return self.backup_bgs.contains(&g);
        }
        if g == 1 || self.ro_compat & RO_SPARSE_SUPER == 0 {
            return true;
        }
        g & 1 == 1
            && [3u64, 5, 7].iter().any(|&p| {
                let mut n = p;
                while n < g {
                    n *= p;
                }
                n == g
            })
    }

    /// Group `g`'s inode table (a block number).
    fn table<R: Read + Seek>(&mut self, r: &mut Region<R>, g: u64) -> Result<u64> {
        if let Some(&t) = self.tables.get(&g) {
            return Ok(t);
        }
        if g >= self.groups {
            bail!("block group {g} doesn't exist (damaged)");
        }
        let per_block = self.block / self.desc_size;
        let at = if self.incompat & INCOMPAT_META_BG != 0 && g / per_block >= self.first_meta_bg {
            let first = g / per_block * per_block;
            let blk = self.group_start(first) + u64::from(self.has_super(first));
            blk * self.block + (g % per_block) * self.desc_size
        } else {
            (self.first_data_block + 1) * self.block + g * self.desc_size
        };
        let d = r.read_vec(at, self.desc_size)?;
        let mut t = le(&d, 8, 4);
        if self.desc_size >= 64 && self.incompat & INCOMPAT_64BIT != 0 {
            t |= le(&d, 0x28, 4) << 32;
        }
        if t == 0 || t >= self.blocks {
            bail!("block group {g}'s inode table is outside the volume (damaged)");
        }
        if self.tables.len() >= 1 << 16 {
            self.tables.clear();
        }
        self.tables.insert(g, t);
        Ok(t)
    }

    fn inode<R: Read + Seek>(&mut self, r: &mut Region<R>, n: u32) -> Result<Inode> {
        let n64 = u64::from(n);
        if n == 0 || n64 > self.inodes {
            bail!("inode {n} is out of range (damaged)");
        }
        let (g, i) = ((n64 - 1) / self.ipg, (n64 - 1) % self.ipg);
        let off = self.table(r, g)? * self.block + i * self.inode_size;
        let blk = off / self.block;
        if blk >= self.blocks {
            bail!("inode {n} is outside the volume (damaged)");
        }
        if !self.cache.contains_key(&blk) {
            if self.cache.len() >= CACHED_BLOCKS {
                self.cache.clear();
            }
            let bytes = r.read_vec(blk * self.block, self.block)?;
            self.cache.insert(blk, bytes);
        }
        let at = (off % self.block) as usize;
        let raw = self.cache[&blk][at..at + self.inode_size as usize].to_vec();
        let mode = le(&raw, 0, 2);
        let mut size = le(&raw, 4, 4);
        if mode & S_IFMT == S_IFREG || self.incompat & INCOMPAT_LARGEDIR != 0 {
            size |= le(&raw, 0x6C, 4) << 32;
        }
        let mut mtime = i64::from(le(&raw, 0x10, 4) as u32 as i32);
        if self.inode_size > 128 && le(&raw, 0x80, 2) >= 12 {
            mtime += ((le(&raw, 0x88, 4) & 3) as i64) << 32; // past 2038
        }
        Ok(Inode {
            mode,
            size,
            flags: le(&raw, 0x20, 4),
            mtime: (mtime != 0).then_some(mtime),
            raw,
        })
    }

    /// An extended attribute kept in the inode itself.
    fn xattr<'a>(&self, raw: &'a [u8], index: u8, name: &[u8]) -> Option<&'a [u8]> {
        if self.inode_size <= 128 {
            return None;
        }
        let start = 128 + le(raw, 0x80, 2) as usize;
        if le(raw, start, 4) != 0xEA02_0000 {
            return None;
        }
        let base = start + 4;
        let mut e = base;
        while e + 16 <= raw.len() && le(raw, e, 4) != 0 {
            let nl = usize::from(raw[e]);
            if e + 16 + nl > raw.len() {
                break;
            }
            if raw[e + 1] == index && &raw[e + 16..e + 16 + nl] == name && le(raw, e + 4, 4) == 0 {
                let (off, n) = (le(raw, e + 2, 2) as usize, le(raw, e + 8, 4) as usize);
                return raw.get(base + off..base + off + n);
            }
            e += (16 + nl + 3) & !3;
        }
        None
    }

    /// Inline data: the 60 bytes of i_block, then the "system.data" attribute.
    fn inline(&self, i: &Inode) -> Vec<u8> {
        let mut out = i.iblock().to_vec();
        if let Some(more) = self.xattr(&i.raw, 7, b"data") {
            out.extend_from_slice(more);
        }
        out.truncate(i.size.min(out.len() as u64) as usize);
        out
    }

    /// Where a file's first `limit` bytes are.
    fn data_of<R: Read + Seek>(
        &mut self,
        r: &mut Region<R>,
        i: &Inode,
        limit: u64,
    ) -> Result<Data> {
        if i.flags & FL_INLINE != 0 {
            let mut b = self.inline(i);
            b.truncate(limit.min(b.len() as u64) as usize);
            return Ok(Data::Bytes(b));
        }
        let size = i.size.min(limit);
        let nblocks = size.div_ceil(self.block);
        let mut runs = Vec::new();
        if i.flags & FL_EXTENTS != 0 {
            let mut visits = nblocks.div_ceil(300) + 64;
            self.extents(r, i.iblock(), nblocks, None, &mut runs, &mut visits)?;
        } else {
            let per = self.block / 4;
            let mut budget = nblocks.div_ceil(per) + nblocks.div_ceil(per * per) + 3;
            for k in 0..12u64.min(nblocks) {
                self.map_block(le(i.iblock(), 4 * k as usize, 4), k, &mut runs)?;
            }
            let (mut first, mut span) = (12u64, per);
            for level in 1..=3u32 {
                if first >= nblocks {
                    break;
                }
                let ptr = le(i.iblock(), 4 * (11 + level as usize), 4);
                self.indirect(r, ptr, level, first, nblocks, &mut runs, &mut budget)?;
                first += span;
                span *= per;
            }
        }
        Ok(Data::Runs {
            runs,
            size,
            valid: size,
        })
    }

    fn extents<R: Read + Seek>(
        &mut self,
        r: &mut Region<R>,
        node: &[u8],
        nblocks: u64,
        depth: Option<u64>,
        out: &mut Vec<Run>,
        visits: &mut u64,
    ) -> Result<()> {
        if *visits == 0 {
            bail!("the extent tree is larger than the file needs (damaged)");
        }
        *visits -= 1;
        let (n, max, d) = (le(node, 2, 2), le(node, 4, 2), le(node, 6, 2));
        let fits = (node.len() as u64 - 12) / 12;
        if le(node, 0, 2) != 0xF30A
            || n > max
            || max > fits
            || d > 5
            || depth.is_some_and(|x| x != d)
        {
            bail!("a damaged extent tree");
        }
        for k in 0..n as usize {
            let e = &node[12 + 12 * k..24 + 12 * k];
            let lblk = le(e, 0, 4);
            if lblk >= nblocks {
                break;
            }
            if d == 0 {
                let (mut len, start) = (le(e, 4, 2), le(e, 8, 4) | le(e, 6, 2) << 32);
                let uninit = len > 32768;
                if uninit {
                    len -= 32768; // allocated but never written: reads as zeros
                }
                if start.saturating_add(len) > self.blocks {
                    bail!("an extent points outside the volume (damaged)");
                }
                let len = len.min(nblocks - lblk);
                push_run(
                    out,
                    Run {
                        at: lblk * self.block,
                        len: len * self.block,
                        phys: (!uninit).then_some(start * self.block),
                    },
                )?;
            } else {
                let child = le(e, 4, 4) | le(e, 8, 2) << 32;
                if child == 0 || child >= self.blocks {
                    bail!("an extent index points outside the volume (damaged)");
                }
                let b = r.read_vec(child * self.block, self.block)?;
                self.extents(r, &b, nblocks, Some(d - 1), out, visits)?;
            }
        }
        Ok(())
    }

    fn map_block(&self, p: u64, lblk: u64, out: &mut Vec<Run>) -> Result<()> {
        if p == 0 {
            return Ok(()); // a hole
        }
        if p >= self.blocks {
            bail!("a block pointer points outside the volume (damaged)");
        }
        push_run(
            out,
            Run {
                at: lblk * self.block,
                len: self.block,
                phys: Some(p * self.block),
            },
        )
    }

    /// An indirect block at `level` (1: it lists data blocks) mapping from logical block `first`.
    #[allow(clippy::too_many_arguments)]
    fn indirect<R: Read + Seek>(
        &mut self,
        r: &mut Region<R>,
        ptr: u64,
        level: u32,
        first: u64,
        nblocks: u64,
        out: &mut Vec<Run>,
        budget: &mut u64,
    ) -> Result<()> {
        if ptr == 0 {
            return Ok(());
        }
        if ptr >= self.blocks {
            bail!("a block pointer points outside the volume (damaged)");
        }
        if *budget == 0 {
            bail!("the block map is larger than the file needs (damaged)");
        }
        *budget -= 1;
        let b = r.read_vec(ptr * self.block, self.block)?;
        let per = self.block / 4;
        let span = per.pow(level - 1);
        for k in 0..per {
            let at = first + k * span;
            if at >= nblocks {
                break;
            }
            let p = le(&b, 4 * k as usize, 4);
            if level == 1 {
                self.map_block(p, at, out)?;
            } else {
                self.indirect(r, p, level - 1, at, nblocks, out, budget)?;
            }
        }
        Ok(())
    }

    fn link_target<R: Read + Seek>(&mut self, r: &mut Region<R>, i: &Inode) -> Result<String> {
        let size = i.size.min(4096);
        let data_blocks = le(&i.raw, 0x1C, 4).saturating_sub(if le(&i.raw, 0x68, 4) != 0 {
            self.block / 512
        } else {
            0
        });
        let bytes = if i.flags & FL_INLINE != 0 {
            self.inline(i)
        } else if size < 60 && data_blocks == 0 {
            i.iblock()[..size as usize].to_vec() // a "fast" link, kept in the inode
        } else {
            let data = self.data_of(r, i, size)?;
            read_all(r, data, size)?
        };
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn entry<R: Read + Seek>(&mut self, r: &mut Region<R>, name: String, ino: u32) -> Entry {
        let mut e = Entry {
            name,
            dir: false,
            size: 0,
            mtime: None,
            link: None,
            unreadable: None,
            flags: Vec::new(),
            node: Node::Ext(ino),
        };
        let i = match self.inode(r, ino) {
            Ok(i) => i,
            Err(err) => {
                e.unreadable = Some(format!("its inode can't be read: {err:#}"));
                return e;
            }
        };
        e.mtime = i.mtime;
        match i.kind() {
            S_IFDIR => e.dir = true,
            S_IFREG => e.size = i.size,
            S_IFLNK => {
                e.link = Some(
                    self.link_target(r, &i)
                        .unwrap_or_else(|err| format!("(unreadable: {err:#})")),
                );
            }
            0x2000 | 0x6000 => e.unreadable = Some("a device file: it has no contents".into()),
            0x1000 => e.unreadable = Some("a named pipe: it has no contents".into()),
            0xC000 => e.unreadable = Some("a socket: it has no contents".into()),
            _ => e.unreadable = Some("an inode of no known kind (damaged)".into()),
        }
        if i.flags & FL_ENCRYPT != 0 {
            e.flags.push("encrypted");
            if !e.dir {
                e.unreadable = Some("encrypted (fscrypt): it can't be read without its key".into());
            }
        }
        if i.flags & FL_INLINE != 0 {
            e.flags.push("inline");
        }
        e
    }

    /// The entries in one block of a folder (or one inline area).
    fn dirents(&self, b: &[u8], out: &mut Vec<(String, u32)>) {
        let mut off = 0usize;
        while off + 8 <= b.len() {
            let ino = le(b, off, 4) as u32;
            let raw = le(b, off + 4, 2) as usize;
            let rec = if self.block < 65536 {
                raw
            } else if raw == 65535 || raw == 0 {
                self.block as usize
            } else {
                (raw & 65532) | ((raw & 3) << 16)
            };
            if rec < 8 || rec % 4 != 0 || off + rec > b.len() {
                break; // damaged: the rest of this block is skipped
            }
            let nl = if self.incompat & INCOMPAT_FILETYPE != 0 {
                usize::from(b[off + 6])
            } else {
                le(b, off + 6, 2) as usize
            };
            if ino != 0 && nl > 0 && 8 + nl <= rec {
                let name = String::from_utf8_lossy(&b[off + 8..off + 8 + nl]).into_owned();
                out.push((name, ino));
            }
            off += rec;
        }
    }

    pub(crate) fn read_dir<R: Read + Seek>(
        &mut self,
        r: &mut Region<R>,
        node: &Node,
        want: Option<&str>,
        each: &mut dyn FnMut(Entry) -> bool,
    ) -> Result<()> {
        let Node::Ext(ino) = *node else {
            bail!("not an ext folder");
        };
        let d = self.inode(r, ino)?;
        if d.kind() != S_IFDIR {
            bail!("inode {ino} isn't a folder (damaged)");
        }
        let mut names = Vec::new();
        let mut emit = |this: &mut Ext, r: &mut Region<R>, names: &mut Vec<(String, u32)>| {
            for (name, ino) in names.drain(..) {
                let Some(name) = clean_name(name) else {
                    continue;
                };
                if want.is_some_and(|w| w != name) {
                    continue;
                }
                if !each(this.entry(r, name, ino)) {
                    return false;
                }
            }
            true
        };
        if d.flags & FL_INLINE != 0 {
            // the parent's inode number, then entries; more in the "system.data" attribute
            self.dirents(&d.iblock()[4..], &mut names);
            if let Some(more) = self.xattr(&d.raw, 7, b"data") {
                self.dirents(more, &mut names);
            }
            emit(self, r, &mut names);
            return Ok(());
        }
        let Data::Runs { runs, size, .. } = self.data_of(r, &d, DIR_CAP)? else {
            return Ok(());
        };
        let mut block = vec![0u8; self.block as usize];
        for run in runs {
            let Some(phys) = run.phys else { continue };
            let mut off = 0;
            while off < run.len && run.at + off < size {
                r.read_exact_at(phys + off, &mut block)?;
                self.dirents(&block, &mut names);
                if !emit(self, r, &mut names) {
                    return Ok(());
                }
                off += self.block;
            }
        }
        Ok(())
    }

    pub(crate) fn data<R: Read + Seek>(&mut self, r: &mut Region<R>, node: &Node) -> Result<Data> {
        let Node::Ext(ino) = *node else {
            bail!("not an ext file");
        };
        let i = self.inode(r, ino)?;
        if i.kind() != S_IFREG {
            bail!("inode {ino} isn't a file (damaged)");
        }
        self.data_of(r, &i, u64::MAX)
    }
}
