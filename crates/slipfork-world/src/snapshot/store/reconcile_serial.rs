//! Serial reconcile: one caller thread, observer-driven rows, the same store core and invariants.
//!
//! The image builder needs to apply a step, stream `RUN` child lines, and publish live rows through
//! one borrowed log without `Send` bounds or interior mutation across threads. This driver wraps
//! [`super::reconcile::NodeOutcome`] from the shared core with observer transitions; it never
//! re-implements gate, seal, record, exchange, or singleflight rules.
//!
//! Every tail phase (clone, seal, record, publish) is announced through [`Observed::Tail`] *before*
//! the store performs it, so a slow Btrfs commit is on the caller's screen while it runs. The same
//! announcements are timed here, and [`Observed::Built`] carries the real per-phase split.

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::snapshot::{
    BuildGraph, Cache, GC_GRACE_MS, Node, PhaseTiming, ReconcileFailed, Reconciled, SnapshotError,
    SnapshotResult, TailPhase,
};

use super::reconcile::NodeOutcome;
use super::{Facts, SnapshotStore};

/// One serial reconcile transition, delivered to the caller's observer before it returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observed {
    /// A sealed node was reused unchanged.
    Reused,
    /// The node is about to be materialized into its slot.
    Building,
    /// One tail phase is about to run; announce it before the syscall so it is never a silent gap.
    Tail {
        /// The phase that is starting now.
        phase: TailPhase,
    },
    /// The node was materialized, sealed and recorded.
    Built {
        /// The measured split of the whole node, including every tail phase.
        timing: PhaseTiming,
    },
    /// The materializer failed; the slot was destroyed.
    Failed {
        /// Wall time spent before the failure.
        elapsed: Duration,
    },
}

impl SnapshotStore<'_> {
    /// Reconciles `graph` on the caller thread, reporting every transition to `observer`.
    ///
    /// `Cache::Warm` reuses sealed recorded nodes; `Cache::Cold` rebuilds every materialized node
    /// and exchanges same-key twins. `built_keys`/`reused_keys` list sealed nodes only. A key is
    /// built at most once per call through the same per-key gate as the parallel reconcile.
    pub fn reconcile_serial<O, F>(
        &self,
        graph: &BuildGraph,
        cache: Cache,
        now_ms: u64,
        observer: O,
        mut materialize: F,
    ) -> std::result::Result<Reconciled, ReconcileFailed>
    where
        O: FnMut(&Node, Observed),
        F: FnMut(&Node, &Path, &mut dyn FnMut(&str)) -> SnapshotResult<Facts>,
    {
        if let Err(source) = fs::create_dir_all(&self.root) {
            return Err(failed(
                graph.head(),
                0,
                SnapshotError::io("create", &self.root, source),
                0,
                0,
            ));
        }
        let reachable = graph.closure(&graph.head());
        let pin = super::Pin::new(graph.head(), reachable.clone());
        let _lock = self
            .lock_shared()
            .map_err(|cause| failed(graph.head(), 0, cause, 0, 0))?;
        let mut built_keys = Vec::new();
        let mut reused_keys = Vec::new();
        let observer = RefCell::new(observer);
        for wave in graph.waves() {
            for key in wave.iter().filter(|key| reachable.contains(*key)) {
                let Some(node) = graph.node(key) else {
                    continue;
                };
                let outcome = {
                    let clock = TailClock::default();
                    let mut shim = |node: &Node, target: &Path, child: &mut dyn FnMut(&str)| {
                        clock.begin_exec();
                        (observer.borrow_mut())(node, Observed::Building);
                        let started = Instant::now();
                        let result = materialize(node, target, child);
                        if result.is_err() {
                            (observer.borrow_mut())(
                                node,
                                Observed::Failed {
                                    elapsed: started.elapsed(),
                                },
                            );
                        }
                        result
                    };
                    let mut tail = |phase: TailPhase| {
                        clock.mark(phase);
                        (observer.borrow_mut())(node, Observed::Tail { phase });
                    };
                    // `process_node` owns the per-key gate and its `forget_gate`.
                    let outcome = self.process_node(
                        node,
                        cache,
                        now_ms,
                        &mut shim,
                        &mut |_: &str| {},
                        &mut tail,
                    );
                    if let Ok(settled) = &outcome {
                        let timing = clock.finish();
                        match settled {
                            NodeOutcome::SealedReused => {
                                (observer.borrow_mut())(node, Observed::Reused);
                            }
                            NodeOutcome::SealedBuilt | NodeOutcome::RecordedBuilt => {
                                (observer.borrow_mut())(node, Observed::Built { timing });
                            }
                            NodeOutcome::RecordedReused => {}
                        }
                    }
                    outcome
                };
                match outcome {
                    Ok(NodeOutcome::SealedBuilt) => built_keys.push(*key),
                    Ok(NodeOutcome::SealedReused) => reused_keys.push(*key),
                    Ok(NodeOutcome::RecordedBuilt | NodeOutcome::RecordedReused) => {}
                    Err(cause) => {
                        return Err(failed(
                            node.key,
                            node.line,
                            cause,
                            built_keys.len(),
                            reused_keys.len(),
                        ));
                    }
                }
            }
        }
        drop(_lock);
        let gc = self
            .gc(now_ms, GC_GRACE_MS, std::slice::from_ref(&pin))
            .map_err(|cause| failed(graph.head(), 0, cause, built_keys.len(), reused_keys.len()))?;
        Ok(Reconciled {
            head: graph.head(),
            built: built_keys.len(),
            reused: reused_keys.len(),
            pruned: gc.pruned(),
            pin,
            built_keys,
            reused_keys,
        })
    }
}

/// Times the store's phase announcements into one [`PhaseTiming`].
///
/// `begin_exec` closes the clone phase (the slot exists when the materializer starts) and every
/// `mark` closes the previous phase, so the caller never has to re-derive phase boundaries.
#[derive(Debug, Default)]
struct TailClock {
    timing: Cell<PhaseTiming>,
    phase: Cell<Option<(TailPhase, Instant)>>,
    exec: Cell<Option<Instant>>,
}

impl TailClock {
    fn begin_exec(&self) {
        self.begin_exec_at(Instant::now());
    }

    fn begin_exec_at(&self, at: Instant) {
        self.close_phase_at(at);
        self.exec.set(Some(at));
    }

    fn mark(&self, phase: TailPhase) {
        self.mark_at(phase, Instant::now());
    }

    fn mark_at(&self, phase: TailPhase, at: Instant) {
        self.close_phase_at(at);
        self.close_exec_at(at);
        self.phase.set(Some((phase, at)));
    }

    fn finish(&self) -> PhaseTiming {
        self.finish_at(Instant::now())
    }

    fn finish_at(&self, at: Instant) -> PhaseTiming {
        self.close_phase_at(at);
        self.close_exec_at(at);
        self.timing.get()
    }

    fn close_phase_at(&self, at: Instant) {
        if let Some((phase, started)) = self.phase.take() {
            self.record(phase, at.saturating_duration_since(started));
        }
    }

    fn close_exec_at(&self, at: Instant) {
        if let Some(started) = self.exec.take() {
            self.record(TailPhase::Executing, at.saturating_duration_since(started));
        }
    }

    fn record(&self, phase: TailPhase, elapsed: Duration) {
        let mut timing = self.timing.get();
        timing.add(phase, elapsed);
        self.timing.set(timing);
    }
}

fn failed(
    node: crate::snapshot::NodeKey,
    line: u32,
    cause: SnapshotError,
    built: usize,
    reused: usize,
) -> ReconcileFailed {
    ReconcileFailed {
        node,
        line,
        cause: Box::new(cause),
        built,
        reused,
    }
}

#[cfg(test)]
mod tests {
    use super::TailClock;
    use crate::snapshot::{PhaseTiming, TailPhase};
    use std::time::{Duration, Instant};

    #[test]
    fn tail_clock_splits_a_settled_node_by_phase() {
        let base = Instant::now();
        let clock = TailClock::default();
        clock.mark_at(TailPhase::Cloning, base);
        clock.begin_exec_at(base + Duration::from_secs(2));
        clock.mark_at(TailPhase::Sealing, base + Duration::from_secs(5));
        clock.mark_at(TailPhase::Recording, base + Duration::from_secs(7));
        clock.mark_at(TailPhase::Publishing, base + Duration::from_millis(7010));
        let timing = clock.finish_at(base + Duration::from_millis(7015));
        assert_eq!(
            timing,
            PhaseTiming {
                clone: Duration::from_secs(2),
                exec: Duration::from_secs(3),
                seal: Duration::from_secs(2),
                record: Duration::from_millis(10),
                publish: Duration::from_millis(5),
            }
        );
        assert_eq!(timing.total(), Duration::from_millis(7015));
    }

    #[test]
    fn record_only_nodes_measure_exec_and_leave_the_tail_zero() {
        let base = Instant::now();
        let clock = TailClock::default();
        clock.begin_exec_at(base);
        let timing = clock.finish_at(base + Duration::from_millis(150));
        assert_eq!(
            timing,
            PhaseTiming {
                exec: Duration::from_millis(150),
                ..PhaseTiming::default()
            }
        );
    }
}
