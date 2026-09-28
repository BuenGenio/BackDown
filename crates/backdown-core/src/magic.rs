//! Filesystems and partition types recognised by their on-disk bytes.
//!
//! Two callers need the same knowledge: the browser's disk-image viewer (a partition inside an
//! `.img` file) and `volumes` (a partition udev couldn't describe, when its device node happens to
//! be readable). Both hand over an open file and an offset; everything here only reads, and only
//! a few kilobytes at a handful of well-known offsets.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

use serde::Serialize;

/// Up to `len` bytes at `off` (fewer at the end of the file).
pub fn read_at(f: &File, off: u64, len: usize) -> io::Result<Vec<u8>> {
    let mut r = f;
    r.seek(SeekFrom::Start(off))?;
    let mut buf = Vec::with_capacity(len.min(1 << 20));
    r.take(len as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Little- and big-endian integers of `n` bytes at `at`; 0 when out of range.
pub fn le(b: &[u8], at: usize, n: usize) -> u64 {
    b.get(at..at + n)
        .map_or(0, |s| s.iter().rev().fold(0, |a, &x| a << 8 | u64::from(x)))
}

pub fn be(b: &[u8], at: usize, n: usize) -> u64 {
    b.get(at..at + n)
        .map_or(0, |s| s.iter().fold(0, |a, &x| a << 8 | u64::from(x)))
}

pub fn has(b: &[u8], at: usize, magic: &[u8]) -> bool {
    b.get(at..at + magic.len()) == Some(magic)
}

/// A NUL- or space-padded fixed-width text field.
pub fn field(b: &[u8], at: usize, len: usize) -> String {
    let s = b.get(at..at + len).unwrap_or_default();
    let s = &s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())];
    String::from_utf8_lossy(s).trim().to_string()
}

pub fn utf16(b: &[u8], big_endian: bool) -> String {
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| {
            if big_endian {
                u16::from_be_bytes(c)
            } else {
                u16::from_le_bytes(c)
            }
        })
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units).trim().to_string()
}

/// A filesystem (or other content signature) found by its magic bytes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fs {
    /// For people: "ext4", "NTFS", "LUKS2 (encrypted)".
    pub kind: String,
    pub label: Option<String>,
    pub size: Option<u64>,
    /// blkid's name for the same thing ("ntfs", "vfat", "crypto_LUKS"), which is also what udev
    /// reports, so both sources describe a volume in one vocabulary.
    #[serde(skip)]
    pub id: &'static str,
}

/// A filesystem guessed from its superblock at `base`: kind, label where it's cheap, size.
pub fn probe_fs(f: &File, base: u64) -> io::Result<Option<Fs>> {
    let b = read_at(f, base, 4096)?;
    if b.len() < 512 {
        return Ok(None);
    }
    let fs = |id: &'static str, kind: &str, label: String, size: Option<u64>| {
        Ok(Some(Fs {
            kind: kind.to_string(),
            label: (!label.is_empty()).then_some(label),
            size: size.filter(|&s| s > 0),
            id,
        }))
    };
    let boot = has(&b, 510, &[0x55, 0xAA]);
    let bps = le(&b, 11, 2);
    if has(&b, 3, b"NTFS    ") {
        let label = ntfs_label(f, base, &b).unwrap_or_default();
        return fs("ntfs", "NTFS", label, Some(le(&b, 40, 8) * bps));
    }
    if has(&b, 3, b"EXFAT   ") {
        let label = exfat_label(f, base, &b).unwrap_or_default();
        return fs(
            "exfat",
            "exFAT",
            label,
            Some(le(&b, 72, 8) << le(&b, 108, 1)),
        );
    }
    if has(&b, 3, b"-FVE-FS-") {
        return fs("BitLocker", "BitLocker (encrypted)", String::new(), None);
    }
    if has(&b, 3, b"ReFS\0\0\0\0") {
        return fs("refs", "ReFS", String::new(), None);
    }
    let bpb_ok = boot
        && matches!(bps, 512 | 1024 | 2048 | 4096)
        && b[13].is_power_of_two()
        && le(&b, 14, 2) > 0
        && matches!(b[16], 1 | 2);
    if bpb_ok {
        let sectors = match le(&b, 19, 2) {
            0 => le(&b, 32, 4),
            n => n,
        };
        let label = |at: usize| {
            Some(field(&b, at, 11))
                .filter(|l| l != "NO NAME")
                .unwrap_or_default()
        };
        if has(&b, 82, b"FAT32   ") {
            return fs("vfat", "FAT32", label(71), Some(sectors * bps));
        }
        for kind in [&b"FAT12   "[..], b"FAT16   ", b"FAT     "] {
            if has(&b, 54, kind) {
                let name = if kind == b"FAT     " {
                    "FAT"
                } else {
                    std::str::from_utf8(&kind[..5]).unwrap_or("FAT")
                };
                return fs("vfat", name, label(43), Some(sectors * bps));
            }
        }
    }
    if has(&b, 0, b"XFSB") {
        return fs(
            "xfs",
            "XFS",
            field(&b, 108, 12),
            Some(be(&b, 4, 4) * be(&b, 8, 8)),
        );
    }
    if has(&b, 0, b"LUKS\xba\xbe") {
        let v = be(&b, 6, 2);
        return fs(
            "crypto_LUKS",
            &format!("LUKS{v} (encrypted)"),
            if v == 2 {
                field(&b, 24, 48)
            } else {
                String::new()
            },
            None,
        );
    }
    if has(&b, 0, b"hsqs") {
        return fs("squashfs", "SquashFS", String::new(), Some(le(&b, 40, 8)));
    }
    if has(&b, 32, b"NXSB") {
        return fs(
            "apfs",
            "APFS",
            String::new(),
            Some(le(&b, 36, 4) * le(&b, 40, 8)),
        );
    }
    if b.len() >= 2048 {
        if le(&b, 1080, 2) == 0xEF53 {
            let sb = &b[1024..];
            let (compat, incompat) = (le(sb, 92, 4), le(sb, 96, 4));
            let kind = if incompat & (0x40 | 0x80 | 0x200) != 0 {
                "ext4"
            } else if compat & 4 != 0 {
                "ext3"
            } else {
                "ext2"
            };
            let mut blocks = le(sb, 4, 4);
            if incompat & 0x80 != 0 {
                blocks |= le(sb, 0x150, 4) << 32;
            }
            return fs(
                kind,
                kind,
                field(sb, 120, 16),
                Some(blocks << (10 + le(sb, 24, 4))),
            );
        }
        match &b[1024..1026] {
            b"H+" | b"HX" => {
                let kind = if &b[1024..1026] == b"HX" {
                    "HFSX"
                } else {
                    "HFS+"
                };
                return fs(
                    "hfsplus",
                    kind,
                    String::new(),
                    Some(be(&b, 1024 + 40, 4) * be(&b, 1024 + 44, 4)),
                );
            }
            b"BD" => {
                let n = (b[1024 + 36] as usize).min(27);
                let label = String::from_utf8_lossy(&b[1024 + 37..1024 + 37 + n]).into_owned();
                let (id, kind) = if &b[1024 + 124..1024 + 126] == b"H+" {
                    ("hfsplus", "HFS+ (in an HFS wrapper)")
                } else {
                    ("hfs", "HFS")
                };
                return fs(id, kind, label, None);
            }
            _ => {}
        }
        if le(&b, 1024, 4) == 0xF2F5_2010 {
            return fs("f2fs", "F2FS", String::new(), None);
        }
        for s in 1..4 {
            if has(&b, s * 512, b"LABELONE") && has(&b, s * 512 + 24, b"LVM2 001") {
                return fs("LVM2_member", "LVM2 physical volume", String::new(), None);
            }
        }
    }
    if has(&b, 4086, b"SWAPSPACE2") || has(&b, 4086, b"SWAP-SPACE") {
        return fs(
            "swap",
            "Linux swap",
            field(&b, 1052, 16),
            Some((le(&b, 1028, 4) + 1) * 4096),
        );
    }
    for page in [8192u64, 16384, 65536] {
        let sig = read_at(f, base + page - 10, 10)?;
        if sig == b"SWAPSPACE2" {
            return fs(
                "swap",
                "Linux swap",
                field(&b, 1052, 16),
                Some((le(&b, 1028, 4) + 1) * page),
            );
        }
    }
    // md RAID superblock 1.2 (mdadm's default) sits 4 KiB in; its array name is at 32
    let md = read_at(f, base + 4096, 64)?;
    if le(&md, 0, 4) == 0xA92B_4EFC {
        return fs(
            "linux_raid_member",
            "Linux RAID member",
            field(&md, 32, 32),
            None,
        );
    }
    let sb = read_at(f, base + 0x10000, 0x200)?;
    if has(&sb, 0x40, b"_BHRfS_M") {
        let big = read_at(f, base + 0x10000 + 0x12B, 256)?;
        return fs(
            "btrfs",
            "btrfs",
            field(&big, 0, 256),
            Some(le(&sb, 0x70, 8)),
        );
    }
    let pvd = read_at(f, base + 32768, 2048)?;
    if has(&pvd, 1, b"CD001") {
        return fs(
            "iso9660",
            "ISO 9660",
            field(&pvd, 40, 32),
            Some(le(&pvd, 80, 4) * le(&pvd, 128, 2)),
        );
    }
    if has(&pvd, 1, b"BEA01") || has(&pvd, 1, b"NSR02") || has(&pvd, 1, b"NSR03") {
        return fs("udf", "UDF", String::new(), None);
    }
    Ok(None)
}

/// An NTFS volume's name: the $VOLUME_NAME attribute of MFT record 3 (one record read). `base`
/// is where the volume starts and `b` its boot sector (which may be the backup copy at its end).
pub fn ntfs_label(f: &File, base: u64, b: &[u8]) -> Option<String> {
    let cluster = le(b, 11, 2) * u64::from(b[13]);
    let rec = match b[64] as i8 {
        n if n < 0 => 1u64 << n.unsigned_abs(),
        n => n as u64 * cluster,
    };
    if cluster == 0 || !(512..=4096).contains(&rec) {
        return None;
    }
    let at = base + le(b, 48, 8).checked_mul(cluster)? + 3 * rec;
    let mut r = read_at(f, at, rec as usize).ok()?;
    if !has(&r, 0, b"FILE") {
        return None;
    }
    // each 512-byte stride ends in a check value; the real bytes are in the update sequence array
    let (usa, n) = (le(&r, 4, 2) as usize, le(&r, 6, 2) as usize);
    for i in 1..n {
        let (s, o) = (i * 512 - 2, usa + 2 * i);
        if o + 2 <= r.len() && s + 2 <= r.len() {
            let v = [r[o], r[o + 1]];
            r[s..s + 2].copy_from_slice(&v);
        }
    }
    let mut a = le(&r, 20, 2) as usize;
    while a + 24 <= r.len() {
        let (kind, len) = (le(&r, a, 4), le(&r, a + 4, 4) as usize);
        if kind == 0xFFFF_FFFF || len == 0 {
            break;
        }
        if kind == 0x60 {
            let (n, off) = (le(&r, a + 16, 4) as usize, le(&r, a + 20, 2) as usize);
            return r.get(a + off..a + off + n).map(|s| utf16(s, false));
        }
        a += len;
    }
    None
}

/// An exFAT volume's label: an entry in the root directory's first cluster. `base` is where the
/// volume starts and `b` its boot sector (which may be the backup copy at sector 12).
pub fn exfat_label(f: &File, base: u64, b: &[u8]) -> Option<String> {
    let sector = 1u64 << b[108].min(12);
    let cluster = sector << b[109].min(16);
    let at = base + le(b, 88, 4) * sector + le(b, 96, 4).checked_sub(2)? * cluster;
    let d = read_at(f, at, cluster.min(64 << 10) as usize).ok()?;
    for e in d.as_chunks::<32>().0 {
        match e[0] {
            0x83 => return Some(utf16(&e[2..2 + 2 * usize::from(e[1]).min(11)], false)),
            0 => break,
            _ => {}
        }
    }
    None
}

/// An MBR partition type byte, by name.
pub fn mbr_type(t: u8) -> &'static str {
    match t {
        0x01 => "FAT12",
        0x04 | 0x06 | 0x0E => "FAT16",
        0x05 | 0x0F => "Extended",
        0x07 => "NTFS / exFAT",
        0x0B | 0x0C => "FAT32",
        0x11 | 0x14 | 0x16 | 0x1B | 0x1C | 0x1E => "Hidden FAT",
        0x17 => "Hidden NTFS",
        0x27 => "Windows recovery",
        0x42 => "Windows dynamic disk",
        0x82 => "Linux swap",
        0x83 => "Linux",
        0x85 => "Linux extended",
        0x8E => "Linux LVM",
        0xA5 => "FreeBSD",
        0xA6 => "OpenBSD",
        0xA8 => "Apple UFS",
        0xA9 => "NetBSD",
        0xAB => "Apple boot",
        0xAF => "Apple HFS / HFS+",
        0xEE => "GPT protective",
        0xEF => "EFI system",
        0xFB => "VMware VMFS",
        0xFD => "Linux RAID",
        _ => "other",
    }
}

/// A GPT partition type GUID (upper case, as `guid` prints it), by name.
pub fn gpt_type(g: &str) -> &'static str {
    match g {
        "C12A7328-F81F-11D2-BA4B-00A0C93EC93B" => "EFI system",
        "21686148-6449-6E6F-744E-656564454649" => "BIOS boot",
        "E3C9E316-0B5C-4DB8-817D-F92DF00215AE" => "Microsoft reserved",
        "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7" => "Microsoft basic data",
        "DE94BBA4-06D1-4D40-A16A-BFD50179D6AC" => "Windows recovery",
        "5808C8AA-7E8F-42E0-85D2-E1E90434CFB3" => "Windows LDM metadata",
        "AF9B60A0-1431-4F62-BC68-3311714A69AD" => "Windows LDM data",
        "E75CAF8F-F680-4CEE-AFA3-B001E56EFC2D" => "Windows Storage Spaces",
        "0FC63DAF-8483-4772-8E79-3D69D8477DE4" => "Linux filesystem",
        "0657FD6D-A4AB-43C4-84E5-0933C84B4F4F" => "Linux swap",
        "E6D6D379-F507-44C2-A23C-238F2A3DF928" => "Linux LVM",
        "A19D880F-05FC-4D3B-A006-743F0F84911E" => "Linux RAID",
        "44479540-F297-41B2-9AF7-D131D5F0458A" => "Linux root (x86)",
        "4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709" => "Linux root (x86-64)",
        "69DAD710-2CE4-4E3C-B16C-21A1D49ABED3" => "Linux root (ARM)",
        "B921B045-1DF0-41C3-AF44-4C6F280D3FAE" => "Linux root (ARM64)",
        "933AC7E1-2EB4-4F13-B844-0E14E2AEF915" => "Linux /home",
        "3B8F8425-20E0-4F3B-907F-1A25A76F98E8" => "Linux /srv",
        "BC13C2FF-59E6-4262-A352-B275FD6F7172" => "Linux extended boot",
        "CA7D7CCB-63ED-4C53-861C-1742536059CC" => "Linux LUKS",
        "7FFEC5C9-2D00-49B7-8941-3EA10A5586B7" => "Linux dm-crypt",
        "8DA63339-0007-60C0-C436-083AC8230908" => "Linux reserved",
        "48465300-0000-11AA-AA11-00306543ECAC" => "Apple HFS+",
        "7C3457EF-0000-11AA-AA11-00306543ECAC" => "Apple APFS",
        "426F6F74-0000-11AA-AA11-00306543ECAC" => "Apple boot",
        "55465300-0000-11AA-AA11-00306543ECAC" => "Apple UFS",
        "52414944-0000-11AA-AA11-00306543ECAC" => "Apple RAID",
        "69646961-6700-11AA-AA11-00306543ECAC" => "Apple Silicon boot",
        "52637672-7900-11AA-AA11-00306543ECAC" => "Apple Silicon recovery",
        "6A898CC3-1DD2-11B2-99A6-080020736631" => "ZFS",
        "516E7CB4-6ECF-11D6-8FF8-00022D09712B" => "FreeBSD data",
        "83BD6B9D-7F41-11DC-BE0B-001560B84F0F" => "FreeBSD boot",
        "516E7CB5-6ECF-11D6-8FF8-00022D09712B" => "FreeBSD swap",
        "516E7CB6-6ECF-11D6-8FF8-00022D09712B" => "FreeBSD UFS",
        "516E7CBA-6ECF-11D6-8FF8-00022D09712B" => "FreeBSD ZFS",
        "AA31E02A-400F-11DB-9590-000C2911D1B8" => "VMware VMFS",
        "FE3A2A5D-4F32-41A7-B725-ACCC3285A309" => "ChromeOS kernel",
        "3CB8E202-3B7E-47DD-8A3C-7FF2A13CFCEC" => "ChromeOS root",
        _ => "other",
    }
}

/// The name of a partition type as udev, PowerShell or diskutil write it: a GPT GUID in any case,
/// with or without braces, or an MBR byte as `0x83` / `131`.
pub fn part_type_name(code: &str) -> Option<&'static str> {
    let c = code.trim().trim_matches(|c| c == '{' || c == '}');
    let name = if c.len() == 36 && c.matches('-').count() == 4 {
        gpt_type(&c.to_ascii_uppercase())
    } else if let Some(hex) = c.strip_prefix("0x").or_else(|| c.strip_prefix("0X")) {
        mbr_type(u8::from_str_radix(hex, 16).ok()?)
    } else {
        mbr_type(c.parse().ok()?)
    };
    (name != "other").then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_type_names() {
        assert_eq!(
            part_type_name("c12a7328-f81f-11d2-ba4b-00a0c93ec93b"),
            Some("EFI system")
        );
        assert_eq!(
            part_type_name("{ebd0a0a2-b9e5-4433-87c0-68b6b72699c7}"),
            Some("Microsoft basic data")
        );
        assert_eq!(part_type_name("0x83"), Some("Linux"));
        assert_eq!(part_type_name("7"), Some("NTFS / exFAT"));
        assert_eq!(part_type_name("0x99"), None);
        assert_eq!(part_type_name("nonsense"), None);
    }

    #[test]
    fn md_and_luks_by_their_magic() {
        use std::io::Write;
        let mut img = vec![0u8; 128 << 10];
        img[4096..4100].copy_from_slice(&0xA92B_4EFCu32.to_le_bytes());
        img[4096 + 32..4096 + 38].copy_from_slice(b"nas:0\0");
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&img).unwrap();
        let fs = probe_fs(f.as_file(), 0).unwrap().unwrap();
        assert_eq!(
            (fs.id, fs.label.as_deref()),
            ("linux_raid_member", Some("nas:0"))
        );
        let mut img = vec![0u8; 128 << 10];
        img[..6].copy_from_slice(b"LUKS\xba\xbe");
        img[7] = 2;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&img).unwrap();
        let fs = probe_fs(f.as_file(), 0).unwrap().unwrap();
        assert_eq!(
            (fs.id, fs.kind.as_str()),
            ("crypto_LUKS", "LUKS2 (encrypted)")
        );
    }
}
