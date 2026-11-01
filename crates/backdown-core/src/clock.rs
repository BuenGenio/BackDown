//! The time as BackDown records it: whole seconds since the Unix epoch (0 if the clock is set
//! before 1970), the unit of every `*_at` column in the index.

use std::time::{SystemTime, UNIX_EPOCH};

/// Now, in seconds since the Unix epoch.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
