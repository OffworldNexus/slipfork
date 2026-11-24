//! Per-key singleflight slots: concurrent requests for one image id or OCI reference share one
//! leader's outcome instead of pulling or building twice.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};

pub(super) enum FlightState<T> {
    Running,
    Done(Result<T, String>),
}

pub(super) type Flight<T> = Arc<(Mutex<FlightState<T>>, Condvar)>;
pub(super) type FlightTable<T> = Arc<Mutex<BTreeMap<String, Flight<T>>>>;

pub(super) struct FlightGuard<T: Clone> {
    key: String,
    table: FlightTable<T>,
    flight: Flight<T>,
    completed: bool,
}

impl<T: Clone> FlightGuard<T> {
    /// Publishes the leader's outcome to every waiter and closes the slot.
    pub(super) fn finish(mut self, outcome: Result<T, String>) {
        self.completed = true;
        if let Ok(mut table) = self.table.lock() {
            table.remove(&self.key);
        }
        let (lock, cvar) = &*self.flight;
        if let Ok(mut slot) = lock.lock() {
            *slot = FlightState::Done(outcome);
            cvar.notify_all();
        }
    }
}

impl<T: Clone> Drop for FlightGuard<T> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        if let Ok(mut table) = self.table.lock() {
            table.remove(&self.key);
        }
        let (lock, cvar) = &*self.flight;
        if let Ok(mut slot) = lock.lock() {
            *slot = FlightState::Done(Err("an image worker panicked".to_owned()));
            cvar.notify_all();
        }
    }
}

pub(super) enum FlightRole<T: Clone> {
    Lead(FlightGuard<T>),
    Wait(Flight<T>),
}

/// Joins the slot for `key`, leading it when it is the first request.
pub(super) fn acquire_flight<T: Clone>(
    table: &FlightTable<T>,
    key: String,
) -> Result<FlightRole<T>, String> {
    let mut guard = table
        .lock()
        .map_err(|_| "singleflight lock poisoned".to_owned())?;
    if let Some(existing) = guard.get(&key) {
        return Ok(FlightRole::Wait(Arc::clone(existing)));
    }
    let flight = Arc::new((Mutex::new(FlightState::Running), Condvar::new()));
    guard.insert(key.clone(), Arc::clone(&flight));
    Ok(FlightRole::Lead(FlightGuard {
        key,
        table: Arc::clone(table),
        flight,
        completed: false,
    }))
}

/// Blocks until the leading request settles, then clones its outcome.
pub(super) fn wait_flight<T: Clone>(flight: &Flight<T>) -> Result<T, String> {
    let (lock, cvar) = &**flight;
    let mut guard = lock
        .lock()
        .map_err(|_| "singleflight state poisoned".to_owned())?;
    loop {
        match &*guard {
            FlightState::Running => {
                guard = cvar
                    .wait(guard)
                    .map_err(|_| "singleflight wait poisoned".to_owned())?;
            }
            FlightState::Done(outcome) => return outcome.clone(),
        }
    }
}
