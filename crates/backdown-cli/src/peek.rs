//! The container viewers now live in the `backdown-viewers` crate. This shim keeps `crate::peek`
//! working while branches that predate the move are open; new code uses `backdown_viewers::peek`.
pub use backdown_viewers::peek::*;
