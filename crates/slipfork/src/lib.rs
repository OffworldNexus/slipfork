//! Pure rendering shared by the `slipfork` binary and its public tests.
//!
//! The binary is the composition root; this library target exposes the append-only boot driver
//! (completed rows in scrollback plus one `\r`-rewritten status line) and the `ps` schema so both
//! can be asserted without a daemon.

#[path = "daemon/ps.rs"]
pub mod ps;

/// The pure boot renderer and its live driver: rows, bars, and status lines shared with the binary.
pub mod boot;

/// The one output-mode/colour layer, shared with the binary so both draw through one `paint`.
pub mod ui;

/// Compile-time descriptor the ANSI matrix test iterates; the shipped surface is `cli`'s clap tree.
pub mod surface;

pub mod mounts;
pub mod status;
