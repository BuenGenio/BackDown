//! Paths are stored as raw OS bytes (UTF-16LE on Windows) so every name round-trips exactly:
//! the file has to be reopened later to hash it, and not every name on an old drive is UTF-8.
//!
//! On Windows a path is stored and shown in its normal form (`C:\…`, `\\server\share\…`) and
//! opened in its verbatim form (`\\?\C:\…`, `\\?\UNC\server\share\…`), which reaches past 260
//! characters and keeps names that Win32 would otherwise change (a trailing dot or space) as they
//! are. `to_bytes` stores the normal form whichever form it's given, `os` gives the form to open,
//! and `canonical` and `show` never let a verbatim prefix out. The conversions themselves work on
//! UTF-16 code units and are tested on every OS.
//!
//! `open_read` opens a source file for reading only; on Windows with backup semantics, which
//! `enable_backup_privilege` makes count when BackDown runs elevated, so files the user's own
//! account can't read (another user's folders on an old drive) can still be indexed and hashed.
//! It is never used for writing, and it leaves online-only (cloud placeholder) files alone, since
//! reading one would download it onto the source drive.

use std::borrow::Cow;
use std::fs::File;
use std::io;
use std::path::{MAIN_SEPARATOR_STR, Path, PathBuf};

#[cfg(unix)]
pub fn to_bytes(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().to_vec()
}

#[cfg(unix)]
pub fn from_bytes(b: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(b))
}

/// The normal form's UTF-16LE bytes (a verbatim `\\?\` path is stored without its prefix).
#[cfg(windows)]
pub fn to_bytes(p: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = p.as_os_str().encode_wide().collect();
    let wide = strip_verbatim(&wide).unwrap_or(wide);
    wide.into_iter().flat_map(u16::to_le_bytes).collect()
}

#[cfg(windows)]
pub fn from_bytes(b: &[u8]) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    let wide: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    PathBuf::from(std::ffi::OsString::from_wide(&wide))
}

/// The byte prefix shared by every path strictly inside `dir`.
pub fn dir_prefix(dir: &Path) -> Vec<u8> {
    let mut p = to_bytes(dir);
    let sep = to_bytes(Path::new(MAIN_SEPARATOR_STR));
    if !p.ends_with(&sep) {
        p.extend_from_slice(&sep);
    }
    p
}

// ---------- verbatim paths (Windows), as UTF-16 code units

const BACKSLASH: u16 = b'\\' as u16;
const SLASH: u16 = b'/' as u16;
const COLON: u16 = b':' as u16;
const DOT: u16 = b'.' as u16;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn starts_with_ci(w: &[u16], prefix: &str) -> bool {
    let p = wide(prefix);
    w.len() >= p.len()
        && w.iter().zip(&p).all(|(a, b)| {
            char::from_u32(u32::from(*a)).map(|c| c.to_ascii_uppercase())
                == char::from_u32(u32::from(*b)).map(|c| c.to_ascii_uppercase())
        })
}

fn drive_letter(w: &[u16]) -> bool {
    w.len() >= 2 && w[1] == COLON && u8::try_from(w[0]).is_ok_and(|c| c.is_ascii_alphabetic())
}

/// `\\?\C:\x` → `C:\x`, `\\?\UNC\server\share\x` → `\\server\share\x`; `None` for anything else
/// (a normal path, a device path, or a verbatim path with no normal form, like `\\?\Volume{…}\`).
pub fn strip_verbatim(w: &[u16]) -> Option<Vec<u16>> {
    if !starts_with_ci(w, r"\\?\") {
        return None;
    }
    let rest = &w[4..];
    if starts_with_ci(rest, r"UNC\") {
        let unc = &rest[4..];
        if unc.is_empty() || unc[0] == BACKSLASH {
            return None; // no server name
        }
        let mut out = vec![BACKSLASH, BACKSLASH];
        out.extend_from_slice(unc);
        return Some(out);
    }
    if drive_letter(rest) && (rest.len() == 2 || rest[2] == BACKSLASH) {
        return Some(rest.to_vec());
    }
    None
}

/// The verbatim form of an absolute path: `C:\x` → `\\?\C:\x`, `\\server\share\x` →
/// `\\?\UNC\server\share\x`, with `/` read as `\` and `.` and `..` resolved (a verbatim path is
/// taken literally, so they must be gone), but every name otherwise kept as it is: a trailing dot
/// or space stays. Verbatim and device (`\\.\`) paths are returned unchanged; `None` for relative
/// and drive-relative paths.
pub fn make_verbatim(w: &[u16]) -> Option<Vec<u16>> {
    if starts_with_ci(w, r"\\?\") || starts_with_ci(w, r"\??\") || starts_with_ci(w, r"\\.\") {
        return Some(w.to_vec());
    }
    let w: Vec<u16> = w
        .iter()
        .map(|&c| if c == SLASH { BACKSLASH } else { c })
        .collect();
    let (mut out, rest, keep) = if w.len() > 2 && w[0] == BACKSLASH && w[1] == BACKSLASH {
        // \\server\share\…: the server and share are the root, `..` can't climb above them
        (wide(r"\\?\UNC"), &w[2..], 2)
    } else if drive_letter(&w) && w.len() > 2 && w[2] == BACKSLASH {
        let mut o = wide(r"\\?\");
        o.extend_from_slice(&w[..2]);
        (o, &w[3..], 0)
    } else {
        return None;
    };
    let mut parts: Vec<&[u16]> = Vec::new();
    for (i, part) in rest.split(|&c| c == BACKSLASH).enumerate() {
        if i < keep {
            if part.is_empty() {
                return None; // `\\\x` or `\\server\\x`: no server or share
            }
            parts.push(part);
            continue;
        }
        match part {
            [] | [DOT] => {}
            [DOT, DOT] => {
                if parts.len() > keep {
                    parts.pop();
                }
            }
            _ => parts.push(part),
        }
    }
    if parts.len() < keep {
        return None; // `\\server` without a share
    }
    if parts.is_empty() {
        out.push(BACKSLASH); // `C:\` itself
    }
    for p in parts {
        out.push(BACKSLASH);
        out.extend_from_slice(p);
    }
    Some(out)
}

/// The form a path is stored and shown in: without a verbatim prefix (Windows; elsewhere as is).
#[cfg(windows)]
pub fn normal(p: &Path) -> Cow<'_, Path> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let w: Vec<u16> = p.as_os_str().encode_wide().collect();
    match strip_verbatim(&w) {
        Some(n) => Cow::Owned(PathBuf::from(std::ffi::OsString::from_wide(&n))),
        None => Cow::Borrowed(p),
    }
}

#[cfg(not(windows))]
pub fn normal(p: &Path) -> Cow<'_, Path> {
    Cow::Borrowed(p)
}

/// The form to open a path with: verbatim on Windows (absolute paths only), as is elsewhere.
#[cfg(windows)]
pub fn os(p: &Path) -> Cow<'_, Path> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let w: Vec<u16> = p.as_os_str().encode_wide().collect();
    match make_verbatim(&w) {
        Some(v) if v != w => Cow::Owned(PathBuf::from(std::ffi::OsString::from_wide(&v))),
        _ => Cow::Borrowed(p),
    }
}

#[cfg(not(windows))]
pub fn os(p: &Path) -> Cow<'_, Path> {
    Cow::Borrowed(p)
}

/// The absolute path with links resolved, in its normal form (unlike `dunce`, which keeps the
/// verbatim prefix on paths longer than 260 characters).
pub fn canonical(p: &Path) -> io::Result<PathBuf> {
    let c = std::fs::canonicalize(p)?;
    Ok(normal(&c).into_owned())
}

/// A path for people: its normal form, with names composed (NFC) as most systems write them.
pub fn show(p: &Path) -> String {
    crate::names::nfc(&normal(p).to_string_lossy()).into_owned()
}

// ---------- reading sources

/// Windows file attributes of a file whose data isn't on the drive: reading it downloads (or
/// recalls) it first. `FILE_ATTRIBUTE_OFFLINE`, `…_RECALL_ON_OPEN`, `…_RECALL_ON_DATA_ACCESS`.
pub fn online_only(attributes: u32) -> bool {
    attributes & (0x1000 | 0x4_0000 | 0x40_0000) != 0
}

/// Open a source file to read it: on Windows verbatim, with backup semantics, and never an
/// online-only placeholder.
#[cfg(windows)]
pub fn open_read(p: &Path) -> io::Result<File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let p = os(p);
    if std::fs::symlink_metadata(&p).is_ok_and(|m| online_only(m.file_attributes())) {
        return Err(io::Error::other(
            "an online-only file (a cloud placeholder): reading it would download it onto the \
             drive, so it's left alone",
        ));
    }
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(&p)
}

#[cfg(not(windows))]
pub fn open_read(p: &Path) -> io::Result<File> {
    File::open(p)
}

/// When BackDown runs elevated on Windows: enable the backup privilege for this process, so
/// `open_read` and folder listings get past folders' permissions (reading only). Whether it's on;
/// always false elsewhere, and on Windows without an administrator.
pub fn enable_backup_privilege() -> bool {
    #[cfg(windows)]
    {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| crate::win::enable_privilege("SeBackupPrivilege"))
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let p = Path::new("some").join("dir").join("naïve file.txt");
        assert_eq!(from_bytes(&to_bytes(&p)), p);
    }

    #[cfg(unix)]
    #[test]
    fn round_trips_non_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let p = PathBuf::from(std::ffi::OsStr::from_bytes(b"/old/drive/caf\xe9.txt"));
        assert_eq!(from_bytes(&to_bytes(&p)), p);
    }

    #[test]
    fn prefix_ends_with_one_separator() {
        let d = Path::new("a").join("b");
        let pre = dir_prefix(&d);
        assert!(pre.ends_with(&to_bytes(Path::new(MAIN_SEPARATOR_STR))));
        assert_eq!(dir_prefix(&from_bytes(&pre)), pre);
    }

    fn s(w: &[u16]) -> String {
        String::from_utf16_lossy(w)
    }

    fn strip(p: &str) -> Option<String> {
        strip_verbatim(&wide(p)).map(|w| s(&w))
    }

    fn verb(p: &str) -> Option<String> {
        make_verbatim(&wide(p)).map(|w| s(&w))
    }

    #[test]
    fn verbatim_prefixes_come_off() {
        assert_eq!(strip(r"\\?\C:\Users\x").as_deref(), Some(r"C:\Users\x"));
        assert_eq!(strip(r"\\?\d:\").as_deref(), Some(r"d:\"));
        assert_eq!(strip(r"\\?\D:").as_deref(), Some(r"D:"));
        assert_eq!(
            strip(r"\\?\UNC\nas\share\photos").as_deref(),
            Some(r"\\nas\share\photos")
        );
        assert_eq!(strip(r"\\?\unc\nas\share").as_deref(), Some(r"\\nas\share"));
        // no normal form, or not verbatim at all
        assert_eq!(
            strip(r"\\?\Volume{0b1c2d3e-0000-0000-0000-100000000000}\x"),
            None
        );
        assert_eq!(strip(r"\\?\UNC\"), None);
        assert_eq!(strip(r"\\?\C:x"), None);
        assert_eq!(strip(r"C:\Users"), None);
        assert_eq!(strip(r"\\.\PhysicalDrive1"), None);
    }

    #[test]
    fn verbatim_forms_go_on() {
        assert_eq!(verb(r"C:\Users\x").as_deref(), Some(r"\\?\C:\Users\x"));
        assert_eq!(verb(r"C:\").as_deref(), Some(r"\\?\C:\"));
        assert_eq!(
            verb("E:/Archive/./Mail/../Photos/").as_deref(),
            Some(r"\\?\E:\Archive\Photos")
        );
        assert_eq!(verb(r"C:\..\..\x").as_deref(), Some(r"\\?\C:\x"));
        assert_eq!(
            verb(r"\\nas\share\a\..\..\b").as_deref(),
            Some(r"\\?\UNC\nas\share\b")
        );
        // a trailing dot or space is part of the name: kept
        assert_eq!(
            verb(r"E:\from linux\notes. \draft.").as_deref(),
            Some(r"\\?\E:\from linux\notes. \draft.")
        );
        // already verbatim, or a device: unchanged
        assert_eq!(verb(r"\\?\C:\x\..\y").as_deref(), Some(r"\\?\C:\x\..\y"));
        assert_eq!(verb(r"\\.\E:").as_deref(), Some(r"\\.\E:"));
        // relative, drive-relative, rooted without a drive, no server: left alone
        for p in [r"x\y", r"C:x", r"\x", r"\\\x", r"\\server"] {
            assert_eq!(verb(p), None, "{p}");
        }
    }

    #[test]
    fn long_and_odd_paths_round_trip() {
        // longer than MAX_PATH (260), with a name outside the BMP and a lone surrogate (a name
        // Windows accepts but that isn't valid Unicode)
        let long = format!(r"D:\{}\{}\😀 photo.jpg", "a".repeat(200), "b".repeat(120));
        let mut w = wide(&long);
        w.extend_from_slice(&[BACKSLASH, 0xD800, b'x' as u16]);
        assert!(w.len() > 260);
        let v = make_verbatim(&w).unwrap();
        assert!(starts_with_ci(&v, r"\\?\D:\"));
        assert_eq!(strip_verbatim(&v).unwrap(), w);
        assert_eq!(make_verbatim(&strip_verbatim(&v).unwrap()).unwrap(), v);
        let unc = wide(&format!(r"\\nas\backups\{}", "c".repeat(300)));
        let v = make_verbatim(&unc).unwrap();
        assert!(starts_with_ci(&v, r"\\?\UNC\nas\backups\"));
        assert_eq!(strip_verbatim(&v).unwrap(), unc);
    }

    #[test]
    fn placeholders_are_recognised() {
        assert!(online_only(0x40_0000 | 0x20));
        assert!(online_only(0x1000));
        assert!(!online_only(0x20 | 0x400)); // archive + reparse point (e.g. deduplicated)
    }

    #[cfg(windows)]
    #[test]
    fn stored_paths_lose_the_verbatim_prefix() {
        let long = format!(r"C:\{}\file.txt", "x".repeat(280));
        let verbatim = PathBuf::from(format!(r"\\?\{long}"));
        // dunce keeps the prefix on a path this long; storing and showing never do
        assert_eq!(dunce::simplified(&verbatim), verbatim.as_path());
        assert_eq!(from_bytes(&to_bytes(&verbatim)), PathBuf::from(&long));
        assert_eq!(normal(&verbatim), Path::new(&long));
        assert_eq!(os(Path::new(&long)), verbatim.as_path());
        assert_eq!(show(&verbatim), long);
        let short = PathBuf::from(r"\\?\C:\Users");
        assert_eq!(dunce::simplified(&short), Path::new(r"C:\Users"));
        assert_eq!(normal(&short), dunce::simplified(&short));
    }
}
