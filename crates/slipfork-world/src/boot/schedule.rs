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
    /// Minted run id when the step actually executed work; `None` for a cache hit.
    pub run_id: Option<String>,
}

/// A step failure carrying the `Forkfile` line of the owning declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootFailure {
    /// One human line; never contains secret values.
    pub message: String,
    /// Typed child exit code when the step's source knows it; never re-parsed from `message`.
    pub exit_code: Option<i32>,
    /// Concrete next action when the step's `Forkfile` line is not the thing to fix.
    pub remedy: Option<String>,
    /// Run id of the step's capture, so the failure line can name `slipfork logs -r <id>`.
    pub run_id: Option<String>,
}

impl BootFailure {
    /// Builds a failure from one human line.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: None,
            remedy: None,
            run_id: None,
        }
    }

    /// Builds a failure whose cure is not "edit this `Forkfile` line".
    #[must_use]
    pub fn with_remedy(message: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: None,
            remedy: Some(remedy.into()),
            run_id: None,
        }
    }

    /// Attaches the child exit code the caller's process status carried.
    #[must_use]
    pub fn with_exit_code(mut self, code: i32) -> Self {
        self.exit_code = Some(code);
        self
    }
}

/// Executes one ready step; the composition root injects the real implementation.
pub type StepExecutor =
    dyn Fn(usize, &BootStep) -> std::result::Result<StepOutcome, BootFailure> + Send + Sync;

/// Receives live boot events; the CLI renders them, tests collect them.
pub type EventSink = dyn Fn(BootEvent) + Send + Sync;

/// One announced boot row: the kind column plus the bare node name.
///
/// The CLI measures every column from this complete list before it prints row 1, so a long name or
/// a later `up to date` state can never shift a row that is already on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRow {
    /// What kind of work this row performs.
    pub kind: BootKind,
    /// Bare node name, exactly as the matching `Started`/`Finished` frames spell it.
    pub name: String,
}

/// One typed boot event. Every variant carries the bare node `name`; the kind is its own column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootEvent {
    /// The complete plan, emitted once before the first [`BootEvent::Started`].
    Plan {
        /// Every step in the plan, in declaration order.
        rows: Vec<PlanRow>,
    },
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
        /// Minted run id; `None` while the step is still running.
        run_id: Option<String>,
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
        /// Id of the step's run capture, when it executed work.
        run_id: Option<String>,
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
        /// Id of the step's run capture; the failure line replays it.
        run_id: Option<String>,
        /// The command the step ran, when it has one.
        command: Option<String>,
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
    steps: Arc<[BootStep]>,
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
        let graph = BootGraph::with_gated(&plan.steps, &plan.gated)?;
        let total = graph.len();
        if total == 0 {
            return Ok(());
        }
        // The plan frame is the very first event: the CLI latches every column width from it, so
        // no later name or state can move a row that is already printed.
        (*self.sink)(BootEvent::Plan {
            rows: plan
                .steps
                .iter()
                .map(|step| PlanRow {
                    kind: step.kind,
                    name: step.node_name(),
                })
                .collect(),
        });
        let steps: Arc<[BootStep]> = plan.steps.clone().into();
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

        for (index, state_tx) in senders.into_iter().enumerate() {
            let steps = Arc::clone(&steps);
            let deps: Vec<watch::Receiver<NodeState>> = graph
                .deps(index)
                .iter()
                .map(|&dep| receivers[dep].clone())
                .collect();
            let gated: Vec<watch::Receiver<NodeState>> = graph
                .gated_deps(index)
                .iter()
                .map(|&dep| receivers[dep].clone())
                .collect();
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
                // Stamp-gated edges (`pre-deploy` -> `pg`/`redis`/`s3`) are awaited only while the
                // node's own stamp is stale (`specs/03` §3 rule 2). On a current epoch the node is
                // released — and every service behind it — in 0ms without touching the resources.
                if !scheduler.stamps.is_step_current(&steps, index) {
                    for mut edge in gated {
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
                }
                if *cancel.borrow() {
                    return;
                }
                // The node's own clock starts here, after its upstream wait, so `elapsed` is this
                // node's work and never a slice of the DAG's total wall time.
                let task = NodeTask {
                    index,
                    total,
                    steps,
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
            steps,
            node_start,
        } = task;
        let step = &steps[index];
        let name = step.node_name();
        if self.stamps.is_step_current(&steps, index) {
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
            run_id: None,
        });
        match (*self.execute)(index, step) {
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
                    run_id: done.run_id,
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
                    run_id: error.run_id.clone(),
                    command: step.command.clone(),
                });
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{BootEvent, BootScheduler, EventSink, StepExecutor};
    use crate::boot::{BootKind, BootState, BootStep, StampStore, StepOutcome, WorldPlan};

    fn service_step(id: &str) -> BootStep {
        BootStep {
            kind: BootKind::Service,
            image: None,
            subject: id.to_owned(),
            phase: None,
            label: format!("service {id}"),
            command: Some("true".to_owned()),
            stamp: None,
            uses_env: false,
            line: 1,
            deps: Vec::new(),
        }
    }

    /// The plan frame is the first event and names every planned step before any worker runs.
    #[test]
    fn the_plan_frame_precedes_the_first_started_and_lists_every_step() {
        let dir = tempfile::Builder::new()
            .prefix("slipfork-plan-frame")
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory");
        let plan = WorldPlan {
            steps: vec![service_step("api"), service_step("worker")],
            ..WorldPlan::default()
        };
        let seen: Arc<Mutex<Vec<BootEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink: Arc<EventSink> = {
            let seen = Arc::clone(&seen);
            Arc::new(move |event: BootEvent| {
                seen.lock().expect("event lock").push(event);
            })
        };
        let executor: Arc<StepExecutor> = Arc::new(|_: usize, _: &BootStep| {
            Ok(StepOutcome {
                state: BootState::Ready,
                detail: String::new(),
                run_id: None,
            })
        });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build the test runtime");
        let scheduler = Arc::new(BootScheduler::new(
            runtime.handle().clone(),
            StampStore::gated(dir.path(), false),
            executor,
            sink,
        ));
        scheduler.run(&plan).expect("an acyclic plan boots");

        let seen = seen.lock().expect("event lock");
        let BootEvent::Plan { rows } = &seen[0] else {
            panic!("the first event must be `Plan`, got {:?}", seen[0]);
        };
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["api", "worker"]);
        assert!(
            seen[1..]
                .iter()
                .any(|event| matches!(event, BootEvent::Started { .. })),
            "the plan must precede a real `Started`"
        );
        assert!(
            seen[1..].iter().all(|event| matches!(
                event,
                BootEvent::Started { .. } | BootEvent::Finished { .. }
            )),
            "the plan is announced exactly once: {:?}",
            &seen[1..]
        );
    }
}
