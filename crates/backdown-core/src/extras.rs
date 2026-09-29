//! What a copy carries besides the file's bytes, and what it says when it can't.
//!
//! - Windows: NTFS alternate data streams (`streams`). Each is copied (`file:name`) after the
//!   verified copy of the main stream and read back; the download marks (`Zone.Identifier`,
//!   `SmartScreen`) are left behind without a warning but listed.
//! - macOS: extended attributes, which hold resource forks (`com.apple.ResourceFork`), Finder
//!   info, tags and the like, copied with `copyfile(COPYFILE_XATTR)` from libSystem (no framework
//!   is linked); a drive that can't hold them gets AppleDouble `._` files from it, as the Finder
//!   does. Each attribute is then looked for on the copy.
//! - Elsewhere: nothing (Linux's `user.` attributes aren't carried).
//!
//! Either way the copy then gets the source's modification time back, since writing streams or
//! attributes can change it (and a mirror tells changed files by it). Whatever wasn't kept goes in
//! the journal with the copy.

use std::fs::{self, FileTimes};
use std::path::Path;

/// What a copy carried besides the bytes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Carried {
    /// streams or attributes copied
    pub kept: Vec<String>,
    /// download marks left behind on purpose
    pub skipped: Vec<String>,
    /// not kept, and why
    pub lost: Vec<(String, String)>,
}

impl Carried {
    /// For the journal: what wasn't kept, and the marks left behind; `None` when all was kept.
    pub fn note(&self) -> Option<String> {
        let mut parts = Vec::new();
        if !self.lost.is_empty() {
            parts.push(format!(
                "not kept: {}",
                self.lost
                    .iter()
                    .map(|(n, why)| format!("{n} ({why})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !self.skipped.is_empty() {
            parts.push(format!(
                "left behind: {} (a \"downloaded from the internet\" mark)",
                self.skipped.join(", ")
            ));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }

    pub fn lost_any(&self) -> bool {
        !self.lost.is_empty()
    }
}

/// After a verified copy of `src`'s bytes at `dest`: copy what else it has, and give `dest` the
/// source's modification time again.
pub fn carry(src: &Path, dest: &Path, meta: &fs::Metadata) -> Carried {
    let c = carry_os(src, dest);
    if !(c.kept.is_empty() && c.lost.is_empty())
        && let Ok(t) = meta.modified()
        && let Ok(f) = fs::OpenOptions::new()
            .write(true)
            .open(crate::paths::os(dest))
    {
        let _ = f.set_times(FileTimes::new().set_modified(t));
    }
    c
}

#[cfg(windows)]
fn carry_os(src: &Path, dest: &Path) -> Carried {
    use crate::streams;
    let mut c = Carried::default();
    let s = match streams::list(src) {
        Ok(s) => s,
        Err(e) => {
            c.lost.push((
                "alternate data streams".into(),
                format!("can't list them: {e}"),
            ));
            return c;
        }
    };
    for (name, _) in &s.names {
        if streams::quiet(name) {
            c.skipped.push(name.clone());
            continue;
        }
        match copy_stream(src, dest, name) {
            Ok(()) => c.kept.push(name.clone()),
            Err(e) => c.lost.push((name.clone(), e.to_string())),
        }
    }
    c
}

/// Copy one stream, hashing it, and read the copy back.
#[cfg(windows)]
fn copy_stream(src: &Path, dest: &Path, name: &str) -> std::io::Result<()> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let hash = |f: &mut dyn Read, out: Option<&mut fs::File>| -> std::io::Result<[u8; 32]> {
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut out = out;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            if let Some(o) = out.as_deref_mut() {
                o.write_all(&buf[..n])?;
            }
        }
        Ok(h.finalize().into())
    };
    let from = crate::streams::stream_path(src, name);
    let to = crate::streams::stream_path(dest, name);
    let mut input = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(&from)?;
    let mut out = fs::File::create(&to)?;
    let sum = hash(&mut input, Some(&mut out))?;
    out.sync_all()?;
    drop(out);
    if hash(&mut fs::File::open(&to)?, None)? != sum {
        let _ = fs::remove_file(&to);
        return Err(std::io::Error::other("the copy read back differently"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn carry_os(src: &Path, dest: &Path) -> Carried {
    let mut c = Carried::default();
    let names = match mac::xattrs(src) {
        Ok(n) => n,
        Err(e) => {
            c.lost.push((
                "extended attributes".into(),
                format!("can't list them: {e}"),
            ));
            return c;
        }
    };
    if names.is_empty() {
        return c;
    }
    let copied = mac::copy_xattrs(src, dest);
    let there = mac::xattrs(dest).unwrap_or_default();
    for n in names {
        if there.contains(&n) {
            c.kept.push(n);
        } else {
            let why = match &copied {
                Err(e) => e.to_string(),
                Ok(()) => "the destination drive didn't keep it".into(),
            };
            c.lost.push((n, why));
        }
    }
    c
}

#[cfg(not(any(windows, target_os = "macos")))]
fn carry_os(_src: &Path, _dest: &Path) -> Carried {
    Carried::default()
}

/// Names in a `listxattr` buffer: NUL-terminated, one after another.
pub fn xattr_names(buf: &[u8]) -> Vec<String> {
    buf.split(|&b| b == 0)
        .filter(|n| !n.is_empty())
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .collect()
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{CString, c_char, c_int, c_void};
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    const COPYFILE_XATTR: u32 = 1 << 2;
    const COPYFILE_NOFOLLOW_SRC: u32 = 1 << 18;
    const XATTR_NOFOLLOW: c_int = 0x0001;

    // in libSystem, which every macOS program links
    unsafe extern "C" {
        fn copyfile(
            from: *const c_char,
            to: *const c_char,
            state: *mut c_void,
            flags: u32,
        ) -> c_int;
        fn listxattr(path: *const c_char, names: *mut c_char, size: usize, options: c_int)
        -> isize;
    }

    fn c_path(p: &Path) -> io::Result<CString> {
        CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::other("a NUL in the path"))
    }

    pub fn xattrs(p: &Path) -> io::Result<Vec<String>> {
        let c = c_path(p)?;
        // SAFETY: a NUL-terminated path; a null buffer asks for the size, then a buffer of it
        unsafe {
            let n = listxattr(c.as_ptr(), std::ptr::null_mut(), 0, XATTR_NOFOLLOW);
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            if n == 0 {
                return Ok(Vec::new());
            }
            let mut buf = vec![0u8; n as usize + 256];
            let got = listxattr(
                c.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                XATTR_NOFOLLOW,
            );
            if got < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(super::xattr_names(&buf[..got as usize]))
        }
    }

    pub fn copy_xattrs(src: &Path, dest: &Path) -> io::Result<()> {
        let (a, b) = (c_path(src)?, c_path(dest)?);
        // SAFETY: two NUL-terminated paths and no state
        let rc = unsafe {
            copyfile(
                a.as_ptr(),
                b.as_ptr(),
                std::ptr::null_mut(),
                COPYFILE_XATTR | COPYFILE_NOFOLLOW_SRC,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_journal_says() {
        assert_eq!(Carried::default().note(), None);
        let kept = Carried {
            kept: vec!["com.apple.FinderInfo".into()],
            ..Carried::default()
        };
        assert_eq!(kept.note(), None);
        let c = Carried {
            kept: vec!["thumbnail".into()],
            skipped: vec!["Zone.Identifier".into()],
            lost: vec![(
                "com.apple.ResourceFork".into(),
                "the destination drive didn't keep it".into(),
            )],
        };
        assert!(c.lost_any());
        assert_eq!(
            c.note().unwrap(),
            "not kept: com.apple.ResourceFork (the destination drive didn't keep it); \
             left behind: Zone.Identifier (a \"downloaded from the internet\" mark)"
        );
        assert_eq!(
            xattr_names(b"com.apple.FinderInfo\0com.apple.metadata:_kMDItemUserTags\0"),
            vec![
                "com.apple.FinderInfo",
                "com.apple.metadata:_kMDItemUserTags"
            ]
        );
    }

    #[test]
    fn a_plain_copy_carries_nothing_here() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "x").unwrap();
        fs::write(&b, "x").unwrap();
        let c = carry(&a, &b, &fs::metadata(&a).unwrap());
        assert!(!c.lost_any(), "{c:?}");
    }
}
