//! Shared bounded-wait primitives for Slipfork's real-process integration suites.
//!
//! This crate is test scaffolding only: it is `publish = false` and carries no Cargo features. The
//! `publish = false` workspace test-crate rule is owned by `ARCHITECTURE.md` §1 as landed by node
//! `M3.25`; this crate is consumed through `[dev-dependencies]` by `slipfork` and `slipfork-engine`.

#![forbid(unsafe_code)]

pub mod conformance;
pub mod live;
pub mod pods;

pub use pods::{PodPlan, ServicePod, group_members, host_pgrp, read_pod_config, world_init_pid};

use std::time::{Duration, Instant};

/// The one poll interval for every bounded wait in Slipfork's integration suites.
pub const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Bound on a freshly spawned `slipforkd` accepting its control socket.
// 15 s CLI spawn budget (`SPAWN_TIMEOUT`) + 10 s product stop grace (`WORLD_STOP_GRACE`) + margin.
pub const DAEMON_STARTUP_DEADLINE: Duration = Duration::from_secs(40);

/// The one settle window: a stopping service releasing a socket or listener it owned.
///
/// `> 10 s WORLD_STOP_GRACE`; the caller must outlast the product's own teardown.
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(15);

/// Poll `probe` every [`POLL_INTERVAL`] until it reports `Ok` or `bound` elapses.
///
/// The last probe error is returned on timeout, so each caller panics with the real captured
/// diagnostic instead of a bare deadline.
///
/// The bound is **soft**: it is checked between probes only, so a probe that blocks past `bound`
/// (a wedged `crun exec`, a `psql` stuck on a dead socket) still overruns it. It bounds how long a
/// suite waits for a state, not how long one probe may run; per-probe timeouts belong to the
/// engine's own readiness contract (`ADR-0002` §2), never to this helper.
pub fn poll_until(
    bound: Duration,
    probe: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    poll_until_with_interval(bound, POLL_INTERVAL, probe)
}

/// Poll `probe` every `interval` until it reports `Ok` or `bound` elapses.
///
/// The one bounded-wait implementation in the workspace: [`poll_until`] is this function with
/// [`POLL_INTERVAL`], and every crate harness re-exports it (never a per-crate copy). The last
/// probe error is returned on timeout; the bound is soft and checked between probes only.
pub fn poll_until_with_interval(
    bound: Duration,
    interval: Duration,
    mut probe: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let deadline = Instant::now() + bound;
    loop {
        match probe() {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(interval),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{poll_until, poll_until_with_interval};
    use std::time::Duration;

    #[test]
    fn returns_as_soon_as_the_probe_succeeds() {
        let mut attempts = 0;
        let result = poll_until(Duration::from_secs(1), || {
            attempts += 1;
            if attempts == 2 {
                Ok(())
            } else {
                Err("not yet".to_owned())
            }
        });
        assert!(result.is_ok(), "the second probe succeeds: {result:?}");
        assert_eq!(attempts, 2);
    }

    #[test]
    fn returns_the_last_probe_error_once_the_bound_has_passed() {
        let mut attempts = 0;
        let result = poll_until(Duration::ZERO, || {
            attempts += 1;
            Err("still not ready".to_owned())
        });
        assert_eq!(result.unwrap_err(), "still not ready");
        assert_eq!(attempts, 1);
    }

    #[test]
    fn explicit_interval_form_is_the_same_bounded_wait() {
        let mut attempts = 0;
        let result = poll_until_with_interval(Duration::ZERO, Duration::from_millis(1), || {
            attempts += 1;
            Err("custom interval".to_owned())
        });
        assert_eq!(result.unwrap_err(), "custom interval");
        assert_eq!(attempts, 1);
    }
}
