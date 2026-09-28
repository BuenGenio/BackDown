//! What deleted files have in common across filesystems: how sure a recovery can be, how a deleted
//! entry is picked again, and a file's map with the parts other files have taken since left as
//! holes (read as zeros), so a partly overwritten file never comes back with another file's bytes.
//!
//! How sure it can be, by filesystem:
//! - FAT12/16/32 forget a deleted file's cluster chain (its FAT entries are zeroed), so only its
//!   first cluster and size are left: its clusters are taken to follow one another, which is how
//!   most files are written. One cluster, or several all still free: recoverable (sure for one
//!   cluster, fairly sure for more, since a file written in pieces would bring back parts of other
//!   deleted files). Some since used by other files: partly overwritten, and hardly likely right
//!   when that's the first one. All of them used (or three quarters, the first among them):
//!   overwritten.
//! - exFAT marks contiguous files as such ("no FAT chain"), so for those the clusters are known;
//!   a chained file's chain usually survives in the FAT (only the allocation bitmap is cleared),
//!   and is followed when it still holds together; otherwise the clusters are taken to follow one
//!   another. The allocation bitmap says which clusters other files have taken since.
//! - NTFS keeps the whole MFT record, with the file's names, folder and map of its data, until the
//!   record is reused: small files kept in the record itself are recoverable for sure; the
//!   clusters of larger ones are checked against $Bitmap.

use serde::Serialize;

use anyhow::Result;

use super::{Run, push_run};

/// Why ext2/3/4 can't list deleted files.
pub(crate) const EXT_WHY: &str = "ext3 and ext4 erase where a deleted file's data was, so deleted \
    files can't be brought back by name here. Rescue's \u{201c}Find files by content\u{201d} can \
    still find photos, videos and documents on it";

/// How likely a deleted file or folder can be recovered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Deleted {
    /// "recoverable", "partly overwritten" or "overwritten".
    pub state: &'static str,
    /// How sure BackDown is that what it recovers is the file as it was, from 0 to 100.
    pub confidence: u8,
    /// Why, in plain words.
    pub why: String,
    /// Picks it among its folder's deleted entries: its place in the folder, or its MFT record.
    pub token: u64,
    /// Its path segment for `Volume::find`: its name and token.
    pub seg: String,
}

impl Deleted {
    pub(crate) fn new(name: &str, token: u64, v: Verdict) -> Deleted {
        Deleted {
            state: v.state,
            confidence: v.confidence,
            why: v.why,
            token,
            seg: seg(name, token),
        }
    }
}

/// How a deleted file's data stands, before it's tied to an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Verdict {
    pub state: &'static str,
    pub confidence: u8,
    pub why: String,
}

impl Verdict {
    pub(crate) fn sure(why: &str) -> Verdict {
        Verdict {
            state: "recoverable",
            confidence: 99,
            why: why.into(),
        }
    }

    pub(crate) fn lost(why: impl Into<String>) -> Verdict {
        Verdict {
            state: "overwritten",
            confidence: 0,
            why: why.into(),
        }
    }
}

/// A deleted entry's path segment: its name, a NUL (which no listed name has), and its token.
pub(crate) fn seg(name: &str, token: u64) -> String {
    format!("{name}\0{token}")
}

pub(crate) fn parse_seg(s: &str) -> Option<(&str, u64)> {
    let (name, token) = s.split_once('\0')?;
    Some((name, token.parse().ok()?))
}

/// A path for messages, without the tokens.
pub(crate) fn shown(path: &str) -> String {
    path.split('/')
        .map(|s| parse_seg(s).map_or(s, |(n, _)| n))
        .collect::<Vec<_>>()
        .join("/")
}

/// A deleted file's map, built stretch by stretch: what's still free is read from the volume, what
/// other files have taken since is a hole.
#[derive(Debug, Default)]
pub(crate) struct Spans {
    pub runs: Vec<Run>,
    /// The file's bytes mapped so far.
    pub at: u64,
    pub free: u64,
    pub used: u64,
    /// Bytes that aren't on the volume at all (past its end).
    pub missing: u64,
    /// Whether the file's first stretch is one other files have taken.
    pub first_used: Option<bool>,
}

impl Spans {
    /// `len` bytes of the file at `phys` in the volume, free (still the file's, as far as anyone
    /// can tell) or taken by another file since.
    pub(crate) fn push(&mut self, phys: u64, len: u64, used: bool) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        self.first_used.get_or_insert(used);
        push_run(
            &mut self.runs,
            Run {
                at: self.at,
                len,
                phys: (!used).then_some(phys),
            },
        )?;
        self.at += len;
        if used {
            self.used += len;
        } else {
            self.free += len;
        }
        Ok(())
    }

    /// `len` bytes the file never stored (a sparse file's hole): zeros, and nothing lost.
    pub(crate) fn hole(&mut self, len: u64) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        push_run(
            &mut self.runs,
            Run {
                at: self.at,
                len,
                phys: None,
            },
        )?;
        self.at += len;
        Ok(())
    }

    /// `len` bytes that can't be read at all (a hole).
    pub(crate) fn gone(&mut self, len: u64) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        self.first_used.get_or_insert(true);
        push_run(
            &mut self.runs,
            Run {
                at: self.at,
                len,
                phys: None,
            },
        )?;
        self.at += len;
        self.missing += len;
        Ok(())
    }

    /// The verdict for a file whose parts are all mapped: `whole` is the confidence when every part
    /// is still free, and `how` says why it's that sure.
    pub(crate) fn verdict(&self, whole: u8, how: &str) -> Verdict {
        let total = self.free + self.used + self.missing;
        if total == 0 || self.used + self.missing == 0 {
            return Verdict {
                state: "recoverable",
                confidence: whole,
                why: how.into(),
            };
        }
        if self.free == 0 || self.first_used == Some(true) && self.free * 4 < total {
            return Verdict::lost(if self.missing == total {
                "where it was is outside the volume now".to_string()
            } else {
                "other files have been saved where it was".to_string()
            });
        }
        let pct = (self.free * 100 / total).clamp(1, 99);
        let conf = (u64::from(whole) * self.free / total).clamp(1, 99) as u8;
        Verdict {
            state: "partly overwritten",
            confidence: if self.first_used == Some(true) {
                conf.min(15)
            } else {
                conf
            },
            why: format!(
                "about {pct}% of it is still there; the rest has been reused by other files since and comes back empty"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_and_verdicts() {
        let s = seg("photo.jpg", 4128);
        assert_eq!(parse_seg(&s), Some(("photo.jpg", 4128)));
        assert_eq!(parse_seg("photo.jpg"), None);
        assert_eq!(parse_seg("x\0nope"), None);
        assert_eq!(shown(&format!("a/{s}/b")), "a/photo.jpg/b");

        let mut sp = Spans::default();
        sp.push(4096, 4096, false).unwrap();
        sp.push(8192, 4096, false).unwrap();
        assert_eq!(sp.runs.len(), 1);
        let v = sp.verdict(70, "its clusters are free");
        assert_eq!((v.state, v.confidence), ("recoverable", 70));

        sp.push(12288, 4096, true).unwrap();
        sp.push(16384, 4096, false).unwrap();
        assert_eq!(sp.runs.len(), 3);
        assert_eq!(sp.runs[1].phys, None);
        let v = sp.verdict(70, "");
        assert_eq!(v.state, "partly overwritten");
        assert!(v.confidence < 70 && v.why.contains("75%"), "{v:?}");

        let mut first = Spans::default();
        first.push(0, 4096, true).unwrap();
        first.push(4096, 4096 * 9, false).unwrap();
        let v = first.verdict(70, "");
        assert_eq!(v.state, "partly overwritten");
        assert!(v.confidence <= 15);
        let mut gone = Spans::default();
        gone.push(0, 4096 * 9, true).unwrap();
        gone.push(0, 4096, false).unwrap();
        assert_eq!(gone.verdict(70, "").state, "overwritten");
        let mut out = Spans::default();
        out.gone(100).unwrap();
        let v = out.verdict(70, "");
        assert_eq!(v.state, "overwritten");
        assert!(v.why.contains("outside"));
    }
}
