//! exFAT: the boot sector's geometry, clusters by chain (through the FAT) or contiguous (the
//! "no FAT chain" flag), and folders of entry sets (a file entry, a stream extension, name
//! entries). Deleted entry sets are skipped.

use std::collections::HashMap;
use std::io::{Read, Seek};

use anyhow::{Result, bail};

use super::{
    DIR_CAP, Data, Entry, Info, Node, Region, Run, clean_name, dos_time, each_chunk, looped,
    push_run, utf16le,
};
use crate::magic::le;

const WINDOW: u64 = 64 << 10;
const WINDOWS: usize = 64;

pub(crate) struct Exfat {
    cluster: u64,
    fat_at: u64,
    fat_len: u64,
    heap_at: u64,
    /// The highest valid cluster number.
    max: u32,
    root: u32,
    cache: HashMap<u64, Vec<u8>>,
}

pub(crate) fn detect(b: &[u8]) -> bool {
    b.len() >= 512 && &b[3..11] == b"EXFAT   "
}

impl Exfat {
    pub(crate) fn open<R: Read + Seek>(r: &mut Region<R>, b: &[u8]) -> Result<(Exfat, Info)> {
        let (ss, cs) = (u32::from(b[108]), u32::from(b[109]));
        if !(9..=12).contains(&ss) || ss + cs > 25 {
            bail!("the exFAT boot sector's sizes are out of range (damaged)");
        }
        let sector = 1u64 << ss;
        let cluster = sector << cs;
        let (fat_off, fat_len) = (le(b, 80, 4), le(b, 84, 4));
        let (heap, count, root) = (le(b, 88, 4), le(b, 92, 4), le(b, 96, 4) as u32);
        if fat_off == 0 || fat_len == 0 || heap == 0 || count == 0 {
            bail!("the exFAT boot sector's layout is missing (damaged)");
        }
        // with two FATs (TexFAT), the flags say which one is in use
        let second = b[110] == 2 && le(b, 106, 2) & 1 != 0;
        let fat_at = (fat_off + if second { fat_len } else { 0 }) * sector;
        let max = (count + 1).min(fat_len * sector / 4 - 1).min(0xFFFF_FFF6) as u32;
        if !(2..=max).contains(&root) {
            bail!("the exFAT root folder is at cluster {root}, outside the volume (damaged)");
        }
        let mut x = Exfat {
            cluster,
            fat_at,
            fat_len: fat_len * sector,
            heap_at: heap * sector,
            max,
            root,
            cache: HashMap::new(),
        };
        let mut notes = Vec::new();
        if le(b, 106, 2) & 2 != 0 {
            notes.push(
                "it wasn't removed safely last time (marked dirty): recent changes may be incomplete"
                    .into(),
            );
        }
        let size = le(b, 72, 8).saturating_mul(sector);
        if size > r.len() {
            notes.push(format!(
                "the filesystem says it's {size} bytes but only {} are here: files near its end may not open",
                r.len()
            ));
        }
        let label = x.label(r).ok().flatten();
        let info = Info {
            kind: "exFAT".into(),
            label,
            size,
            block: cluster,
            notes,
        };
        Ok((x, info))
    }

    /// The root folder has no stream extension: its chain is followed to its end.
    pub(crate) fn root(&self) -> Node {
        Node::Exfat {
            cluster: self.root,
            len: DIR_CAP,
            valid: 0,
            contiguous: false,
        }
    }

    fn label<R: Read + Seek>(&mut self, r: &mut Region<R>) -> Result<Option<String>> {
        let runs = self.chain(r, self.root, self.cluster)?.0;
        let mut label = None;
        each_chunk(r, &runs, self.cluster, |chunk| {
            for e in chunk.as_chunks::<32>().0 {
                match e[0] {
                    0 => return Ok(false),
                    0x83 => {
                        let n = usize::from(e[1]).min(11);
                        label = Some(utf16le(&e[2..2 + 2 * n])).filter(|l| !l.is_empty());
                        return Ok(false);
                    }
                    _ => {}
                }
            }
            Ok(true)
        })?;
        Ok(label)
    }

    fn fat_entry<R: Read + Seek>(&mut self, r: &mut Region<R>, n: u32) -> Result<u32> {
        let off = u64::from(n) * 4;
        if off + 4 > self.fat_len {
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
        Ok(le(&self.cache[&w], (off % WINDOW) as usize, 4) as u32)
    }

    fn phys(&self, c: u32) -> u64 {
        self.heap_at + u64::from(c - 2) * self.cluster
    }

    /// The chain from `first` until it ends or covers `want` bytes, and why it broke off if it
    /// did before either.
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
            push_run(
                &mut runs,
                Run {
                    at,
                    len: self.cluster,
                    phys: Some(self.phys(c)),
                },
            )?;
            at += self.cluster;
            if at >= want {
                return Ok(looped(runs, at, None));
            }
            match self.fat_entry(r, c)? {
                0xFFFF_FFFF => return Ok(looped(runs, at, None)),
                n if (2..=self.max).contains(&n) => c = n,
                n => {
                    return Ok((
                        runs,
                        at,
                        Some(format!(
                            "its cluster chain breaks off (a FAT entry of {n:#x})"
                        )),
                    ));
                }
            }
        }
        Ok((runs, at, Some("its cluster chain loops".into())))
    }

    /// The runs holding `len` bytes from `first`: contiguous, or by the FAT.
    fn runs<R: Read + Seek>(
        &mut self,
        r: &mut Region<R>,
        first: u32,
        len: u64,
        contiguous: bool,
    ) -> Result<(Vec<Run>, u64, Option<String>)> {
        if !contiguous {
            return self.chain(r, first, len);
        }
        let clusters = len.div_ceil(self.cluster);
        if first < 2 || u64::from(first) + clusters > u64::from(self.max) + 1 {
            return Ok((
                Vec::new(),
                0,
                Some("its clusters lie outside the volume".into()),
            ));
        }
        let run = Run {
            at: 0,
            len: clusters * self.cluster,
            phys: Some(self.phys(first)),
        };
        Ok((vec![run], run.len, None))
    }

    pub(crate) fn read_dir<R: Read + Seek>(
        &mut self,
        r: &mut Region<R>,
        node: &Node,
        want: Option<&str>,
        each: &mut dyn FnMut(Entry) -> bool,
    ) -> Result<()> {
        let Node::Exfat {
            cluster,
            len,
            contiguous,
            ..
        } = *node
        else {
            bail!("not an exFAT folder");
        };
        if len == 0 {
            return Ok(()); // a folder without clusters: damaged, so empty
        }
        let len = len.min(DIR_CAP);
        let (runs, _, _) = self.runs(r, cluster, len, contiguous)?;
        let mut set: Vec<[u8; 32]> = Vec::new();
        let mut need = 0usize;
        each_chunk(r, &runs, len, |chunk| {
            for e in chunk.as_chunks::<32>().0 {
                if e[0] == 0 {
                    return Ok(false); // the end of the folder
                }
                if e[0] == 0x85 {
                    set.clear();
                    need = 1 + usize::from(e[1]);
                    if !(3..=19).contains(&need) {
                        need = 0;
                        continue;
                    }
                } else if need == 0 || e[0] & 0xC0 != 0xC0 {
                    // deleted or unused (the top bit clear), another kind of entry, or a
                    // secondary entry outside a set
                    need = 0;
                    continue;
                }
                set.push(*e);
                if set.len() < need {
                    continue;
                }
                need = 0;
                if let Some(entry) = entry_set(&set)
                    && want.is_none_or(|w| w == entry.name)
                    && !each(entry)
                {
                    return Ok(false);
                }
            }
            Ok(true)
        })
    }

    pub(crate) fn data<R: Read + Seek>(&mut self, r: &mut Region<R>, node: &Node) -> Result<Data> {
        let Node::Exfat {
            cluster,
            len,
            valid,
            contiguous,
        } = *node
        else {
            bail!("not an exFAT file");
        };
        if len == 0 {
            return Ok(Data::Bytes(Vec::new()));
        }
        let (runs, got, why) = self.runs(r, cluster, len, contiguous)?;
        if got < len {
            bail!(
                "only {got} of its {len} bytes can be found: {} (damaged)",
                why.unwrap_or_else(|| "its cluster chain ends early".into())
            );
        }
        Ok(Data::Runs {
            runs,
            size: len,
            valid: valid.min(len),
        })
    }
}

/// A file entry set (file, stream extension, names) as an entry.
fn entry_set(set: &[[u8; 32]]) -> Option<Entry> {
    let (file, stream) = (&set[0], set.get(1)?);
    if stream[0] != 0xC0 {
        return None;
    }
    let name_len = usize::from(stream[3]);
    let mut raw = Vec::with_capacity(name_len * 2);
    for n in set[2..].iter().filter(|n| n[0] == 0xC1) {
        raw.extend_from_slice(&n[2..32]);
    }
    raw.truncate(name_len * 2);
    let name = clean_name(utf16le(&raw))?;
    let attr = le(file, 4, 2);
    let dir = attr & 0x10 != 0;
    let (len, valid) = (le(stream, 24, 8), le(stream, 8, 8));
    // the modified time, with its 10 ms part and (when marked valid) its offset from UTC
    let ts = le(file, 12, 4);
    let mut mtime = dos_time(ts >> 16, ts & 0xFFFF).map(|t| t + i64::from(file[21] / 100));
    if file[23] & 0x80 != 0 {
        let quarter = i64::from(((file[23] << 1) as i8) >> 1);
        mtime = mtime.map(|t| t - quarter * 15 * 60);
    }
    let mut flags = Vec::new();
    if attr & 0x02 != 0 {
        flags.push("hidden");
    }
    if attr & 0x04 != 0 {
        flags.push("system");
    }
    Some(Entry {
        name,
        dir,
        size: if dir { 0 } else { len },
        mtime,
        link: None,
        unreadable: None,
        flags,
        node: Node::Exfat {
            cluster: le(stream, 20, 4) as u32,
            len,
            valid,
            contiguous: stream[1] & 2 != 0,
        },
    })
}
