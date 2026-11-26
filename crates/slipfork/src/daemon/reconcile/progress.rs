//! Image progress for the daemon's pre-plan: typed `BuildEvent`s become the same boot frames
//! the supervisor used to stream, plus one run capture per image attempt for `logs -r`.
//!
//! The run record carries the instruction a build announced (at START), the typed child exit
//! code, and — when no child printed its own cause — the engine's one-line explanation. Captures
//! stay pure command output; every record fact is published onto the attempt's catalog run.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};
use slipfork_image::{BuildEvent, StepLog, StepPlace, StepState, StepTrace};
use slipfork_world::run_log::RunNote;
use slipfork_world::{ImageNode, WorldPlan};

use crate::daemon::Failure;
use crate::daemon::lifecycle::plan::PlanPosition;
use crate::ui::report::{Counter, FailureFacts, Outcome};
use crate::world::{RunNotes, RunTarget, StepPin, log_frame, note_frame, step_frame};

mod notes;

pub(super) use self::notes::AttemptFailure;
use self::notes::{
    failed_note, failure_parts, file_label, settlement, started_note, step_label, success_note,
};

/// Which settled state one finished image attempt publishes; the wire words stay unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ImageSettle {
    /// A cache hit with no executed steps.
    Cached,
    /// A `FROM`/resource reference fetched from a registry.
    Pulled,
    /// A Dockerfile build that executed at least one instruction.
    Built(NonZeroUsize),
}

impl ImageSettle {
    /// The exact `state` word this attempt publishes.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Cached => "cached",
            Self::Pulled => "pulled",
            Self::Built(_) => "built",
        }
    }
}

/// One image row a build announces: its merged-plan counter, its capture, and the checkout whose
/// Dockerfile names the build source.
pub(super) struct ImageRow<'a> {
    pub(super) position: PlanPosition,
    pub(super) run: RunTarget,
    pub(super) notes: RunNotes,
    pub(super) project: &'a Path,
    pub(super) image: &'a str,
}

/// One image attempt: a `started`/`finished` image row, its inner `step`/`log`/`note` frames, and
/// the run capture that makes `slipfork logs -r <id>` resolve.
pub(super) struct ImageAttempt<'a> {
    emit: &'a mut dyn FnMut(Value),
    /// Frames held back while this attempt might still turn out to be a cache hit.
    ///
    /// A reused instruction performs no work, so a fully cached image must settle as one `cached`
    /// verdict with zero inner rows (and no `→` opener). Frames are published the moment the
    /// attempt proves it is really building (`tail`/output), and dropped when it settles cached.
    withheld: Vec<Value>,
    /// Whether the withheld frames were released and every later frame streams live.
    live: bool,
    /// Whether the attempt streamed at least one child output line.
    lines: bool,
    total: usize,
    step: usize,
    label: String,
    image: String,
    index: Option<usize>,
    inner_total: Option<usize>,
    run: Option<RunTarget>,
    notes: RunNotes,
    nodes: &'a [ImageNode],
    dockerfile: Option<PathBuf>,
    started: Option<RunNote>,
    wrote: bool,
}

impl<'a> ImageAttempt<'a> {
    /// Announces the image row and opens its eager capture.
    ///
    /// `row.position` is the 1-based merged-plan counter the daemon resolved once
    /// (`index`/`total`); `plan.steps` supplies only the human label here, never a counter.
    pub(super) fn begin(
        emit: &'a mut dyn FnMut(Value),
        plan: &'a WorldPlan,
        row: ImageRow<'a>,
    ) -> Result<Self, Failure> {
        let ImageRow {
            position,
            run,
            notes,
            project,
            image,
        } = row;
        let step = usize::try_from(position.step).unwrap_or(usize::MAX);
        let total = usize::try_from(position.total).unwrap_or(usize::MAX);
        // `plan.steps` supplies only the human label here; the counter came from the merged plan.
        let label = plan
            .steps
            .iter()
            .position(|step| step.kind.as_str() == "image" && step.subject == image)
            .map_or_else(
                || format!("image {image}"),
                |index| plan.steps[index].label.clone(),
            );
        let run_id = run.run_id().map(str::to_owned);
        // A `from:` image fetched a reference; a Dockerfile build names its source file.
        let dockerfile = plan
            .images
            .iter()
            .find(|node| node.id == image)
            .filter(|node| node.from.is_none())
            .map(|node| node.paths(project).dockerfile);
        emit(json!({
            "event": "started",
            "step": step,
            "total": total,
            "kind": "image",
            "name": image,
            "label": label,
            "state": "running",
            "outcome": Outcome::Running,
            "run_id": run_id,
        }));
        Ok(Self {
            emit,
            withheld: Vec::new(),
            live: false,
            lines: false,
            total,
            step,
            label,
            image: image.to_owned(),
            index: None,
            inner_total: None,
            run: Some(run),
            notes,
            nodes: &plan.images,
            dockerfile,
            started: None,
            wrote: false,
        })
    }

    /// The capture id a human can replay with `slipfork logs -r <id>`.
    pub(super) fn run_id(&self) -> Option<&str> {
        self.run.as_ref().and_then(RunTarget::run_id)
    }

    /// The last instruction this attempt started (`RUN apt-get update`), or its Dockerfile.
    pub(super) fn instruction(&self) -> Option<String> {
        if let Some(note) = &self.started {
            let phase = note.phase.as_deref().unwrap_or_default().to_uppercase();
            let command = note.instruction.as_deref().unwrap_or_default();
            let text = format!("{phase} {command}").trim().to_owned();
            if !text.is_empty() {
                return Some(text);
            }
        }
        self.dockerfile.as_deref().map(file_label)
    }

    /// Runs one engine call with a typed bus that re-emits the supervisor frame shapes.
    pub(super) fn events<T>(
        &mut self,
        task: impl FnOnce(&mut StepLog<'_>) -> slipfork_image::Result<T>,
    ) -> slipfork_image::Result<T> {
        let mut bus = |event: BuildEvent<'_>| self.observe(event);
        task(&mut StepLog::with_events(&mut bus))
    }

    /// Pulls one `from:` reference as its single `FROM` row.
    ///
    /// The row is announced before the reference is parsed, so an invalid reference fails *as
    /// that step*; the sealed cache is then probed without touching the registry, so a cache hit
    /// settles as one `cached` verdict with zero inner rows while a real pull streams live.
    pub(super) fn pull(
        &mut self,
        puller: &slipfork_image::OciPuller,
        reference: &str,
    ) -> slipfork_image::Result<slipfork_image::PulledImage> {
        {
            let mut bus = |event: BuildEvent<'_>| self.observe(event);
            let mut log = StepLog::with_events(&mut bus);
            log.announce("FROM", reference);
        }
        let parsed = slipfork_image::ImageReference::parse(reference)?;
        if let Some(cached) = puller.cached(&parsed)? {
            return Ok(cached);
        }
        self.stream();
        let started = Instant::now();
        let mut bus = |event: BuildEvent<'_>| self.observe(event);
        let mut log = StepLog::with_events(&mut bus);
        // The announce above used another `StepLog` (the probe had to run between them); re-open
        // the same identity so this instance settles the row it announced, not a phantom.
        log.resume_at(StepPlace::new(1, 1, 0), "FROM", reference, true);
        match puller.pull_into(&parsed, &mut log) {
            Ok(image) => {
                log.done(started.elapsed());
                Ok(image)
            }
            Err(error) => {
                log.failed(started.elapsed(), error.exit_code());
                Err(error)
            }
        }
    }

    /// Releases every frame held back while the attempt still looked like a cache hit.
    fn stream(&mut self) {
        if self.live {
            return;
        }
        self.live = true;
        for frame in std::mem::take(&mut self.withheld) {
            (self.emit)(frame);
        }
    }

    /// Emits a frame live, or holds it until this attempt proves it is doing real work.
    fn publish(&mut self, frame: Value) {
        if self.live {
            (self.emit)(frame);
        } else {
            self.withheld.push(frame);
        }
    }

    /// Settles the image row and its capture; a cached attempt with no output drops its capture.
    ///
    /// A kept capture names the last instruction this attempt applied (or the `FROM` reference it
    /// pulled), so a replay never has to guess which instruction produced its rows. Every row a
    /// real build held back is released first; a cache hit drops them, so it settles as one
    /// `cached` row.
    pub(super) fn finish(
        &mut self,
        settle: ImageSettle,
        elapsed_ms: u64,
        last: Option<&StepTrace>,
        executed: usize,
    ) {
        let state = settle.as_str();
        let discard = !self.wrote && state == "cached";
        if discard {
            self.withheld.clear();
        } else {
            self.stream();
        }
        // The capture must settle in the catalog *before* its note is appended: `annotate_run`
        // copies the newest record, so annotating first would let `finish` overwrite the phase,
        // instruction, exit code and reason with empty fallbacks.
        let run_id = self.run.as_mut().and_then(|run| {
            if discard {
                run.discard();
                None
            } else {
                run.finish(state, elapsed_ms)
            }
        });
        if !discard {
            let note = success_note(self.nodes, &self.image, last);
            let _ = self.notes.succeeded(run_id.as_deref(), &note);
        }
        let settlement = settlement(settle, elapsed_ms, run_id.as_deref());
        let mut frame = json!({
            "event": "finished",
            "step": self.step,
            "total": self.total,
            "kind": "image",
            "name": self.image,
            "state": state,
            "detail": state,
            "executed": executed,
            "elapsed_ms": elapsed_ms,
            "duration_ms": elapsed_ms,
            "run_id": run_id,
        });
        frame["settlement"] = json!(settlement);
        (self.emit)(frame);
    }

    /// Settles a failed attempt, keeping its capture and publishing the instruction that failed.
    pub(super) fn fail(&mut self, failure: AttemptFailure, elapsed_ms: u64) -> Failure {
        self.wrote = true;
        self.stream();
        let note = failure
            .note
            .map(|note| *note)
            .or_else(|| self.started.take())
            .or_else(|| self.dockerfile.as_deref().map(failed_note));
        // A step that printed nothing still explains itself in its own gutter: stream the engine's
        // one-line cause under the failing row (never twice for a `RUN` that already printed its
        // own output), and keep it in the run capture so a replay explains itself too.
        let cause = self
            .index
            .is_some()
            .then(|| failure.reason.clone())
            .flatten()
            .filter(|_| !self.lines);
        if let Some(cause) = cause {
            let frame = {
                let pin = self.pin();
                log_frame(&pin, "stderr", &cause)
            };
            if let Some(run) = self.run.as_mut() {
                run.append_stream(slipfork_world::Stream::Stderr, &cause);
            }
            self.publish(frame);
        }
        // Settle the capture first, then patch the finished record with the failing instruction:
        // `annotate_run` copies the newest record, so the reverse order loses the note.
        let run_id = self
            .run
            .as_mut()
            .and_then(|run| run.finish("failed", elapsed_ms));
        if let Some(run_id) = run_id.as_deref() {
            let _ = self.notes.failed(
                Some(run_id),
                note.as_ref(),
                failure.exit_code,
                failure.reason.as_deref(),
            );
        }
        let (reason, next) = failure_parts(&failure.message);
        // A failed attempt that minted a run capture names that replay as its one remedy; the
        // descriptive engine message stays on the RPC error for `--json`, never as a second row.
        let next = run_id
            .as_deref()
            .map_or(next, |id| format!("slipfork logs -r {id}"));
        let facts = FailureFacts {
            code: "image-build".to_owned(),
            kind: note
                .as_ref()
                .and_then(|note| note.phase.clone())
                .map_or_else(|| "image".to_owned(), |phase| phase.to_uppercase()),
            phase: note.as_ref().and_then(|note| note.phase.clone()),
            target: Some(self.image.clone()),
            at: self
                .index
                .zip(self.inner_total)
                .map(|(index, total)| Counter { index, total }),
            exit_code: failure.exit_code,
            run_id: run_id.clone(),
            reason: failure.reason.clone().unwrap_or_else(|| reason.clone()),
            runtime_ms: Some(elapsed_ms),
            next,
        };
        let cause = failure.message.lines().next().unwrap_or(&failure.message);
        // One durable attempt row: `critical_status_live` reads it as the image's `finished`
        // frame carrying the run id, and the CLI prints the error envelope once for it.
        let mut frame = json!({
            "event": "finished",
            "step": self.step,
            "total": self.total,
            "kind": "image",
            "name": self.image,
            "state": "failed",
            "detail": cause,
            "executed": 0,
            "elapsed_ms": elapsed_ms,
            "duration_ms": elapsed_ms,
            "run_id": run_id,
        });
        frame["failure"] = json!(facts);
        (self.emit)(frame);
        // A message that names no remedy yet still gets the one replay a failure owes the user.
        let message = if failure
            .message
            .lines()
            .any(|line| line.starts_with("next:"))
        {
            failure.message
        } else {
            match run_id.as_deref() {
                Some(id) => format!("{}\nnext: slipfork logs -r {id}", failure.message),
                None => failure.message,
            }
        };
        Failure::new("image-build", message)
    }

    fn observe(&mut self, event: BuildEvent<'_>) {
        match event {
            BuildEvent::Step(step) => {
                if step.state == StepState::Started {
                    self.index = Some(step.index);
                    self.inner_total = Some(step.total);
                    let note = started_note(self.dockerfile.as_deref(), &step);
                    if let Some(run_id) = self.run.as_ref().and_then(RunTarget::run_id) {
                        let _ = self.notes.started(Some(run_id), &note);
                    }
                    self.started = Some(note);
                    if let Some(run) = self.run.as_mut() {
                        run.step_row(&step_label(&step));
                    }
                }
                // A tail is the first proof the instruction is being materialized rather than
                // served from the step cache: every reused row and metadata row is a bare
                // `started`+`done` pair, so only a tail (or child output) opens the build live.
                if step.state == StepState::Tail || step.state == StepState::Failed {
                    self.wrote = true;
                    self.stream();
                }
                let frame = step_frame(&self.image, &step);
                self.publish(frame);
            }
            BuildEvent::Line { stream, text } => {
                // Stored and streamed raw: the viewer strips when its output cannot take colour.
                let text = text.trim_end_matches('\r');
                if text.trim().is_empty() {
                    return;
                }
                self.wrote = true;
                self.lines = true;
                self.stream();
                let stream = match stream {
                    slipfork_image::Stream::Stdout => slipfork_world::Stream::Stdout,
                    slipfork_image::Stream::Stderr => slipfork_world::Stream::Stderr,
                };
                if let Some(run) = self.run.as_mut() {
                    run.append_stream(stream, text);
                }
                let frame = {
                    let pin = self.pin();
                    log_frame(&pin, stream.as_str(), text)
                };
                self.publish(frame);
            }
            BuildEvent::Note { text } => {
                let text = crate::ui::strip(text);
                if text.trim().is_empty() {
                    return;
                }
                let frame = {
                    let pin = self.pin();
                    note_frame(&pin, text.trim_end_matches('\r'))
                };
                self.publish(frame);
            }
        }
    }

    fn pin(&self) -> StepPin<'_> {
        StepPin {
            step: self.step,
            total: self.total,
            label: &self.label,
            name: &self.image,
            image: Some(&self.image),
            index: self.index,
        }
    }
}

#[cfg(test)]
mod tests;
