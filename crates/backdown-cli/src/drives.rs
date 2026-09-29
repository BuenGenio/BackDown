//! The browser's requests that reach drives rather than the index, for the person at the screen
//! (their own `serve` or daemon, never the machine-wide service): mount a partition or attach a
//! disk image read-only, and undo only that (`mounting`); let BackDown read a drive through udisks
//! (`grant`, Linux); browse a filesystem a Rescue found; copy files out of a filesystem.
//!
//! Sources are looked up here, never taken from the browser as paths: a device must be one the
//! volume probe lists, a disk image is picked by its index id, a found filesystem by its place on
//! a Rescue record (which names the drive or image). Everything reads; nothing is mounted
//! read-write.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use backdown_core::{Db, browse, rescue, volumes};
use serde_json::{Value, json};

use crate::{grant, locations, mounting, peek};

/// An error for the browser: what happened, and when there is one, the command to run instead.
pub fn error_json(e: &anyhow::Error) -> Value {
    if let Some(r) = e.downcast_ref::<mounting::Refused>() {
        return json!({ "error": r.message, "command": r.command });
    }
    if let Some(d) = e.downcast_ref::<grant::Denied>() {
        return json!({ "error": d.said(), "denied": d.name });
    }
    json!({ "error": format!("{e:#}") })
}

/// The requests that change something (POSTs), by path.
pub fn post(db: &Path, path: &str, param: &dyn Fn(&str) -> Option<String>) -> Result<Value> {
    let state = locations::state_dir();
    match path {
        "/api/mount" => {
            let dev = param("device").context("which partition?")?;
            check_mountable(&dev)?;
            mounting::mount(&state, &dev)
        }
        "/api/unmount" => mounting::unmount(&state, &param("target").context("unmount what?")?),
        "/api/attach" | "/api/detach" => {
            let id: i64 = param("id")
                .context("which disk image?")?
                .parse()
                .context("bad id")?;
            let db = Db::open_read_only(db)?;
            let (image, _) =
                browse::resolve_content(&db, id)?.context("that image isn't on any drive now")?;
            if path == "/api/attach" {
                mounting::attach(&state, &image)
            } else {
                let image = dunce::canonicalize(&image).unwrap_or(image);
                mounting::unmount(&state, &image.display().to_string())
            }
        }
        "/api/rescue-allow" => allow(&param("path").context("which drive?")?),
        _ => bail!("no such endpoint"),
    }
}

/// What BackDown has mounted or attached (and can undo); whether this BackDown mounts partitions
/// (`can`) and images (`images`) at all; and, with `image`, what's attached from that image.
pub fn mounts(desktop: bool, image: Option<&Path>) -> Value {
    let all = mounting::current(&locations::state_dir());
    let image = image.and_then(|p| {
        let s = dunce::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .display()
            .to_string();
        all.iter().find(|m| m.source == s).cloned()
    });
    json!({
        "can": desktop && cfg!(any(target_os = "linux", target_os = "macos")),
        "images": desktop && cfg!(any(target_os = "linux", target_os = "macos", windows)),
        "mounts": all,
        "image": image,
    })
}

/// Every volume the probe found, holders included: (device, state, first mount point).
fn probed() -> Result<Vec<(String, volumes::State, Option<String>)>> {
    fn walk(v: &volumes::Volume, out: &mut Vec<(String, volumes::State, Option<String>)>) {
        out.push((
            v.device.clone(),
            v.state,
            v.mounts.first().map(|m| m.path.clone()),
        ));
        for h in &v.holders {
            walk(h, out);
        }
    }
    let mut out = Vec::new();
    for d in volumes::probe()?.disks {
        out.push((d.device.clone(), volumes::State::Unknown, None));
        for v in &d.volumes {
            walk(v, &mut out);
        }
    }
    Ok(out)
}

/// `dev` is a partition the probe calls mountable: a known filesystem, not mounted, not the
/// system's, not locked or part of something bigger.
pub fn check_mountable(dev: &str) -> Result<()> {
    let all = probed()?;
    let Some((_, state, at)) = all.iter().find(|(d, _, _)| d == dev) else {
        bail!("{dev} isn't a drive or partition on this computer");
    };
    use volumes::State::*;
    match state {
        Mountable => Ok(()),
        Mounted => bail!(
            "{dev} is already mounted{}",
            at.as_deref()
                .map(|a| format!(" at {a}"))
                .unwrap_or_default()
        ),
        System => bail!("{dev} belongs to the running system: it's left alone"),
        Encrypted => {
            bail!("{dev} is encrypted: unlock it first (the file manager asks for its passphrase)")
        }
        Member => bail!("{dev} is part of something bigger (LVM, RAID…): it's read through that"),
        Swap => bail!("{dev} is swap space: there are no files on it"),
        Empty => bail!(
            "there's no filesystem on {dev}: Rescue can look for lost partitions and files on it"
        ),
        Unknown => bail!("BackDown can't tell what's on {dev}, so it doesn't mount it"),
    }
}

/// Asks the system (udisks, which may show a password dialog) for a read-only file for `path`,
/// and keeps it for this process: rescues and browsing then read the drive through it.
fn allow(path: &str) -> Result<Value> {
    if !cfg!(target_os = "linux") {
        bail!("this is only possible on Linux; use the command instead");
    }
    // only a drive or partition the probe lists
    if !probed()?.iter().any(|(d, _, _)| d == path) {
        bail!("{path} isn't a drive or partition on this computer");
    }
    let f = grant::open_for_backup(Path::new(path), true)?;
    rescue::grant(Path::new(path), f)?;
    Ok(json!(rescue::access(Path::new(path))))
}

/// Where a filesystem is that a Rescue record found, or that the partition table it read lists:
/// the drive or image the record names, its first byte and its length, and what it is.
pub fn found_region(db: &Db, rescue_id: i64, at: u64) -> Result<(PathBuf, u64, u64, String)> {
    let r = rescue::load(db, rescue_id)?;
    if r.mode != "parts" {
        bail!("that rescue didn't look for partitions");
    }
    let size = r.result["size"].as_u64().unwrap_or(0);
    let rest = size.saturating_sub(at);
    let hit = r.result["found"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|f| f["start"].as_u64() == Some(at))
        .map(|f| {
            let label = f["label"]
                .as_str()
                .map(|l| format!(" “{l}”"))
                .unwrap_or_default();
            (
                f["size"].as_u64().unwrap_or(rest),
                format!("{}{label}", f["fs"].as_str().unwrap_or("file system")),
            )
        })
        .or_else(|| {
            r.result["table"]["entries"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|e| e["start"].as_u64() == Some(at))
                .map(|e| {
                    (
                        e["size"].as_u64().unwrap_or(rest),
                        format!("partition {}", e["index"]),
                    )
                })
        });
    let Some((len, what)) = hit else {
        bail!("that rescue found no file system at byte {at}");
    };
    Ok((PathBuf::from(&r.source), at, len, what))
}

/// A request to copy files out of a filesystem, checked: the source and where on it (from a
/// Rescue record's found filesystem, or a disk image in the index and its partition), the
/// destination, and the record's options.
pub fn files_request(
    db: &Db,
    param: &dyn Fn(&str) -> Option<String>,
) -> Result<(PathBuf, PathBuf, Value)> {
    let (src, start, len, what) = match (param("from"), param("id")) {
        (Some(from), _) => found_region(
            db,
            from.parse().context("bad rescue")?,
            param("at")
                .context("where on the drive?")?
                .parse()
                .context("bad at")?,
        )?,
        (None, Some(id)) => {
            let (image, _) = browse::resolve_content(db, id.parse().context("bad id")?)?
                .context("that image isn't on any drive now")?;
            let part = param("part").unwrap_or_else(|| "0".into());
            let (start, len) = peek::fs_region(&image, &part)?;
            (image, start, len, format!("partition {part}"))
        }
        _ => bail!("copy files out of what?"),
    };
    let dest = PathBuf::from(param("dest").context("where should the files go?")?.trim());
    if !dest.is_absolute() {
        bail!("the files need a full path, on another drive");
    }
    let a = rescue::access(&src);
    if !a.readable {
        bail!(
            "{} can't be read: {}",
            src.display(),
            a.why.unwrap_or_default()
        );
    }
    if a.device && rescue::carve::same_device(&src, &dest) {
        bail!(
            "{} is on the drive being read: saving there could overwrite the very files being recovered. Choose a folder on another drive",
            dest.display()
        );
    }
    let items: Vec<String> =
        serde_json::from_str(&param("items").context("which files?")?).context("bad items")?;
    if items.is_empty() || items.len() > 10_000 {
        bail!("choose between 1 and 10,000 files and folders");
    }
    let opts = rescue::files::Options {
        start,
        len: Some(len),
        items,
        with_deleted: param("deleted").as_deref() == Some("1"),
        resume: false,
    };
    let mut o = serde_json::to_value(&opts)?;
    o["what"] = json!(what);
    Ok((src, dest, o))
}
