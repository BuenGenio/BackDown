//! The SQL dump viewer now lives in the `backdown-viewers` crate. This shim keeps `crate::sqldump`
//! working while branches that predate the move are open; new code uses `backdown_viewers::sqldump`.
pub use backdown_viewers::sqldump::*;
