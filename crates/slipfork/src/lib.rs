//! Pure rendering shared by the `slipfork` binary and its public tests.
//!
//! The binary is the composition root; this library target exposes only the side-effect-free
//! status renderer, the `ps` schema, and the unit writer so both can be asserted without a daemon.

#[path = "daemon/ps.rs"]
pub mod ps;

mod render;

pub mod install;
pub mod status;
