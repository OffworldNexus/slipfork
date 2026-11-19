//! Bounded `/proc`-observed waits shared by every world engine.
//!
//! Every stop path is a bounded poll: a child reap, a process-group disappearance, and the
//! `D`-state retry policy that keeps a `SIGKILL`ed task in uninterruptible sleep from being
//! reported as unkillable. The policy observes through a closure, so it is exercised without a
//! real kernel wait.

#![forbid(unsafe_code)]

use std::process::Child;
use std::thread;
use std::time::{Duration, Instant};

use slipfork_world::EngineError;

use crate::supervise::ProcessIdentity;

/// Poll interval for every bounded wait (`/proc` disappearance, child reaping, readiness probes).
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Upper bound on retrying a `SIGKILL`ed process that is stuck in uninterruptible sleep (`D`).
///
/// `SIGKILL` is only delivered when the task leaves the kernel wait it is blocked in. A `D`-state
/// task is therefore not unkillable, merely slow to observe the signal: the stop path keeps
/// retrying with exponential backoff for this window instead of declaring it unkillable after the
/// bare per-signal grace.
pub(crate) const D_STATE_KILL_GRACE: Duration = Duration::from_secs(60);

/// Poll `step` every [`POLL_INTERVAL`] until it is ready or `grace` elapses.
///
/// The one bounded-wait loop: `step` returns `Some(..)` to stop with a value, `None` to keep
/// polling. A caller that must stop early returns its terminal value from `step` (for example
/// [`wait_for_exit`]'s `Some(false)` on `try_wait` error).
pub(crate) fn poll_until<T>(grace: Duration, mut step: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + grace;
    loop {
        if let Some(value) = step() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Wait (bounded) for `child` to exit, reaping it.
pub(crate) fn wait_for_exit(child: &mut Child, grace: Duration) -> bool {
    poll_until(grace, || match child.try_wait() {
        Ok(Some(_)) => Some(true),
        Ok(None) => None,
        Err(_) => Some(false),
    }) == Some(true)
}

/// Wait for `identity` to disappear, retrying a `D`-state survivor with exponential backoff.
///
/// A task in uninterruptible sleep has not acted on `SIGKILL` yet but is not unkillable; once the
/// kernel wait completes the pending signal is delivered. Only a survivor that is *not* in `D`
/// after `grace`, or a `D`-state survivor that outlasts `d_state_grace`, is reported.
///
/// `d_state_grace` is the caller's retry budget ([`D_STATE_KILL_GRACE`] for a service group,
/// `Duration::ZERO` for a caller that must report at once and keeps its existing grace policy).
/// `term_started` is when the caller sent its first `SIGTERM`: the error reports the **measured**
/// elapsed time since then, which is the only honest number once a `D`-state survivor has outlived
/// both configured budgets.
pub(crate) fn wait_reaped(
    identity: &ProcessIdentity,
    term_started: Instant,
    grace: Duration,
    d_state_grace: Duration,
    label: &str,
) -> Result<(), EngineError> {
    wait_reaped_observing(term_started, grace, d_state_grace, label, || {
        identity.survivor()
    })
}

/// The retry policy behind [`wait_reaped`], over a caller-supplied survivor observation.
///
/// `survivor` returns the still-live pid and its `/proc` state, or `None` once nothing is left.
/// Production reaches this policy only through [`wait_reaped`], which always passes
/// [`ProcessIdentity::survivor`]; the private seam lets the `D`-state retry be unit-tested with a
/// deterministic state source without being part of the crate-internal surface.
fn wait_reaped_observing(
    term_started: Instant,
    grace: Duration,
    d_state_grace: Duration,
    label: &str,
    mut survivor: impl FnMut() -> Option<(u32, char)>,
) -> Result<(), EngineError> {
    if poll_until(grace, || survivor().is_none().then_some(())).is_some() {
        return Ok(());
    }
    let deadline = Instant::now() + d_state_grace;
    let mut backoff = POLL_INTERVAL;
    loop {
        let Some((pid, state)) = survivor() else {
            return Ok(());
        };
        if state != 'D' || Instant::now() >= deadline {
            return Err(EngineError::Execution(format!(
                "{label} survived SIGTERM and SIGKILL for {:?} (kill grace {grace:?}, D-state \
                 grace {d_state_grace:?}): pid {pid} is still in /proc state '{state}'",
                term_started.elapsed()
            )));
        }
        thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_d_state_survivor_is_retried_past_the_nominal_grace_instead_of_being_reported() {
        // Deterministic state source: the survivor stays in `D` well past the nominal grace and
        // only then disappears. With a zero nominal grace the only way this can succeed is the
        // `D`-state retry loop; a non-`D` survivor would have been reported on the first probe.
        let mut probes = 0_u32;
        let result = wait_reaped_observing(
            Instant::now(),
            Duration::ZERO,
            Duration::from_secs(5),
            "unit service 'api'",
            || {
                probes += 1;
                (probes < 6).then_some((4242, 'D'))
            },
        );
        assert!(
            result.is_ok(),
            "the D-state survivor must be retried: {result:?}"
        );
        assert_eq!(probes, 6, "every retry must probe the survivor again");
    }

    #[test]
    fn a_d_state_survivor_that_outlasts_its_budget_is_reported_with_its_state() {
        // The survivor burned both budgets: the report must name the *measured* elapsed time since
        // the first `SIGTERM`, not the configured grace, which is what used to make a ~70 s stop
        // read "survived … for 10s". Both budgets are zero here so the probe is instant; the
        // measurement still comes from the caller's first-`SIGTERM` instant, not from a budget.
        let err = wait_reaped_observing(
            Instant::now() - Duration::from_secs(70),
            Duration::ZERO,
            Duration::ZERO,
            "unit service 'api'",
            || Some((4242, 'D')),
        )
        .expect_err("a D-state survivor past its budget must be reported");
        let message = err.to_string();
        assert!(message.contains("pid 4242"), "{message}");
        assert!(message.contains("/proc state 'D'"), "{message}");
        assert!(
            message.contains("for 70."),
            "the report must measure the time since the first SIGTERM: {message}"
        );
        assert!(
            message.contains("kill grace"),
            "the configured deadline stays visible next to the measurement: {message}"
        );
    }
}
