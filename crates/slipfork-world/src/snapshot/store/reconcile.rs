//! Reconcile waves: per-key singleflight, sealed slots, record-only nodes, drained progress.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use crate::snapshot::{
    BuildGraph, Cache, GC_GRACE_MS, Node, NodeKey, Parallelism, Pin, ReconcileFailed, Reconciled,
    SnapshotError, SnapshotResult, SnapshotSlot,
};

use super::{Facts, NodeRecord, RECORD_VERSION, SnapshotStore};

enum Outcome {
    SealedBuilt,
    SealedReused,
    Recorded,
}

/// `(node, line, cause, sealed built, sealed reused)` after a wave failure.
type WaveFailure = (NodeKey, u32, Box<SnapshotError>, usize, usize);

type WaveResult = Result<(usize, usize), WaveFailure>;

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
        let (built, reused) = match waves {
            Ok(counts) => counts,
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
                built,
                reused,
            })?;
        Ok(Reconciled {
            head: graph.head(),
            built,
            reused,
            pruned: outcome.pruned(),
            pin,
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
        let mut built = 0usize;
        let mut reused = 0usize;
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
                                self.process_node(node, plan.cache, plan.now_ms, materialize, tx)
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
                        Ok(Outcome::SealedBuilt) => built += 1,
                        Ok(Outcome::SealedReused) => reused += 1,
                        Ok(Outcome::Recorded) => {}
                        Err(cause) => {
                            return Err((node.key, node.line, Box::new(cause), built, reused));
                        }
                    }
                }
            }
        }
        Ok((built, reused))
    }

    fn process_node<F>(
        &self,
        node: &Node,
        cache: Cache,
        now_ms: u64,
        materialize: &F,
        tx: &Sender<(NodeKey, String)>,
    ) -> SnapshotResult<Outcome>
    where
        F: Fn(&Node, &Path, &mut dyn FnMut(&str)) -> SnapshotResult<Facts> + Sync,
    {
        let gate = self.gate(&node.key);
        let outcome = {
            let _held = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            self.process_locked(node, cache, now_ms, materialize, tx)
        };
        drop(gate);
        self.forget_gate(&node.key);
        outcome
    }

    fn process_locked<F>(
        &self,
        node: &Node,
        cache: Cache,
        now_ms: u64,
        materialize: &F,
        tx: &Sender<(NodeKey, String)>,
    ) -> SnapshotResult<Outcome>
    where
        F: Fn(&Node, &Path, &mut dyn FnMut(&str)) -> SnapshotResult<Facts>,
    {
        let dir = self.node_dir(&node.key);
        let record = self.read_record(&node.key).ok().flatten();
        if !node.kind.is_materialized() {
            if cache == Cache::Warm
                && let Some(mut record) = record
            {
                record.last_used_ms = now_ms;
                self.write_record(&node.key, &record)?;
                return Ok(Outcome::Recorded);
            }
            let target = node
                .parents
                .iter()
                .map(|parent| self.node_dir(parent))
                .find(|path| path.is_dir())
                .unwrap_or_else(|| self.root.clone());
            let mut sink = |line: &str| {
                let _ = tx.send((node.key, line.to_owned()));
            };
            let facts = materialize(node, &target, &mut sink)?;
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
            return Ok(Outcome::Recorded);
        }
        let sealed = dir.is_dir() && self.engine.is_sealed(&dir).unwrap_or(false);
        if cache == Cache::Warm
            && sealed
            && let Some(mut record) = record
        {
            record.last_used_ms = now_ms;
            self.write_record(&node.key, &record)?;
            return Ok(Outcome::SealedReused);
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
        let mut slot = SnapshotSlot::create(
            self.engine,
            &self.root,
            node.key,
            self.next_ordinal(),
            parent.as_deref(),
        )?;
        let mut sink = |line: &str| {
            let _ = tx.send((node.key, line.to_owned()));
        };
        let published = (|| -> SnapshotResult<()> {
            let facts = materialize(node, slot.path(), &mut sink)?;
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
            self.write_record(&node.key, &record)?;
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
        Ok(Outcome::SealedBuilt)
    }

    fn gate(&self, key: &NodeKey) -> Arc<Mutex<()>> {
        let mut gates = self
            .gates
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        gates
            .entry(*key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn forget_gate(&self, key: &NodeKey) {
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
