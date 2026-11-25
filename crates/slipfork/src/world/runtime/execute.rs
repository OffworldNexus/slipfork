//! One DAG node's execution: the eager run capture, the staged hand-off, and the record naming
//! the instruction that actually ran.
//!
//! Captures stay pure command output. [`RunNotes`] publishes the executed instruction onto the
//! step's catalog run once it settles, so the replay header never has to guess.

use std::sync::Arc;

use slipfork_world::run_log::RunNote;
use slipfork_world::{BootFailure, BootKind, BootState, BootStep, StepOutcome};

use crate::world::supervise::BootSink;

use super::notes::{RunNotes, StepFailure};
use super::ready::{Ready, Staged};
use super::{WorldRuntime, elapsed_ms, resources};

impl WorldRuntime {
    /// Executes one DAG node with zero global lock: staging uses narrow collaborator scopes and
    /// [`Staged::await_ready`] polls sockets outside all locks.
    ///
    /// Every step opens its run capture eagerly, before it executes anything; a `cached`/`up to
    /// date` outcome discards it. A capture that cannot be opened fails the step instead of
    /// silently running it unrecorded.
    pub(in crate::world) fn execute(
        &self,
        index: usize,
        step: &BootStep,
    ) -> Result<StepOutcome, BootFailure> {
        // Captured before the launch so a crash line written inside the first milliseconds is not
        // filtered out of the service's run capture by its own `since_ms` window.
        let started_ms = slipfork_world::pod_log::now_ms();
        let mut log = BootSink::for_step(
            Arc::clone(&self.emitter),
            index + 1,
            self.total_steps,
            &step.label,
            &step.node_name(),
        );
        // Armed for every kind: a pod that dies inside `launch_pod` must still leave a record and a
        // readable capture, and only a cached outcome throws it away.
        let command = step.command.clone().unwrap_or_else(|| step.subject.clone());
        let store = slipfork_world::LogStore::at(self.layout.store_root(), self.world.clone());
        log = log.capture_run(
            &store,
            &self.project,
            step.kind.as_str(),
            &step.node_name(),
            &command,
        )?;
        let notes = RunNotes::new(&store);
        let started = std::time::Instant::now();
        let mut staged = match self.execute_step(step, &mut log) {
            Ok(staged) => staged,
            Err(StepFailure { mut failure, note }) => {
                failure.run_id = log.finish_run("failed", elapsed_ms(started));
                if let Err(annotate) = notes.failed(
                    failure.run_id.as_deref(),
                    note.as_deref(),
                    failure.exit_code,
                    None,
                ) {
                    failure
                        .message
                        .push_str(&format!("\ncould not annotate run: {annotate}"));
                }
                log.flush();
                return Err(failure);
            }
        };
        log.flush();
        staged.attach_progress(log.clone(), self.layout.world_logs(&self.world), started_ms);
        let outcome = staged.await_ready(&self.pods_dir);
        let duration = elapsed_ms(started);
        let mut record_error = None;
        let run_id = match &outcome {
            // A no-op step executed zero commands: its capture is removed and no id is shown.
            Ok(done) if matches!(done.state, BootState::Cached | BootState::UpToDate) => {
                log.discard_run();
                None
            }
            Ok(done) => log.finish_run(done.state.as_str(), duration),
            Err(error) => {
                let run_id = log.finish_run("failed", duration);
                // A readiness failure names its one-line cause and the child code when the wait
                // learned them, so the catalog record explains the failure without a rerun.
                let reason = error
                    .message
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(str::to_owned);
                let note = reason.map(|reason| RunNote {
                    reason: Some(reason),
                    exit_code: error.exit_code,
                    ..Default::default()
                });
                if let Err(annotate) =
                    notes.failed(run_id.as_deref(), note.as_ref(), error.exit_code, None)
                {
                    record_error = Some(annotate);
                }
                run_id
            }
        };
        log.flush();
        let mut done = match outcome {
            Ok(done) => done,
            Err(mut error) => {
                error.run_id = run_id;
                if let Some(annotate) = record_error {
                    error
                        .message
                        .push_str(&format!("\ncould not annotate run: {annotate}"));
                }
                return Err(error);
            }
        };
        if let Some(annotate) = record_error {
            return Err(BootFailure::new(annotate));
        }
        done.run_id = run_id;
        Ok(done)
    }

    fn execute_step(&self, step: &BootStep, log: &mut BootSink) -> Result<Staged, StepFailure> {
        match step.kind {
            // The daemon already sealed every image; the boot step only reports the recorded head.
            BootKind::Image => {
                let head = self
                    .head_for(&step.subject)
                    .map_err(|error| StepFailure::plain(BootFailure::new(error)))?;
                let state = match head.state.as_str() {
                    "cached" => BootState::Cached,
                    "pulled" => BootState::Pulled,
                    _ => BootState::Built,
                };
                Ok(Staged {
                    state,
                    detail: head.state,
                    ready: Ready::None,
                    progress: None,
                })
            }
            BootKind::Resource => resources::start_resource(self, &step.subject, log)
                .map_err(|error| StepFailure::plain(BootFailure::new(error))),
            BootKind::Setup | BootKind::Hook => {
                let detail = self.run_once(step, log).map_err(StepFailure::plain)?;
                Ok(Staged {
                    state: BootState::Ready,
                    detail,
                    ready: Ready::None,
                    progress: None,
                })
            }
            BootKind::Service => self.start_service(step, log).map_err(StepFailure::plain),
        }
    }
}
