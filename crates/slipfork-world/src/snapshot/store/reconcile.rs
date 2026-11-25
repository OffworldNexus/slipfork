//! Reconcile waves: per-key singleflight, sealed slots, record-only nodes, drained progress.
//!
//! [`SnapshotStore::process_node`] is the one reconcile core shared by the parallel wave driver
//! here and the caller-thread [`super::reconcile_serial`] driver. Both hold the same per-key gate,
//! seal/record/exchange rules, and singleflight invariants.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use crate::snapshot::{
    BuildGraph, Cache, GC_GRACE_MS, Node, NodeKey, Parallelism, Pin, ReconcileFailed, Reconciled,
    SnapshotError, SnapshotResult, SnapshotSlot, TailPhase,
};

use super::{Facts, NodeRecord, RECORD_VERSION, SnapshotStore};

/// How one node settled. Sealed outcomes count as built/reused; record-only outcomes never do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeOutcome {
    /// A sealed subvolume was materialized, sealed and recorded.
    SealedBuilt,
    /// A sealed subvolume and its record were reused.
    SealedReused,
    /// A record-only node was materialized and recorded.
    RecordedBuilt,
    /// A record-only node's existing record was reused.
    RecordedReused,
}

/// `(node, line, cause, sealed built, sealed reused)` after a wave failure.
type WaveFailure = (NodeKey, u32, Box<SnapshotError>, usize, usize);

type WaveResult = Result<(Vec<NodeKey>, Vec<NodeKey>), WaveFailure>;

/// The materializer shim both drivers hand the core.
pub(crate) type Materialize<'a> =
    dyn FnMut(&Node, &Path, &mut dyn FnMut(&str)) -> SnapshotResult<Facts> + 'a;

/// Immutable per-call knobs for one wave pass.
struct Plan {
    cache: Cache,
    parallelism: Parallelism,
    now_ms: u64,
}

impl SnapshotStore<'_> {
    /// Reconciles `graph` into sealed and record-only nodes, then GCs with a 24h grace.
    ///
    /// Every node in the head's parent closure is processed in wave order. Materialized kinds
    /// (base/external/step/seed) clone a parent into a slot, materialize, seal, record and rename;
    /// record-only kinds (image/effect/service/world) call `materialize(node,
    /// parent_materialized_dir_or_snapshots_root, sink)` and write `<key>.json` with no slot, so
    /// gc can walk a leased image's record down to its step/base chain. `built`/`reused` count
    /// sealed nodes only. A key is built at most once per call (per-key gate + double-check).
    pub fn reconcile<F>(
        &self,
        graph: &BuildGraph,
        cache: Cache,
        parallelism: Parallelism,
        now_ms: u64,
        progress: &mut (dyn FnMut(&str) + Send),
        materialize: F,
    ) -> std::result::Result<Reconciled, ReconcileFailed>
    where
        F: Fn(&Node, &Path, &mut dyn FnMut(&str)) -> SnapshotResult<Facts> + Send + Sync,
    {
        if let Err(source) = fs::create_dir_all(&self.root) {
            return Err(ReconcileFailed {
                node: graph.head(),
                line: 0,
                cause: Box::new(SnapshotError::io("create", &self.root, source)),
                built: 0,
                reused: 0,
            });
        }
        let reachable = graph.closure(&graph.head());
        let pin = Pin::new(graph.head(), reachable.clone());
        let (tx, rx) = mpsc::channel::<(NodeKey, String)>();
        let waves = std::thread::scope(|outer| {
            let drainer = outer.spawn(move || {
                while let Ok((key, line)) = rx.recv() {
                    let hex = key.as_hex();
                    progress(&format!("{} {line}", &hex[..12]));
                }
            });
            let result = self.reconcile_waves(
                graph,
                &Plan {
                    cache,
                    parallelism,
                    now_ms,
                },
                &reachable,
                &materialize,
                &tx,
            );
            drop(tx);
            let _ = drainer.join();
            result
        });
        let (built_keys, reused_keys) = match waves {
            Ok(keys) => keys,
            Err((node, line, cause, built, reused)) => {
                return Err(ReconcileFailed {
                    node,
                    line,
                    cause,
                    built,
                    reused,
                });
            }
        };
        let outcome = self
            .gc(now_ms, GC_GRACE_MS, std::slice::from_ref(&pin))
            .map_err(|cause| ReconcileFailed {
                node: graph.head(),
                line: 0,
                cause: Box::new(cause),
                built: built_keys.len(),
                reused: reused_keys.len(),
            })?;
        Ok(Reconciled {
            head: graph.head(),
            built: built_keys.len(),
            reused: reused_keys.len(),
            pruned: outcome.pruned(),
            pin,
            built_keys,
            reused_keys,
        })
    }

    fn reconcile_waves<F>(
        &self,
        graph: &BuildGraph,
        plan: &Plan,
        reachable: &BTreeSet<NodeKey>,
        materialize: &F,
        tx: &Sender<(NodeKey, String)>,
    ) -> WaveResult
    where
        F: Fn(&Node, &Path, &mut dyn FnMut(&str)) -> SnapshotResult<Facts> + Send + Sync,
    {
        let _lock = self
            .lock_shared()
            .map_err(|cause| (graph.head(), 0u32, Box::new(cause), 0usize, 0usize))?;
        let mut built = Vec::new();
        let mut reused = Vec::new();
        for wave in graph.waves() {
            let nodes: Vec<&Node> = wave
                .iter()
                .filter(|key| reachable.contains(*key))
                .filter_map(|key| graph.node(key))
                .collect();
            for chunk in nodes.chunks(plan.parallelism.get()) {
                let outcomes = std::thread::scope(|scope| {
                    let handles: Vec<_> = chunk
                        .iter()
                        .map(|node| {
                            let node = *node;
                            scope.spawn(move || {
                                let mut sink = |line: &str| {
                                    let _ = tx.send((node.key, line.to_owned()));
                                };
                                let mut shim =
                                    |node: &Node, path: &Path, child: &mut dyn FnMut(&str)| {
                                        materialize(node, path, child)
                                    };
                                self.process_node(
                                    node,
                                    plan.cache,
                                    plan.now_ms,
                                    &mut shim,
                                    &mut sink,
                                    &mut |_: TailPhase| {},
                                )
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .map(|handle| {
                            handle.join().unwrap_or_else(|_| {
                                Err(SnapshotError::message(
                                    "a reconcile worker panicked\nnext: re-run `slipfork up`; sealed nodes are reused",
                                ))
                            })
                        })
                        .collect::<Vec<_>>()
                });
                for (node, outcome) in chunk.iter().zip(outcomes) {
                    match outcome {
                        Ok(NodeOutcome::SealedBuilt) => built.push(node.key),
                        Ok(NodeOutcome::SealedReused) => reused.push(node.key),
                        Ok(NodeOutcome::RecordedBuilt | NodeOutcome::RecordedReused) => {}
                        Err(cause) => {
                            return Err((
                                node.key,
                                node.line,
                                Box::new(cause),
                                built.len(),
                                reused.len(),
                            ));
                        }
                    }
                }
            }
        }
        Ok((built, reused))
    }

    /// The one reconcile core, called by the parallel and serial drivers under a per-key gate.
    ///
    /// `tail` receives each post-create phase *before* the store starts it, so the serial driver can
    /// paint a slow seal/publish while it runs; the parallel wave driver passes a no-op.
    pub(crate) fn process_node(
        &self,
        node: &Node,
        cache: Cache,
        now_ms: u64,
        materialize: &mut Materialize<'_>,
        sink: &mut dyn FnMut(&str),
        tail: &mut dyn FnMut(TailPhase),
    ) -> SnapshotResult<NodeOutcome> {
        let gate = self.gate(&node.key);
        let outcome = {
            let _held = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            self.process_locked(node, cache, now_ms, materialize, sink, tail)
        };
        drop(gate);
        self.forget_gate(&node.key);
        outcome
    }

    fn process_locked(
        &self,
        node: &Node,
        cache: Cache,
        now_ms: u64,
        materialize: &mut Materialize<'_>,
        sink: &mut dyn FnMut(&str),
        tail: &mut dyn FnMut(TailPhase),
    ) -> SnapshotResult<NodeOutcome> {
        let dir = self.node_dir(&node.key);
        let record = self.read_record(&node.key).ok().flatten();
        if !node.kind.is_materialized() {
            if cache == Cache::Warm
                && let Some(mut record) = record
            {
                record.last_used_ms = now_ms;
                self.write_record(&node.key, &record)?;
                return Ok(NodeOutcome::RecordedReused);
            }
            let target = node
                .parents
                .iter()
                .map(|parent| self.node_dir(parent))
                .find(|path| path.is_dir())
                .unwrap_or_else(|| self.root.clone());
            let facts = materialize(node, &target, &mut *sink)?;
            let record = NodeRecord {
                v: RECORD_VERSION,
                kind: node.kind,
                parents: node.parents.clone(),
                canon: node.canon.clone(),
                line: node.line,
                created_ms: now_ms,
                last_used_ms: now_ms,
                facts,
            };
            self.write_record(&node.key, &record)?;
            return Ok(NodeOutcome::RecordedBuilt);
        }
        let sealed = dir.is_dir() && self.engine.is_sealed(&dir).unwrap_or(false);
        if cache == Cache::Warm
            && sealed
            && let Some(mut record) = record
        {
            record.last_used_ms = now_ms;
            self.write_record(&node.key, &record)?;
            return Ok(NodeOutcome::SealedReused);
        }
        let exchange = cache == Cache::Cold && sealed;
        if !exchange {
            self.destroy_path(&dir)?;
            let record_path = self.record_path(&node.key);
            if record_path.exists() {
                fs::remove_file(&record_path)
                    .map_err(|source| SnapshotError::io("remove", &record_path, source))?;
            }
        }
        let parent = node
            .parents
            .iter()
            .map(|parent| self.node_dir(parent))
            .find(|path| path.is_dir() && self.engine.is_sealed(path).unwrap_or(false));
        let mut slot = {
            tail(TailPhase::Cloning);
            SnapshotSlot::create(
                self.engine,
                &self.root,
                node.key,
                self.next_ordinal(),
                parent.as_deref(),
            )?
        };
        let published = (|| -> SnapshotResult<()> {
            let facts = materialize(node, slot.path(), &mut *sink)?;
            tail(TailPhase::Sealing);
            slot.seal()?;
            let record = NodeRecord {
                v: RECORD_VERSION,
                kind: node.kind,
                parents: node.parents.clone(),
                canon: node.canon.clone(),
                line: node.line,
                created_ms: now_ms,
                last_used_ms: now_ms,
                facts,
            };
            tail(TailPhase::Recording);
            self.write_record(&node.key, &record)?;
            tail(TailPhase::Publishing);
            if exchange {
                slot.publish_exchange()?;
            } else {
                slot.publish()?;
            }
            Ok(())
        })();
        if let Err(error) = published {
            slot.abandon();
            return Err(error);
        }
        Ok(NodeOutcome::SealedBuilt)
    }

    pub(crate) fn gate(&self, key: &NodeKey) -> Arc<Mutex<()>> {
        let mut gates = self
            .gates
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        gates
            .entry(*key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub(crate) fn forget_gate(&self, key: &NodeKey) {
        let mut gates = self
            .gates
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if gates
            .get(key)
            .is_some_and(|gate| Arc::strong_count(gate) == 1)
        {
            gates.remove(key);
        }
    }
}
