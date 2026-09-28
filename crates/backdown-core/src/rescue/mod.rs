//! Rescue: getting data off failing, damaged or emptied media. Everything here only reads the
//! source (a drive, a partition, a memory card, or an image file of one) and writes somewhere else.
//!
//! - `image`: copy a failing drive or card into an image file, safely: the good areas first, the
//!   bad ones skipped and come back to later, with a map so it can stop and resume (like GNU
//!   ddrescue, whose mapfile format it reads and writes).
//! - `carve`: find files by their content, whatever the filesystem says: deleted photos,
//!   documents and videos on a card that was formatted or a drive whose filesystem is gone (like
//!   PhotoRec).
//! - `parts`: find partitions missing from the partition table, by their filesystems' own
//!   signatures (like TestDisk's search).
//!
//! Each takes a path (a device node or an image file), a progress callback and a stop flag, so the
//! daemon can run it as a job that reports how far it is and can be cancelled.

use std::fs::File;
use std::io::{self, Seek, SeekFrom};

pub mod carve;
pub mod image;
pub mod parts;

/// The size of a file or a block device (a device's metadata says 0; seeking to its end doesn't).
pub fn size_of(f: &File) -> io::Result<u64> {
    let mut r = f;
    let n = r.seek(SeekFrom::End(0))?;
    r.seek(SeekFrom::Start(0))?;
    Ok(n)
}
