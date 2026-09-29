//! A drive's change log, per platform: where it stands (noted when a scan begins) and what it
//! says changed since then. Windows: the NTFS USN journal (`usn`). macOS: the volume's FSEvents
//! history (`fsevents`). Linux keeps no persistent change log, so rescans there are walks or,
//! for NTFS, reads of the file table.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::ChangeSet;
use crate::Db;

/// Where a change log stood.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum Cursor {
    /// An NTFS volume's USN journal: the volume's serial number, the journal's id (a new one when
    /// the journal is deleted and made again) and the next USN it would write.
    Usn { volume: u64, journal: u64, usn: i64 },
    /// A volume's FSEvents history: its store's UUID (a new one when the history is reset) and
    /// the event id reached.
    Fsevents { uuid: String, event: u64 },
}

/// Where the change log of the volume holding `root` stands now: None where there's none to read
/// (Linux, a filesystem without one), an error saying why it can't be read.
pub(crate) fn cursor_now(root: &Path) -> Result<Option<Cursor>, String> {
    #[cfg(windows)]
    {
        super::usn::sys::cursor(root)
    }
    #[cfg(target_os = "macos")]
    {
        super::fsevents::sys::cursor(root)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = root;
        Ok(None)
    }
}

/// What the change log says changed since `from`, up to `to` (read when this scan began), or why
/// it can't say (it was reset or trimmed, it's another volume's, it dropped events…).
pub(crate) fn changes_since(
    db: &Db,
    root: &Path,
    root_id: i64,
    from: &Cursor,
    to: &Cursor,
) -> Result<ChangeSet, String> {
    #[cfg(windows)]
    {
        super::usn::sys::changes(db, root, root_id, from, to)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (db, root_id);
        super::fsevents::sys::changes(root, from, to)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (db, root, root_id, from, to);
        Err("this system keeps no change log for drives".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_as_json() {
        let c = Cursor::Usn {
            volume: 0x1234,
            journal: 99,
            usn: 4096,
        };
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(s, r#"{"kind":"usn","volume":4660,"journal":99,"usn":4096}"#);
        assert_eq!(serde_json::from_str::<Cursor>(&s).unwrap(), c);
        let f = Cursor::Fsevents {
            uuid: "A-B".into(),
            event: 7,
        };
        let s = serde_json::to_string(&f).unwrap();
        assert_eq!(serde_json::from_str::<Cursor>(&s).unwrap(), f);
    }
}
