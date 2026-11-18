//! The one duration conversion every engine adapter shares.
//!
//! Measured facts (`uptime_secs`, build/resolve/seal/restore `millis`) are taken as a plain
//! [`Instant`] pair inside the adapter that performed the work, so no [`Instant`] ever crosses a
//! wire and there is no injected clock (nor `dyn` clock trait) anywhere in the graph.

use std::time::Instant;

/// Elapsed whole milliseconds between two observed instants, saturating at `u64::MAX`.
///
/// The one duration conversion: an adapter takes the two instants around its own work, so no
/// caller keeps a second rounding rule (or a second time source).
pub fn elapsed_millis(started: Instant, finished: Instant) -> u64 {
    u64::try_from(finished.saturating_duration_since(started).as_millis()).unwrap_or(u64::MAX)
}
