//! The mailbox viewer now lives in the `backdown-viewers` crate. This shim keeps `crate::mailbox`
//! working while branches that predate the move are open; new code uses `backdown_viewers::mailbox`.
pub use backdown_viewers::mailbox::*;
