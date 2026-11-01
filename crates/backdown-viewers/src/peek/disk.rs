//! Disk images: partition tables, filesystems by their magic (backdown_core::magic), ISO 9660,
//! and virtual disks (VMDK, VHD/VHDX, DMG).

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, bail};
use backdown_core::fmt;
use backdown_core::magic::{be, field, gpt_type, has, le, mbr_type, probe_fs, read_at, utf16};
use quick_xml::events::Event;
use serde_json::{Value, json};

use super::*;

// ---- disk images: partition tables, filesystems by their magic (backdown_core::magic), ISO 9660,
// virtual disks

/// A power-of-two size the way formats document it (64 KiB, 2 MiB).
pub(super) fn binary(n: u64) -> String {
    match n {
        n if n >= 1 << 30 && n.is_multiple_of(1 << 30) => format!("{} GiB", n >> 30),
        n if n >= 1 << 20 && n.is_multiple_of(1 << 20) => format!("{} MiB", n >> 20),
        n if n >= 1 << 10 && n.is_multiple_of(1 << 10) => format!("{} KiB", n >> 10),
        n => format!("{n} bytes"),
    }
}

/// A partition table: its kind, the partitions, and facts about the disk.
pub(super) type Table = (&'static str, Vec<Value>, Vec<(String, String)>);

/// The partition table at `base` (GPT, or MBR with its extended partitions), each partition with a
/// filesystem guess. None if there's no table there.
pub(super) fn partitions(f: &File, base: u64) -> Result<Option<Table>> {
    let mbr = read_at(f, base, 512)?;
    if !has(&mbr, 510, &[0x55, 0xAA]) {
        return Ok(None);
    }
    let slot = |i: usize| &mbr[446 + 16 * i..462 + 16 * i];
    let sane = (0..4).all(|i| {
        let e = slot(i);
        matches!(e[0], 0 | 0x80) && (e[4] == 0 || (le(e, 8, 4) > 0 && le(e, 12, 4) > 0))
    });
    if !sane || (0..4).all(|i| slot(i)[4] == 0) {
        return Ok(None);
    }
    let mut parts = Vec::new();
    let mut details = Vec::new();
    let probe = |start: u64| probe_fs(f, base + start).ok().flatten();
    if (0..4).any(|i| slot(i)[4] == 0xEE) {
        for ss in [512u64, 4096] {
            let h = read_at(f, base + ss, 512)?;
            if !has(&h, 0, b"EFI PART") {
                continue;
            }
            details.push(("Disk GUID".into(), guid(&h[56..72])));
            let (lba, n, size) = (le(&h, 72, 8), le(&h, 80, 4).min(256), le(&h, 84, 4));
            if !(128..=4096).contains(&size) {
                bail!("damaged GPT header");
            }
            let table = read_at(f, base + lba * ss, (n * size) as usize)?;
            for (i, e) in table.chunks_exact(size as usize).enumerate() {
                if e[..16].iter().all(|&b| b == 0) {
                    continue;
                }
                let t = guid(&e[..16]);
                let (first, last) = (le(e, 32, 8), le(e, 40, 8));
                let start = first * ss;
                parts.push(json!({
                    "index": i + 1,
                    "type": gpt_type(&t),
                    "code": t,
                    "name": Some(utf16(&e[56..128], false)).filter(|s| !s.is_empty()),
                    "start": start,
                    "size": (last.saturating_sub(first) + 1) * ss,
                    "fs": probe(start),
                }));
            }
            return Ok(Some(("GPT", parts, details)));
        }
    }
    details.push(("Disk signature".into(), format!("{:08X}", le(&mbr, 440, 4))));
    for i in 0..4 {
        let e = slot(i);
        if e[4] == 0 {
            continue;
        }
        let (start, len) = (le(e, 8, 4) * 512, le(e, 12, 4) * 512);
        let extended = matches!(e[4], 0x05 | 0x0F | 0x85);
        parts.push(json!({
            "index": i + 1,
            "type": mbr_type(e[4]),
            "code": format!("0x{:02X}", e[4]),
            "start": start,
            "size": len,
            "bootable": e[0] == 0x80,
            "fs": if extended { None } else { probe(start) },
        }));
        if extended {
            // logical partitions: a chain of EBRs, each relative to the extended partition's start
            let mut ebr = start;
            for n in 0..64 {
                let b = read_at(f, base + ebr, 512)?;
                if !has(&b, 510, &[0x55, 0xAA]) || b[446 + 4] == 0 {
                    break;
                }
                let s = ebr + le(&b, 446 + 8, 4) * 512;
                parts.push(json!({
                    "index": 5 + n,
                    "type": mbr_type(b[446 + 4]),
                    "code": format!("0x{:02X}", b[446 + 4]),
                    "start": s,
                    "size": le(&b, 446 + 12, 4) * 512,
                    "bootable": b[446] == 0x80,
                    "fs": probe(s),
                }));
                let next = le(&b, 462 + 8, 4);
                if next == 0 {
                    break;
                }
                ebr = start + next * 512;
            }
        }
    }
    Ok(Some(("MBR", parts, details)))
}

/// A raw disk's layout at `base`: its partition table, else the filesystem that fills it.
pub(super) fn raw_layout(
    f: &File,
    base: u64,
    out: &mut serde_json::Map<String, Value>,
) -> Result<bool> {
    // a volume boot record also ends in 55 AA, so a filesystem at sector 0 wins over an "MBR"
    // (ISO 9660 starts at 32 KiB, though, and hybrid images carry a partition table too)
    let fs = probe_fs(f, base)?;
    let table = match &fs {
        Some(v) if v.kind != "ISO 9660" => None,
        _ => partitions(f, base)?,
    };
    if let Some((table, parts, details)) = table {
        out.insert("table".into(), json!(table));
        out.insert("partitions".into(), json!(parts));
        for (k, v) in details {
            detail(out, &k, v);
        }
        return Ok(true);
    }
    if let Some(fs) = fs {
        out.insert("fs".into(), json!(fs));
        return Ok(true);
    }
    Ok(false)
}

pub(super) struct IsoVol {
    pub(super) block: u64,
    pub(super) root: (u64, u64),
    pub(super) joliet: bool,
    pub(super) rock_ridge: bool,
    pub(super) pvd: Vec<u8>,
    pub(super) boot: bool,
    pub(super) udf: bool,
}

pub(super) struct IsoEnt {
    pub(super) name: String,
    pub(super) lba: u64,
    pub(super) size: u64,
    pub(super) dir: bool,
    pub(super) mtime: Option<i64>,
}

/// The ISO 9660 volume descriptors; names come from Rock Ridge if the tree has them, else Joliet,
/// else the plain 8.3-ish names.
pub(super) fn iso_volume(f: &File) -> Result<Option<IsoVol>> {
    let (mut pvd, mut svd, mut boot, mut udf) = (None, None, false, false);
    for i in 16..48u64 {
        let d = read_at(f, i * 2048, 2048)?;
        if d.len() < 2048 {
            break;
        }
        match &d[1..6] {
            b"CD001" => match d[0] {
                0 => boot = true,
                1 if pvd.is_none() => pvd = Some(d),
                2 if matches!(&d[88..91], b"%/@" | b"%/C" | b"%/E") => svd = Some(d),
                _ => {}
            },
            b"BEA01" | b"NSR02" | b"NSR03" | b"TEA01" => udf = true,
            _ => break,
        }
    }
    let Some(pvd) = pvd else {
        return Ok(None);
    };
    let block = match le(&pvd, 128, 2) {
        512 | 1024 | 2048 | 4096 => le(&pvd, 128, 2),
        _ => 2048,
    };
    let mut v = IsoVol {
        block,
        root: (le(&pvd, 158, 4), le(&pvd, 166, 4)),
        joliet: false,
        rock_ridge: true,
        pvd,
        boot,
        udf,
    };
    let (_, rr) = iso_dir(f, &v, v.root)?;
    v.rock_ridge = rr;
    if !rr && let Some(s) = svd {
        v.root = (le(&s, 158, 4), le(&s, 166, 4));
        v.joliet = true;
    }
    Ok(Some(v))
}

/// A directory's entries (at most `MAX_ENTRIES`, 4 MB of records), and whether any carried a Rock
/// Ridge name.
pub(super) fn iso_dir(f: &File, v: &IsoVol, (lba, len): (u64, u64)) -> Result<(Vec<IsoEnt>, bool)> {
    let data = read_at(f, lba * v.block, len.min(4 << 20) as usize)?;
    let (mut out, mut rr, mut i) = (Vec::<IsoEnt>::new(), false, 0usize);
    let block = v.block as usize;
    while i < data.len() && out.len() < MAX_ENTRIES {
        let rl = data[i] as usize;
        if rl == 0 {
            i = (i / block + 1) * block; // records never cross a block boundary
            continue;
        }
        if rl < 34 || i + rl > data.len() {
            break;
        }
        let r = &data[i..i + rl];
        i += rl;
        let nl = (r[32] as usize).min(rl - 33);
        let raw = &r[33..33 + nl];
        if nl == 1 && raw[0] <= 1 {
            continue; // "." and ".."
        }
        let mut name = String::new();
        if v.rock_ridge && !v.joliet {
            let mut su = &r[(33 + nl + (1 - nl % 2)).min(rl)..];
            while su.len() >= 4 {
                let n = su[2] as usize;
                if n < 4 || n > su.len() {
                    break;
                }
                if &su[..2] == b"NM" && n > 5 {
                    rr = true;
                    name.push_str(&String::from_utf8_lossy(&su[5..n]));
                }
                su = &su[n..];
            }
        }
        if name.is_empty() {
            name = if v.joliet {
                utf16(raw, true)
            } else {
                String::from_utf8_lossy(raw).into_owned()
            };
            if let Some((stem, ver)) = name.rsplit_once(';')
                && ver.chars().all(|c| c.is_ascii_digit())
            {
                name = stem.to_string();
            }
            if name.ends_with('.') && !name.starts_with('.') {
                name.pop();
            }
        }
        let size = le(r, 10, 4);
        // a file over 4 GiB is several records with the same name
        if let Some(prev) = out.last_mut()
            && prev.name == name
            && !prev.dir
        {
            prev.size += size;
            continue;
        }
        let d = &r[18..25];
        let mtime = (d[1] >= 1 && d[1] <= 12 && d[2] >= 1).then(|| {
            days_from_civil(1900 + i64::from(d[0]), i64::from(d[1]), i64::from(d[2])) * 86_400
                + i64::from(d[3]) * 3600
                + i64::from(d[4]) * 60
                + i64::from(d[5])
                - i64::from(d[6] as i8) * 15 * 60
        });
        out.push(IsoEnt {
            name,
            lba: le(r, 2, 4),
            size,
            dir: r[25] & 2 != 0,
            mtime,
        });
    }
    Ok((out, rr))
}

/// Walks `path` ("a/b/c", from the ISO's root) through the directory tree.
pub(super) fn iso_find(f: &File, v: &IsoVol, path: &str) -> Result<IsoEnt> {
    let mut cur = IsoEnt {
        name: String::new(),
        lba: v.root.0,
        size: v.root.1,
        dir: true,
        mtime: None,
    };
    for (depth, seg) in path.split('/').filter(|s| !s.is_empty()).enumerate() {
        if !cur.dir || depth > 64 {
            bail!("no {path:?} in this image");
        }
        let (list, _) = iso_dir(f, v, (cur.lba, cur.size))?;
        cur = match list.iter().position(|e| e.name == seg) {
            Some(i) => list.into_iter().nth(i),
            None => list.into_iter().find(|e| e.name.eq_ignore_ascii_case(seg)),
        }
        .with_context(|| format!("no {path:?} in this image"))?;
    }
    Ok(cur)
}

pub(super) fn iso_json(
    f: &File,
    v: &IsoVol,
    dir: Option<&str>,
    out: &mut serde_json::Map<String, Value>,
) -> Result<()> {
    let p = &v.pvd;
    let label = field(p, 40, 32);
    let mut details = vec![
        ("Volume".to_string(), label.clone()),
        ("Size".into(), fmt::bytes(le(p, 80, 4) * v.block)),
        (
            "Names".into(),
            if v.rock_ridge {
                "Rock Ridge (Unix)".into()
            } else if v.joliet {
                "Joliet (Windows)".into()
            } else {
                "ISO 9660".into()
            },
        ),
    ];
    for (k, at, n) in [
        ("System", 8, 32),
        ("Publisher", 318, 128),
        ("Application", 574, 128),
    ] {
        let s = field(p, at, n);
        if !s.is_empty() {
            details.push((k.into(), s));
        }
    }
    let c = field(p, 813, 14);
    if c.len() == 14 && c.chars().all(|x| x.is_ascii_digit()) && !c.starts_with("0000") {
        details.push((
            "Created".into(),
            format!(
                "{}-{}-{} {}:{}",
                &c[..4],
                &c[4..6],
                &c[6..8],
                &c[8..10],
                &c[10..12]
            ),
        ));
    }
    if v.boot {
        details.push(("Bootable".into(), "yes (El Torito)".into()));
    }
    if v.udf {
        details.push(("Also".into(), "a UDF filesystem (not listed here)".into()));
    }
    let path = dir.unwrap_or("").trim_matches('/');
    let here = iso_find(f, v, path)?;
    if !here.dir {
        bail!("{path} is a file");
    }
    let (list, _) = iso_dir(f, v, (here.lba, here.size))?;
    out.insert("label".into(), json!(label));
    out.insert("details".into(), json!(details));
    out.insert("path".into(), json!(path));
    out.insert(
        "entries".into(),
        json!(
            list.iter()
                .map(|e| json!({ "name": e.name, "size": e.size, "dir": e.dir, "mtime": e.mtime }))
                .collect::<Vec<_>>()
        ),
    );
    out.insert("truncated".into(), json!(list.len() >= MAX_ENTRIES));
    Ok(())
}

pub(super) fn detail(out: &mut serde_json::Map<String, Value>, k: &str, v: impl Into<String>) {
    if let Some(d) = out
        .entry("details")
        .or_insert_with(|| json!([]))
        .as_array_mut()
    {
        d.push(json!([k, v.into()]));
    }
}

/// VMDK descriptor text: create type, adapter, parent, extents (and their total size).
pub(super) fn vmdk_descriptor(text: &str, out: &mut serde_json::Map<String, Value>) -> u64 {
    let mut total = 0;
    let mut extents = Vec::new();
    for l in text.lines().map(str::trim) {
        if let Some((k, v)) = l.split_once('=') {
            let v = v.trim().trim_matches('"');
            match k.trim() {
                "createType" => detail(out, "Type", v),
                "ddb.adapterType" => detail(out, "Adapter", v),
                "parentFileNameHint" => detail(out, "Parent", v),
                "ddb.virtualHWVersion" => detail(out, "Hardware version", v),
                _ => {}
            }
            continue;
        }
        let mut w = l.split_whitespace();
        if let (Some("RW" | "RDONLY" | "NOACCESS"), Some(n), Some(kind)) =
            (w.next(), w.next(), w.next())
            && let Ok(n) = n.parse::<u64>()
        {
            total += n * 512;
            let file = l.split('"').nth(1).unwrap_or("");
            if extents.len() < 20 {
                extents.push(format!("{kind} {} {file}", fmt::bytes(n * 512)));
            }
        }
    }
    if !extents.is_empty() {
        detail(out, "Extents", extents.join("; "));
    }
    total
}

pub(super) const VHDX_METADATA: &str = "8B7CA206-4790-4B9A-B8FE-575F050F886E";
pub(super) const VHDX_SIZE: &str = "2FA54224-CD1B-4876-B211-5DBED83BF4B8";
pub(super) const VHDX_PARAMS: &str = "CAA16737-FA36-4D43-B3B6-33F0AA44E76B";
pub(super) const VHDX_SECTOR: &str = "8141BF1D-A96F-4709-BA47-F233A8FAAB5F";

pub(super) fn vhdx(
    f: &File,
    head: &[u8],
    out: &mut serde_json::Map<String, Value>,
) -> Result<Option<u64>> {
    let creator = utf16(head.get(8..520).unwrap_or_default(), false);
    if !creator.is_empty() {
        detail(out, "Created by", creator);
    }
    let rt = read_at(f, 192 << 10, 64 << 10)?;
    if !has(&rt, 0, b"regi") {
        return Ok(None);
    }
    let n = le(&rt, 8, 4).min(2047) as usize;
    let Some(meta) = (0..n)
        .map(|i| &rt[16 + 32 * i..48 + 32 * i])
        .find(|e| guid(&e[..16]) == VHDX_METADATA)
        .map(|e| le(e, 16, 8))
    else {
        return Ok(None);
    };
    let mt = read_at(f, meta, 64 << 10)?;
    if !has(&mt, 0, b"metadata") {
        return Ok(None);
    }
    let mut size = None;
    for i in 0..(le(&mt, 10, 2) as usize).min(2047) {
        let e = &mt[32 + 32 * i..64 + 32 * i];
        let at = meta + le(e, 16, 4);
        match guid(&e[..16]).as_str() {
            VHDX_SIZE => size = Some(le(&read_at(f, at, 8)?, 0, 8)),
            VHDX_SECTOR => detail(
                out,
                "Sector size",
                format!("{} bytes", le(&read_at(f, at, 4)?, 0, 4)),
            ),
            VHDX_PARAMS => {
                let p = read_at(f, at, 8)?;
                detail(out, "Block size", binary(le(&p, 0, 4)));
                detail(
                    out,
                    "Kind",
                    if le(&p, 4, 4) & 2 != 0 {
                        "differencing (needs its parent)"
                    } else if le(&p, 4, 4) & 1 != 0 {
                        "fixed"
                    } else {
                        "dynamic"
                    },
                );
            }
            _ => {}
        }
    }
    Ok(size)
}

/// A UDIF "mish" block (one partition's chunk table, base64 in the plist): its size in bytes and
/// the compressions its chunks use.
pub(super) fn mish(b64: &str) -> Option<(u64, Vec<&'static str>)> {
    use base64::Engine;
    let clean: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
    let m = base64::engine::general_purpose::STANDARD
        .decode(clean)
        .ok()?;
    if !has(&m, 0, b"mish") {
        return None;
    }
    let n = (be(&m, 200, 4) as usize).min(1 << 20);
    let chunks = m.get(204..204 + 40 * n).unwrap_or_default();
    let kinds = chunks
        .as_chunks::<40>()
        .0
        .iter()
        .filter_map(|c| match be(c, 0, 4) {
            0x8000_0004 => Some("ADC"),
            0x8000_0005 => Some("zlib"),
            0x8000_0006 => Some("bzip2"),
            0x8000_0007 => Some("LZFSE"),
            0x8000_0008 => Some("LZMA"),
            0x0000_0001 => Some("raw"),
            _ => None,
        })
        .collect();
    Some((be(&m, 16, 8) * 512, kinds))
}

/// Apple UDIF (.dmg): the "koly" trailer and, from its plist, each partition and how its chunks
/// are compressed.
pub(super) fn dmg(f: &File, k: &[u8], out: &mut serde_json::Map<String, Value>) -> Result<u64> {
    let sectors = be(k, 492, 8);
    detail(out, "Data", fmt::bytes(be(k, 32, 8)));
    if be(k, 12, 4) & 4 != 0 {
        detail(out, "Flags", "internet-enabled");
    }
    let (xml_at, xml_len) = (be(k, 216, 8), be(k, 224, 8));
    let mut kinds = std::collections::BTreeSet::new();
    let mut parts = Vec::new();
    if xml_len > 0 && xml_len <= XML_CAP {
        let bytes = read_at(f, xml_at, xml_len as usize)?;
        let mut r = xml(&bytes);
        let (mut key, mut depth, mut blkx, mut name) = (String::new(), 0usize, None, String::new());
        loop {
            match r.read_event()? {
                Event::Start(e) => {
                    depth += 1;
                    match e.local_name().as_ref() {
                        b"key" => {
                            key = r.read_text(e.name())?.decode()?.into_owned();
                            depth -= 1;
                        }
                        b"array" if key == "blkx" && blkx.is_none() => blkx = Some(depth),
                        b"dict" if blkx.is_some_and(|b| depth == b + 1) => name.clear(),
                        b"string" if blkx.is_some_and(|b| depth == b + 2) => {
                            let t = r.read_text(e.name())?.decode()?.into_owned();
                            depth -= 1;
                            if key == "Name" || (key == "CFName" && name.is_empty()) {
                                name = t;
                            }
                        }
                        b"data" if blkx.is_some_and(|b| depth == b + 2) && key == "Data" => {
                            let t = r.read_text(e.name())?.decode()?.into_owned();
                            depth -= 1;
                            if let Some((size, used)) = mish(&t) {
                                kinds.extend(used);
                                if parts.len() < 64 {
                                    parts.push(json!({ "name": name, "size": size }));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Event::End(e) => {
                    if e.local_name().as_ref() == b"array" && blkx == Some(depth) {
                        blkx = Some(usize::MAX); // done with the one we wanted
                    }
                    depth = depth.saturating_sub(1);
                }
                Event::Eof => break,
                _ => {}
            }
        }
    }
    let packed: Vec<&str> = kinds.iter().copied().filter(|k| *k != "raw").collect();
    detail(
        out,
        "Compression",
        if packed.is_empty() {
            "none (read/write or read-only image)".to_string()
        } else {
            let fmt_name = match packed[0] {
                "zlib" => " (UDZO)",
                "bzip2" => " (UDBZ)",
                "LZFSE" => " (ULFO)",
                "LZMA" => " (ULMO)",
                _ => "",
            };
            format!("{}{fmt_name}", packed.join(", "))
        },
    );
    out.insert("compressed".into(), json!(!packed.is_empty()));
    if !parts.is_empty() {
        out.insert("dmg_parts".into(), json!(parts));
    }
    Ok(sectors * 512)
}

/// What a disk image holds, reading only headers and tables: an ISO's files (the folder `dir`),
/// a raw image's partitions and their filesystems, or a virtual disk's format and size.
pub fn disk(path: &Path, dir: Option<&str>) -> Result<Value> {
    let f = File::open(path)?;
    let len = f.metadata()?.len();
    let head = read_at(&f, 0, 4096)?;
    let tail = if len >= 512 {
        read_at(&f, len - 512, 512)?
    } else {
        Vec::new()
    };
    let mut out = serde_json::Map::new();
    out.insert("file_size".into(), json!(len));
    fn put(out: &mut serde_json::Map<String, Value>, format: &str, size: Option<u64>) {
        out.insert("format".into(), json!(format));
        if let Some(s) = size.filter(|&s| s > 0) {
            out.insert("virtual_size".into(), json!(s));
        }
    }
    if has(&head, 0, b"QFI\xfb") {
        let v = be(&head, 4, 4);
        let (size, crypt) = if v == 1 {
            (be(&head, 24, 8), be(&head, 36, 4))
        } else {
            (be(&head, 24, 8), be(&head, 32, 4))
        };
        put(
            &mut out,
            &format!("qcow{}", if v == 1 { "" } else { "2" }),
            Some(size),
        );
        detail(&mut out, "Version", v.to_string());
        let cluster = if v == 1 {
            head[32] as u64
        } else {
            be(&head, 20, 4)
        };
        if cluster < 32 {
            detail(&mut out, "Cluster size", binary(1 << cluster));
        }
        if crypt != 0 {
            detail(
                &mut out,
                "Encrypted",
                if crypt == 1 { "AES" } else { "LUKS" },
            );
        }
        if v >= 2 && be(&head, 60, 4) > 0 {
            detail(&mut out, "Snapshots", be(&head, 60, 4).to_string());
        }
        if v >= 3 && be(&head, 72, 8) & 1 != 0 {
            detail(&mut out, "State", "dirty (not closed cleanly)");
        }
        let (bo, bl) = (be(&head, 8, 8), be(&head, 16, 4).min(1023));
        if bo > 0 && bl > 0 {
            detail(
                &mut out,
                "Backing file",
                String::from_utf8_lossy(&read_at(&f, bo, bl as usize)?),
            );
        }
    } else if has(&head, 0, b"KDMV") {
        put(&mut out, "vmdk", Some(le(&head, 12, 8) * 512));
        let flags = le(&head, 8, 4);
        if flags & (1 << 16) != 0 {
            detail(&mut out, "Grains", "compressed (stream-optimized)");
        }
        let (at, n) = (
            le(&head, 28, 8) * 512,
            (le(&head, 36, 8) * 512).min(64 << 10),
        );
        if at > 0 && n > 0 {
            let text = read_at(&f, at, n as usize)?;
            vmdk_descriptor(&String::from_utf8_lossy(&text).replace('\0', ""), &mut out);
        }
    } else if head.starts_with(b"# Disk DescriptorFile") {
        let text = String::from_utf8_lossy(&read_at(&f, 0, 64 << 10)?).into_owned();
        let total = vmdk_descriptor(&text, &mut out);
        put(&mut out, "vmdk", Some(total));
        detail(
            &mut out,
            "Note",
            "a descriptor: the data is in the extent files it names",
        );
    } else if has(&head, 0, b"vhdxfile") {
        let size = vhdx(&f, &head, &mut out)?;
        put(&mut out, "vhdx", size);
    } else if le(&head, 0x40, 4) == 0xBEDA_107F {
        let size = le(&head, 0x170, 8);
        put(&mut out, "vdi", Some(size));
        let kind = le(&head, 0x4C, 4);
        detail(
            &mut out,
            "Kind",
            match kind {
                1 => "dynamic",
                2 => "fixed",
                3 => "undo",
                4 => "differencing (needs its parent)",
                _ => "unknown",
            },
        );
        let comment = field(&head, 0x54, 256);
        if !comment.is_empty() {
            detail(&mut out, "Comment", comment);
        }
        detail(
            &mut out,
            "Allocated",
            format!(
                "{} of {}",
                fmt::bytes(le(&head, 0x184, 4) * le(&head, 0x178, 4)),
                fmt::bytes(size)
            ),
        );
        if kind == 2 {
            raw_layout(&f, le(&head, 0x158, 4), &mut out)?;
        }
    } else if has(&tail, 0, b"conectix") || has(&head, 0, b"conectix") {
        let ft = if has(&tail, 0, b"conectix") {
            &tail[..]
        } else {
            &head[..512]
        };
        put(&mut out, "vhd", Some(be(ft, 48, 8)));
        let kind = be(ft, 60, 4);
        detail(
            &mut out,
            "Kind",
            match kind {
                2 => "fixed",
                3 => "dynamic",
                4 => "differencing (needs its parent)",
                _ => "unknown",
            },
        );
        detail(&mut out, "Created by", field(ft, 28, 4));
        if kind == 4 {
            let dh = read_at(&f, be(ft, 16, 8), 1024)?;
            if has(&dh, 0, b"cxsparse") {
                detail(&mut out, "Parent", utf16(&dh[64..576], true));
            }
        }
        if kind == 2 {
            raw_layout(&f, 0, &mut out)?;
        }
    } else if has(&tail, 0, b"koly") {
        let size = dmg(&f, &tail, &mut out)?;
        put(&mut out, "dmg", Some(size));
    } else if has(&head, 0, b"encrcdsa") || has(&tail, 0, b"cdsaencr") {
        put(&mut out, "dmg", None);
        detail(
            &mut out,
            "Encrypted",
            "yes (needs its password; nothing more to show)",
        );
    } else if let Some(v) = iso_volume(&f)? {
        put(&mut out, "iso9660", None);
        iso_json(&f, &v, dir, &mut out)?;
        // a hybrid image (most Linux installers) also carries a partition table
        if let Some((table, parts, _)) = partitions(&f, 0)? {
            out.insert("table".into(), json!(table));
            out.insert("partitions".into(), json!(parts));
        }
    } else {
        put(&mut out, "raw", Some(len));
        if !raw_layout(&f, 0, &mut out)? {
            out.insert("format".into(), json!("unknown"));
        }
    }
    Ok(Value::Object(out))
}
