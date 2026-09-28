//! The files inside a disk image's filesystems (FAT, exFAT, ext2/3/4, NTFS), through
//! `backdown_core::fsread`: a folder's listing for the viewer, and one file's bytes by its path in
//! those listings. A partition is picked by its number in the image's own table ("0": the whole
//! image, for an image of one filesystem), and a file only by names its folders list; nothing here
//! is ever a path on this computer, and nothing is written.
//!
//! Raw images (ISO hybrids included) are read as they are, a fixed VHD without its footer and a
//! fixed VDI from its data offset. Other virtual disks keep their sectors in their own layouts
//! (allocation tables, compressed grains), so they have to be converted to a raw image first.
//!
//! Unlike archive entries, a file here is a plain read of the image, streamed like the image
//! itself would be, so it has no size cap (a preview still asks for only what it shows).

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use backdown_core::fsread::{MAX_LIST, Volume};
use backdown_core::magic::{be, has, le, read_at};
use serde_json::{Value, json};

use super::{Part, SCAN_BUDGET, leaf, partitions};

/// Where the disk's sectors are in the file: (offset, length).
fn disk_data(f: &File) -> Result<(u64, u64)> {
    let len = f.metadata()?.len();
    let head = read_at(f, 0, 4096)?;
    let tail = if len >= 512 {
        read_at(f, len - 512, 512)?
    } else {
        Vec::new()
    };
    let convert = |what: &str| -> Result<(u64, u64)> {
        bail!(
            "{what} keeps its sectors in its own layout, so its files can't be browsed here; \
             convert it to a raw image first (for example: qemu-img convert -O raw)"
        )
    };
    if has(&head, 0, b"QFI\xfb") {
        return convert("a QEMU (qcow) disk");
    }
    if has(&head, 0, b"KDMV") || head.starts_with(b"# Disk DescriptorFile") {
        return convert("a VMware (VMDK) disk");
    }
    if has(&head, 0, b"vhdxfile") {
        return convert("a Hyper-V (VHDX) disk");
    }
    if le(&head, 0x40, 4) == 0xBEDA_107F {
        if le(&head, 0x4C, 4) != 2 {
            return convert("a dynamic VirtualBox (VDI) disk");
        }
        let at = le(&head, 0x158, 4);
        return Ok((at, le(&head, 0x170, 8).min(len.saturating_sub(at))));
    }
    if has(&tail, 0, b"conectix") || has(&head, 0, b"conectix") {
        let footer = if has(&tail, 0, b"conectix") {
            &tail[..]
        } else {
            &head[..512]
        };
        if be(footer, 60, 4) != 2 || !has(&tail, 0, b"conectix") {
            return convert("a dynamic or differencing VHD");
        }
        return Ok((0, len - 512));
    }
    if has(&tail, 0, b"koly") || has(&head, 0, b"encrcdsa") || has(&tail, 0, b"cdsaencr") {
        bail!("an Apple disk image (DMG): its files can't be browsed here");
    }
    Ok((0, len))
}

/// The bytes of partition `part` ("0": the whole disk), as (offset, length).
fn region(f: &File, part: &str) -> Result<(u64, u64)> {
    let (base, len) = disk_data(f)?;
    if part == "0" {
        return Ok((base, len));
    }
    let n: u64 = part.parse().context("bad partition number")?;
    let (_, parts, _) = partitions(f, base)?.context("this image has no partition table")?;
    let p = parts
        .iter()
        .find(|p| p["index"].as_u64() == Some(n))
        .with_context(|| format!("no partition {n} in this image"))?;
    let (start, size) = (
        p["start"].as_u64().unwrap_or(0),
        p["size"].as_u64().unwrap_or(0),
    );
    if start >= len {
        bail!("partition {n} starts past the end of the image (a partial image?)");
    }
    // the table's size, even past the end of a cut-short image: reads there say so
    Ok((base + start, size))
}

fn volume(path: &Path, part: &str) -> Result<Volume<File>> {
    let f = File::open(path)?;
    let (base, len) = region(&f, part)?;
    let mut v = Volume::open(f, base, len)?;
    v.set_deadline(Instant::now() + SCAN_BUDGET);
    Ok(v)
}

/// A folder of the filesystem in partition `part`: its entries (at most `MAX_LIST`) and what
/// the filesystem says about itself.
pub fn fs_list(path: &Path, part: &str, dir: Option<&str>) -> Result<Value> {
    let mut v = volume(path, part)?;
    let dir = dir.unwrap_or("").trim_matches('/');
    let list = v.list_path(dir, MAX_LIST)?;
    Ok(json!({
        "part": part,
        "fs": v.info(),
        "path": dir,
        "entries": list.entries,
        "truncated": list.truncated,
    }))
}

/// One file of the filesystem in partition `part`, by its path in that filesystem's listings;
/// at most `max` bytes of it.
pub fn fs_entry(path: &Path, part: &str, name: &str, max: Option<u64>) -> Result<Part> {
    let mut v = volume(path, part)?;
    let e = v.find(name.trim_matches('/'))?;
    if e.dir {
        bail!("{} is a folder", e.name);
    }
    let r = v.into_reader(&e)?;
    let size = r.size();
    let n = max.map_or(size, |m| m.min(size));
    Ok(Part {
        name: leaf(&e.name).to_string(),
        len: Some(n),
        body: Box::new(r.take(n)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peek::disk;
    use std::io::Write;

    fn put(b: &mut [u8], at: usize, v: u64, n: usize) {
        b[at..at + n].copy_from_slice(&v.to_le_bytes()[..n]);
    }

    fn set12(fat: &mut [u8], n: usize, v: u16) {
        let at = n + n / 2;
        if n & 1 == 1 {
            fat[at] = (fat[at] & 0x0F) | ((v << 4) as u8);
            fat[at + 1] = (v >> 4) as u8;
        } else {
            fat[at] = v as u8;
            fat[at + 1] = (fat[at + 1] & 0xF0) | ((v >> 8) as u8 & 0x0F);
        }
    }

    fn dirent(name: &[u8; 11], attr: u8, cluster: u16, size: u32) -> [u8; 32] {
        let mut e = [0u8; 32];
        e[..11].copy_from_slice(name);
        e[11] = attr;
        put(&mut e, 24, (44 << 9) | (5 << 5) | 6, 2); // 2024-05-06
        put(&mut e, 26, u64::from(cluster), 2);
        put(&mut e, 28, u64::from(size), 4);
        e
    }

    /// A 1 MiB FAT12 volume: HELLO.TXT, and DIR/A.TXT.
    fn fat12(label: &[u8; 11], hello: &[u8]) -> Vec<u8> {
        let mut img = vec![0u8; 1 << 20];
        img[..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
        img[3..11].copy_from_slice(b"BDTEST  ");
        put(&mut img, 11, 512, 2);
        img[13] = 1;
        put(&mut img, 14, 1, 2);
        img[16] = 2;
        put(&mut img, 17, 16, 2);
        put(&mut img, 19, 2048, 2);
        img[21] = 0xF8;
        put(&mut img, 22, 6, 2);
        img[38] = 0x29;
        img[43..54].copy_from_slice(label);
        img[54..62].copy_from_slice(b"FAT12   ");
        img[510] = 0x55;
        img[511] = 0xAA;
        for fat in [512, 7 * 512] {
            let f = &mut img[fat..fat + 6 * 512];
            for (n, v) in [(0, 0xFF8), (1, 0xFFF), (2, 0xFFF), (3, 0xFFF), (4, 0xFFF)] {
                set12(f, n, v);
            }
        }
        let (root, data) = (13 * 512, 14 * 512);
        let cl = |c: usize| data + (c - 2) * 512;
        let mut ents = vec![dirent(label, 0x08, 0, 0)];
        ents.push(dirent(b"HELLO   TXT", 0x20, 2, hello.len() as u32));
        ents.push(dirent(b"DIR        ", 0x10, 3, 0));
        for (i, e) in ents.iter().enumerate() {
            img[root + 32 * i..root + 32 * i + 32].copy_from_slice(e);
        }
        let sub = [
            dirent(b".          ", 0x10, 3, 0),
            dirent(b"..         ", 0x10, 0, 0),
            dirent(b"A       TXT", 0x20, 4, 3),
        ];
        for (i, e) in sub.iter().enumerate() {
            img[cl(3) + 32 * i..cl(3) + 32 * i + 32].copy_from_slice(e);
        }
        img[cl(2)..cl(2) + hello.len()].copy_from_slice(hello);
        img[cl(4)..cl(4) + 3].copy_from_slice(b"abc");
        img
    }

    fn tmp(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    fn body(p: Part) -> Vec<u8> {
        let mut out = Vec::new();
        let mut b = p.body;
        b.read_to_end(&mut out).unwrap();
        assert_eq!(Some(out.len() as u64), p.len);
        out
    }

    fn names(v: &Value) -> Vec<&str> {
        v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect()
    }

    /// A 4 MiB disk with FAT12 volumes at 1 MiB and 2 MiB, in an MBR or a GPT.
    fn two_part_disk(gpt: bool) -> Vec<u8> {
        let mut d = vec![0u8; 4 << 20];
        d[1 << 20..2 << 20].copy_from_slice(&fat12(b"FIRST      ", b"one\n"));
        d[2 << 20..3 << 20].copy_from_slice(&fat12(b"SECOND     ", b"two\n"));
        if gpt {
            d[446 + 4] = 0xEE;
            put(&mut d, 446 + 8, 1, 4);
            put(&mut d, 446 + 12, (4 << 20) / 512 - 1, 4);
            let h = 512;
            d[h..h + 8].copy_from_slice(b"EFI PART");
            put(&mut d, h + 72, 2, 8);
            put(&mut d, h + 80, 128, 4);
            put(&mut d, h + 84, 128, 4);
            // Microsoft basic data, stored mixed-endian
            let t = [
                0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26,
                0x99, 0xC7,
            ];
            for (i, first) in [2048u64, 4096].iter().enumerate() {
                let e = 1024 + 128 * i;
                d[e..e + 16].copy_from_slice(&t);
                d[e + 16] = 1 + i as u8;
                put(&mut d, e + 32, *first, 8);
                put(&mut d, e + 40, first + 2047, 8);
            }
        } else {
            for (i, first) in [2048u64, 4096].iter().enumerate() {
                let e = 446 + 16 * i;
                d[e + 4] = 0x01;
                put(&mut d, e + 8, *first, 4);
                put(&mut d, e + 12, 2048, 4);
            }
        }
        d[510] = 0x55;
        d[511] = 0xAA;
        d
    }

    #[test]
    fn partitions_are_browsed_by_number() {
        for gpt in [false, true] {
            let f = tmp(&two_part_disk(gpt));
            let d = disk(f.path(), None).unwrap();
            assert_eq!(d["table"], if gpt { "GPT" } else { "MBR" });
            assert_eq!(d["partitions"][1]["fs"]["kind"], "FAT12");
            let one = fs_list(f.path(), "1", None).unwrap();
            assert_eq!(names(&one), ["HELLO.TXT", "DIR"]);
            assert_eq!(
                (one["fs"]["kind"].as_str(), one["fs"]["label"].as_str()),
                (Some("FAT12"), Some("FIRST"))
            );
            assert_eq!(one["entries"][0]["size"], 4);
            assert_eq!(one["entries"][1]["dir"], true);
            let two = fs_list(f.path(), "2", Some("/DIR/")).unwrap();
            assert_eq!(
                (names(&two), two["path"].as_str()),
                (vec!["A.TXT"], Some("DIR"))
            );
            assert_eq!(
                body(fs_entry(f.path(), "1", "HELLO.TXT", None).unwrap()),
                b"one\n"
            );
            assert_eq!(
                body(fs_entry(f.path(), "2", "/HELLO.TXT", None).unwrap()),
                b"two\n"
            );
            assert_eq!(
                body(fs_entry(f.path(), "2", "DIR/A.TXT", Some(2)).unwrap()),
                b"ab"
            );
            let p = fs_entry(f.path(), "2", "DIR/A.TXT", None).unwrap();
            assert_eq!(p.name, "A.TXT");
            // names come only from the listings
            for (part, name) in [
                ("1", "DIR"),
                ("1", "../../etc/passwd"),
                ("1", "nope"),
                ("1", "DIR/.."),
            ] {
                assert!(fs_entry(f.path(), part, name, None).is_err(), "{name}");
            }
            let err = fs_list(f.path(), "3", None).unwrap_err().to_string();
            assert!(err.contains("no partition 3"), "{err}");
            assert!(fs_list(f.path(), "x", None).is_err());
            assert!(fs_list(f.path(), "0", None).is_err()); // the table isn't a filesystem
        }
    }

    #[test]
    fn whole_images_and_virtual_disks() {
        let img = fat12(b"WHOLE      ", b"whole\n");
        let f = tmp(&img);
        assert_eq!(disk(f.path(), None).unwrap()["fs"]["kind"], "FAT12");
        assert_eq!(
            names(&fs_list(f.path(), "0", None).unwrap()),
            ["HELLO.TXT", "DIR"]
        );
        assert!(fs_list(f.path(), "1", None).is_err());
        // a fixed VHD: the disk, then a footer
        let mut vhd = img.clone();
        let mut footer = vec![0u8; 512];
        footer[..8].copy_from_slice(b"conectix");
        footer[48..56].copy_from_slice(&(img.len() as u64).to_be_bytes());
        footer[60..64].copy_from_slice(&2u32.to_be_bytes());
        vhd.extend(&footer);
        let f = tmp(&vhd);
        assert_eq!(
            body(fs_entry(f.path(), "0", "HELLO.TXT", None).unwrap()),
            b"whole\n"
        );
        // a dynamic one can't be mapped cheaply
        let mut dynamic = vhd.clone();
        let at = dynamic.len() - 512 + 60;
        dynamic[at..at + 4].copy_from_slice(&3u32.to_be_bytes());
        let err = fs_list(tmp(&dynamic).path(), "0", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("convert it to a raw image"), "{err}");
        // a fixed VDI: its header, then the disk at its data offset
        let mut vdi = vec![0u8; 4096];
        vdi[..40].copy_from_slice(b"<<< Oracle VM VirtualBox Disk Image >>>\n");
        put(&mut vdi, 0x40, 0xBEDA_107F, 4);
        put(&mut vdi, 0x4C, 2, 4);
        put(&mut vdi, 0x158, 4096, 4);
        put(&mut vdi, 0x170, img.len() as u64, 8);
        vdi.extend(&img);
        let f = tmp(&vdi);
        assert_eq!(disk(f.path(), None).unwrap()["fs"]["kind"], "FAT12");
        assert_eq!(
            names(&fs_list(f.path(), "0", Some("DIR")).unwrap()),
            ["A.TXT"]
        );
        // qcow2 isn't raw
        let mut q = vec![0u8; 4096];
        q[..4].copy_from_slice(b"QFI\xfb");
        let err = fs_list(tmp(&q).path(), "0", None).unwrap_err().to_string();
        assert!(err.contains("qemu-img"), "{err}");
        // not a filesystem, and cut short
        let err = fs_list(tmp(&[0x5Au8; 8192]).path(), "0", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no filesystem"), "{err}");
        let mut cut = two_part_disk(false);
        cut.truncate((2 << 20) + 8 * 512);
        let f = tmp(&cut);
        assert!(fs_list(f.path(), "1", None).is_ok());
        let err = fs_list(f.path(), "2", Some("DIR")).unwrap_err().to_string();
        assert!(err.contains("ends before"), "{err}");
    }

    #[test]
    fn a_hybrid_iso_partition() {
        // an ISO 9660 volume (just its descriptors and an empty root) with an MBR in its system
        // area pointing at a FAT volume after it, as installer images do with their EFI partition
        let mut iso = vec![0u8; 1 << 20];
        let pvd = 16 * 2048;
        iso[pvd] = 1;
        iso[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        iso[pvd + 6] = 1;
        iso[pvd + 40..pvd + 48].copy_from_slice(b"HYBRID  ");
        put(&mut iso, pvd + 80, 512, 4);
        put(&mut iso, pvd + 128, 2048, 2);
        let rec = pvd + 156;
        iso[rec] = 34;
        put(&mut iso, rec + 2, 20, 4);
        put(&mut iso, rec + 10, 2048, 4);
        iso[rec + 25] = 2;
        iso[17 * 2048] = 255;
        iso[17 * 2048 + 1..17 * 2048 + 6].copy_from_slice(b"CD001");
        let dot = 20 * 2048;
        iso[dot] = 34;
        iso[dot + 25] = 2;
        iso[dot + 32] = 1;
        iso.extend(fat12(b"EFI        ", b"efi\n"));
        put(&mut iso, 446 + 4, 0xEF, 1);
        put(&mut iso, 446 + 8, 2048, 4);
        put(&mut iso, 446 + 12, 2048, 4);
        iso[510] = 0x55;
        iso[511] = 0xAA;
        let f = tmp(&iso);
        let d = disk(f.path(), None).unwrap();
        assert_eq!(d["format"], "iso9660");
        assert_eq!(d["partitions"][0]["fs"]["kind"], "FAT12");
        assert_eq!(
            body(fs_entry(f.path(), "1", "HELLO.TXT", None).unwrap()),
            b"efi\n"
        );
    }

    #[test]
    fn made_by_mkfs_in_a_partition() {
        let Some(mkfs) = ["/usr/sbin/mkfs.ext4", "/sbin/mkfs.ext4"]
            .into_iter()
            .find(|p| Path::new(p).exists())
        else {
            eprintln!("skipping: mkfs.ext4 isn't installed");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("photos")).unwrap();
        std::fs::write(src.join("photos/pic.txt"), "a picture\n").unwrap();
        std::os::unix::fs::symlink("photos/pic.txt", src.join("shortcut")).unwrap();
        let part = dir.path().join("part.img");
        File::create(&part).unwrap().set_len(8 << 20).unwrap();
        let out = std::process::Command::new(mkfs)
            .args(["-q", "-F", "-d"])
            .arg(&src)
            .arg(&part)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let mut d = vec![0u8; 1 << 20];
        d[446 + 4] = 0x83;
        put(&mut d, 446 + 8, 2048, 4);
        put(&mut d, 446 + 12, (8 << 20) / 512, 4);
        d[510] = 0x55;
        d[511] = 0xAA;
        d.extend(std::fs::read(&part).unwrap());
        let f = tmp(&d);
        let root = fs_list(f.path(), "1", None).unwrap();
        assert_eq!(root["fs"]["kind"], "ext4");
        let link = root["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "shortcut")
            .unwrap();
        assert_eq!(link["link"], "photos/pic.txt");
        assert!(fs_entry(f.path(), "1", "shortcut", None).is_err());
        assert_eq!(
            body(fs_entry(f.path(), "1", "photos/pic.txt", None).unwrap()),
            b"a picture\n"
        );
    }
}
