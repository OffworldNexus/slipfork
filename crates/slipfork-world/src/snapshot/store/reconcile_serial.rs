//! Serial reconcile: one caller thread, observer-driven rows, the same store core and invariants.
//!
//! The image builder needs to apply a step, stream `RUN` child lines, and publish live rows through
//! one borrowed log without `Send` bounds or interior mutation across threads. This driver wraps
//! [`super::reconcile::NodeOutcome`] from the shared core with observer transitions; it never
//! re-implements gate, seal, record, exchange, or singleflight rules.

use std::cell::Cell;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::snapshot::{
    BuildGraph, Cache, GC_GRACE_MS, Node, ReconcileFailed, Reconciled, SnapshotError,
    SnapshotResult,
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
    /// The node was materialized, sealed and recorded.
    Built {
        /// Wall time the materializer spent.
        elapsed: Duration,
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
        mut observer: O,
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
        for wave in graph.waves() {
            for key in wave.iter().filter(|key| reachable.contains(*key)) {
                let Some(node) = graph.node(key) else {
                    continue;
                };
                let outcome = {
                    let elapsed = Cell::new(Duration::ZERO);
                    let mut shim = |node: &Node, target: &Path, child: &mut dyn FnMut(&str)| {
                        observer(node, Observed::Building);
                        let started = Instant::now();
                        let result = materialize(node, target, child);
                        elapsed.set(started.elapsed());
                        if result.is_err() {
                            observer(
                                node,
                                Observed::Failed {
                                    elapsed: elapsed.get(),
                                },
                            );
                        }
                        result
                    };
                    // `process_node` owns the per-key gate and its `forget_gate`.
                    let outcome =
                        self.process_node(node, cache, now_ms, &mut shim, &mut |_: &str| {});
                    drop(shim);
                    if let Ok(settled) = &outcome {
                        match settled {
                            NodeOutcome::SealedReused => observer(node, Observed::Reused),
                            NodeOutcome::SealedBuilt | NodeOutcome::RecordedBuilt => observer(
                                node,
                                Observed::Built {
                                    elapsed: elapsed.get(),
                                },
                            ),
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
