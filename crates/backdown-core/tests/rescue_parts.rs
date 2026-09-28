//! Finding lost partitions: disk images built here, with hand-made filesystem structures and with
//! real filesystems made by the mkfs tools (those tests are skipped where the tools aren't
//! installed), their partition tables wiped and their boot sectors damaged.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use backdown_core::rescue::parts::{self, Found, Options, Search, Table};

const MIB: u64 = 1 << 20;

fn put(b: &mut [u8], at: usize, v: u64, n: usize) {
    for i in 0..n {
        b[at + i] = (v >> (8 * i)) as u8;
    }
}

fn put_be(b: &mut [u8], at: usize, v: u64, n: usize) {
    for i in 0..n {
        b[at + i] = (v >> (8 * (n - 1 - i))) as u8;
    }
}

/// CRC-32 the slow way, independent of the module's own.
fn crc32(b: &[u8]) -> u32 {
    let mut c = !0u32;
    for &x in b {
        c ^= u32::from(x);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

fn guid_bytes(s: &str) -> Vec<u8> {
    let h: Vec<u8> = s
        .replace('-', "")
        .as_bytes()
        .chunks(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect();
    let mut g = vec![h[3], h[2], h[1], h[0], h[5], h[4], h[7], h[6]];
    g.extend_from_slice(&h[8..]);
    g
}

/// A sparse disk image in a temporary directory.
struct Disk {
    dir: tempfile::TempDir,
    path: PathBuf,
    size: u64,
}

impl Disk {
    fn new(size: u64) -> Disk {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disk.img");
        File::create(&path).unwrap().set_len(size).unwrap();
        Disk { dir, path, size }
    }

    fn write(&self, at: u64, bytes: &[u8]) {
        let mut f = OpenOptions::new().write(true).open(&self.path).unwrap();
        f.seek(SeekFrom::Start(at)).unwrap();
        f.write_all(bytes).unwrap();
    }

    fn zero(&self, at: u64, len: u64) {
        self.write(at, &vec![0u8; len as usize]);
    }

    fn read(&self, at: u64, len: usize) -> Vec<u8> {
        let mut f = File::open(&self.path).unwrap();
        f.seek(SeekFrom::Start(at)).unwrap();
        let mut b = vec![0u8; len];
        f.read_exact(&mut b).unwrap();
        b
    }

    /// Copies a filesystem image in at `at`, leaving its all-zero stretches as holes.
    fn copy_in(&self, at: u64, img: &Path) {
        let mut src = File::open(img).unwrap();
        let mut dst = OpenOptions::new().write(true).open(&self.path).unwrap();
        let mut buf = vec![0u8; 64 << 10];
        let mut off = 0u64;
        loop {
            let n = src.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            if buf[..n].iter().any(|&b| b != 0) {
                dst.seek(SeekFrom::Start(at + off)).unwrap();
                dst.write_all(&buf[..n]).unwrap();
            }
            off += n as u64;
        }
    }

    fn table(&self) -> Table {
        let f = File::open(&self.path).unwrap();
        parts::table(&f, self.size).unwrap()
    }

    fn search(&self, deep: bool) -> Search {
        let opts = Options {
            deep,
            ..Default::default()
        };
        parts::search(&self.path, &opts, &mut |_| {}, &AtomicBool::new(false)).unwrap()
    }
}

fn at(s: &Search, start: u64) -> &Found {
    s.found
        .iter()
        .find(|f| f.start == start)
        .unwrap_or_else(|| panic!("nothing found at {start}: {:#?}", s.found))
}

fn starts(s: &Search) -> Vec<u64> {
    s.found.iter().map(|f| f.start).collect()
}

/// An MBR: (type, first sector, sectors, bootable) for each slot.
fn mbr(d: &Disk, at: u64, slots: &[(u8, u64, u64, bool)]) {
    let mut s = vec![0u8; 512];
    put(&mut s, 440, 0x5EED_0001, 4);
    for (i, &(kind, start, len, boot)) in slots.iter().enumerate() {
        let e = 446 + 16 * i;
        s[e] = if boot { 0x80 } else { 0 };
        s[e + 4] = kind;
        put(&mut s, e + 8, start, 4);
        put(&mut s, e + 12, len, 4);
    }
    s[510] = 0x55;
    s[511] = 0xAA;
    d.write(at, &s);
}

struct Part {
    kind: &'static str,
    first: u64,
    sectors: u64,
    name: &'static str,
}

const LINUX: &str = "0FC63DAF-8483-4772-8E79-3D69D8477DE4";
const BASIC: &str = "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7";

/// 128 GPT entries for `parts`.
fn gpt_entries(parts: &[Part]) -> Vec<u8> {
    let mut entries = vec![0u8; 128 * 128];
    for (i, p) in parts.iter().enumerate() {
        let e = &mut entries[128 * i..128 * (i + 1)];
        e[..16].copy_from_slice(&guid_bytes(p.kind));
        e[16..32].copy_from_slice(&guid_bytes(&format!(
            "00000000-0000-4000-8000-{:012X}",
            i + 1
        )));
        put(e, 32, p.first, 8);
        put(e, 40, p.first + p.sectors - 1, 8);
        for (j, c) in p.name.encode_utf16().enumerate() {
            put(e, 56 + 2 * j, u64::from(c), 2);
        }
    }
    entries
}

/// A GPT header for a disk of `ss`-byte sectors, and 128 entries (16 KiB) at `first_entry`.
fn gpt_header(d: &Disk, ss: u64, my: u64, alt: u64, first_entry: u64, entries: &[u8]) -> Vec<u8> {
    let lbas = d.size / ss;
    let n = 16384 / ss;
    let mut h = vec![0u8; ss as usize];
    h[..8].copy_from_slice(b"EFI PART");
    put(&mut h, 8, 0x0001_0000, 4);
    put(&mut h, 12, 92, 4);
    put(&mut h, 24, my, 8);
    put(&mut h, 32, alt, 8);
    put(&mut h, 40, 2 + n, 8);
    put(&mut h, 48, lbas - 2 - n, 8);
    h[56..72].copy_from_slice(&guid_bytes("11111111-2222-4333-8444-555555555555"));
    put(&mut h, 72, first_entry, 8);
    put(&mut h, 80, 128, 4);
    put(&mut h, 84, 128, 4);
    put(&mut h, 88, u64::from(crc32(entries)), 4);
    let c = crc32(&h[..92]);
    put(&mut h, 16, u64::from(c), 4);
    h
}

/// A protective MBR, and a GPT with both copies, for `ss`-byte sectors.
fn gpt_ss(d: &Disk, ss: u64, parts: &[Part]) {
    let (lbas, n) = (d.size / ss, 16384 / ss);
    mbr(d, 0, &[(0xEE, 1, (lbas - 1).min(0xFFFF_FFFF), false)]);
    let entries = gpt_entries(parts);
    d.write(ss, &gpt_header(d, ss, 1, lbas - 1, 2, &entries));
    d.write(2 * ss, &entries);
    d.write((lbas - 1 - n) * ss, &entries);
    d.write(
        (lbas - 1) * ss,
        &gpt_header(d, ss, lbas - 1, 1, lbas - 1 - n, &entries),
    );
}

fn gpt(d: &Disk, parts: &[Part]) {
    gpt_ss(d, 512, parts);
}

/// The first sectors of a FAT32 volume at `at`: boot sector, its backup at sector 6, and the
/// start of both FATs.
fn fat32(d: &Disk, at: u64, sectors: u64, label: &str) {
    let mut b = vec![0u8; 512];
    b[..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    b[3..11].copy_from_slice(b"MSWIN4.1");
    put(&mut b, 11, 512, 2);
    b[13] = 8;
    put(&mut b, 14, 32, 2);
    b[16] = 2;
    b[21] = 0xF8;
    put(&mut b, 32, sectors, 4);
    put(&mut b, 36, 64, 4);
    put(&mut b, 44, 2, 4);
    put(&mut b, 48, 1, 2);
    put(&mut b, 50, 6, 2);
    b[66] = 0x29;
    b[71..82].copy_from_slice(format!("{label:<11}").as_bytes());
    b[82..90].copy_from_slice(b"FAT32   ");
    b[510] = 0x55;
    b[511] = 0xAA;
    d.write(at, &b);
    d.write(at + 6 * 512, &b);
    let fat = [0xF8, 0xFF, 0xFF, 0x0F, 0xFF, 0xFF, 0xFF, 0x0F];
    d.write(at + 32 * 512, &fat);
    d.write(at + (32 + 64) * 512, &fat);
}

/// An ext4 superblock for a filesystem at `at` of `blocks` blocks of `bs` bytes, and its backups
/// in `groups`.
fn ext4(d: &Disk, at: u64, bs: u64, blocks: u64, per_group: u64, label: &str, groups: &[u64]) {
    let mut sb = vec![0u8; 1024];
    let first = u64::from(bs == 1024);
    let log = (bs / 1024).trailing_zeros() as u64;
    put(&mut sb, 0, 1024, 4);
    put(&mut sb, 4, blocks, 4);
    put(&mut sb, 20, first, 4);
    put(&mut sb, 24, log, 4);
    put(&mut sb, 28, log, 4);
    put(&mut sb, 32, per_group, 4);
    put(&mut sb, 36, per_group, 4);
    put(&mut sb, 40, 256, 4);
    put(&mut sb, 56, 0xEF53, 2);
    put(&mut sb, 58, 1, 2);
    put(&mut sb, 76, 1, 4);
    put(&mut sb, 88, 256, 2);
    put(&mut sb, 96, 0x40, 4);
    sb[120..120 + label.len()].copy_from_slice(label.as_bytes());
    d.write(at + 1024, &sb);
    for &g in groups {
        let mut c = sb.clone();
        put(&mut c, 90, g, 2);
        d.write(at + (first + g * per_group) * bs, &c);
    }
}

#[test]
fn hand_built_filesystems_after_the_table_is_wiped() {
    let d = Disk::new(96 * MIB);
    // FAT32 at 1 MiB; ext4 with 4 KiB blocks at an odd offset; ext4 with 1 KiB blocks after it
    let odd = 20 * MIB + 7 * 512;
    let one_k = 48 * MIB + 512;
    fat32(&d, MIB, 32768, "CAMERA");
    ext4(&d, odd, 4096, 4096, 1024, "home", &[1, 3]);
    ext4(&d, one_k, 1024, 16384, 8192, "boot", &[1]);
    mbr(
        &d,
        0,
        &[
            (0x0C, 2048, 32768, true),
            (0x83, odd / 512, 32768, false),
            (0x83, one_k / 512, 32768, false),
        ],
    );
    let t = d.table();
    assert_eq!((t.scheme, t.entries.len()), ("mbr", 3), "{t:#?}");
    assert!(t.problems.is_empty(), "{:?}", t.problems);
    assert_eq!(
        (t.entries[0].kind.as_str(), t.entries[0].bootable),
        ("FAT32", true)
    );

    let s = d.search(false);
    assert_eq!(starts(&s), [MIB, odd, one_k], "{:#?}", s.found);
    assert!(
        s.found
            .iter()
            .all(|f| f.in_table && !f.nested && !f.truncated)
    );
    let f = at(&s, MIB);
    assert_eq!(
        (f.fs.as_str(), f.label.as_deref(), f.size, f.via),
        ("FAT32", Some("CAMERA"), Some(16 * MIB), "boot sector")
    );
    let e = at(&s, odd);
    assert_eq!(
        (e.fs.as_str(), e.label.as_deref(), e.size, e.via),
        ("ext4", Some("home"), Some(16 * MIB), "superblock")
    );

    // no table: the quick search finds only what's aligned, the deep one everything
    d.zero(0, 512);
    let t = d.table();
    assert_eq!((t.scheme, t.entries.len()), ("none", 0));
    let s = d.search(false);
    assert_eq!(starts(&s), [MIB], "{:#?}", s.found);
    assert!(!s.found[0].in_table);
    let s = d.search(true);
    assert_eq!(starts(&s), [MIB, odd, one_k], "{:#?}", s.found);
    assert!(s.found.iter().all(|f| !f.in_table));
    assert_eq!(at(&s, one_k).size, Some(16 * MIB));

    // their first sectors gone too: found through their backups, where they start
    d.zero(MIB, 512);
    d.zero(odd + 1024, 1024);
    d.zero(one_k + 1024, 1024);
    let s = d.search(true);
    assert_eq!(starts(&s), [MIB, odd, one_k], "{:#?}", s.found);
    let f = at(&s, MIB);
    assert_eq!(
        (f.fs.as_str(), f.label.as_deref(), f.size, f.via),
        (
            "FAT32",
            Some("CAMERA"),
            Some(16 * MIB),
            "backup boot sector"
        )
    );
    for (start, label) in [(odd, "home"), (one_k, "boot")] {
        let e = at(&s, start);
        assert_eq!(
            (e.fs.as_str(), e.label.as_deref(), e.size, e.via),
            ("ext4", Some(label), Some(16 * MIB), "backup superblock")
        );
    }
}

#[test]
fn gpt_copies_are_checked_against_each_other() {
    let d = Disk::new(16 * MIB);
    let parts = [
        Part {
            kind: LINUX,
            first: 2048,
            sectors: 8192,
            name: "data",
        },
        Part {
            kind: BASIC,
            first: 10240,
            sectors: 8192,
            name: "shared",
        },
    ];
    gpt(&d, &parts);
    let t = d.table();
    assert_eq!(
        (t.scheme, t.sector, t.entries.len()),
        ("gpt", 512, 2),
        "{t:#?}"
    );
    assert!(t.problems.is_empty(), "{:?}", t.problems);
    let e = &t.entries[1];
    assert_eq!(
        (e.index, e.start, e.size, e.kind.as_str(), e.name.as_deref()),
        (
            2,
            10240 * 512,
            4 * MIB,
            "Microsoft basic data",
            Some("shared")
        )
    );
    assert_eq!(
        e.guid.as_deref(),
        Some("00000000-0000-4000-8000-000000000002")
    );

    // the primary header damaged: the backup's list, and the reason
    let good = d.read(512, 512);
    let mut bad = good.clone();
    bad[60] ^= 0xFF;
    d.write(512, &bad);
    let t = d.table();
    assert_eq!((t.scheme, t.entries.len()), ("gpt", 2), "{t:#?}");
    assert!(
        t.problems
            .iter()
            .any(|p| p.contains("primary GPT") && p.contains("damaged") && p.contains("backup")),
        "{:?}",
        t.problems
    );
    assert_eq!(t.entries[0].name.as_deref(), Some("data"));

    // the primary's entries damaged (the header's CRC of them no longer matches)
    d.write(512, &good);
    d.write(1024 + 60, &[0x42]);
    let t = d.table();
    assert_eq!(t.entries.len(), 2);
    assert!(
        t.problems
            .iter()
            .any(|p| p.contains("primary GPT") && p.contains("checksum")),
        "{:?}",
        t.problems
    );
    d.write(1024, &gpt_entries(&parts)[..128]);

    // the backup gone
    let lbas = d.size / 512;
    let backup = d.read((lbas - 1) * 512, 512);
    d.zero((lbas - 1) * 512, 512);
    let t = d.table();
    assert_eq!(t.entries.len(), 2);
    assert_eq!(
        t.problems,
        ["the backup GPT at the end of the disk is missing"]
    );
    d.write((lbas - 1) * 512, &backup);

    // the two copies disagree: an older layout in the backup
    let older = gpt_entries(&parts[..1]);
    d.write((lbas - 33) * 512, &older);
    d.write(
        (lbas - 1) * 512,
        &gpt_header(&d, 512, lbas - 1, 1, lbas - 33, &older),
    );
    let t = d.table();
    assert_eq!(t.entries.len(), 2);
    assert!(
        t.problems.iter().any(|p| p.contains("disagree")),
        "{:?}",
        t.problems
    );

    // no protective MBR (a zeroed first sector) and no primary: the backup still tells
    d.zero(0, 34 * 512);
    let t = d.table();
    assert_eq!((t.scheme, t.entries.len()), ("gpt", 1), "{t:#?}");
    assert!(t.problems.iter().any(|p| p.contains("protective MBR")));
    assert!(
        t.problems
            .iter()
            .any(|p| p.contains("primary GPT") && p.contains("missing"))
    );
}

#[test]
fn mbr_with_logical_partitions() {
    let d = Disk::new(8 * MIB);
    // the extended partition spans sectors 4096..12288; its EBRs are at 4096 and 8192
    mbr(
        &d,
        0,
        &[(0x83, 2048, 2048, true), (0x05, 4096, 8192, false)],
    );
    let ebr = |at: u64, len: u64, next: Option<u64>| {
        let mut s = vec![0u8; 512];
        s[446 + 4] = 0x07;
        put(&mut s, 446 + 8, 63, 4);
        put(&mut s, 446 + 12, len, 4);
        if let Some(n) = next {
            s[462 + 4] = 0x05;
            put(&mut s, 462 + 8, n, 4);
            put(&mut s, 462 + 12, 4096, 4);
        }
        s[510] = 0x55;
        s[511] = 0xAA;
        d.write(at * 512, &s);
    };
    ebr(4096, 4000, Some(4096));
    ebr(8192, 4000, None);
    let t = d.table();
    let got: Vec<_> = t
        .entries
        .iter()
        .map(|e| (e.index, e.start / 512, e.size / 512, e.kind.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            (1, 2048, 2048, "Linux"),
            (2, 4096, 8192, "Extended"),
            (5, 4096 + 63, 4000, "NTFS / exFAT"),
            (6, 8192 + 63, 4000, "NTFS / exFAT"),
        ]
    );
    assert!(t.problems.is_empty(), "{:?}", t.problems);

    // the second EBR gone: the chain breaks after partition 5
    d.zero(8192 * 512, 512);
    let t = d.table();
    assert_eq!(t.entries.len(), 3);
    assert_eq!(
        t.problems,
        ["the chain of logical partitions is broken after partition 5"]
    );
}

#[test]
fn a_whole_disk_filesystem_and_a_stale_gpt_behind_it() {
    let d = Disk::new(32 * MIB);
    gpt(
        &d,
        &[Part {
            kind: BASIC,
            first: 2048,
            sectors: 8192,
            name: "old",
        }],
    );
    // formatted as a whole: the boot sector over the protective MBR, the FSInfo over the GPT
    d.zero(0, 34 * 512);
    fat32(&d, 0, 65536, "CARD");
    let t = d.table();
    assert_eq!((t.scheme, t.entries.len()), ("none", 0), "{t:#?}");
    assert!(
        t.problems.iter().any(|p| p.contains("earlier layout")),
        "{:?}",
        t.problems
    );
    let s = d.search(false);
    let f = at(&s, 0);
    assert_eq!(
        (f.fs.as_str(), f.label.as_deref(), f.in_table),
        ("FAT32", Some("CARD"), true)
    );
    let json = serde_json::to_value(&s).unwrap();
    assert_eq!(json["found"][0]["via"], "boot sector");
    assert_eq!(json["table"]["scheme"], "none");
}

#[test]
fn gpt_with_4096_byte_sectors() {
    let d = Disk::new(64 * MIB);
    gpt_ss(
        &d,
        4096,
        &[Part {
            kind: LINUX,
            first: 256,
            sectors: 2048,
            name: "big",
        }],
    );
    let t = d.table();
    assert_eq!(
        (t.scheme, t.sector, t.entries.len()),
        ("gpt", 4096, 1),
        "{t:#?}"
    );
    assert_eq!((t.entries[0].start, t.entries[0].size), (MIB, 8 * MIB));
    assert!(t.problems.is_empty(), "{:?}", t.problems);
    // only the backup left
    d.zero(0, MIB);
    let t = d.table();
    assert_eq!(
        (t.scheme, t.sector, t.entries.len()),
        ("gpt", 4096, 1),
        "{t:#?}"
    );
}

#[test]
fn an_mbr_counting_in_4096_byte_sectors() {
    let d = Disk::new(64 * MIB);
    fat32(&d, MIB, 32768, "USB");
    mbr(&d, 0, &[(0x0C, 256, 4096, false)]);
    let t = d.table();
    assert_eq!((t.scheme, t.sector), ("mbr", 4096), "{t:#?}");
    assert_eq!((t.entries[0].start, t.entries[0].size), (MIB, 16 * MIB));
    assert!(t.problems.iter().any(|p| p.contains("4096-byte sectors")));
    assert!(at(&d.search(false), MIB).in_table);
}

#[test]
fn backups_of_xfs_btrfs_and_luks2() {
    let d = Disk::new(160 * MIB);
    // XFS at 1 MiB: 4 allocation groups of 2048 4 KiB blocks, a superblock at the start of each,
    // and each group's header after it saying which one it is
    let xfs = MIB;
    for g in 0..4 {
        let mut b = vec![0u8; 1024];
        b[..4].copy_from_slice(b"XFSB");
        put_be(&mut b, 4, 4096, 4);
        put_be(&mut b, 8, 8192, 8);
        put_be(&mut b, 84, 2048, 4);
        put_be(&mut b, 88, 4, 4);
        put_be(&mut b, 102, 512, 2);
        b[108..114].copy_from_slice(b"xfsvol");
        b[512..516].copy_from_slice(b"XAGF");
        put_be(&mut b, 512 + 8, g, 4);
        d.write(xfs + g * 8 * MIB, &b);
    }
    // btrfs at 40 MiB, 96 MiB of it: its superblock 64 KiB in, and the mirror at 64 MiB
    let btrfs = 40 * MIB;
    for bytenr in [0x10000u64, 0x400_0000] {
        let mut b = vec![0u8; 4096];
        put(&mut b, 0x30, bytenr, 8);
        b[0x40..0x48].copy_from_slice(b"_BHRfS_M");
        put(&mut b, 0x70, 96 * MIB, 8);
        b[0x12B..0x12B + 4].copy_from_slice(b"pool");
        d.write(btrfs + bytenr, &b);
    }
    // LUKS2 at 140 MiB, its second header 16 KiB in
    let luks = 140 * MIB;
    for (magic, off) in [(b"LUKS\xba\xbe", 0u64), (b"SKUL\xba\xbe", 0x4000)] {
        let mut h = vec![0u8; 4096];
        h[..6].copy_from_slice(magic);
        put_be(&mut h, 6, 2, 2);
        put_be(&mut h, 8, 0x4000, 8);
        h[24..29].copy_from_slice(b"vault");
        put_be(&mut h, 256, off, 8);
        d.write(luks + off, &h);
    }
    let s = d.search(true);
    assert_eq!(starts(&s), [xfs, btrfs, luks], "{:#?}", s.found);
    let vias: Vec<_> = s.found.iter().map(|f| f.via).collect();
    assert_eq!(vias, ["superblock", "superblock", "header"]);

    d.zero(xfs, 1024);
    d.zero(btrfs + 0x10000, 4096);
    d.zero(luks, 4096);
    let s = d.search(true);
    assert_eq!(starts(&s), [xfs, btrfs, luks], "{:#?}", s.found);
    let got: Vec<_> = s
        .found
        .iter()
        .map(|f| (f.fs.as_str(), f.label.as_deref(), f.size, f.via))
        .collect();
    assert_eq!(
        got,
        [
            ("XFS", Some("xfsvol"), Some(32 * MIB), "backup superblock"),
            ("btrfs", Some("pool"), Some(96 * MIB), "backup superblock"),
            ("LUKS2 (encrypted)", Some("vault"), None, "backup header"),
        ]
    );
}

#[test]
fn nested_truncated_and_not_fitting_the_table() {
    let d = Disk::new(64 * MIB);
    fat32(&d, MIB, 32768, "OUTER");
    // a 1 MiB ext4 image stored in it as a file
    ext4(&d, 5 * MIB, 1024, 1024, 8192, "inner", &[]);
    // bytes in it that look like a boot sector for something bigger than the disk
    fat32(&d, 9 * MIB, 262_144, "JUNK");
    // 16 MiB from 60 MiB: past the end of the disk (a partial image)
    fat32(&d, 60 * MIB, 32768, "TAIL");
    // a table whose partition is too small for the filesystem at its start
    mbr(&d, 0, &[(0x0C, 2048, 16384, false)]);
    let s = d.search(true);
    assert_eq!(starts(&s), [MIB, 5 * MIB, 60 * MIB], "{:#?}", s.found);
    let flags: Vec<_> = s
        .found
        .iter()
        .map(|f| (f.in_table, f.nested, f.truncated))
        .collect();
    assert_eq!(
        flags,
        [
            (false, false, false),
            (false, true, false),
            (false, false, true)
        ]
    );
}

#[test]
fn stopping_and_progress() {
    let d = Disk::new(64 * MIB);
    fat32(&d, 3 * MIB, 32768, "X");
    let stop = AtomicBool::new(true);
    let s = parts::search(
        &d.path,
        &Options {
            deep: true,
            ..Default::default()
        },
        &mut |_| {},
        &stop,
    )
    .unwrap();
    assert!(s.stopped);
    let mut seen = Vec::new();
    let s = parts::search(
        &d.path,
        &Options {
            deep: true,
            start: 2 * MIB,
            end: Some(40 * MIB),
            ..Default::default()
        },
        &mut |p| seen.push(*p),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(!s.stopped);
    assert_eq!(starts(&s), [3 * MIB]);
    let last = seen.last().unwrap();
    assert_eq!((last.pos, last.size, last.found), (40 * MIB, 40 * MIB, 1));
    assert!(
        parts::search(
            &d.path,
            &Options {
                sector: 1000,
                ..Default::default()
            },
            &mut |_| {},
            &AtomicBool::new(false)
        )
        .is_err()
    );
}

// ---- real filesystems, made by the mkfs tools

fn tool(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(["/usr/sbin", "/sbin", "/usr/local/sbin"].map(PathBuf::from))
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// A filesystem image of `size` bytes made by `name` with `args`; None (and why) without the tool.
fn mkfs(dir: &Path, name: &str, args: &[&str], size: u64) -> Option<PathBuf> {
    let Some(t) = tool(name) else {
        eprintln!("skipped: {name} isn't installed");
        return None;
    };
    let img = dir.join(format!("{name}.img"));
    File::create(&img).unwrap().set_len(size).unwrap();
    let out = Command::new(t).args(args).arg(&img).output().unwrap();
    assert!(
        out.status.success(),
        "{name} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(img)
}

const EXT: u64 = MIB;
const NTFS: u64 = 33 * MIB;
const FAT: u64 = 57 * MIB + 7 * 512; // not on a MiB (or cylinder) boundary
const EXFAT: u64 = 98 * MIB;

/// A GPT disk with ext4 (1 KiB blocks, so it has backup superblocks close by), NTFS, FAT32 (at an
/// odd offset) and exFAT on it. None when a tool is missing.
fn real_disk() -> Option<Disk> {
    let d = Disk::new(128 * MIB);
    let dir = d.dir.path();
    let ext = mkfs(
        dir,
        "mkfs.ext4",
        &["-F", "-q", "-b", "1024", "-L", "linuxdata"],
        32 * MIB,
    )?;
    let hidden = (NTFS / 512).to_string();
    let ntfs = mkfs(
        dir,
        "mkfs.ntfs",
        &[
            "-F", "-Q", "-q", "-s", "512", "-p", &hidden, "-H", "255", "-S", "63", "-L", "WinData",
        ],
        24 * MIB,
    )?;
    let fat = mkfs(dir, "mkfs.vfat", &["-F", "32", "-n", "PHOTOS"], 40 * MIB)?;
    let exfat = mkfs(dir, "mkfs.exfat", &["-L", "Stick"], 16 * MIB)?;
    for (at, img) in [(EXT, &ext), (NTFS, &ntfs), (FAT, &fat), (EXFAT, &exfat)] {
        d.copy_in(at, img);
    }
    let p = |kind, at: u64, size: u64, name| Part {
        kind,
        first: at / 512,
        sectors: size / 512,
        name,
    };
    gpt(
        &d,
        &[
            p(LINUX, EXT, 32 * MIB, "linux"),
            p(BASIC, NTFS, 24 * MIB, "windows"),
            p(BASIC, FAT, 40 * MIB, "photos"),
            p(BASIC, EXFAT, 16 * MIB, "stick"),
        ],
    );
    Some(d)
}

fn expect(f: &Found, fs: &str, label: &str, size: u64, via: &str) {
    assert_eq!(
        (f.fs.as_str(), f.label.as_deref(), f.size, f.via),
        (fs, Some(label), Some(size), via),
        "{f:#?}"
    );
}

#[test]
fn real_filesystems_after_the_table_is_wiped() {
    let Some(d) = real_disk() else { return };
    let t = d.table();
    assert_eq!((t.scheme, t.entries.len()), ("gpt", 4), "{t:#?}");
    assert!(t.problems.is_empty(), "{:?}", t.problems);
    // with the table, even the quick search looks where it points
    let s = d.search(false);
    assert_eq!(starts(&s), [EXT, NTFS, FAT, EXFAT], "{:#?}", s.found);
    assert!(
        s.found
            .iter()
            .all(|f| f.in_table && !f.nested && !f.truncated)
    );

    // the primary GPT gone: the backup lists them all still
    d.zero(0, 34 * 512);
    let t = d.table();
    assert_eq!((t.scheme, t.entries.len()), ("gpt", 4));
    assert!(!t.problems.is_empty());

    // both gone
    d.zero(d.size - 33 * 512, 33 * 512);
    let t = d.table();
    assert_eq!((t.scheme, t.entries.len()), ("none", 0), "{t:#?}");
    let s = d.search(false);
    assert_eq!(starts(&s), [EXT, NTFS, EXFAT], "{:#?}", s.found);
    let s = d.search(true);
    assert_eq!(starts(&s), [EXT, NTFS, FAT, EXFAT], "{:#?}", s.found);
    expect(at(&s, EXT), "ext4", "linuxdata", 32 * MIB, "superblock");
    expect(at(&s, NTFS), "NTFS", "WinData", 24 * MIB, "boot sector");
    expect(at(&s, FAT), "FAT32", "PHOTOS", 40 * MIB, "boot sector");
    expect(at(&s, EXFAT), "exFAT", "Stick", 16 * MIB, "boot sector");
    assert!(
        s.found
            .iter()
            .all(|f| !f.in_table && !f.nested && !f.truncated)
    );
}

#[test]
fn real_filesystems_through_their_backups() {
    let Some(d) = real_disk() else { return };
    d.zero(0, 34 * 512);
    d.zero(d.size - 33 * 512, 33 * 512);
    // each one's first structure overwritten
    d.zero(EXT + 1024, 1024);
    d.zero(NTFS, 512);
    d.zero(FAT, 512);
    d.zero(EXFAT, 512);
    let s = d.search(true);
    assert_eq!(starts(&s), [EXT, NTFS, FAT, EXFAT], "{:#?}", s.found);
    expect(
        at(&s, EXT),
        "ext4",
        "linuxdata",
        32 * MIB,
        "backup superblock",
    );
    expect(
        at(&s, NTFS),
        "NTFS",
        "WinData",
        24 * MIB,
        "backup boot sector",
    );
    expect(
        at(&s, FAT),
        "FAT32",
        "PHOTOS",
        40 * MIB,
        "backup boot sector",
    );
    expect(
        at(&s, EXFAT),
        "exFAT",
        "Stick",
        16 * MIB,
        "backup boot sector",
    );
    // the quick search looks just before and after each likely start, where the backups are
    let s = d.search(false);
    assert_eq!(starts(&s), [EXT, NTFS, EXFAT], "{:#?}", s.found);
    expect(
        at(&s, NTFS),
        "NTFS",
        "WinData",
        24 * MIB,
        "backup boot sector",
    );
    // and so does a deep search in 4 KiB steps (which can't see the FAT32 off a 4 KiB boundary)
    let opts = Options {
        deep: true,
        sector: 4096,
        ..Default::default()
    };
    let s = parts::search(&d.path, &opts, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    assert_eq!(starts(&s), [EXT, NTFS, EXFAT], "{:#?}", s.found);
    let vias: Vec<_> = s.found.iter().map(|f| f.via).collect();
    assert_eq!(
        vias,
        [
            "backup superblock",
            "backup boot sector",
            "backup boot sector"
        ]
    );
}

#[test]
fn real_whole_disk_filesystem() {
    let dir = tempfile::tempdir().unwrap();
    let Some(img) = mkfs(dir.path(), "mkfs.exfat", &["-L", "SDCARD"], 64 * MIB) else {
        return;
    };
    let f = File::open(&img).unwrap();
    let t = parts::table(&f, 64 * MIB).unwrap();
    assert_eq!((t.scheme, t.entries.len()), ("none", 0));
    assert!(t.problems.is_empty(), "{:?}", t.problems);
    let s = parts::search(
        &img,
        &Options::default(),
        &mut |_| {},
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(starts(&s), [0]);
    expect(&s.found[0], "exFAT", "SDCARD", 64 * MIB, "boot sector");
    assert!(s.found[0].in_table);
}

#[test]
fn real_hfs_plus_through_its_alternate_header() {
    let d = Disk::new(48 * MIB);
    let start = 5 * MIB + 16 * 512;
    let Some(img) = mkfs(d.dir.path(), "mkfs.hfsplus", &["-v", "MacData"], 16 * MIB) else {
        return;
    };
    d.copy_in(start, &img);
    let s = d.search(true);
    assert_eq!(starts(&s), [start], "{:#?}", s.found);
    let f = at(&s, start);
    assert_eq!(
        (f.fs.as_str(), f.size, f.via),
        ("HFS+", Some(16 * MIB), "volume header")
    );
    d.zero(start + 1024, 512);
    let s = d.search(true);
    assert_eq!(starts(&s), [start], "{:#?}", s.found);
    let f = at(&s, start);
    assert_eq!(
        (f.fs.as_str(), f.size, f.via),
        ("HFS+", Some(16 * MIB), "alternate volume header")
    );

    // a partition three sectors longer than its volume's last allocation block: the alternate
    // header is 1 KiB before the end of the partition, not of the volume
    let d = Disk::new(48 * MIB);
    let dir = tempfile::tempdir().unwrap();
    let img = mkfs(
        dir.path(),
        "mkfs.hfsplus",
        &["-v", "Slack"],
        16 * MIB + 1536,
    )
    .unwrap();
    d.copy_in(start, &img);
    d.zero(start + 1024, 512);
    let s = d.search(true);
    assert_eq!(starts(&s), [start], "{:#?}", s.found);
    let f = at(&s, start);
    assert_eq!(
        (f.fs.as_str(), f.size, f.via),
        ("HFS+", Some(16 * MIB), "alternate volume header")
    );
}

/// Deep-search throughput over a few hundred MB of noise with filesystems in it. Run with
/// `cargo test --release -p backdown-core --test rescue_parts -- --ignored --nocapture`.
#[test]
#[ignore]
fn deep_search_throughput() {
    let size = 512 * MIB;
    let d = Disk::new(size);
    {
        let mut f = OpenOptions::new().write(true).open(&d.path).unwrap();
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut buf = vec![0u8; MIB as usize];
        for _ in 0..size / MIB {
            for c in buf.as_chunks_mut::<8>().0 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *c = x.to_le_bytes();
            }
            f.write_all(&buf).unwrap();
        }
    }
    fat32(&d, MIB, 32768, "ONE");
    ext4(&d, 100 * MIB + 512, 4096, 4096, 1024, "two", &[1, 3]);
    fat32(&d, 400 * MIB, 32768, "THREE");
    // warm the page cache, so this measures the search rather than the disk
    let _ = d.read(0, size as usize);
    for sector in [512, 4096] {
        let t = Instant::now();
        let s = parts::search(
            &d.path,
            &Options {
                deep: true,
                sector,
                ..Default::default()
            },
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        let secs = t.elapsed().as_secs_f64();
        eprintln!(
            "deep search, {sector}-byte steps: {} MB in {secs:.3} s = {:.0} MB/s, found {:?}",
            size / 1_000_000,
            size as f64 / secs / 1e6,
            starts(&s)
        );
        assert!(starts(&s).contains(&MIB) && starts(&s).contains(&(400 * MIB)));
    }
    let t = Instant::now();
    let s = d.search(false);
    eprintln!(
        "quick search: {:.3} s, found {:?}",
        t.elapsed().as_secs_f64(),
        starts(&s)
    );
}
