//! Fast copies, for `apply::copy_verified` (filing, mirror syncs, wipe's moves, rescued files).
//!
//! Three ways to copy a file, fastest first:
//!
//! 1. **Clone**: the new file shares the source's blocks until either is changed (copy on write).
//!    Instant and takes no space. Linux `FICLONE` (btrfs, XFS, bcachefs), macOS `fclonefileat`
//!    (APFS). Only within one filesystem; anything else answers "not supported" at once.
//! 2. **Kernel copy**: Linux `copy_file_range`, the kernel copying from file to file without the
//!    bytes passing through this program (and on NFS or SMB, the server copying without the
//!    network). Meanwhile a second thread reads the source and hashes it.
//! 3. **Loop**: read, hash, write, as before; the source is read once.
//!
//! Whichever is used, the caller (`copy_verified`) then flushes the copy, asks the kernel to forget
//! its cached pages, reads it back from the disk and compares hashes with the source as it was
//! read: a clone or a kernel copy is verified exactly like a plain one. When a fast way fails part
//! way (or isn't supported), the partial copy is emptied and the loop starts again from the top.
//!
//! Windows' block cloning (ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE`) isn't used: it needs cluster
//! alignment, matching integrity-stream and sparse settings and a pre-sized file, and ReFS is rare
//! on the drives people bring; Windows copies with the loop.
//!
//! `unsafe` is confined to the three system calls below (and `dedupe_range` for dedup.rs), each
//! over file descriptors borrowed from open `File`s for the duration of the call.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const BUF: usize = 1 << 20;
/// One kernel copy call's share: small enough to fall back quickly, big enough to be cheap.
const CHUNK: usize = 16 << 20;

/// How a file was copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Method {
    Clone,
    Kernel,
    Loop,
}

impl Method {
    pub fn describe(self) -> &'static str {
        match self {
            Method::Clone => "cloned (shares the source's blocks)",
            Method::Kernel => "copied by the kernel",
            Method::Loop => "copied",
        }
    }
}

/// The platform's copying primitives; tests swap in fakes to walk the fallbacks.
#[derive(Clone, Copy)]
pub(crate) struct Ops {
    /// Create `dest` (which doesn't exist) as a clone of `src`, returning it open. On failure
    /// `dest` doesn't exist.
    pub clone: fn(&File, &Path) -> io::Result<File>,
    /// Copy up to `max` bytes at `offset` of `src` to the same offset of `dest`, in the kernel;
    /// 0 at the end of the source.
    pub kernel: fn(&File, &File, u64, usize) -> io::Result<usize>,
}

pub(crate) const NATIVE: Ops = Ops {
    clone: native_clone,
    kernel: native_kernel_copy,
};

/// Copy `src` (open as `input`, at offset 0) into the new file `tmp`, the fastest way that works.
/// Returns the copy (open; not yet flushed), how it was made, and the SHA-256 of the source as it
/// was read for this copy.
pub(crate) fn copy_into(
    input: &mut File,
    tmp: &Path,
    ops: &Ops,
) -> io::Result<(File, Method, [u8; 32])> {
    // a partial file of ours left by an interrupted run (a clone can't be made over it)
    let _ = fs::remove_file(tmp);
    if let Ok(out) = (ops.clone)(input, tmp) {
        let sum = hash_from_start(input)?;
        return Ok((out, Method::Clone, sum));
    }
    let mut out = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(tmp)?;
    if let Some(sum) = kernel_copy(input, &out, ops)? {
        return Ok((out, Method::Kernel, sum));
    }
    // the plain loop, from the top (whatever a failed fast copy left is emptied first)
    out.set_len(0)?;
    rewind(&mut out)?;
    rewind(input)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; BUF];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        out.write_all(&buf[..n])?;
    }
    Ok((out, Method::Loop, h.finalize().into()))
}

fn rewind(f: &mut File) -> io::Result<()> {
    use std::io::Seek;
    f.seek(io::SeekFrom::Start(0)).map(|_| ())
}

fn hash_from_start(f: &mut File) -> io::Result<[u8; 32]> {
    rewind(f)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; BUF];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().into())
}

/// The kernel copy with the source hashed alongside: `None` if the kernel can't copy these files
/// (or stopped with an error part way: the loop then starts over and meets any real read error
/// itself).
fn kernel_copy(input: &mut File, out: &File, ops: &Ops) -> io::Result<Option<[u8; 32]>> {
    // the first call says whether it works at all, before a thread is spent hashing
    let first = match (ops.kernel)(input, out, 0, CHUNK) {
        Ok(n) => n as u64,
        Err(_) => return Ok(None),
    };
    // positional reads on a second handle: neither side moves the other's offset
    let reader = input.try_clone()?;
    let copied = std::thread::scope(|s| {
        let hasher = s.spawn(move || hash_at_offsets(&reader));
        let mut at = first;
        let mut ok = true;
        if first > 0 {
            loop {
                match (ops.kernel)(input, out, at, CHUNK) {
                    Ok(0) => break,
                    Ok(n) => at += n as u64,
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
        }
        let sum = hasher
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("hashing panicked")));
        (ok, sum)
    });
    match copied {
        (true, Ok(sum)) => Ok(Some(sum)),
        _ => Ok(None),
    }
}

#[cfg(unix)]
fn hash_at_offsets(f: &File) -> io::Result<[u8; 32]> {
    use std::os::unix::fs::FileExt;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; BUF];
    let mut at = 0u64;
    loop {
        let n = f.read_at(&mut buf, at)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        at += n as u64;
    }
    Ok(h.finalize().into())
}

#[cfg(not(unix))]
fn hash_at_offsets(f: &File) -> io::Result<[u8; 32]> {
    // no kernel copy here (see native_kernel_copy), so this never runs; kept for the types
    let mut f = f.try_clone()?;
    hash_from_start(&mut f)
}

/// Ask the kernel to drop its cached pages of `f` (flushed already), so reading it back reads the
/// disk rather than memory. Advisory: where it isn't available, the read-back may come from cache.
pub(crate) fn forget_cached(f: &File) {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: `f` is an open file for the duration of the call; the advice only affects the
        // page cache, never the file's contents.
        unsafe {
            libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let _ = f;
}

// ---------- the platforms' primitives

#[cfg(target_os = "linux")]
fn native_clone(src: &File, dest: &Path) -> io::Result<File> {
    use std::os::fd::AsRawFd;
    let out = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(dest)?;
    // SAFETY: both descriptors belong to open `File`s that outlive the call; FICLONE reads the
    // source descriptor from its argument and keeps nothing.
    let r = unsafe { libc::ioctl(out.as_raw_fd(), libc::FICLONE, src.as_raw_fd()) };
    if r == 0 {
        Ok(out)
    } else {
        let e = io::Error::last_os_error();
        drop(out);
        let _ = fs::remove_file(dest);
        Err(e)
    }
}

#[cfg(target_os = "macos")]
fn native_clone(src: &File, dest: &Path) -> io::Result<File> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    /// <sys/clonefile.h>: don't follow a symbolic link at the source (it's an open file anyway)
    const CLONE_NOFOLLOW: u32 = 0x0001;
    let path = std::ffi::CString::new(dest.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `src` is an open file for the duration of the call and `path` a NUL-terminated
    // string that outlives it; fclonefileat creates `dest` and keeps neither.
    let r = unsafe {
        libc::fclonefileat(
            src.as_raw_fd(),
            libc::AT_FDCWD,
            path.as_ptr(),
            CLONE_NOFOLLOW,
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    // the clone has the source's permissions, which may not allow writing: flushing and setting
    // its times need only an open file
    File::open(dest).inspect_err(|_| {
        let _ = fs::remove_file(dest);
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn native_clone(_src: &File, _dest: &Path) -> io::Result<File> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(target_os = "linux")]
fn native_kernel_copy(src: &File, dest: &File, offset: u64, max: usize) -> io::Result<usize> {
    use std::os::fd::AsRawFd;
    let mut off_in = offset as libc::loff_t;
    let mut off_out = offset as libc::loff_t;
    // SAFETY: both descriptors belong to open `File`s that outlive the call, and the offsets are
    // locals the kernel updates in place (so neither file's own offset moves). The raw system call
    // rather than libc's wrapper: some C libraries emulate it in user space, which gains nothing.
    let n = unsafe {
        libc::syscall(
            libc::SYS_copy_file_range,
            src.as_raw_fd(),
            &mut off_in as *mut libc::loff_t,
            dest.as_raw_fd(),
            &mut off_out as *mut libc::loff_t,
            max,
            0u32,
        )
    };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

#[cfg(not(target_os = "linux"))]
fn native_kernel_copy(_src: &File, _dest: &File, _offset: u64, _max: usize) -> io::Result<usize> {
    Err(io::ErrorKind::Unsupported.into())
}

// ---------- in-place deduplication (dedup.rs)

/// The result of asking the kernel to share one file's blocks with another's.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dedupe {
    /// every byte now shares the keeper's blocks
    Shared,
    /// the kernel compared the files and found them different: nothing changed
    Differs,
}

/// Make `dup` share `keeper`'s blocks where the kernel confirms the bytes are identical
/// (Linux `FIDEDUPERANGE`: the kernel locks both ranges and compares them itself, so a file that
/// changes meanwhile is simply left alone). `dup` keeps its own inode, name, owner, permissions,
/// dates and extended attributes; only its storage is shared. Needs btrfs, XFS or bcachefs, and
/// `dup` open for writing.
#[cfg(target_os = "linux")]
pub(crate) fn dedupe_range(keeper: &File, dup: &File, len: u64) -> io::Result<Dedupe> {
    use std::os::fd::AsRawFd;
    /// <linux/fs.h>: _IOWR(0x94, 54, struct file_dedupe_range) with its 24-byte header
    const FIDEDUPERANGE: u32 = 0xC018_9436;
    const SAME: i32 = 0;
    const DIFFERS: i32 = 1;
    /// struct file_dedupe_range with room for one destination (struct file_dedupe_range_info)
    #[repr(C)]
    struct Range {
        src_offset: u64,
        src_length: u64,
        dest_count: u16,
        reserved1: u16,
        reserved2: u32,
        dest_fd: i64,
        dest_offset: u64,
        bytes_deduped: u64,
        status: i32,
        reserved: u32,
    }
    // filesystems cap one request (btrfs at 16 MiB), so go a piece at a time
    let mut at = 0u64;
    while at < len {
        let want = (len - at).min(CHUNK as u64);
        let mut r = Range {
            src_offset: at,
            src_length: want,
            dest_count: 1,
            reserved1: 0,
            reserved2: 0,
            dest_fd: dup.as_raw_fd() as i64,
            dest_offset: at,
            bytes_deduped: 0,
            status: 0,
            reserved: 0,
        };
        // SAFETY: `r` is a properly laid out file_dedupe_range with one info record, living for the
        // whole call; both descriptors belong to open `File`s. The kernel writes back only
        // `bytes_deduped` and `status` inside `r`.
        let rc = unsafe {
            libc::ioctl(
                keeper.as_raw_fd(),
                FIDEDUPERANGE as libc::Ioctl,
                &mut r as *mut Range,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        match r.status {
            SAME if r.bytes_deduped > 0 => at += r.bytes_deduped,
            SAME => return Err(io::Error::other("the kernel shared nothing")),
            DIFFERS => return Ok(Dedupe::Differs),
            e if e < 0 => return Err(io::Error::from_raw_os_error(-e)),
            e => return Err(io::Error::other(format!("unexpected dedupe status {e}"))),
        }
    }
    Ok(Dedupe::Shared)
}

/// Clone `src` to the new file `dest` (for dedup.rs on macOS, where there's no in-place dedupe).
#[cfg(target_os = "macos")]
pub(crate) fn clone_new(src: &File, dest: &Path) -> io::Result<File> {
    native_clone(src, dest)
}

/// Can files in `dir` share blocks at all? From the filesystem's type, without writing anything:
/// `Some(name)` of a filesystem that can (btrfs, XFS, bcachefs, APFS), `None` otherwise or when it
/// can't be told.
pub fn clone_capable(dir: &Path) -> Option<&'static str> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        // SAFETY: statfs fills the zeroed struct we own from a NUL-terminated path that outlives
        // the call.
        let mut s: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
            return None;
        }
        // f_type's width differs between C libraries; compare as u32 (the magic numbers are)
        match s.f_type as u32 {
            0x9123_683E => Some("btrfs"),
            0x5846_5342 => Some("XFS"),
            0xCA45_1A4E => Some("bcachefs"),
            _ => None,
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        // SAFETY: as above.
        let mut s: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
            return None;
        }
        let name: Vec<u8> = s
            .f_fstypename
            .iter()
            .take_while(|&&b| b != 0)
            .map(|&b| b as u8)
            .collect();
        (name == b"apfs").then_some("APFS")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = dir;
        None
    }
}

/// The name of the filesystem `dir` is on, for messages ("ext4", "btrfs", …), where it can be told.
pub fn fs_name(dir: &Path) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        // SAFETY: as in clone_capable.
        let mut s: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
            return None;
        }
        Some(
            match s.f_type as u32 {
                0x9123_683E => "btrfs",
                0x5846_5342 => "XFS",
                0xCA45_1A4E => "bcachefs",
                0xEF53 => "ext4",
                0x0102_1994 => "tmpfs",
                0x5346_544E => "NTFS",
                0x4D44 => "FAT",
                0x2011_BAB0 => "exFAT",
                0x6969 => "NFS",
                0xFF53_4D42 | 0xFE53_4D42 => "SMB",
                0x6573_5546 => "FUSE",
                0x2FC1_2FC1 => "ZFS",
                0xF15F => "eCryptfs",
                0x794C_7630 => "overlayfs",
                _ => return None,
            }
            .to_string(),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        clone_capable(dir).map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unsupported_clone(_: &File, _: &Path) -> io::Result<File> {
        Err(io::Error::from_raw_os_error(95)) // EOPNOTSUPP, as ext4 and tmpfs answer
    }
    fn unsupported_kernel(_: &File, _: &File, _: u64, _: usize) -> io::Result<usize> {
        Err(io::Error::from_raw_os_error(18)) // EXDEV: another filesystem
    }
    /// a kernel copy that copies one piece, then fails (a flaky network filesystem)
    fn failing_kernel(src: &File, dest: &File, at: u64, max: usize) -> io::Result<usize> {
        if at > 0 {
            return Err(io::Error::from_raw_os_error(5)); // EIO
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            let mut buf = vec![0u8; max.min(4096)];
            let n = src.read_at(&mut buf, at)?;
            dest.write_at(&buf[..n], at)?;
            Ok(n)
        }
        #[cfg(not(unix))]
        {
            let _ = (src, dest, max);
            Err(io::ErrorKind::Unsupported.into())
        }
    }

    fn data(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
    }

    fn run(ops: &Ops, len: usize) -> (Method, Vec<u8>, [u8; 32], [u8; 32]) {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        let bytes = data(len);
        fs::write(&src, &bytes).unwrap();
        let tmp = t.path().join(".dest.backdown-partial");
        let mut input = File::open(&src).unwrap();
        let (out, how, sum) = copy_into(&mut input, &tmp, ops).unwrap();
        drop(out);
        let back = fs::read(&tmp).unwrap();
        (how, back, sum, Sha256::digest(&bytes).into())
    }

    #[test]
    fn falls_back_from_clone_to_the_kernel_to_the_loop() {
        // no clone: the kernel copies (on Linux; elsewhere the loop does)
        let ops = Ops {
            clone: unsupported_clone,
            kernel: NATIVE.kernel,
        };
        let (how, back, sum, want) = run(&ops, 5 << 20);
        assert_eq!(sum, want);
        assert_eq!(Sha256::digest(&back)[..], want[..]);
        if cfg!(target_os = "linux") {
            assert_eq!(how, Method::Kernel);
        } else {
            assert_eq!(how, Method::Loop);
        }
        // neither: the loop
        let ops = Ops {
            clone: unsupported_clone,
            kernel: unsupported_kernel,
        };
        let (how, back, sum, want) = run(&ops, 3 << 20);
        assert_eq!((how, sum), (Method::Loop, want));
        assert_eq!(Sha256::digest(&back)[..], want[..]);
    }

    #[test]
    fn a_kernel_copy_failing_part_way_starts_over_with_the_loop() {
        let ops = Ops {
            clone: unsupported_clone,
            kernel: failing_kernel,
        };
        let (how, back, sum, want) = run(&ops, 100_000);
        assert_eq!((how, sum), (Method::Loop, want));
        assert_eq!(
            back.len(),
            100_000,
            "nothing of the failed attempt is left over"
        );
        assert_eq!(Sha256::digest(&back)[..], want[..]);
    }

    #[test]
    fn native_primitives_on_this_filesystem() {
        // whatever this machine's temp folder is (tmpfs, ext4, btrfs…), the native path copies
        // correctly; a clone is only reported where the filesystem really made one
        let (how, back, sum, want) = run(&NATIVE, 2 << 20);
        assert_eq!(sum, want);
        assert_eq!(Sha256::digest(&back)[..], want[..]);
        let t = tempfile::tempdir().unwrap();
        if how == Method::Clone {
            assert!(clone_capable(t.path()).is_some());
        }
        // an empty file copies too
        let (_, back, sum, want) = run(&NATIVE, 0);
        assert!(back.is_empty());
        assert_eq!(sum, want);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_real_clone_attempt_off_a_cloning_filesystem_is_refused_cleanly() {
        let t = tempfile::tempdir().unwrap();
        if clone_capable(t.path()).is_some() {
            return; // btrfs or XFS here: nothing to refuse
        }
        let src = t.path().join("a");
        fs::write(&src, b"some bytes").unwrap();
        let dest = t.path().join("b");
        let e = native_clone(&File::open(&src).unwrap(), &dest).unwrap_err();
        assert!(
            matches!(e.raw_os_error(), Some(95 | 18 | 22 | 25)),
            "EOPNOTSUPP, EXDEV, EINVAL or ENOTTY, not {e}"
        );
        assert!(!dest.exists(), "a failed clone leaves nothing behind");
        // and in-place dedupe says the same
        fs::write(&dest, b"some bytes").unwrap();
        let dup = OpenOptions::new().write(true).open(&dest).unwrap();
        assert!(dedupe_range(&File::open(&src).unwrap(), &dup, 10).is_err());
        assert_eq!(fs::read(&dest).unwrap(), b"some bytes");
    }

    /// `cargo test -p backdown-core --release fastcopy::tests::bench -- --ignored --nocapture`:
    /// time a 1 GB file copied by the kernel and by the loop (each hashing the source), written
    /// under the workspace's target/ folder (on disk, not a RAM-backed /tmp).
    #[test]
    #[ignore]
    fn bench() {
        use std::time::Instant;
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fastcopy-bench");
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.bin");
        let len: u64 = std::env::var("BENCH_MB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1024)
            << 20;
        if fs::metadata(&src).map(|m| m.len()).ok() != Some(len) {
            let mut f = File::create(&src).unwrap();
            let block = data(BUF);
            for i in 0..len / BUF as u64 {
                let mut b = block.clone();
                b[..8].copy_from_slice(&i.to_le_bytes());
                f.write_all(&b).unwrap();
            }
            f.sync_all().unwrap();
        }
        println!(
            "{} MB on {}",
            len >> 20,
            fs_name(&dir).unwrap_or_else(|| "?".into())
        );
        let evict = |p: &Path| {
            let f = File::open(p).unwrap();
            forget_cached(&f);
        };
        for (name, ops) in [
            (
                "loop",
                Ops {
                    clone: unsupported_clone,
                    kernel: unsupported_kernel,
                },
            ),
            (
                "kernel",
                Ops {
                    clone: unsupported_clone,
                    kernel: NATIVE.kernel,
                },
            ),
            ("native", NATIVE),
        ] {
            for cache in ["cold", "warm"] {
                let tmp = dir.join(format!("dest-{name}"));
                let _ = fs::remove_file(&tmp);
                if cache == "cold" {
                    evict(&src);
                } else {
                    let _ = hash_from_start(&mut File::open(&src).unwrap());
                }
                let t0 = Instant::now();
                let mut input = File::open(&src).unwrap();
                let (out, how, sum) = copy_into(&mut input, &tmp, &ops).unwrap();
                let copied = t0.elapsed();
                out.sync_all().unwrap();
                let synced = t0.elapsed();
                forget_cached(&out);
                drop(out);
                let back = hash_from_start(&mut File::open(&tmp).unwrap()).unwrap();
                let total = t0.elapsed();
                assert_eq!(back, sum);
                println!(
                    "{name:>6} ({how:?}), {cache} source: copy+hash {:>6.2}s, +flush {:>6.2}s, +verify from disk {:>6.2}s  ({:.0} MB/s overall)",
                    copied.as_secs_f64(),
                    synced.as_secs_f64(),
                    total.as_secs_f64(),
                    (len >> 20) as f64 / total.as_secs_f64()
                );
                let _ = fs::remove_file(&tmp);
            }
        }
    }
}
