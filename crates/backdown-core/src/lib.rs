//! BackDown core: a resumable file index, tiered content hashing, duplicate detection and
//! "is this safe to wipe" verification, recognition of what things are (mail stores, backups,
//! repositories…), and `volumes`, what disks and partitions this machine has, over one SQLite database.
//! `fsread` lists and reads the files inside a disk image or partition (FAT, exFAT, ext, NTFS)
//! without mounting it.
//!
//! Sources are only ever opened read-only. Every long step is resumable: re-running `index`
//! keeps the hashes of unchanged files, and `hash` only computes what is still missing.

pub mod apply;
pub mod browse;
pub mod db;
pub mod fmt;
pub mod fsread;
pub mod hash;
pub mod index;
pub mod legacy;
pub mod magic;
pub mod mirror;
pub mod overlap;
pub mod paths;
pub mod plan;
pub mod recognise;
pub mod report;
pub mod rescue;
pub mod summary;
pub mod volumes;
pub mod wipe;

pub use db::Db;
