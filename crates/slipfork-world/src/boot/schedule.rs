//! Edge-driven `tokio` scheduling and live boot events for one [`WorldPlan`].
//!
//! Every node is spawned once and awaits exactly its own upstream `watch` edges, so a node starts
//! the instant its in-degree reaches zero — there are no phase barriers. Blocking bodies
//! (Btrfs ioctls, `unshare`, `crun`, git) run on the injected runtime's blocking pool.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tokio::sync::watch;

use super::{BootGraph, BootKind, BootStep, StampStore, WorldPlan};
use crate::error::{Error, Result};

/// The contract state of one boot node, appended to every finished or failed row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootState {
    /// The image was already sealed and no work ran.
    Cached,
    /// The image was resolved from an OCI reference.
    Pulled,
    /// A `Dockerfile` build ran at least one uncached step.
    Built,
    /// A resource, setup step, or service is up.
    Ready,
    /// A conditional step's stamp matched; nothing ran.
    UpToDate,
    /// The node failed; the world stays up for inspection.
    Failed,
}

impl BootState {
    /// The `--json` state string shared by every boot verb.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cached => "cached",
            Self::Pulled => "pulled",
            Self::Built => "built",
            Self::Ready => "ready",
            Self::UpToDate => "up to date",
            Self::Failed => "failed",
        }
    }
}

/// A completed step's contract state plus its human detail (`cached 0.1s`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepOutcome {
    /// Why the node is finished; rendered as the `state` column.
    pub state: BootState,
    /// Short detail appended after the label.
    pub detail: String,
}

/// A step failure carrying the `Forkfile` line of the owning declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootFailure {
    /// One human line; never contains secret values.
    pub message: String,
    /// Concrete next action when the step's `Forkfile` line is not the thing to fix.
    pub remedy: Option<String>,
}

impl BootFailure {
    /// Builds a failure from one human line.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            remedy: None,
        }
    }

    /// Builds a failure whose cure is not "edit this `Forkfile` line".
    #[must_use]
    pub fn with_remedy(message: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            remedy: Some(remedy.into()),
        }
    }
}

/// Executes one ready step; the composition root injects the real implementation.
pub type StepExecutor =
    dyn Fn(usize, &BootStep) -> std::result::Result<StepOutcome, BootFailure> + Send + Sync;

/// Receives live boot events; the CLI renders them, tests collect them.
pub type EventSink = dyn Fn(BootEvent) + Send + Sync;

/// One typed boot event. Every variant carries the bare node `name`; the kind is its own column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootEvent {
    /// A worker started the step.
    Started {
        /// Index into [`WorldPlan::steps`].
        step: usize,
        /// Schedulable step count, so the CLI can print a real `[i/n]` prefix.
        total: usize,
        /// Step kind.
        kind: BootKind,
        /// Bare node name.
        name: String,
    },
    /// The step finished successfully.
    Finished {
        /// Index into [`WorldPlan::steps`].
        step: usize,
        /// Step kind.
        kind: BootKind,
        /// Bare node name.
        name: String,
        /// Wall time of this node's own work; its upstream wait is never included.
        elapsed: Duration,
        /// Contract state (`cached`, `pulled`, `built`, `ready`).
        state: BootState,
        /// Short detail.
        detail: String,
    },
    /// A conditional step's stamp matched; the executor did not run.
    UpToDate {
        /// Index into [`WorldPlan::steps`].
        step: usize,
        /// Step kind.
        kind: BootKind,
        /// Bare node name.
        name: String,
        /// Time from this node's release, including its stamp check.
        elapsed: Duration,
    },
    /// The step failed; the world stays up for inspection.
    Failed {
        /// Index into [`WorldPlan::steps`].
        step: usize,
        /// Step kind.
        kind: BootKind,
        /// Bare node name.
        name: String,
        /// `Forkfile` line of the declaration.
        line: u32,
        /// One human line.
        message: String,
        /// Concrete next action when editing `line` is not the fix.
        remedy: Option<String>,
        /// Wall time of this node's own work up to the failure.
        elapsed: Duration,
    },
}

/// Classifies a service liveness probe: an exit inside the window is a failed service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceHealth {
    /// The process was still alive when the readiness window elapsed.
    Running,
    /// The process exited inside the window with this code.
    Exited(i32),
}

/// Classifies a service liveness probe: an exit inside the window is a failed service.
#[must_use]
pub fn classify_service(exit_within_window: Option<i32>) -> ServiceHealth {
    match exit_within_window {
        Some(code) => ServiceHealth::Exited(code),
        None => ServiceHealth::Running,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeState {
    Pending,
    Done,
    Failed,
}

/// One released node: its identity plus the clock that starts once its upstream edges resolve.
#[derive(Debug, Clone)]
struct NodeTask {
    index: usize,
    total: usize,
    step: BootStep,
    node_start: Instant,
}

/// Reactive executor: one `watch` edge per node, zero phase barriers.
pub struct BootScheduler {
    handle: Handle,
    stamps: StampStore,
    execute: Arc<StepExecutor>,
    sink: Arc<EventSink>,
}

impl BootScheduler {
    /// Builds the scheduler around the single composition-root runtime handle.
    #[must_use]
    pub fn new(
        handle: Handle,
        stamps: StampStore,
        execute: Arc<StepExecutor>,
        sink: Arc<EventSink>,
    ) -> Self {
        Self {
            handle,
            stamps,
            execute,
            sink,
        }
    }

    /// Releases every ready node and blocks until the whole DAG settles or the first node fails.
    ///
    /// A failure cancels every node that has not started yet, emits [`BootEvent::Failed`] for the
    /// lowest failing index, and returns [`Error::Boot`] carrying that step's line.
    pub fn run(self: &Arc<Self>, plan: &WorldPlan) -> Result<()> {
        self.handle.block_on(self.drive(plan))
    }

    async fn drive(self: &Arc<Self>, plan: &WorldPlan) -> Result<()> {
        let graph = BootGraph::new(&plan.steps)?;
        let total = graph.len();
        if total == 0 {
            return Ok(());
        }
        let mut senders = Vec::with_capacity(total);
        let mut receivers = Vec::with_capacity(total);
        for _ in 0..total {
            let (sender, receiver) = watch::channel(NodeState::Pending);
            senders.push(sender);
            receivers.push(receiver);
        }
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let failure: Arc<Mutex<Option<(usize, BootFailure)>>> = Arc::new(Mutex::new(None));
        let mut tasks = Vec::with_capacity(total);

        for index in 0..total {
            let step = plan.steps[index].clone();
            let deps: Vec<watch::Receiver<NodeState>> = graph
                .deps(index)
                .iter()
                .map(|&dep| receivers[dep].clone())
                .collect();
            let state_tx = senders[index].clone();
            let mut cancel = cancel_rx.clone();
            let failure = Arc::clone(&failure);
            let cancel_tx = cancel_tx.clone();
            let scheduler = Arc::clone(self);
            tasks.push(self.handle.spawn(async move {
                for mut edge in deps {
                    loop {
                        match *edge.borrow_and_update() {
                            NodeState::Done => break,
                            NodeState::Failed => return,
                            NodeState::Pending => {}
                        }
                        tokio::select! {
                            changed = edge.changed() => {
                                if changed.is_err() {
                                    return;
                                }
                            }
                            _ = cancel.changed() => return,
                        }
                    }
                }
                if *cancel.borrow() {
                    return;
                }
                // The node's own clock starts here, after its upstream wait, so `elapsed` is this
                // node's work and never a slice of the DAG's total wall time.
                let task = NodeTask {
                    index,
                    total,
                    step,
                    node_start: Instant::now(),
                };
                let worker = Arc::clone(&scheduler);
                let outcome = match scheduler
                    .handle
                    .spawn_blocking(move || worker.run_node(task))
                    .await
                {
                    Ok(outcome) => outcome,
                    Err(_) => Err(BootFailure::new("a boot worker panicked")),
                };
                match outcome {
                    Ok(()) => {
                        let _ = state_tx.send(NodeState::Done);
                    }
                    Err(error) => {
                        if let Ok(mut slot) = failure.lock() {
                            let replace =
                                slot.as_ref().is_none_or(|(existing, _)| index < *existing);
                            if replace {
                                *slot = Some((index, error));
                            }
                        }
                        let _ = cancel_tx.send(true);
                        let _ = state_tx.send(NodeState::Failed);
                    }
                }
            }));
        }

        for task in tasks {
            let _ = task.await;
        }
        let first = failure.lock().ok().and_then(|mut slot| slot.take());
        if let Some((index, error)) = first {
            let step = &plan.steps[index];
            return Err(Error::Boot {
                step: step.label.clone(),
                line: step.line,
                message: error.message,
                remedy: error.remedy,
            });
        }
        Ok(())
    }

    fn run_node(&self, task: NodeTask) -> std::result::Result<(), BootFailure> {
        let NodeTask {
            index,
            total,
            step,
            node_start,
        } = task;
        let name = step.node_name();
        if let Some(stamp) = &step.stamp
            && self.stamps.is_current(stamp)
        {
            (*self.sink)(BootEvent::UpToDate {
                step: index,
                kind: step.kind,
                name,
                elapsed: node_start.elapsed(),
            });
            return Ok(());
        }
        (*self.sink)(BootEvent::Started {
            step: index,
            total,
            kind: step.kind,
            name: name.clone(),
        });
        match (*self.execute)(index, &step) {
            Ok(done) => {
                if let Some(stamp) = &step.stamp
                    && let Err(error) = self.stamps.record(stamp)
                {
                    return Err(BootFailure::new(error.to_string()));
                }
                (*self.sink)(BootEvent::Finished {
                    step: index,
                    kind: step.kind,
                    name,
                    elapsed: node_start.elapsed(),
                    state: done.state,
                    detail: done.detail,
                });
                Ok(())
            }
            Err(error) => {
                (*self.sink)(BootEvent::Failed {
                    step: index,
                    kind: step.kind,
                    name,
                    line: step.line,
                    message: error.message.clone(),
                    remedy: error.remedy.clone(),
                    elapsed: node_start.elapsed(),
                });
                Err(error)
            }
        }
    }
}
