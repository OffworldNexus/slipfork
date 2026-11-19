//! Concurrent retained-log fan-in: one reader per service and hook, merged by capture `ts_ms`.
//!
//! A live `slipfork logs -f` must show every writer at once. Each [`LogSource`] is drained by its
//! own thread into one **bounded** channel; the consumer batches whatever has arrived, sorts by
//! `(ts_ms, target, arrival)` and emits, so a slow source never starves a fast one and equal
//! timestamps stay deterministic. The channel is capped by the retention window
//! (`SERVICE_LOG_CAP_BYTES`) and every send is non-blocking: a stalled client aborts the fan-in
//! instead of growing daemon memory without bound.
//!
//! [`follow_bus`] keeps that fan-in alive across `restart` and `down` + `up`: when every source
//! closes, it blocks on the world's [`LogBus`] and re-attaches after the next epoch instead of
//! ending the stream. Each source keeps a cursor keyed by the window's identity token, so a reused
//! hook window emits only lines appended after the previous epoch, while a window minted by a
//! restart replays its own retained output.

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TrySendError};
use std::thread;

use slipfork_edge::EdgeError;
use slipfork_world::{CONTROL_PROTOCOL_VERSION, LogChunk, LogStream, ServiceLogs};

use super::super::types::{HookLog, LogBus};

/// Chunks the fan-in may queue between the reader threads and the client. Derived from the
/// per-service retention (64 KiB); at a ~1 KiB frame this caps in-flight memory at the same order
/// as one retained window, so a client that stops reading can never widen it.
const MAX_IN_FLIGHT: usize = 64;

/// One retained log a follower can attach to: an engine service window or a daemon hook window.
pub(crate) enum LogSource {
    Service(ServiceLogs),
    Hook(Arc<HookLog>),
}

impl Clone for LogSource {
    fn clone(&self) -> Self {
        match self {
            Self::Service(log) => Self::Service(log.clone()),
            Self::Hook(log) => Self::Hook(Arc::clone(log)),
        }
    }
}

impl LogSource {
    /// Identity of the underlying retained window, used as the cursor key.
    fn token(&self) -> Arc<()> {
        match self {
            Self::Service(log) => log.token(),
            Self::Hook(log) => log.token(),
        }
    }

    fn cursor(&self) -> u64 {
        match self {
            Self::Service(log) => log.cursor(),
            Self::Hook(log) => log.cursor(),
        }
    }

    fn snapshot(&self) -> Vec<LogChunk> {
        match self {
            Self::Service(log) => log.snapshot(),
            Self::Hook(log) => log.snapshot(),
        }
    }

    fn follow_from(
        self,
        start: u64,
        keep_going: &dyn Fn() -> bool,
        emit: &mut dyn FnMut(LogChunk) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        match self {
            Self::Service(log) => log.follow_from(start, keep_going, emit),
            Self::Hook(log) => log.follow_from(start, keep_going, emit),
        }
    }
}

/// Per-window cursors. The token is kept alive alongside its cursor so a freshly minted window can
/// never alias an address a dropped one used to occupy.
#[derive(Default)]
struct Cursors(BTreeMap<usize, (Arc<()>, u64)>);

impl Cursors {
    fn start(&self, token: &Arc<()>) -> u64 {
        self.0
            .get(&Self::key(token))
            .map_or(0, |(_, cursor)| *cursor)
    }

    fn record(&mut self, token: Arc<()>, cursor: u64) {
        self.0.insert(Self::key(&token), (token, cursor));
    }

    fn key(token: &Arc<()>) -> usize {
        Arc::as_ptr(token).cast::<()>() as usize
    }
}

/// The cohesive fan-in state: the sources of one epoch, the requested stream, and their cursors.
pub(crate) struct FanIn {
    sources: Vec<LogSource>,
    stderr_only: bool,
    cursors: Cursors,
}

impl FanIn {
    pub(crate) fn new(sources: Vec<LogSource>, stderr_only: bool) -> Self {
        Self {
            sources,
            stderr_only,
            cursors: Cursors::default(),
        }
    }

    /// Swap in the world's current source set, keeping every cursor already recorded.
    pub(super) fn reattach(&mut self, sources: Vec<LogSource>) {
        self.sources = sources;
    }

    pub(super) fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    fn wanted(&self, chunk: &LogChunk) -> bool {
        !self.stderr_only || chunk.stream == LogStream::Stderr
    }

    /// Snapshot every source once, keep the requested stream, and merge chronologically
    /// (stable: equal `ts_ms` keeps target order, then collection order).
    pub(super) fn merged_snapshot(&self) -> Vec<LogChunk> {
        let mut chunks: Vec<LogChunk> = self
            .sources
            .iter()
            .flat_map(LogSource::snapshot)
            .filter(|chunk| self.wanted(chunk))
            .collect();
        chunks.sort_by(|a, b| (a.ts_ms, a.target.as_str()).cmp(&(b.ts_ms, b.target.as_str())));
        chunks
    }

    /// Keep only the last `lines` lines of each target, preserving each line's own metadata.
    ///
    /// The returned chunks are chronological like [`Self::merged_snapshot`]. The cursors are then
    /// advanced past everything currently retained, so `-f -n <N>` seeds history exactly once and
    /// the live follow resumes with new lines only.
    pub(super) fn tail(&mut self, lines: usize) -> Vec<LogChunk> {
        let mut tailed = tail(self.merged_snapshot(), lines);
        tailed.sort_by(|a, b| (a.ts_ms, a.target.as_str()).cmp(&(b.ts_ms, b.target.as_str())));
        self.seed();
        tailed
    }

    fn seed(&mut self) {
        for source in &self.sources {
            self.cursors.record(source.token(), source.cursor());
        }
    }

    /// Follow every source concurrently until all close, the peer disconnects, or a send overflows.
    pub(crate) fn follow(
        &mut self,
        aborted: &AtomicBool,
        emit: &mut dyn FnMut(LogChunk) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        if self.sources.is_empty() {
            return ControlFlow::Continue(());
        }
        let (sender, receiver) = mpsc::sync_channel::<LogChunk>(MAX_IN_FLIGHT);
        let stop = Arc::new(AtomicBool::new(false));
        let mut readers = Vec::with_capacity(self.sources.len());
        for source in std::mem::take(&mut self.sources) {
            let token = source.token();
            let start = self.cursors.start(&token);
            let sender = sender.clone();
            let stop = Arc::clone(&stop);
            let reader = source.clone();
            let handle = thread::spawn(move || {
                let keep_going = || !stop.load(Ordering::Relaxed);
                let _ = reader.follow_from(start, &keep_going, &mut |chunk| {
                    match sender.try_send(chunk) {
                        Ok(()) => ControlFlow::Continue(()),
                        // A full queue means the client stalled; a closed one means the consumer
                        // is gone. Either way abort instead of buffering without bound.
                        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                            stop.store(true, Ordering::Relaxed);
                            ControlFlow::Break(())
                        }
                    }
                });
            });
            readers.push((token, source, handle));
        }
        drop(sender);

        let outcome = self.drain(&receiver, &stop, aborted, emit);
        stop.store(true, Ordering::Relaxed);
        drop(receiver);
        for (token, source, handle) in readers {
            let _ = handle.join();
            self.cursors.record(token, source.cursor());
        }
        outcome
    }

    fn drain(
        &self,
        receiver: &mpsc::Receiver<LogChunk>,
        stop: &AtomicBool,
        aborted: &AtomicBool,
        emit: &mut dyn FnMut(LogChunk) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        let mut pending: Vec<(u64, LogChunk)> = Vec::new();
        let mut arrival = 0_u64;
        'consume: loop {
            if aborted.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
                return ControlFlow::Break(());
            }
            let Ok(first) = receiver.recv() else {
                return ControlFlow::Continue(());
            };
            pending.push((arrival, first));
            arrival += 1;
            while let Ok(chunk) = receiver.try_recv() {
                pending.push((arrival, chunk));
                arrival += 1;
            }
            pending.sort_by(|(left_seq, left), (right_seq, right)| {
                (left.ts_ms, left.target.as_str(), *left_seq).cmp(&(
                    right.ts_ms,
                    right.target.as_str(),
                    *right_seq,
                ))
            });
            for (_, chunk) in pending.drain(..) {
                if !self.wanted(&chunk) {
                    continue;
                }
                if emit(chunk).is_break() {
                    break 'consume ControlFlow::Break(());
                }
            }
        }
    }
}

/// Keep only the last `lines` lines of each target, preserving each line's own `(ts_ms, stream)`.
fn tail(chunks: Vec<LogChunk>, lines: usize) -> Vec<LogChunk> {
    let mut by_target: BTreeMap<String, Vec<(Vec<u8>, u64, LogStream)>> = BTreeMap::new();
    for chunk in chunks {
        let entry = by_target.entry(chunk.target.clone()).or_default();
        let mut current: Vec<u8> = Vec::new();
        for &byte in &chunk.bytes {
            current.push(byte);
            if byte == b'\n' {
                entry.push((std::mem::take(&mut current), chunk.ts_ms, chunk.stream));
            }
        }
        if !current.is_empty() {
            entry.push((current, chunk.ts_ms, chunk.stream));
        }
    }
    by_target
        .into_iter()
        .flat_map(|(target, lines_of_target)| {
            let start = lines_of_target.len().saturating_sub(lines);
            lines_of_target
                .into_iter()
                .skip(start)
                .map(move |(bytes, ts_ms, stream)| LogChunk {
                    protocol: CONTROL_PROTOCOL_VERSION,
                    ts_ms,
                    target: target.clone(),
                    stream,
                    bytes,
                })
        })
        .collect()
}

/// Follow a world across restart epochs: fan-in, then wait for the next boot divider and re-attach.
///
/// `resolve` runs **after** the daemon has inserted the new world record and published its epoch,
/// so there is no re-resolve race window. `aborted` is the edge server's disconnect signal: it is
/// polled through `wait_timeout` so an abandoned `logs -f` reaps its reader threads within one poll.
pub(super) fn follow_bus(
    bus: &LogBus,
    mut resolve: impl FnMut() -> Result<Vec<LogSource>, EdgeError>,
    stderr_only: bool,
    lines: Option<usize>,
    aborted: &AtomicBool,
    emit: &mut dyn FnMut(LogChunk) -> ControlFlow<()>,
) -> Result<(), EdgeError> {
    let mut seen = bus.mark();
    let mut fan_in: Option<FanIn> = None;
    let mut seed = lines;
    loop {
        let sources = resolve()?;
        let mut fan = match fan_in.take() {
            Some(mut fan) => {
                fan.reattach(sources);
                fan
            }
            None => FanIn::new(sources, stderr_only),
        };
        if let Some(lines) = seed.take() {
            for chunk in fan.tail(lines) {
                if emit(chunk).is_break() {
                    return Ok(());
                }
            }
        }
        if !fan.is_empty() && fan.follow(aborted, emit).is_break() {
            return Ok(());
        }
        fan_in = Some(fan);
        let keep_going = || !aborted.load(Ordering::Relaxed);
        let Some((next_seen, dividers)) = bus.wait_past(seen, &keep_going) else {
            return Ok(());
        };
        seen = next_seen;
        for ts_ms in dividers {
            if emit(LogChunk::divider(ts_ms)).is_break() {
                return Ok(());
            }
        }
    }
}
