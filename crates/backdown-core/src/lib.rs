//! BackDown core: a resumable file index, tiered content hashing, duplicate detection and
//! "is this safe to wipe" verification, and recognition of what things are (mail stores, backups,
//! repositories…), over one SQLite database.
//!
//! Sources are only ever opened read-only. Every long step is resumable: re-running `index`
//! keeps the hashes of unchanged files, and `hash` only computes what is still missing.

pub mod apply;
pub mod browse;
pub mod db;
pub mod fmt;
pub mod hash;
pub mod index;
pub mod legacy;
pub mod overlap;
pub mod paths;
pub mod plan;
pub mod recognise;
pub mod report;
pub mod summary;

pub use db::Db;
