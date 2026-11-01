//! `/proc/self/mountinfo` (Linux): what's mounted where, read the same way by everything that asks
//! (the volume probe, the NTFS file-table reader, Rescue's same-disk checks, and mounting drives).
//!
//! One line per mount, in the order they were mounted:
//! `id parent major:minor root mount-point options [optional…] - fstype source super-options`.
//! Fields are separated by single spaces; the kernel writes a space, tab, newline or backslash
//! inside a field as an octal escape (`\040`, `\011`, `\012`, `\134`). Paths are kept as the bytes
//! they are, so a mount point whose name isn't UTF-8 still matches the path it is.

use std::path::{Path, PathBuf};

/// Where the running process's mounts are listed.
pub const PATH: &str = "/proc/self/mountinfo";

/// One mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// the device number, (major, minor): 0:N for btrfs, FUSE and other filesystems without one
    pub dev: (u64, u64),
    /// the folder of the mounted filesystem that appears at `point` (usually "/")
    pub root: PathBuf,
    pub point: PathBuf,
    pub fstype: String,
    /// what was mounted: a device (`/dev/sdb1`), a share (`//nas/x`) or just a name (`tmpfs`)
    pub source: String,
    /// read-only, by the mount's own options or the filesystem's
    pub ro: bool,
}

/// This process's mounts (none if mountinfo can't be read).
pub fn read() -> Vec<Mount> {
    read_from(Path::new(PATH))
}

/// The mounts listed in a mountinfo file.
pub fn read_from(path: &Path) -> Vec<Mount> {
    std::fs::read(path).map(|b| parse(&b)).unwrap_or_default()
}

/// The mounts in mountinfo's text; lines that aren't one are skipped.
pub fn parse(text: &[u8]) -> Vec<Mount> {
    text.split(|&c| c == b'\n').filter_map(line).collect()
}

fn line(l: &[u8]) -> Option<Mount> {
    let f: Vec<&[u8]> = l.split(|&c| c == b' ').collect();
    // the optional fields end at a lone "-"
    let sep = 6 + f.get(6..)?.iter().position(|x| *x == b"-")?;
    let (maj, min) = std::str::from_utf8(f.get(2)?).ok()?.split_once(':')?;
    let ro = |opts: &[u8]| opts.split(|&c| c == b',').any(|o| o == b"ro");
    Some(Mount {
        dev: (maj.parse().ok()?, min.parse().ok()?),
        root: path(f.get(3)?),
        point: path(f.get(4)?),
        fstype: String::from_utf8_lossy(f.get(sep + 1)?).into_owned(),
        source: unescape(f.get(sep + 2)?),
        ro: ro(f.get(5)?) || f.get(sep + 3).is_some_and(|o| ro(o)),
    })
}

/// The mount `path` is on: the deepest mount point containing it, and of several mounts on that
/// point the latest (it hides the ones before it).
pub fn containing<'a>(mounts: &'a [Mount], path: &Path) -> Option<&'a Mount> {
    mounts
        .iter()
        .filter(|m| path.starts_with(&m.point))
        .max_by_key(|m| m.point.components().count())
}

/// The block device (its device number) that the filesystem holding `path` was mounted from, if
/// it was mounted from one.
#[cfg(unix)]
pub fn block_device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let path = std::fs::canonicalize(path).ok()?;
    let mounts = read();
    let source = &containing(&mounts, &path)?.source;
    // a name like `tmpfs` isn't a path to look at
    if !source.starts_with('/') {
        return None;
    }
    let meta = std::fs::metadata(source).ok()?;
    meta.file_type().is_block_device().then(|| meta.rdev())
}

/// A field with mountinfo's octal escapes undone, as text (bytes that aren't UTF-8 become U+FFFD).
/// /proc/swaps escapes its file names the same way.
pub fn unescape(field: impl AsRef<[u8]>) -> String {
    String::from_utf8_lossy(&unescape_bytes(field.as_ref())).into_owned()
}

fn unescape_bytes(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'\\'
            && let Some(d) = s.get(i + 1..i + 4)
            && d.iter().all(|c| (b'0'..=b'7').contains(c))
            && let Ok(v) = u8::try_from(
                u32::from(d[0] - b'0') * 64 + u32::from(d[1] - b'0') * 8 + u32::from(d[2] - b'0'),
            )
        {
            out.push(v);
            i += 4;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out
}

fn path(field: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(&unescape_bytes(field)))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(unescape(field))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &[u8] = b"36 25 0:57 / /media/u/Old\\040Drive rw,nosuid,nodev,relatime shared:112 - fuseblk /dev/sdb1 rw,user_id=0,allow_other,blksize=4096\n\
37 25 8:2 /photos /mnt/p rw,relatime - ext4 /dev/sda2 rw\n\
36 35 98:0 /mnt1 /mnt/parent\\040dir rw,noatime master:1 - ext3 /dev/root rw,errors=continue\n\
90 29 8:17 / /media/me/My\\040Card ro,nosuid,nodev shared:5 - vfat /dev/sdb1 ro,uid=1000\n\
91 29 8:18 / /media/me/Locked rw,relatime - ext4 /dev/sdb2 ro\n\
garbage\n\
\n\
92 29 0:40 / /mnt/p/inner rw - tmpfs tmpfs rw\n\
93 29 0:41 / /mnt/p/inner rw - tmpfs tmpfs2 rw\n";

    #[test]
    fn lines() {
        let m = parse(TEXT);
        assert_eq!(m.len(), 7, "the garbage and the empty line are skipped");
        assert_eq!(m[0].point, PathBuf::from("/media/u/Old Drive"));
        assert_eq!((m[0].dev, m[0].fstype.as_str()), ((0, 57), "fuseblk"));
        assert_eq!(m[0].source, "/dev/sdb1");
        assert!(!m[0].ro);
        assert_eq!(m[1].root, PathBuf::from("/photos"));
        assert_eq!(m[1].dev, (8, 2));
        // optional fields before the "-"
        assert_eq!(
            (m[2].source.as_str(), m[2].point.clone()),
            ("/dev/root", "/mnt/parent dir".into())
        );
        // read-only by the mount's options, or by the filesystem's
        assert!(m[3].ro);
        assert!(m[4].ro);
    }

    #[test]
    fn the_deepest_and_latest_mount_holds_a_path() {
        let m = parse(TEXT);
        let at = |p: &str| containing(&m, Path::new(p)).map(|m| m.source.as_str());
        assert_eq!(at("/mnt/p/x/y"), Some("/dev/sda2"));
        assert_eq!(
            at("/mnt/p/inner/z"),
            Some("tmpfs2"),
            "the later mount on a point hides the earlier"
        );
        assert_eq!(at("/mnt/pp"), None, "by whole components, not by text");
        assert_eq!(at("/elsewhere"), None);
    }

    #[test]
    fn escapes() {
        assert_eq!(unescape(r"/media/a\040b\134c"), r"/media/a b\c");
        assert_eq!(unescape(r"/tab\011end\"), "/tab\tend\\");
        assert_eq!(unescape(r"/short\04"), r"/short\04");
        assert_eq!(unescape(r"/not\08x"), r"/not\08x");
        // past a byte: left as it is, not wrapped or a panic
        assert_eq!(unescape(r"/big\777"), r"/big\777");
        assert_eq!(unescape(r"/plain"), "/plain");
    }

    #[cfg(unix)]
    #[test]
    fn names_that_arent_utf8_stay_the_bytes_they_are() {
        use std::os::unix::ffi::OsStrExt;
        let m = parse(b"1 1 8:1 / /media/caf\\351 rw - ext4 /dev/sda1 rw\n");
        assert_eq!(m[0].point.as_os_str().as_bytes(), b"/media/caf\xe9");
    }
}
