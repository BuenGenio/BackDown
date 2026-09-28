//! Paths are stored as raw OS bytes (UTF-16LE on Windows) so every name round-trips exactly:
//! the file has to be reopened later to hash it, and not every name on an old drive is UTF-8.

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

#[cfg(windows)]
pub fn to_bytes(p: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(windows)]
pub fn from_bytes(b: &[u8]) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    let wide: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
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
}
