//! Mounting read-only now lives in backdown-core (`volumes::mount`), beside the probe that says
//! what's mountable. This shim keeps `crate::mounting` working while branches that predate the move
//! are open; new code uses `backdown_core::volumes::mount`.
pub use backdown_core::volumes::mount::*;
