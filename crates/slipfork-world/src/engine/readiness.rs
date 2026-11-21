//! `2ms` readiness polling shared by the resource launchers.
//!
//! The transport belongs to the composition root (only it owns the world netns), so the poll loop
//! takes a probe closure and owns the interval, the budget, and the fail-closed remedy.

use std::thread;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// Readiness probes fire every `2ms`; the spec's promise, never a `sleep 1`.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(2);
/// A resource pod must answer inside this window or the boot fails closed.
pub(crate) const READINESS_BUDGET: Duration = Duration::from_secs(2);

/// Polls `probe` every `interval` until it succeeds and reports the wait.
///
/// A miss for the whole `budget` becomes one typed error; the one-line `next:` remedy is rendered
/// by [`Error::NotReady`], never embedded mid-sentence in an `io::Error`.
pub(crate) fn wait_until(
    probe: &mut dyn FnMut() -> bool,
    tool: &'static str,
    endpoint: &str,
    interval: Duration,
    budget: Duration,
) -> Result<Duration> {
    let started = Instant::now();
    loop {
        if probe() {
            return Ok(started.elapsed());
        }
        if started.elapsed() >= budget {
            return Err(Error::NotReady {
                tool,
                endpoint: endpoint.to_owned(),
            });
        }
        thread::sleep(interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_budget_and_interval_fail_closed_with_a_typed_error() {
        let mut up = || true;
        let waited = wait_until(
            &mut up,
            "postgres",
            "/data/.sock/.s.PGSQL.5432",
            Duration::from_millis(1),
            Duration::from_millis(20),
        )
        .expect("an immediately ready probe succeeds");
        assert!(waited <= Duration::from_millis(20), "waited {waited:?}");

        let mut down = || false;
        let started = Instant::now();
        let error = wait_until(
            &mut down,
            "redis",
            ":6379",
            Duration::from_millis(1),
            Duration::from_millis(5),
        )
        .expect_err("a dead probe must fail closed");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the injected budget must not burn the 2s default"
        );
        assert!(matches!(error, Error::NotReady { .. }), "{error:?}");
        let message = error.to_string();
        assert!(
            message.contains("did not answer on `:6379`"),
            "the timeout must name the dead endpoint: {message}"
        );
        assert!(
            message.contains("next: run `slipfork logs -a`"),
            "the timeout must carry one `next:` remedy: {message}"
        );
        assert_eq!(
            POLL_INTERVAL,
            Duration::from_millis(2),
            "readiness must poll every 2ms, not every 10/100/200ms"
        );
    }
}
