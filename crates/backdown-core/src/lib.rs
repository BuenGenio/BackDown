//! BackDown core: a resumable file index, tiered content hashing, duplicate detection and
//! "is this safe to wipe" verification, over one SQLite database; and `volumes`, what disks and
//! partitions this machine has and what's on them.
//!
//! Sources are only ever opened read-only. Every long step is resumable: re-running `index`
//! keeps the hashes of unchanged files, and `hash` only computes what is still missing.

pub mod browse;
pub mod db;
pub mod fmt;
pub mod hash;
pub mod index;
pub mod magic;
pub mod paths;
pub mod report;
pub mod volumes;

pub use db::Db;
