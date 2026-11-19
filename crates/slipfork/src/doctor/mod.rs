//! `slipfork doctor`: the pre-daemon host diagnostic seam (`ADR-0010` §4, `M3.25`).
//!
//! The composition root builds the concrete probes once ([`OsHostProbe`], [`SystemdIngressProbe`],
//! and the engine's Btrfs driver), observes them into plain [`HostFacts`], and hands them to
//! [`run`]; nothing here spawns in `run`, so `doctor` still works when `slipforkd` refuses to boot.
//! [`DoctorReport::render_text`] is the single verdict-first renderer (three rows) and
//! [`DoctorReport::to_json`] the single `--json` projection.

#![forbid(unsafe_code)]

mod probe;
mod report;
#[cfg(test)]
mod tests;

pub use probe::{HostCheckOutcome, IngressCheck, NetnsCheck, OsHostProbe, SystemdIngressProbe};
pub use report::{DoctorReport, HostFacts, run};
