//! The one injected clock every engine, builder, image store, and daemon measure shares.
//!
//! Measured facts (`uptime_secs`, build/resolve/seal/restore `millis`) are stamped from this clock
//! before they leave the adapter that produced them, so no [`std::time::Instant`] ever crosses a
//! wire and no second time source exists in the daemon. The production adapter is [`SystemClock`];
//! the composition root constructs it once and injects the same `Arc<dyn MonotonicClock>` into
//! every adapter that measures or labels time, while tests inject a deterministic fake.

use std::time::{Instant, SystemTime};

/// The one clock port: monotonic instants for durations, wall time for labels.
pub trait MonotonicClock: Send + Sync {
    /// The current monotonic instant. Only differences are meaningful (`uptime_secs`, `millis`).
    fn now(&self) -> Instant;

    /// The current wall-clock time. Used only for values that must vary within one instant (a
    /// unique staging suffix), never for a duration.
    fn now_wall(&self) -> SystemTime;
}

/// The single production [`MonotonicClock`]: `Instant::now()` / `SystemTime::now()`.
///
/// Wired exactly once by the composition root (`slipfork::daemon::compose_daemon`) and shared by
/// the world engine, the rootfs builder, the OCI image store, the postmaster-readiness gate, and
/// the daemon's own base-seed measurement.
#[derive(Debug, Clone, Copy)]
pub struct SystemClock;

impl MonotonicClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn now_wall(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Elapsed whole milliseconds between two injected-clock instants, saturating at `u64::MAX`.
///
/// The one duration conversion: an adapter passes the two instants it took from the same clock, so
/// no caller keeps a second rounding rule (or a second time source).
pub fn elapsed_millis(started: Instant, finished: Instant) -> u64 {
    u64::try_from(finished.saturating_duration_since(started).as_millis()).unwrap_or(u64::MAX)
}
