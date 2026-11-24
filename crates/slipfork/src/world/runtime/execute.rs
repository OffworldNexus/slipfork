//! One DAG node's execution: the eager run capture, the staged hand-off, and the record naming
//! the instruction that actually ran.
//!
//! Captures stay pure command output. [`RunNotes`] publishes the executed instruction onto the
//! step's catalog run once it settles, so the replay header never has to guess.

use std::io::Write;
use std::sync::Arc;

use slipfork_world::{BootFailure, BootKind, BootState, BootStep, StepOutcome};

use crate::world::supervise::BootSink;

use super::images::record::{RunNotes, StepFailure};
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
                ) {
                    failure
                        .message
                        .push_str(&format!("\ncould not annotate run: {annotate}"));
                }
                let _ = log.flush();
                return Err(failure);
            }
        };
        let _ = log.flush();
        staged.attach_progress(log.clone());
        let outcome = staged.await_ready();
        let duration = elapsed_ms(started);
        let mut record_error = None;
        let run_id = match &outcome {
            // A no-op step executed zero commands: its capture is removed and no id is shown.
            Ok(done) if matches!(done.state, BootState::Cached | BootState::UpToDate) => {
                log.discard_run();
                None
            }
            Ok(done) => {
                let run_id = log.finish_run(done.state.as_str(), duration);
                if let BootKind::Image = step.kind
                    && let Err(annotate) =
                        notes.succeeded(run_id.as_deref(), &self.images.success_note(&step.subject))
                {
                    record_error = Some(annotate);
                }
                run_id
            }
            Err(error) => {
                let run_id = log.finish_run("failed", duration);
                if let Err(annotate) = notes.failed(run_id.as_deref(), None, error.exit_code) {
                    record_error = Some(annotate);
                }
                run_id
            }
        };
        let _ = log.flush();
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

    /// The `-v`-only container plumbing a failed image build reported, keyed by image id.
    pub(crate) fn image_plumbing(&self, image: &str) -> Option<String> {
        self.images.plumbing(image)
    }

    fn execute_step(&self, step: &BootStep, log: &mut BootSink) -> Result<Staged, StepFailure> {
        match step.kind {
            BootKind::Image => self
                .images
                .build(&step.subject, log)
                .map(|(_, state)| Staged {
                    state,
                    detail: state.as_str().to_owned(),
                    ready: Ready::None,
                    progress: None,
                })
                .map_err(|failure| {
                    StepFailure::described(BootFailure::new(failure.message), failure.note)
                }),
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
            BootKind::Service => self
                .start_service(step, log)
                .map_err(|error| StepFailure::plain(BootFailure::new(error))),
        }
    }
}
