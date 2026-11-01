//! Read-only looks inside container files, for the browser's viewers (`backdown serve`):
//!
//! - `peek`: spreadsheets, slide decks, mail (.eml and mbox), archives (zip and tar, plain or
//!   compressed) and disk images, with the files inside the filesystems those images hold;
//! - `mailbox`: an mbox file or a Maildir folder read as conversations;
//! - `sqldump`: a SQL dump read as the schema it creates.
//!
//! The web server resolves an index id to a path and hands it here; nothing in this crate picks
//! files by itself, writes anything or runs anything, and every read is capped in size and time.
//! What comes back is JSON shaped for the viewers in `crates/backdown-cli/ui`.

pub mod mailbox;
pub mod peek;
pub mod sqldump;

/// A random token, hex: what a parked read is resumed by.
fn new_token() -> anyhow::Result<String> {
    let mut raw = [0u8; 24];
    getrandom::fill(&mut raw).map_err(|e| anyhow::anyhow!("no randomness for the token: {e}"))?;
    Ok(backdown_core::fmt::hex(&raw))
}
