//! Wave scheduling and live boot events for one [`WorldPlan`].

use std::thread;
use std::time::{Duration, Instant};

use super::{BootKind, BootStep, StampStore, WorldPlan};
use crate::error::{Error, Result};

/// A completed step's human detail (`cached 0.1s`, `built 1.4s`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepOutcome {
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
pub type StepExecutor<'a> =
    dyn Fn(usize, &BootStep) -> std::result::Result<StepOutcome, BootFailure> + Sync + 'a;

/// Receives live boot events; the CLI renders them, tests collect them.
pub type EventSink<'a> = dyn Fn(BootEvent) + Sync + 'a;

/// One typed boot event.
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
        /// Human label.
        label: String,
    },
    /// The step finished successfully.
    Finished {
        /// Index into [`WorldPlan::steps`].
        step: usize,
        /// Step kind.
        kind: BootKind,
        /// Human label.
        label: String,
        /// Wall time inside the executor.
        elapsed: Duration,
        /// Short detail.
        detail: String,
    },
    /// A conditional step's stamp matched; the executor did not run.
    UpToDate {
        /// Index into [`WorldPlan::steps`].
        step: usize,
        /// Step kind.
        kind: BootKind,
        /// Human label.
        label: String,
        /// Time spent checking the stamp (<1ms).
        elapsed: Duration,
    },
    /// The step failed; the world stays up for inspection.
    Failed {
        /// Index into [`WorldPlan::steps`].
        step: usize,
        /// Step kind.
        kind: BootKind,
        /// Human label.
        label: String,
        /// `Forkfile` line of the declaration.
        line: u32,
        /// One human line.
        message: String,
        /// Concrete next action when editing `line` is not the fix.
        remedy: Option<String>,
    },
}

/// Runs `plan` wave by wave, in parallel inside each wave.
///
/// A failure stops all later waves, emits [`BootEvent::Failed`], and returns [`Error::Boot`]
/// with the first failure's line. Stamped steps that match are skipped and reported as
/// [`BootEvent::UpToDate`]; a successful conditional step records its stamp.
/// A launched service either stayed up through the readiness window or exited early.
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

pub fn run_plan(
    plan: &WorldPlan,
    stamps: &StampStore,
    execute: &StepExecutor<'_>,
    sink: &EventSink<'_>,
) -> Result<()> {
    for wave in plan.waves()? {
        let total = plan.steps.len();
        let outcomes = thread::scope(|scope| {
            let handles: Vec<_> = wave
                .iter()
                .map(|&index| {
                    let step = &plan.steps[index];
                    scope.spawn(move || run_step(index, total, step, stamps, execute, sink))
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("a boot worker must not panic"))
                .collect::<Vec<_>>()
        });
        if let Some((index, failure)) = outcomes
            .iter()
            .find_map(|(index, outcome)| outcome.as_ref().err().map(|failure| (*index, failure)))
        {
            let step = &plan.steps[index];
            sink(BootEvent::Failed {
                step: index,
                kind: step.kind,
                label: step.label.clone(),
                line: step.line,
                message: failure.message.clone(),
                remedy: failure.remedy.clone(),
            });
            return Err(Error::Boot {
                step: step.label.clone(),
                line: step.line,
                message: failure.message.clone(),
                remedy: failure.remedy.clone(),
            });
        }
    }
    Ok(())
}

fn run_step(
    index: usize,
    total: usize,
    step: &BootStep,
    stamps: &StampStore,
    execute: &StepExecutor<'_>,
    sink: &EventSink<'_>,
) -> (usize, std::result::Result<StepOutcome, BootFailure>) {
    // The stamp probe is the timer's subject: arming it after `is_current` would report a
    // hard-coded 0ms and make every `elapsed` assertion unfalsifiable.
    let checked_at = Instant::now();
    if let Some(stamp) = &step.stamp
        && stamps.is_current(stamp)
    {
        sink(BootEvent::UpToDate {
            step: index,
            kind: step.kind,
            label: step.label.clone(),
            elapsed: checked_at.elapsed(),
        });
        return (
            index,
            Ok(StepOutcome {
                detail: "up to date".to_owned(),
            }),
        );
    }
    sink(BootEvent::Started {
        step: index,
        total,
        kind: step.kind,
        label: step.label.clone(),
    });
    let started = Instant::now();
    let outcome = execute(index, step);
    let elapsed = started.elapsed();
    if outcome.is_ok()
        && let Some(stamp) = &step.stamp
        && let Err(error) = stamps.record(stamp)
    {
        return (index, Err(BootFailure::new(error.to_string())));
    }
    if let Ok(done) = &outcome {
        sink(BootEvent::Finished {
            step: index,
            kind: step.kind,
            label: step.label.clone(),
            elapsed,
            detail: done.detail.clone(),
        });
    }
    (index, outcome)
}
