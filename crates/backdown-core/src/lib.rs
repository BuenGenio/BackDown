//! BackDown core: a resumable file index, tiered content hashing, duplicate detection and
//! "is this safe to wipe" verification, recognition of what things are (mail stores, backups,
//! repositories…), and `volumes`, what disks and partitions this machine has, over one SQLite database.
//! `fsread` lists and reads the files inside a disk image or partition (FAT, exFAT, ext, NTFS)
//! without mounting it. `health` reads each drive's SMART data (without root) and keeps readings,
//! so a drive that starts failing is noticed.
//!
//! Sources are only ever opened read-only. Every long step is resumable: re-running `index`
//! keeps the hashes of unchanged files, and `hash` only computes what is still missing.

pub mod alerts;
pub mod apply;
pub mod browse;
pub mod clock;
pub mod custody;
pub mod db;
pub mod dedup;
pub mod extras;
pub mod fastcopy;
pub mod find;
pub mod fmt;
pub mod fsread;
pub mod hash;
pub mod health;
pub mod index;
pub mod legacy;
pub mod magic;
pub mod mailmerge;
pub mod mailstores;
pub mod manifest;
pub mod mbox;
pub mod mirror;
pub mod names;
pub mod overlap;
pub mod paths;
pub mod plan;
pub mod plist;
pub mod pool;
pub mod recognise;
pub mod remote;
pub mod report;
pub mod rescue;
pub mod signing;
pub mod streams;
pub mod summary;
pub mod swarm;
pub mod timemachine;
pub mod volumes;
#[cfg(windows)]
mod win;
pub mod wipe;

pub use db::Db;
