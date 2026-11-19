//! Retained, size-capped service output shared by every [`crate::WorldEngine`] adapter.
//!
//! A [`ServiceLogs`] handle is minted by an engine when a service starts and stays readable after
//! [`crate::WorldEngine::stop_service`] or a failed hook — failed worlds must remain inspectable
//! (`ADR-0008` §4). Output is captured per service, never per call, so no consumer can grow memory
//! without bound: the retained window keeps the newest [`SERVICE_LOG_CAP_BYTES`] bytes.

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::ops::ControlFlow;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::{LogChunk, StreamChunk, StreamSource, epoch_millis_now};

/// How long a follower sleeps before re-checking its cancellation predicate while idle.
const FOLLOW_POLL: Duration = Duration::from_millis(50);

/// Bytes of service output retained per service (oldest chunks are evicted first).
pub const SERVICE_LOG_CAP_BYTES: usize = 64 * 1024;

/// Size-capped retained output of one world-owned service.
///
/// Cloning shares the same retained window; every clone observes all chunks captured from that
/// point on.
///
/// # Poisoning
///
/// A panic in an engine thread must never erase a failed service's output. The retained window has
/// no cross-field invariant a panic can corrupt, so every lock acquisition recovers the guard
/// instead of discarding the data; [`ServiceLogs::poisoned`] reports the poisoning as a diagnostic
/// for callers that want to surface it.
#[derive(Clone)]
pub struct ServiceLogs {
    target: Arc<str>,
    /// Stable identity of this retained window. A restarted service mints a new one, so a follower
    /// never mistakes a fresh window's backfill for lines it already printed.
    token: Arc<()>,
    inner: Arc<LogBuffer>,
}

impl ServiceLogs {
    /// Mint the retained window of the service/hook `target` the engine is about to supervise.
    pub fn new(target: impl Into<Arc<str>>) -> Self {
        Self {
            target: target.into(),
            token: Arc::new(()),
            inner: Arc::new(LogBuffer::default()),
        }
    }

    /// Identity of this retained window (pointer-stable for as long as any clone lives).
    ///
    /// A follower keys its cursor by this token, so a reused hook window skips what it already
    /// printed while a window minted by a restart replays its own retained output.
    pub fn token(&self) -> Arc<()> {
        Arc::clone(&self.token)
    }

    /// How many chunks this window has ever appended (monotonic, survives retention eviction).
    pub fn cursor(&self) -> u64 {
        self.lock().appended
    }

    /// The component or hook id this retained window belongs to.
    ///
    /// Recorded when the service was spawned, so `slipfork logs` attributes every chunk to the
    /// engine's own record instead of echoing whatever the caller asked for.
    pub fn target(&self) -> &str {
        &self.target
    }

    fn lock(&self) -> MutexGuard<'_, LogState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Every retained chunk, oldest first, in capture order (each with its capture `ts_ms`).
    pub fn snapshot(&self) -> Vec<LogChunk> {
        self.lock().chunks.iter().cloned().collect()
    }

    /// `true` when a panic poisoned the retained window.
    ///
    /// Output is still delivered by [`ServiceLogs::snapshot`] and [`ServiceLogs::follow`] — this is
    /// a diagnostic, never a reason to report empty logs.
    pub fn poisoned(&self) -> bool {
        self.inner.state.is_poisoned()
    }

    /// Replay every retained chunk, then deliver new chunks as they are captured until the service
    /// stops or exits.
    ///
    /// Returns once the log is closed — i.e. after [`crate::WorldEngine::stop_service`], the
    /// service exiting on its own, or the parent world being destroyed.
    pub fn follow(&self, on_chunk: &mut dyn FnMut(LogChunk)) {
        let _ = self.follow_from(0, &|| true, &mut |chunk| {
            on_chunk(chunk);
            ControlFlow::Continue(())
        });
    }

    /// Replay from `start`, then stream new chunks until the window closes or `keep_going` is false.
    ///
    /// `start` is a [`ServiceLogs::cursor`] value, not an index: retention eviction clamps it to the
    /// oldest surviving chunk. The predicate is polled with a bounded `Condvar::wait_timeout`, so a
    /// disconnected `logs -f` client aborts this reader promptly instead of blocking forever.
    pub fn follow_from(
        &self,
        start: u64,
        keep_going: &dyn Fn() -> bool,
        on_chunk: &mut dyn FnMut(LogChunk) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        let mut state = self.lock();
        let mut cursor = start;
        loop {
            if !keep_going() {
                return ControlFlow::Break(());
            }
            let base = state.appended - state.chunks.len() as u64;
            if cursor < base {
                // The retained window evicted chunks this follower never saw; skip what is gone.
                cursor = base;
            }
            if cursor < state.appended {
                let index = (cursor - base) as usize;
                let chunk = state.chunks[index].clone();
                cursor += 1;
                drop(state);
                let flow = on_chunk(chunk);
                if flow.is_break() {
                    return flow;
                }
                state = self.lock();
                continue;
            }
            if state.closed {
                return ControlFlow::Continue(());
            }
            state = self
                .inner
                .condvar
                .wait_timeout(state, FOLLOW_POLL)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// `true` once no further chunks can be captured (service stopped, exited, or world gone).
    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Retain one chunk stamped at the reader's capture time.
    ///
    /// The engine's pipe reader calls this with the `ts_ms` it observed when the bytes arrived, so
    /// the timestamp is the read instant — never the later moment a daemon forwards the chunk.
    pub fn push_at(&self, ts_ms: u64, source: StreamSource, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        self.inner.push(LogChunk::output(
            self.target.to_string(),
            ts_ms,
            StreamChunk { source, bytes },
        ));
    }

    /// Retain one chunk stamped at this call (the daemon's own hook capture path).
    pub fn push(&self, source: StreamSource, bytes: Vec<u8>) {
        self.push_at(epoch_millis_now(), source, bytes);
    }

    pub fn close(&self) {
        self.inner.close();
    }
}

impl std::fmt::Debug for ServiceLogs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.lock();
        f.debug_struct("ServiceLogs")
            .field("target", &self.target())
            .field("chunks", &state.chunks.len())
            .field("bytes", &state.bytes)
            .field("closed", &state.closed)
            .field("poisoned", &self.poisoned())
            .finish()
    }
}

#[derive(Default)]
struct LogBuffer {
    state: Mutex<LogState>,
    condvar: Condvar,
}

#[derive(Default)]
struct LogState {
    chunks: VecDeque<LogChunk>,
    appended: u64,
    bytes: usize,
    closed: bool,
}

impl LogBuffer {
    fn push(&self, chunk: LogChunk) {
        // Writer side also recovers a poisoned lock: captured output must never be dropped.
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return;
        }
        state.appended += 1;
        state.bytes += chunk.bytes.len();
        state.chunks.push_back(chunk);
        while state.bytes > SERVICE_LOG_CAP_BYTES && state.chunks.len() > 1 {
            if let Some(dropped) = state.chunks.pop_front() {
                state.bytes -= dropped.bytes.len();
            }
        }
        drop(state);
        self.condvar.notify_all();
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.closed = true;
        drop(state);
        self.condvar.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poisoned_log_still_serves_captured_output_and_reports_the_diagnostic() {
        let logs = ServiceLogs::new("api");
        logs.push(StreamSource::Stdout, b"before-panic\n".to_vec());
        let poisoner = logs.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.inner.state.lock().expect("log lock");
            panic!("simulate an engine-thread panic while holding the log lock");
        })
        .join();
        assert!(logs.poisoned(), "the poisoning must be observable");
        assert_eq!(logs.snapshot().len(), 1);
        assert_eq!(
            String::from_utf8_lossy(&logs.snapshot()[0].bytes),
            "before-panic\n"
        );
        logs.push(StreamSource::Stderr, b"after-panic\n".to_vec());
        assert_eq!(logs.snapshot().len(), 2);
        logs.close();
        let mut followed = Vec::new();
        logs.follow(&mut |chunk| followed.push(chunk));
        assert_eq!(followed.len(), 2, "follow must replay the retained window");
    }
}
