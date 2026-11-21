//! Pure rendering shared by the `slipfork` binary and its public tests.
//!
//! The binary is the composition root; this library target exposes only the side-effect-free
//! status renderer, the `ps` schema, and the unit writer so both can be asserted without a daemon.

#[path = "daemon/ps.rs"]
pub mod ps;

/// The pure boot-pane renderer: exact rows, bars, and status lines, shared with the live driver.
#[path = "boot/render.rs"]
pub mod boot;

/// The one output-mode/colour layer, shared with the binary so both draw through one `paint`.
pub mod ui;

/// Compile-time descriptor the ANSI matrix test iterates; the shipped surface is `cli`'s clap tree.
pub mod surface;

pub mod install;
pub mod mounts;
pub mod status;
