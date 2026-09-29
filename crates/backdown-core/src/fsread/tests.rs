//! Tests over real filesystem images: made by mkfs.* where it's installed (a test whose tool is
//! missing says so and skips), filled here where no tool can do it without mounting (FAT and
//! exFAT, checked by fsck where that's installed), and damaged on purpose to check that damage
//! gives errors, never panics or hangs.

use super::*;
use crate::magic::le;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---- helpers

fn tool(name: &str) -> Option<PathBuf> {
    [
        "/usr/sbin",
        "/sbin",
        "/usr/bin",
        "/bin",
        "/usr/local/sbin",
        "/usr/local/bin",
    ]
    .iter()
    .map(|d| Path::new(d).join(name))
    .find(|p| p.exists())
}

macro_rules! need {
    ($name:expr) => {
        match tool($name) {
            Some(p) => p,
            None => {
                eprintln!("skipping: {} isn't installed", $name);
                return;
            }
        }
    };
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().expect("run a tool");
    assert!(
        out.status.success(),
        "{cmd:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Deterministic noise (xorshift64*), for contents and for damage.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn noise(seed: u64, n: usize) -> Vec<u8> {
    let mut r = Rng(seed | 1);
    (0..n).map(|_| r.next() as u8).collect()
}

type Mem = Volume<Cursor<Vec<u8>>>;

fn open(img: &[u8]) -> Mem {
    Volume::open(Cursor::new(img.to_vec()), 0, img.len() as u64).unwrap()
}

fn names(v: &mut Mem, path: &str) -> Vec<String> {
    let mut n: Vec<String> = v
        .list_path(path, MAX_LIST)
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.name)
        .collect();
    n.sort();
    n
}

fn sorted(v: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = v.iter().map(|s| s.to_string()).collect();
    v.sort();
    v
}

fn read(v: &mut Mem, path: &str) -> Vec<u8> {
    let e = v.find(path).unwrap();
    let mut out = Vec::new();
    v.reader(&e).unwrap().read_to_end(&mut out).unwrap();
    assert_eq!(out.len() as u64, e.size, "{path}: the listing's size");
    out
}

/// Everything reachable from the root (up to `max` entries), each file read up to `cap` bytes:
/// what a person browsing does, and what must never panic or hang whatever the image holds.
fn walk<R: Read + Seek>(v: &mut Volume<R>, max: usize, cap: u64) -> usize {
    let mut todo = vec![(String::new(), 0usize)];
    let mut seen = 0;
    while let Some((path, depth)) = todo.pop() {
        let Ok(dir) = v.find(&path) else { continue };
        let Ok(list) = v.list(&dir, 64) else { continue };
        for e in list.entries {
            seen += 1;
            if seen > max {
                return seen;
            }
            let p = if path.is_empty() {
                e.name.clone()
            } else {
                format!("{path}/{}", e.name)
            };
            if e.dir {
                if depth < 6 {
                    todo.push((p, depth + 1));
                }
            } else if let Ok(mut rd) = v.reader(&e) {
                let mut sink = Vec::new();
                let _ = (&mut rd).take(cap).read_to_end(&mut sink);
            }
        }
    }
    seen
}

/// Flips bytes (mostly near the metadata at `hot`) and sometimes cuts the image short, then
/// opens and walks it: every outcome is fine except a panic or a hang. FSREAD_FUZZ=<rounds> runs
/// longer.
fn survive_damage(img: &[u8], hot: &[u64], rounds: u64) {
    let rounds = std::env::var("FSREAD_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(rounds);
    let mut rng = Rng(0x5EED_0000 + img.len() as u64);
    let mut bad = img.to_vec();
    let len = bad.len() as u64;
    let (mut opened, mut walked) = (0, 0);
    for round in 0..rounds {
        let mut undo = Vec::new();
        for _ in 0..1 + rng.below(24) {
            let at = match rng.below(4) {
                0 => rng.below(len),
                1 => rng.below(8192.min(len)),
                _ => {
                    let h = hot[rng.below(hot.len() as u64) as usize];
                    (h + rng.below(16384)).min(len - 1)
                }
            } as usize;
            undo.push((at, bad[at]));
            bad[at] = match rng.below(3) {
                0 => 0,
                1 => 0xFF,
                _ => rng.next() as u8,
            };
        }
        let cut = if round % 7 == 3 {
            rng.below(len) as usize
        } else {
            bad.len()
        };
        if let Ok(mut v) = Volume::open(Cursor::new(&bad[..cut]), 0, len) {
            opened += 1;
            walked += walk(&mut v, 400, 1 << 16);
        }
        for (at, b) in undo.into_iter().rev() {
            bad[at] = b;
        }
    }
    eprintln!("damage: {rounds} rounds, {opened} opened, {walked} entries walked");
}

// ---- FAT: a formatter and a writer, since no tool here fills an image without mounting it

fn put(b: &mut [u8], at: usize, v: u64, n: usize) {
    b[at..at + n].copy_from_slice(&v.to_le_bytes()[..n]);
}

/// A blank FAT12/16/32 volume, 512-byte sectors, `spc` sectors per cluster.
fn format_fat(bits: u32, size: usize, spc: usize) -> Vec<u8> {
    let (bps, total) = (512usize, size / 512);
    let (reserved, root_entries) = match bits {
        32 => (32, 0),
        16 => (1, 512),
        _ => (1, 224),
    };
    let root_secs = root_entries * 32 / bps;
    let mut fat_secs = 1;
    loop {
        let clusters = (total - reserved - root_secs - 2 * fat_secs) / spc;
        let need = ((clusters + 2) * bits as usize).div_ceil(8).div_ceil(bps);
        if need <= fat_secs {
            break;
        }
        fat_secs = need;
    }
    let mut img = vec![0u8; size];
    img[..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
    img[3..11].copy_from_slice(b"BDTEST  ");
    put(&mut img, 11, bps as u64, 2);
    img[13] = spc as u8;
    put(&mut img, 14, reserved as u64, 2);
    img[16] = 2;
    put(&mut img, 17, root_entries as u64, 2);
    if total < 65536 && bits != 32 {
        put(&mut img, 19, total as u64, 2);
    } else {
        put(&mut img, 32, total as u64, 4);
    }
    img[21] = 0xF8;
    put(&mut img, 24, 32, 2);
    put(&mut img, 26, 64, 2);
    let label: &[u8; 11] = b"BD TEST    ";
    if bits == 32 {
        put(&mut img, 36, fat_secs as u64, 4);
        put(&mut img, 44, 2, 4);
        put(&mut img, 48, 1, 2);
        put(&mut img, 50, 6, 2);
        img[64] = 0x80;
        img[66] = 0x29;
        put(&mut img, 67, 0x1234_5678, 4);
        img[71..82].copy_from_slice(label);
        img[82..90].copy_from_slice(b"FAT32   ");
        // FSInfo, free count unknown
        put(&mut img, 512, 0x4161_5252, 4);
        put(&mut img, 512 + 484, 0x6141_7272, 4);
        put(&mut img, 512 + 488, 0xFFFF_FFFF, 4);
        put(&mut img, 512 + 492, 0xFFFF_FFFF, 4);
        put(&mut img, 512 + 508, 0xAA55_0000, 4);
    } else {
        put(&mut img, 22, fat_secs as u64, 2);
        img[36] = 0x80;
        img[38] = 0x29;
        put(&mut img, 39, 0x1234_5678, 4);
        img[43..54].copy_from_slice(label);
        img[54..62].copy_from_slice(if bits == 12 { b"FAT12   " } else { b"FAT16   " });
    }
    img[510] = 0x55;
    img[511] = 0xAA;
    if bits == 32 {
        let boot = img[..1024].to_vec();
        img[6 * 512..6 * 512 + 1024].copy_from_slice(&boot);
    }
    let mut w = FatW::new(img);
    let eoc = w.eoc();
    w.set(0, 0xFFFF_FFF8 & eoc);
    w.set(1, eoc);
    if bits == 32 {
        w.set(2, eoc);
    }
    // the label again, as the root folder's first entry
    let mut e = [0u8; 32];
    e[..11].copy_from_slice(label);
    e[11] = 0x08;
    put(&mut e, 22, DOS_TIME, 2);
    put(&mut e, 24, DOS_DATE, 2);
    w.put_entry(0, &e);
    w.img
}

/// Fills a FAT volume: folders, files (optionally fragmented), long names, deleted entries.
struct FatW {
    img: Vec<u8>,
    bits: u32,
    cluster: usize,
    fat_at: usize,
    fat_len: usize,
    fats: usize,
    root_at: usize,
    root_len: usize,
    data_at: usize,
    max: u32,
    /// Leave a free cluster between the ones handed out (files come out fragmented).
    gaps: bool,
    /// Each folder's clusters and used 32-byte slots (the root is 0).
    dirs: HashMap<u32, (Vec<u32>, usize)>,
    shorts: HashSet<(u32, [u8; 11])>,
}

/// 2024-05-06 07:08:10 as a DOS date and time.
const DOS_DATE: u64 = (44 << 9) | (5 << 5) | 6;
const DOS_TIME: u64 = (7 << 11) | (8 << 5) | 5;
const DOS_WHEN: i64 = 1_714_979_290;

impl FatW {
    fn new(img: Vec<u8>) -> FatW {
        let b = &img;
        let bps = le(b, 11, 2) as usize;
        let spc = b[13] as usize;
        let reserved = le(b, 14, 2) as usize;
        let fats = b[16] as usize;
        let root_entries = le(b, 17, 2) as usize;
        let total = match le(b, 19, 2) {
            0 => le(b, 32, 4),
            n => n,
        } as usize;
        let fat32 = le(b, 22, 2) == 0;
        let fat_secs = if fat32 { le(b, 36, 4) } else { le(b, 22, 2) } as usize;
        let root_secs = (root_entries * 32).div_ceil(bps);
        let data_sec = reserved + fats * fat_secs + root_secs;
        let clusters = (total - data_sec) / spc;
        let bits = if fat32 {
            32
        } else if clusters < 4085 {
            12
        } else {
            16
        };
        let root_cluster = if fat32 { le(b, 44, 4) as u32 } else { 0 };
        let mut w = FatW {
            bits,
            cluster: bps * spc,
            fat_at: reserved * bps,
            fat_len: fat_secs * bps,
            fats,
            root_at: (reserved + fats * fat_secs) * bps,
            root_len: root_secs * bps,
            data_at: data_sec * bps,
            max: clusters as u32 + 1,
            gaps: false,
            dirs: HashMap::new(),
            shorts: HashSet::new(),
            img,
        };
        // the root's used slots (mkfs may have put the volume label there)
        let (start, len) = if fat32 {
            (w.off(root_cluster), w.cluster)
        } else {
            (w.root_at, w.root_len)
        };
        let used = w.img[start..start + len]
            .chunks(32)
            .take_while(|e| e[0] != 0)
            .count();
        let chain = if fat32 { vec![root_cluster] } else { vec![] };
        w.dirs.insert(0, (chain, used));
        w
    }

    fn eoc(&self) -> u32 {
        match self.bits {
            12 => 0xFFF,
            16 => 0xFFFF,
            _ => 0x0FFF_FFFF,
        }
    }

    fn get(&self, c: u32) -> u32 {
        let c = c as usize;
        let f = &self.img[self.fat_at..];
        match self.bits {
            12 => {
                let v = le(f, c + c / 2, 2) as u32;
                if c & 1 == 1 { v >> 4 } else { v & 0xFFF }
            }
            16 => le(f, c * 2, 2) as u32,
            _ => le(f, c * 4, 4) as u32 & 0x0FFF_FFFF,
        }
    }

    fn set(&mut self, c: u32, v: u32) {
        let c = c as usize;
        for k in 0..self.fats {
            let base = self.fat_at + k * self.fat_len;
            let f = &mut self.img[base..base + self.fat_len];
            match self.bits {
                12 => {
                    let at = c + c / 2;
                    if c & 1 == 1 {
                        f[at] = (f[at] & 0x0F) | ((v << 4) as u8 & 0xF0);
                        f[at + 1] = (v >> 4) as u8;
                    } else {
                        f[at] = v as u8;
                        f[at + 1] = (f[at + 1] & 0xF0) | ((v >> 8) as u8 & 0x0F);
                    }
                }
                16 => put(f, c * 2, u64::from(v), 2),
                _ => put(f, c * 4, u64::from(v), 4),
            }
        }
    }

    fn off(&self, c: u32) -> usize {
        self.data_at + (c as usize - 2) * self.cluster
    }

    fn alloc(&mut self, n: usize) -> Vec<u32> {
        let mut out = Vec::new();
        let mut skip = false;
        for c in 2..=self.max {
            if out.len() == n {
                break;
            }
            if self.get(c) != 0 {
                continue;
            }
            if skip {
                skip = false;
                continue;
            }
            out.push(c);
            // the placeholder keeps later allocations in this call off the cluster
            self.set(c, self.eoc());
            skip = self.gaps;
        }
        assert_eq!(out.len(), n, "the test image is full");
        for w in out.windows(2) {
            self.set(w[0], w[1]);
        }
        for &c in &out {
            let o = self.off(c);
            self.img[o..o + self.cluster].fill(0);
        }
        out
    }

    fn data(&mut self, bytes: &[u8]) -> u32 {
        if bytes.is_empty() {
            return 0;
        }
        let chain = self.alloc(bytes.len().div_ceil(self.cluster));
        for (i, &c) in chain.iter().enumerate() {
            let part = &bytes[i * self.cluster..((i + 1) * self.cluster).min(bytes.len())];
            let o = self.off(c);
            self.img[o..o + part.len()].copy_from_slice(part);
        }
        chain[0]
    }

    /// Where the folder's next 32-byte entry goes (growing it by a cluster when it's full).
    fn slot(&mut self, dir: u32) -> usize {
        let (mut chain, used) = self.dirs[&dir].clone();
        let at = if dir == 0 && self.bits != 32 {
            assert!(used < self.root_len / 32, "the root folder is full");
            self.root_at + used * 32
        } else {
            let per = self.cluster / 32;
            if used / per >= chain.len() {
                let c = self.alloc(1)[0];
                self.set(*chain.last().unwrap(), c);
                chain.push(c);
            }
            self.off(chain[used / per]) + (used % per) * 32
        };
        self.dirs.insert(dir, (chain, used + 1));
        at
    }

    fn put_entry(&mut self, dir: u32, e: &[u8; 32]) -> usize {
        let o = self.slot(dir);
        self.img[o..o + 32].copy_from_slice(e);
        o
    }

    /// The short name for `name` in `dir`: as is if it's a valid upper-case 8.3 name, with the
    /// NT lower-case flags if it's a lower-case one, else a ~N alias (and a long name is needed).
    fn short(&mut self, dir: u32, name: &str) -> ([u8; 11], u8, bool) {
        let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
        let plain = |s: &str, n: usize| {
            !s.is_empty()
                && s.len() <= n
                && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        };
        let fits = plain(stem, 8) && (ext.is_empty() || plain(ext, 3));
        let mut s = [b' '; 11];
        if fits && (name == name.to_uppercase() || name == name.to_lowercase()) {
            s[..stem.len()].copy_from_slice(stem.to_uppercase().as_bytes());
            s[8..8 + ext.len()].copy_from_slice(ext.to_uppercase().as_bytes());
            let flags = if name == name.to_uppercase() { 0 } else { 0x18 };
            assert!(self.shorts.insert((dir, s)));
            return (s, flags, false);
        }
        let clean = |t: &str, n: usize| -> Vec<u8> {
            t.bytes()
                .filter(|c| c.is_ascii_alphanumeric())
                .map(|c| c.to_ascii_uppercase())
                .take(n)
                .collect()
        };
        let (base, ext) = (clean(stem, 6), clean(ext, 3));
        for k in 1..10 {
            let mut s = [b' '; 11];
            let b = if base.is_empty() {
                b"X".to_vec()
            } else {
                base.clone()
            };
            s[..b.len()].copy_from_slice(&b);
            s[b.len()] = b'~';
            s[b.len() + 1] = b'0' + k;
            s[8..8 + ext.len()].copy_from_slice(&ext);
            if self.shorts.insert((dir, s)) {
                return (s, 0, true);
            }
        }
        panic!("no short name left for {name}");
    }

    /// A folder entry (with long-name entries before it when needed); where the short one is.
    fn add(&mut self, dir: u32, name: &str, attr: u8, first: u32, size: u32) -> usize {
        let (short, case, long) = self.short(dir, name);
        if long {
            let sum = short.iter().fold(0u8, |s, &c| {
                (if s & 1 != 0 { 0x80u8 } else { 0 })
                    .wrapping_add(s >> 1)
                    .wrapping_add(c)
            });
            let mut units: Vec<u16> = name.encode_utf16().collect();
            let n = units.len().div_ceil(13);
            if !units.len().is_multiple_of(13) {
                units.push(0);
            }
            units.resize(n * 13, 0xFFFF);
            for ord in (1..=n).rev() {
                let mut e = [0u8; 32];
                e[0] = ord as u8 | if ord == n { 0x40 } else { 0 };
                e[11] = 0x0F;
                e[13] = sum;
                let part = &units[(ord - 1) * 13..ord * 13];
                for (i, &p) in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30]
                    .iter()
                    .enumerate()
                {
                    e[p..p + 2].copy_from_slice(&part[i].to_le_bytes());
                }
                self.put_entry(dir, &e);
            }
        }
        let mut e = [0u8; 32];
        e[..11].copy_from_slice(&short);
        e[11] = attr;
        e[12] = case;
        for at in [14, 22] {
            put(&mut e, at, DOS_TIME, 2);
        }
        for at in [16, 18, 24] {
            put(&mut e, at, DOS_DATE, 2);
        }
        if self.bits == 32 {
            put(&mut e, 20, u64::from(first >> 16), 2);
        }
        put(&mut e, 26, u64::from(first & 0xFFFF), 2);
        put(&mut e, 28, u64::from(size), 4);
        self.put_entry(dir, &e)
    }

    fn file(&mut self, dir: u32, name: &str, bytes: &[u8]) -> u32 {
        let first = self.data(bytes);
        self.add(dir, name, 0x20, first, bytes.len() as u32);
        first
    }

    fn mkdir(&mut self, parent: u32, name: &str) -> u32 {
        let c = self.alloc(1)[0];
        self.dirs.insert(c, (vec![c], 0));
        let dot = |n: &[u8], cl: u32, bits: u32| {
            let mut e = [b' '; 32];
            e[11..].fill(0);
            e[..n.len()].copy_from_slice(n);
            e[11] = 0x10;
            put(&mut e, 22, DOS_TIME, 2);
            put(&mut e, 24, DOS_DATE, 2);
            if bits == 32 {
                put(&mut e, 20, u64::from(cl >> 16), 2);
            }
            put(&mut e, 26, u64::from(cl & 0xFFFF), 2);
            e
        };
        let (d1, d2) = (dot(b".", c, self.bits), dot(b"..", parent, self.bits));
        self.put_entry(c, &d1);
        self.put_entry(c, &d2);
        self.add(parent, name, 0x10, c, 0);
        c
    }
}

struct FatFixture {
    img: Vec<u8>,
    big: Vec<u8>,
    long: Vec<u8>,
    big_first: u32,
}

/// The same tree on any FAT volume.
fn fill_fat(img: Vec<u8>) -> FatFixture {
    let mut w = FatW::new(img);
    let big = noise(1, 50_000);
    let long = noise(2, 3000);
    w.file(0, "README.TXT", b"Hello from a FAT volume.\n");
    w.file(0, "Long file name with spaces.txt", &long);
    w.file(0, "ünïcødé ✓.txt", b"unicode\n");
    w.file(0, "lower.txt", b"lower\n");
    w.file(0, "empty.txt", b"");
    w.gaps = true;
    let big_first = w.file(0, "big.bin", &big);
    w.gaps = false;
    let h = w.data(b"h");
    w.add(0, "HIDDEN.SYS", 0x06, h, 1);
    // a deleted file: its entries (long and short) marked E5, its cluster freed
    let gone = w.file(0, "gone file.txt", b"deleted");
    w.set(gone, 0);
    let slots = w.dirs[&0].1;
    let (chain, _) = w.dirs[&0].clone();
    for k in slots - 2..slots {
        let o = if w.bits == 32 {
            let per = w.cluster / 32;
            w.off(chain[k / per]) + (k % per) * 32
        } else {
            w.root_at + k * 32
        };
        w.img[o] = 0xE5;
    }
    let photos = w.mkdir(0, "Photos");
    for i in 1..=40 {
        w.file(
            photos,
            &format!("IMG_{i:04}.JPG"),
            format!("photo {i}\n").as_bytes(),
        );
    }
    let y = w.mkdir(photos, "2024");
    let s = w.mkdir(y, "Summer holiday");
    w.file(s, "beach.txt", b"sand\n");
    if w.bits == 32 {
        // FSInfo's free count and next free cluster: unknown now
        let info = le(&w.img, 48, 2) as usize * 512;
        put(&mut w.img, info + 488, 0xFFFF_FFFF, 8);
    }
    FatFixture {
        img: w.img,
        big,
        long,
        big_first,
    }
}

fn check_fat(fx: &FatFixture, bits: u32) {
    let mut v = open(&fx.img);
    assert_eq!(v.info().kind, format!("FAT{bits}"));
    assert!(v.info().label.is_some());
    assert_eq!(
        names(&mut v, ""),
        sorted(&[
            "README.TXT",
            "Long file name with spaces.txt",
            "ünïcødé ✓.txt",
            "lower.txt",
            "empty.txt",
            "big.bin",
            "HIDDEN.SYS",
            "Photos",
        ])
    );
    assert_eq!(read(&mut v, "README.TXT"), b"Hello from a FAT volume.\n");
    assert_eq!(read(&mut v, "Long file name with spaces.txt"), fx.long);
    assert_eq!(read(&mut v, "ünïcødé ✓.txt"), b"unicode\n");
    assert_eq!(read(&mut v, "lower.txt"), b"lower\n");
    assert_eq!(read(&mut v, "empty.txt"), b"");
    assert_eq!(read(&mut v, "big.bin"), fx.big);
    let e = v.find("README.TXT").unwrap();
    assert_eq!((e.mtime, e.dir, e.size), (Some(DOS_WHEN), false, 25));
    assert_eq!(v.find("HIDDEN.SYS").unwrap().flags, ["hidden", "system"]);
    let photos = v.list_path("Photos", MAX_LIST).unwrap();
    assert_eq!(photos.entries.len(), 41);
    assert!(photos.entries.iter().any(|e| e.name == "2024" && e.dir));
    assert_eq!(read(&mut v, "Photos/IMG_0033.JPG"), b"photo 33\n");
    assert_eq!(
        read(&mut v, "Photos/2024/Summer holiday/beach.txt"),
        b"sand\n"
    );
    let cut = v.list_path("Photos", 10).unwrap();
    assert_eq!((cut.entries.len(), cut.truncated), (10, true));
    // what isn't there, and what isn't a file
    assert!(v.find("gone file.txt").is_err());
    assert!(v.find("Photos/../README.TXT").is_err());
    assert!(v.find("README.TXT/x").is_err());
    let dir = v.find("Photos").unwrap();
    assert!(v.reader(&dir).is_err());
    assert!(v.list_path("README.TXT", 10).is_err());
    // the partial reads a browser makes
    let big = v.find("big.bin").unwrap();
    let mut rd = v.reader(&big).unwrap();
    rd.seek(SeekFrom::Start(40_000)).unwrap();
    let mut buf = vec![0u8; 20_000];
    let n = fill(&mut rd, &mut buf);
    assert_eq!(&buf[..n], &fx.big[40_000..]);
}

fn fill(r: &mut impl Read, buf: &mut [u8]) -> usize {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]).unwrap() {
            0 => break,
            k => n += k,
        }
    }
    n
}

#[test]
fn fat_12_16_32_formatted_here() {
    for (bits, size, spc) in [(12, 2 << 20, 1), (16, 24 << 20, 4), (32, 40 << 20, 1)] {
        let fx = fill_fat(format_fat(bits, size, spc));
        check_fat(&fx, bits);
        if let Some(fsck) = tool("fsck.fat") {
            let t = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(t.path(), &fx.img).unwrap();
            run(Command::new(fsck).arg("-n").arg(t.path()));
        }
    }
}

#[test]
fn fat_made_by_mkfs() {
    let mkfs = need!("mkfs.fat");
    for (bits, size, extra) in [
        (12, 2 << 20, "-s1"),
        (16, 20 << 20, "-s4"),
        (32, 40 << 20, "-s1"),
    ] {
        let t = tempfile::NamedTempFile::new().unwrap();
        t.as_file().set_len(size).unwrap();
        run(Command::new(&mkfs)
            .args(["-F", &bits.to_string(), extra, "-n", "MKFSVOL"])
            .arg(t.path()));
        let fx = fill_fat(std::fs::read(t.path()).unwrap());
        std::fs::write(t.path(), &fx.img).unwrap();
        if let Some(fsck) = tool("fsck.fat") {
            run(Command::new(fsck).arg("-n").arg(t.path()));
        }
        check_fat(&fx, bits);
        // through a real file, as a browser reads it
        let mut v = Volume::open(File::open(t.path()).unwrap(), 0, size).unwrap();
        assert_eq!(v.info().label.as_deref(), Some("MKFSVOL"));
        let e = v.find("big.bin").unwrap();
        let mut out = Vec::new();
        v.into_reader(&e).unwrap().read_to_end(&mut out).unwrap();
        assert_eq!(out, fx.big);
    }
}

#[test]
fn fat_damage_is_an_error() {
    let fx = fill_fat(format_fat(16, 24 << 20, 4));
    let mut w = FatW::new(fx.img.clone());
    // a chain that comes back to its start
    let c = fx.big_first;
    let second = w.get(c);
    let third = w.get(second);
    w.set(third, c);
    let mut v = open(&w.img);
    let e = v.find("big.bin").unwrap();
    let err = v.reader(&e).err().unwrap().to_string();
    assert!(err.contains("loops"), "{err}");
    // a chain that breaks off (a free cluster in the middle)
    w.set(second, 0);
    let mut v = open(&w.img);
    let err = v.reader(&e).err().unwrap().to_string();
    assert!(err.contains("breaks off"), "{err}");
    // an image cut short, read as a partition of its original size
    let half = fx.img[..fx.img.len() / 2].to_vec();
    let mut v = Volume::open(Cursor::new(half.clone()), 0, fx.img.len() as u64).unwrap();
    assert_eq!(read(&mut v, "README.TXT"), b"Hello from a FAT volume.\n");
    // and as what's there
    let mut v = open(&half);
    assert!(v.list_path("", 100).is_ok());
    // the boot sector's numbers not adding up
    let mut bad = fx.img.clone();
    put(&mut bad, 19, 100, 2);
    let err = Volume::open(Cursor::new(bad), 0, 24 << 20).err().unwrap();
    assert!(err.to_string().contains("don't add up"), "{err}");
    // not a filesystem at all
    let err = Volume::open(Cursor::new(noise(3, 1 << 20)), 0, 1 << 20)
        .err()
        .unwrap();
    assert!(err.to_string().contains("no filesystem"), "{err}");
    let err = Volume::open(Cursor::new(vec![0u8; 100]), 0, 100)
        .err()
        .unwrap();
    assert!(err.to_string().contains("too small"), "{err}");
}

// ---- exFAT: mkfs.exfat makes it, the writer here fills it

struct ExW {
    img: Vec<u8>,
    cluster: usize,
    fat_at: usize,
    heap_at: usize,
    count: u32,
    bitmap_at: usize,
    dirs: HashMap<u32, usize>,
}

fn upcase(u: u16) -> u16 {
    match u {
        0x61..=0x7A => u - 0x20,
        0xE0..=0xFE if u != 0xF7 => u - 0x20,
        _ => u,
    }
}

impl ExW {
    fn new(img: Vec<u8>) -> ExW {
        let sector = 1usize << img[108];
        let cluster = sector << img[109];
        let mut w = ExW {
            cluster,
            fat_at: le(&img, 80, 4) as usize * sector,
            heap_at: le(&img, 88, 4) as usize * sector,
            count: le(&img, 92, 4) as u32,
            bitmap_at: 0,
            dirs: HashMap::new(),
            img,
        };
        let root = le(&w.img, 96, 4) as u32;
        let r = w.off(root);
        let mut used = 0;
        while w.img[r + used * 32] != 0 {
            let e = r + used * 32;
            if w.img[e] == 0x81 {
                w.bitmap_at = w.off(le(&w.img, e + 20, 4) as u32);
            }
            used += 1;
        }
        w.dirs.insert(root, used);
        w
    }

    fn off(&self, c: u32) -> usize {
        self.heap_at + (c as usize - 2) * self.cluster
    }

    fn free(&self, c: u32) -> bool {
        let i = (c - 2) as usize;
        self.img[self.bitmap_at + i / 8] & (1 << (i % 8)) == 0
    }

    fn take(&mut self, c: u32) {
        let i = (c - 2) as usize;
        self.img[self.bitmap_at + i / 8] |= 1 << (i % 8);
    }

    /// `n` clusters: contiguous (no FAT chain), or every other free one linked in the FAT.
    fn alloc(&mut self, n: usize, contiguous: bool) -> Vec<u32> {
        let mut out: Vec<u32> = Vec::new();
        let mut c = 2;
        while out.len() < n {
            assert!(c < self.count + 2, "the test image is full");
            if !self.free(c) || (!contiguous && out.last().is_some_and(|&l| l + 1 == c)) {
                if contiguous {
                    out.clear();
                }
                c += 1;
                continue;
            }
            out.push(c);
            c += 1;
        }
        for &c in &out {
            self.take(c);
            let o = self.off(c);
            self.img[o..o + self.cluster].fill(0);
        }
        if !contiguous {
            for w in out.windows(2) {
                put(
                    &mut self.img,
                    self.fat_at + w[0] as usize * 4,
                    u64::from(w[1]),
                    4,
                );
            }
            let last = *out.last().unwrap();
            put(
                &mut self.img,
                self.fat_at + last as usize * 4,
                0xFFFF_FFFF,
                4,
            );
        }
        out
    }

    /// A file or folder entry set in `dir`: its first cluster, stored length, valid length.
    #[allow(clippy::too_many_arguments)]
    fn set(
        &mut self,
        dir: u32,
        name: &str,
        attr: u16,
        first: u32,
        len: u64,
        valid: u64,
        contiguous: bool,
    ) -> usize {
        let units: Vec<u16> = name.encode_utf16().collect();
        let n = units.len().div_ceil(15);
        let mut es = vec![[0u8; 32]; 2 + n];
        es[0][0] = 0x85;
        es[0][1] = (1 + n) as u8;
        put(&mut es[0], 4, u64::from(attr), 2);
        let ts = (DOS_DATE << 16) | DOS_TIME;
        for at in [8, 12, 16] {
            put(&mut es[0], at, ts, 4);
        }
        es[0][21] = 100; // one more second
        for at in [22, 23, 24] {
            es[0][at] = 0x80 | 8; // UTC+2
        }
        es[1][0] = 0xC0;
        es[1][1] = 1 | if contiguous { 2 } else { 0 };
        es[1][3] = units.len() as u8;
        let hash = units.iter().fold(0u16, |h, &u| {
            let u = upcase(u);
            let h = h.rotate_right(1).wrapping_add(u & 0xFF);
            h.rotate_right(1).wrapping_add(u >> 8)
        });
        put(&mut es[1], 4, u64::from(hash), 2);
        put(&mut es[1], 8, valid, 8);
        put(&mut es[1], 20, u64::from(first), 4);
        put(&mut es[1], 24, len, 8);
        for (k, part) in units.chunks(15).enumerate() {
            let e = &mut es[2 + k];
            e[0] = 0xC1;
            for (i, &u) in part.iter().enumerate() {
                put(e, 2 + 2 * i, u64::from(u), 2);
            }
        }
        let sum = es
            .iter()
            .flatten()
            .enumerate()
            .filter(|(i, _)| *i != 2 && *i != 3)
            .fold(0u16, |s, (_, &b)| {
                s.rotate_right(1).wrapping_add(u16::from(b))
            });
        put(&mut es[0], 2, u64::from(sum), 2);
        let used = self.dirs[&dir];
        assert!(
            (used + es.len()) * 32 <= self.cluster,
            "the test folder is full"
        );
        let at = self.off(dir) + used * 32;
        for (i, e) in es.iter().enumerate() {
            self.img[at + i * 32..at + i * 32 + 32].copy_from_slice(e);
        }
        self.dirs.insert(dir, used + es.len());
        at
    }

    fn file(&mut self, dir: u32, name: &str, bytes: &[u8], contiguous: bool) -> usize {
        let chain = self.alloc(bytes.len().div_ceil(self.cluster), contiguous);
        for (i, part) in bytes.chunks(self.cluster).enumerate() {
            let o = self.off(chain[i]);
            self.img[o..o + part.len()].copy_from_slice(part);
        }
        let first = chain.first().copied().unwrap_or(0);
        let n = bytes.len() as u64;
        self.set(dir, name, 0x20, first, n, n, contiguous && n > 0)
    }

    fn mkdir(&mut self, parent: u32, name: &str) -> u32 {
        let c = self.alloc(1, false)[0];
        self.dirs.insert(c, 0);
        let len = self.cluster as u64;
        self.set(parent, name, 0x10, c, len, len, false);
        c
    }
}

#[test]
fn exfat_made_by_mkfs() {
    let mkfs = need!("mkfs.exfat");
    let t = tempfile::NamedTempFile::new().unwrap();
    t.as_file().set_len(8 << 20).unwrap();
    run(Command::new(mkfs).args(["-L", "EXVOL"]).arg(t.path()));
    let mut w = ExW::new(std::fs::read(t.path()).unwrap());
    let root = le(&w.img, 96, 4) as u32;
    let frag = noise(4, 20_000);
    w.file(root, "Hello.txt", b"hello exfat\n", true);
    w.file(root, "frag.bin", &frag, false);
    w.file(
        root,
        "Ünï ✓ a rather long name for one entry.txt",
        b"long\n",
        true,
    );
    // stored bytes past the valid length read as zeros
    let chain = w.alloc(3, true);
    let o = w.off(chain[0]);
    w.img[o..o + 3 * w.cluster].fill(0xAB);
    w.set(root, "vdl.bin", 0x20, chain[0], 10_000, 100, true);
    w.file(root, "empty.txt", b"", true);
    let gone = w.file(root, "gone.txt", b"deleted\n", true);
    for k in 0..3 {
        w.img[gone + 32 * k] &= 0x7F;
    }
    let dir = w.mkdir(root, "Folder");
    w.file(dir, "inner.txt", b"inside\n", false);
    std::fs::write(t.path(), &w.img).unwrap();
    if let Some(fsck) = tool("fsck.exfat") {
        run(Command::new(fsck).arg("-n").arg(t.path()));
    }
    let mut v = open(&w.img);
    let info = v.info().clone();
    assert_eq!(
        (info.kind.as_str(), info.label.as_deref(), info.block),
        ("exFAT", Some("EXVOL"), 4096)
    );
    assert_eq!(
        names(&mut v, ""),
        sorted(&[
            "Hello.txt",
            "frag.bin",
            "Ünï ✓ a rather long name for one entry.txt",
            "vdl.bin",
            "empty.txt",
            "Folder",
        ])
    );
    assert_eq!(read(&mut v, "Hello.txt"), b"hello exfat\n");
    assert_eq!(read(&mut v, "frag.bin"), frag);
    assert_eq!(
        read(&mut v, "Ünï ✓ a rather long name for one entry.txt"),
        b"long\n"
    );
    let vdl = read(&mut v, "vdl.bin");
    assert_eq!(vdl.len(), 10_000);
    assert!(vdl[..100].iter().all(|&b| b == 0xAB) && vdl[100..].iter().all(|&b| b == 0));
    assert_eq!(read(&mut v, "empty.txt"), b"");
    assert_eq!(read(&mut v, "Folder/inner.txt"), b"inside\n");
    let e = v.find("Hello.txt").unwrap();
    assert_eq!(e.mtime, Some(DOS_WHEN + 1 - 2 * 3600));
    assert!(v.find("gone.txt").is_err());
    survive_damage(
        &w.img,
        &[0, le(&w.img, 80, 4) * 512, w.off(root) as u64],
        150,
    );
}

// ---- ext2/3/4, made by mke2fs from a folder

struct ExtSource {
    _dir: tempfile::TempDir,
    root: PathBuf,
    big: Vec<u8>,
    sparse: Vec<u8>,
    frag: Vec<u8>,
}

fn ext_source() -> ExtSource {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    let p = |s: &str| root.join(s);
    std::fs::create_dir_all(p("docs/a/b/c")).unwrap();
    std::fs::create_dir_all(p("many")).unwrap();
    std::fs::write(p("hello.txt"), "Hello from ext.\n").unwrap();
    let f = File::options().write(true).open(p("hello.txt")).unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000))
        .unwrap();
    let big = noise(5, 3 << 20);
    std::fs::write(p("big.bin"), &big).unwrap();
    // holes between three bits of data
    let mut sparse = vec![0u8; 5 << 20];
    for (at, s) in [
        (0, &b"START"[..]),
        (2 << 20, b"MIDDLE"),
        ((5 << 20) - 3, b"END"),
    ] {
        sparse[at..at + s.len()].copy_from_slice(s);
    }
    write_sparse(&p("sparse.bin"), &sparse);
    // eight pieces with holes between: more extents than an inode holds
    let mut frag = vec![0u8; 8 * (68 << 10)];
    for k in 0..8 {
        let at = k * (68 << 10);
        frag[at..at + 4096].copy_from_slice(&noise(10 + k as u64, 4096));
    }
    write_sparse(&p("frag.bin"), &frag);
    std::fs::write(p("docs/a/b/c/deep.txt"), "deep\n").unwrap();
    for i in 0..300 {
        std::fs::write(
            p(&format!("many/file{i:04} with a longer name.txt")),
            format!("{i}\n"),
        )
        .unwrap();
    }
    std::fs::write(p("tiny.txt"), "tiny").unwrap();
    std::fs::write(p("ünïcødé ✓.txt"), "unicode\n").unwrap();
    std::fs::write(p("empty.txt"), "").unwrap();
    std::fs::hard_link(p("hello.txt"), p("hard.txt")).unwrap();
    // (mke2fs, which these trees are for, is a Unix tool: elsewhere the tests skip before using them)
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("hello.txt", p("link")).unwrap();
        let long_target = format!("{}/target.txt", "x".repeat(100));
        std::os::unix::fs::symlink(&long_target, p("longlink")).unwrap();
    }
    if let Some(mkfifo) = tool("mkfifo") {
        run(Command::new(mkfifo).arg(p("fifo")));
    }
    ExtSource {
        _dir: dir,
        root,
        big,
        sparse,
        frag,
    }
}

/// Writes only the non-zero 4 KiB pieces, so the file has holes.
fn write_sparse(path: &Path, data: &[u8]) {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = File::create(path).unwrap();
    f.set_len(data.len() as u64).unwrap();
    for (k, piece) in data.chunks(4096).enumerate() {
        if piece.iter().any(|&b| b != 0) {
            f.seek(SeekFrom::Start((k * 4096) as u64)).unwrap();
            f.write_all(piece).unwrap();
        }
    }
}

fn mke2fs(src: &ExtSource, size: u64, args: &[&str]) -> Option<(tempfile::NamedTempFile, Vec<u8>)> {
    let mkfs = tool("mke2fs")?;
    let t = tempfile::NamedTempFile::new().unwrap();
    t.as_file().set_len(size).unwrap();
    run(Command::new(mkfs)
        .args(["-q", "-F"])
        .args(args)
        .arg("-d")
        .arg(&src.root)
        .arg(t.path()));
    let img = std::fs::read(t.path()).unwrap();
    Some((t, img))
}

fn check_ext(src: &ExtSource, img: &[u8], path: &Path, kind: &str) {
    let mut v = open(img);
    assert_eq!(v.info().kind, kind, "{:?}", v.info());
    let mut want = vec![
        "lost+found",
        "hello.txt",
        "big.bin",
        "sparse.bin",
        "frag.bin",
        "docs",
        "many",
        "tiny.txt",
        "ünïcødé ✓.txt",
        "empty.txt",
        "hard.txt",
        "link",
        "longlink",
    ];
    if src.root.join("fifo").exists() {
        want.push("fifo");
    }
    assert_eq!(names(&mut v, ""), sorted(&want));
    assert_eq!(read(&mut v, "hello.txt"), b"Hello from ext.\n");
    assert_eq!(read(&mut v, "hard.txt"), b"Hello from ext.\n");
    assert_eq!(v.find("hello.txt").unwrap().mtime, Some(1_700_000_000));
    for (name, want) in [
        ("big.bin", &src.big),
        ("sparse.bin", &src.sparse),
        ("frag.bin", &src.frag),
    ] {
        let got = read(&mut v, name);
        if got == *want {
            continue;
        }
        // if mke2fs stored it differently from its source, e2fsprogs' own reading decides
        let at = got.iter().zip(want.iter()).position(|(a, b)| a != b);
        let Some(debugfs) = tool("debugfs") else {
            panic!("{kind}: {name} differs from its source at {at:?}");
        };
        let out = tempfile::NamedTempFile::new().unwrap();
        run(Command::new(debugfs)
            .arg("-R")
            .arg(format!("dump /{name} {}", out.path().display()))
            .arg(path));
        let reference = std::fs::read(out.path()).unwrap();
        assert!(
            reference == got,
            "{kind}: {name} differs from debugfs's reading (at {:?}; from the source at {at:?})",
            reference.iter().zip(&got).position(|(a, b)| a != b)
        );
        eprintln!(
            "note: mke2fs stored {name} unlike its source (at {at:?}); debugfs agrees with this reader"
        );
    }
    assert_eq!(read(&mut v, "docs/a/b/c/deep.txt"), b"deep\n");
    assert_eq!(read(&mut v, "tiny.txt"), b"tiny");
    assert_eq!(read(&mut v, "ünïcødé ✓.txt"), b"unicode\n");
    assert_eq!(read(&mut v, "empty.txt"), b"");
    assert_eq!(names(&mut v, "many").len(), 300);
    assert_eq!(
        read(&mut v, "many/file0123 with a longer name.txt"),
        b"123\n"
    );
    let cut = v.list_path("many", 50).unwrap();
    assert_eq!((cut.entries.len(), cut.truncated), (50, true));
    let link = v.find("link").unwrap();
    assert_eq!((link.link.as_deref(), link.dir), (Some("hello.txt"), false));
    assert!(v.reader(&link).is_err());
    let long = v.find("longlink").unwrap();
    assert_eq!(long.link, Some(format!("{}/target.txt", "x".repeat(100))));
    if src.root.join("fifo").exists() {
        let fifo = v.find("fifo").unwrap();
        assert!(fifo.unreadable.is_some() && v.reader(&fifo).is_err());
    }
    assert!(v.find("docs").unwrap().dir);
    assert!(v.find("nope").is_err());
}

#[test]
fn ext4_with_extents_and_a_hashed_folder() {
    let src = ext_source();
    let Some((t, img)) = mke2fs(&src, 32 << 20, &["-t", "ext4", "-b", "4096"]) else {
        eprintln!("skipping: mke2fs isn't installed");
        return;
    };
    check_ext(&src, &img, t.path(), "ext4");
    // e2fsck -D rebuilds folders as hashed trees (htree), which read the same way
    if let Some(fsck) = tool("e2fsck") {
        let out = Command::new(fsck)
            .args(["-fyD"])
            .arg(t.path())
            .output()
            .unwrap();
        assert!(out.status.code().is_some_and(|c| c <= 2), "{out:?}");
        let img = std::fs::read(t.path()).unwrap();
        if let Some(debugfs) = tool("debugfs") {
            let stat = run(Command::new(debugfs)
                .args(["-R", "stat /many"])
                .arg(t.path()));
            assert!(
                stat.contains("Flags: 0x81000") || stat.contains("Flags: 0x1000"),
                "{stat}"
            );
        }
        check_ext(&src, &img, t.path(), "ext4");
        let table = le(&img, 4096 + 8, 4) * 4096;
        survive_damage(&img, &[1024, 4096, table, table + 4096], 120);
    }
}

#[test]
fn ext2_and_ext3_with_block_maps() {
    let src = ext_source();
    // 1 KiB blocks: big.bin needs double-indirect blocks; 128-byte inodes have no extra times
    let Some((t, img)) = mke2fs(&src, 32 << 20, &["-t", "ext2", "-b", "1024", "-I", "128"]) else {
        eprintln!("skipping: mke2fs isn't installed");
        return;
    };
    check_ext(&src, &img, t.path(), "ext2");
    let table = le(&img, 2048 + 8, 4) * 1024;
    survive_damage(&img, &[1024, 2048, table, table + 1024], 120);
    let (t, img) = mke2fs(&src, 32 << 20, &["-t", "ext3", "-b", "2048"]).unwrap();
    check_ext(&src, &img, t.path(), "ext3");
}

#[test]
fn ext4_inline_data_64bit_and_meta_bg() {
    let src = ext_source();
    let Some((t, img)) = mke2fs(
        &src,
        32 << 20,
        &["-t", "ext4", "-O", "inline_data,64bit", "-I", "256"],
    ) else {
        eprintln!("skipping: mke2fs isn't installed");
        return;
    };
    check_ext(&src, &img, t.path(), "ext4");
    let mut v = open(&img);
    assert!(v.find("tiny.txt").unwrap().flags.contains(&"inline"));
    assert!(v.find("docs/a").unwrap().flags.contains(&"inline"));
    // tiny groups, few inodes each: files land in groups whose descriptors meta_bg moves
    let (t, img) = mke2fs(
        &src,
        64 << 20,
        &[
            "-t",
            "ext4",
            "-b",
            "1024",
            "-g",
            "1024",
            "-N",
            "400",
            "-I",
            "128",
            "-O",
            "meta_bg,^resize_inode,^flex_bg",
        ],
    )
    .unwrap();
    check_ext(&src, &img, t.path(), "ext4");
}

// ---- NTFS, made by mkntfs and filled by ntfscp (which can't make folders), plus a sparse file
// (ntfstruncate) and one with an uninitialised tail (ntfsfallocate)

#[test]
fn ntfs_made_by_mkntfs() {
    let mkntfs = need!("mkntfs");
    let ntfscp = need!("ntfscp");
    for cluster in ["4096", "512", "8192"] {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("ntfs.img");
        File::create(&img).unwrap().set_len(24 << 20).unwrap();
        run(Command::new(&mkntfs)
            .args(["-F", "-Q", "-q", "-L", "NTFSVOL", "-c", cluster])
            .arg(&img));
        let file = |name: &str, bytes: &[u8]| {
            let src = dir.path().join("src.bin");
            std::fs::write(&src, bytes).unwrap();
            run(Command::new(&ntfscp).arg(&img).arg(&src).arg(name));
        };
        let big = noise(6, 300_000);
        file("hello.txt", b"Hello from NTFS.\n");
        file("big.bin", &big);
        file("Ünïcødé ✓.txt", b"unicode\n");
        file("empty.txt", b"");
        // enough names to push the root index out into INDX blocks, and a level below
        for i in 0..70 {
            file(
                &format!("Document number {i:02} with a fairly long name.txt"),
                format!("{i}\n").as_bytes(),
            );
        }
        file("sparse.bin", b"abc\n");
        file("prealloc.bin", b"head");
        let mut sparse_ok = false;
        if let (Some(ls), Some(trunc)) = (tool("ntfsls"), tool("ntfstruncate")) {
            let list = run(Command::new(ls).arg("-i").arg(&img));
            if let Some(ino) = list
                .lines()
                .find_map(|l| l.trim().strip_suffix(" sparse.bin").map(str::trim))
            {
                run(Command::new(trunc).arg(&img).arg(ino).arg("3000000"));
                sparse_ok = true;
            }
        }
        let mut prealloc_ok = false;
        if let Some(fall) = tool("ntfsfallocate") {
            run(Command::new(fall)
                .args(["-l", "1M"])
                .arg(&img)
                .arg("prealloc.bin"));
            prealloc_ok = true;
        }
        let bytes = std::fs::read(&img).unwrap();
        let mut v = open(&bytes);
        let info = v.info().clone();
        assert_eq!(
            (
                info.kind.as_str(),
                info.label.as_deref(),
                info.block.to_string()
            ),
            ("NTFS", Some("NTFSVOL"), cluster.to_string())
        );
        let list = v.list_path("", MAX_LIST).unwrap();
        assert!(
            !list.entries.iter().any(|e| e.name.starts_with('$')),
            "metadata files hidden"
        );
        assert_eq!(list.entries.len(), 4 + 70 + 2);
        assert_eq!(read(&mut v, "hello.txt"), b"Hello from NTFS.\n");
        assert_eq!(read(&mut v, "big.bin"), big);
        assert_eq!(read(&mut v, "Ünïcødé ✓.txt"), b"unicode\n");
        assert_eq!(read(&mut v, "empty.txt"), b"");
        assert_eq!(
            read(&mut v, "Document number 42 with a fairly long name.txt"),
            b"42\n"
        );
        // in the order the index keeps: sorted
        let docs: Vec<&str> = list
            .entries
            .iter()
            .map(|e| e.name.as_str())
            .filter(|n| n.starts_with("Document"))
            .collect();
        assert!(docs.windows(2).all(|w| w[0] < w[1]), "{docs:?}");
        let e = v.find("hello.txt").unwrap();
        assert!(e.mtime.is_some_and(|t| t > 1_600_000_000));
        if sparse_ok {
            let s = v.find("sparse.bin").unwrap();
            assert!(s.flags.contains(&"sparse"), "{s:?}");
            let data = read(&mut v, "sparse.bin");
            assert_eq!(data.len(), 3_000_000);
            assert_eq!(&data[..4], b"abc\n");
            assert!(data[4..].iter().all(|&b| b == 0));
        }
        if prealloc_ok {
            let data = read(&mut v, "prealloc.bin");
            assert_eq!(data.len(), 1 << 20);
            assert_eq!(&data[..4], b"head");
            assert!(data[4..].iter().all(|&b| b == 0));
        }
        // a folder below the root: the metadata folder $Extend (record 11)
        let ext = Entry {
            name: "$Extend".into(),
            dir: true,
            size: 0,
            mtime: None,
            link: None,
            unreadable: None,
            flags: Vec::new(),
            node: Node::Ntfs { record: 11, seq: 0 },
        };
        let inside: Vec<String> = v
            .list(&ext, 100)
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(inside.iter().any(|n| n == "$Reparse"), "{inside:?}");
        if cluster == "4096" {
            let b = &bytes;
            let mft = le(b, 48, 8) * 4096;
            survive_damage(b, &[0, mft, mft + 5 * 1024, mft + 64 * 1024], 150);
        }
    }
}

#[test]
fn lznt1_units() {
    let text: Vec<u8> = b"The quick brown fox jumps over the lazy dog. "
        .iter()
        .cycle()
        .take(8192)
        .copied()
        .collect();
    let mut data = text.clone(); // compresses well
    data.extend(noise(7, 8192)); // doesn't: stored as is
    data.extend(vec![0u8; 8192]); // zeros: all holes
    data.extend(&text[..4000]); // half and half
    data.extend(noise(8, 4192));
    data.extend(b"tail ".repeat(300)); // a short last unit
    let unit = 8192usize; // 16 clusters of 512
    let mut vol = Vec::new();
    let mut runs = Vec::new();
    for (u, piece) in data.chunks(unit).enumerate() {
        let at = (u * unit) as u64;
        let packed = lznt1_compress(piece);
        let len = unit.min(piece.len().div_ceil(512) * 512) as u64;
        if piece.iter().all(|&b| b == 0) {
            push_run(
                &mut runs,
                Run {
                    at,
                    len: unit as u64,
                    phys: None,
                },
            )
            .unwrap();
        } else if packed.len().div_ceil(512) * 512 < len as usize {
            let stored = packed.len().div_ceil(512) * 512;
            let phys = vol.len() as u64;
            vol.extend(&packed);
            vol.resize(phys as usize + stored, 0);
            push_run(
                &mut runs,
                Run {
                    at,
                    len: stored as u64,
                    phys: Some(phys),
                },
            )
            .unwrap();
            push_run(
                &mut runs,
                Run {
                    at: at + stored as u64,
                    len: unit as u64 - stored as u64,
                    phys: None,
                },
            )
            .unwrap();
        } else {
            let phys = vol.len() as u64;
            vol.extend(piece);
            vol.resize(phys as usize + len as usize, 0);
            push_run(
                &mut runs,
                Run {
                    at,
                    len,
                    phys: Some(phys),
                },
            )
            .unwrap();
        }
    }
    let size = data.len() as u64;
    let len = vol.len() as u64;
    let mk = |valid: u64| {
        FileReader::new(
            Cursor::new(vol.clone()),
            0,
            len,
            Data::Lznt1 {
                runs: runs.clone(),
                size,
                valid,
                unit: unit as u64,
            },
        )
    };
    let mut out = Vec::new();
    mk(size).read_to_end(&mut out).unwrap();
    assert_eq!(out, data);
    // reads that start and stop inside units
    let mut r = mk(size);
    r.seek(SeekFrom::Start(8000)).unwrap();
    let mut buf = vec![0u8; 10_000];
    assert_eq!(fill(&mut r, &mut buf), 10_000);
    assert_eq!(buf, data[8000..18_000]);
    // an initialized size short of the end: zeros after it
    let mut out = Vec::new();
    mk(100).read_to_end(&mut out).unwrap();
    assert_eq!(&out[..100], &data[..100]);
    assert!(out[100..].iter().all(|&b| b == 0));
    // damage: a back reference before the start of the chunk
    assert!(ntfs::lznt1(&[0x02, 0xB0, 0x01, 0x10, 0x00], 4096).is_err());
    assert!(ntfs::lznt1(&[0xFF, 0xBF, 0x00], 4096).is_err());
}

/// A greedy LZNT1 compressor, for the test above.
fn lznt1_compress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in data.chunks(4096) {
        let mut c = Vec::new();
        let mut pos = 0;
        while pos < chunk.len() {
            let flag_at = c.len();
            c.push(0u8);
            for bit in 0..8 {
                if pos >= chunk.len() {
                    break;
                }
                let mut lg = 0;
                let mut k = pos.saturating_sub(1);
                while k >= 0x10 {
                    lg += 1;
                    k >>= 1;
                }
                let (max_back, max_len) = (1usize << (12 - lg), (0xFFF >> lg) + 3);
                let mut best = (0, 0);
                for back in 1..=pos.min(max_back) {
                    let mut l = 0;
                    while l < max_len
                        && pos + l < chunk.len()
                        && chunk[pos + l] == chunk[pos + l - back]
                    {
                        l += 1;
                    }
                    if l > best.1 {
                        best = (back, l);
                    }
                }
                if best.1 >= 3 {
                    let t = ((best.0 - 1) << (12 - lg)) | (best.1 - 3);
                    c.extend_from_slice(&(t as u16).to_le_bytes());
                    c[flag_at] |= 1 << bit;
                    pos += best.1;
                } else {
                    c.push(chunk[pos]);
                    pos += 1;
                }
            }
        }
        if c.len() < chunk.len() {
            out.extend_from_slice(&((0xB000 | (c.len() - 1)) as u16).to_le_bytes());
            out.extend(c);
        } else {
            let mut raw = chunk.to_vec();
            raw.resize(4096, 0);
            out.extend_from_slice(&0x3FFFu16.to_le_bytes());
            out.extend(raw);
        }
    }
    out
}

#[test]
fn run_maps() {
    // NTFS run lists: a run, a hole, a run back towards the start
    let runs = ntfs::decode_runs(
        &[0x21, 0x10, 0x00, 0x01, 0x01, 0x05, 0x11, 0x02, 0xF0, 0x00],
        0,
    )
    .unwrap();
    assert_eq!(
        runs,
        [(0, 16, Some(256)), (16, 5, None), (21, 2, Some(240))]
    );
    assert!(ntfs::decode_runs(&[0x21, 0x10], 0).is_err());
    assert!(ntfs::decode_runs(&[0x11, 0x01, 0x80], 0).is_err()); // before the start
    // holes and a short valid length, through the generic reader
    let vol: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
    let runs = vec![
        Run {
            at: 0,
            len: 100,
            phys: Some(1000),
        },
        Run {
            at: 100,
            len: 50,
            phys: None,
        },
        Run {
            at: 300,
            len: 10,
            phys: Some(0),
        },
    ];
    let mut out = Vec::new();
    FileReader::new(
        Cursor::new(vol.clone()),
        0,
        4096,
        Data::Runs {
            runs,
            size: 400,
            valid: 305,
        },
    )
    .read_to_end(&mut out)
    .unwrap();
    assert_eq!(&out[..100], &vol[1000..1100]);
    assert!(out[100..300].iter().all(|&b| b == 0));
    assert_eq!(&out[300..305], &vol[..5]);
    assert!(out[305..].iter().all(|&b| b == 0));
    assert_eq!(out.len(), 400);
    // a run past the end of the volume is an error, not a panic
    let runs = vec![Run {
        at: 0,
        len: 100,
        phys: Some(4090),
    }];
    let mut r = FileReader::new(
        Cursor::new(vol),
        0,
        4096,
        Data::Runs {
            runs,
            size: 100,
            valid: 100,
        },
    );
    assert!(r.read_to_end(&mut Vec::new()).is_err());
    // overlapping pieces
    let mut runs = Vec::new();
    push_run(
        &mut runs,
        Run {
            at: 0,
            len: 10,
            phys: Some(0),
        },
    )
    .unwrap();
    push_run(
        &mut runs,
        Run {
            at: 10,
            len: 10,
            phys: Some(10),
        },
    )
    .unwrap();
    assert_eq!(runs.len(), 1);
    assert!(overlapping(&[
        Run {
            at: 0,
            len: 10,
            phys: Some(0)
        },
        Run {
            at: 10,
            len: 10,
            phys: Some(5)
        }
    ]));
}

// ---- a disk image with two partitions, opened by offset through a real file

#[test]
fn two_partitions_in_one_image() {
    let a = fill_fat(format_fat(16, 24 << 20, 4)).img;
    let b = fill_fat(format_fat(12, 2 << 20, 1)).img;
    let mut disk = vec![0u8; 1 << 20];
    disk.extend(&a);
    let second = disk.len() as u64;
    disk.extend(&b);
    let t = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(t.path(), &disk).unwrap();
    let mut v = Volume::open(File::open(t.path()).unwrap(), 1 << 20, a.len() as u64).unwrap();
    assert_eq!(v.info().kind, "FAT16");
    let mut w = Volume::open(File::open(t.path()).unwrap(), second, b.len() as u64).unwrap();
    assert_eq!(w.info().kind, "FAT12");
    for vol in [&mut v, &mut w] {
        let e = vol.find("Photos/2024/Summer holiday/beach.txt").unwrap();
        let mut out = Vec::new();
        vol.reader(&e).unwrap().read_to_end(&mut out).unwrap();
        assert_eq!(out, b"sand\n");
    }
    // the table sector itself is no filesystem
    assert!(Volume::open(File::open(t.path()).unwrap(), 0, 1 << 20).is_err());
    // past a deadline, folders can't be read, but an open file still can
    let e = v.find("big.bin").unwrap();
    let mut rd = v.into_reader(&e).unwrap();
    let mut v = Volume::open(File::open(t.path()).unwrap(), 1 << 20, a.len() as u64).unwrap();
    v.set_deadline(std::time::Instant::now());
    std::thread::sleep(std::time::Duration::from_millis(2));
    let err = v.list_path("Photos", 10).unwrap_err().to_string();
    assert!(err.contains("longer than allowed"), "{err}");
    let mut out = Vec::new();
    rd.read_to_end(&mut out).unwrap();
    assert_eq!(out.len(), 50_000);
}

#[test]
fn damaged_fat_images_fail_cleanly() {
    for (bits, size, spc) in [(12, 2 << 20, 1), (16, 24 << 20, 4), (32, 40 << 20, 1)] {
        let fx = fill_fat(format_fat(bits, size, spc));
        let w = FatW::new(fx.img.clone());
        let hot = [0, w.fat_at as u64, w.root_at as u64, w.data_at as u64];
        survive_damage(&fx.img, &hot, 200);
    }
}
