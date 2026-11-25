//! The pull singleflight: one leader streams a reference, waiters join its flight.
//!
//! A flight is one `(outcome, condvar)` pair plus the table slot keyed by the OCI reference. The
//! leader's [`PullGuard`] removes that slot and publishes the outcome on [`PullGuard::finish`]; if
//! the leader panics mid-pull, `Drop` publishes "the pull worker panicked" and wakes every waiter
//! instead of leaving them blocked forever.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};

use super::PulledImage;

/// One in-flight pull of one OCI reference; waiters reuse the leader's sealed result.
pub(super) type Flight = Arc<(
    Mutex<Option<std::result::Result<PulledImage, String>>>,
    Condvar,
)>;

/// The in-process in-flight table: one flight per normalized OCI reference string.
pub(super) type FlightTable = Arc<Mutex<BTreeMap<String, Flight>>>;

/// A fresh, empty singleflight table.
pub(super) fn new_table() -> FlightTable {
    Arc::new(Mutex::new(BTreeMap::new()))
}

/// A fresh flight with no recorded outcome yet.
pub(super) fn new_flight() -> Flight {
    Arc::new((Mutex::new(None), Condvar::new()))
}

/// Whether this thread leads the flight or waits on an existing one.
pub(super) enum FlightRole {
    /// This thread streams and seals the image; its guard publishes the outcome.
    Lead(PullGuard),
    /// Another thread is already pulling; wait for its sealed result.
    Wait(Flight),
}

/// Removes the flight from the table and publishes the outcome exactly once.
pub(super) struct PullGuard {
    key: String,
    table: FlightTable,
    flight: Flight,
    finished: bool,
}

impl PullGuard {
    /// Takes ownership of one freshly inserted flight slot.
    pub(super) fn new(key: String, table: FlightTable, flight: Flight) -> Self {
        Self {
            key,
            table,
            flight,
            finished: false,
        }
    }

    /// Publishes `outcome` to every waiter and retires the slot; consumes the guard.
    pub(super) fn finish(mut self, outcome: std::result::Result<PulledImage, String>) {
        self.finished = true;
        self.publish(outcome);
    }

    fn publish(&self, outcome: std::result::Result<PulledImage, String>) {
        if let Ok(mut table) = self.table.lock() {
            table.remove(&self.key);
        }
        let (lock, cvar) = &*self.flight;
        if let Ok(mut slot) = lock.lock() {
            *slot = Some(outcome);
            cvar.notify_all();
        }
    }
}

impl Drop for PullGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.publish(Err("the pull worker panicked".to_owned()));
        }
    }
}

/// Waits for the leader's outcome; a poisoned lock is reported as a failed flight.
pub(super) fn await_flight(flight: &Flight) -> std::result::Result<PulledImage, String> {
    let (lock, cvar) = &**flight;
    let mut state = lock
        .lock()
        .map_err(|_| "the pull singleflight state was poisoned".to_owned())?;
    loop {
        if let Some(outcome) = state.clone() {
            return outcome;
        }
        state = cvar
            .wait(state)
            .map_err(|_| "the pull singleflight wait was poisoned".to_owned())?;
    }
}
