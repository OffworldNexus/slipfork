//! Image progress for the daemon's pre-plan: typed `BuildEvent`s become the same boot frames
//! the supervisor used to stream, plus one run capture per image attempt for `logs -r`.
//!
//! The run record carries the instruction a build announced (at START), the typed child exit
//! code, and — when no child printed its own cause — the engine's one-line explanation. Captures
//! stay pure command output; every record fact is published onto the attempt's catalog run.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use slipfork_image::{BuildEvent, StepEvent, StepLog, StepState, StepTrace};
use slipfork_world::run_log::RunNote;
use slipfork_world::{ImageNode, LogStore, WorldPlan};

use crate::daemon::Failure;
use crate::ui::report::{Counter, Done, FailureFacts, Outcome, StepSettlement};
use crate::world::{RunNotes, RunTarget, StepPin, log_frame, note_frame, step_frame};

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

/// One image attempt: a `started`/`finished` image row, its inner `step`/`log`/`note` frames, and
/// the run capture that makes `slipfork logs -r <id>` resolve.
pub(super) struct ImageAttempt<'a> {
    emit: &'a mut dyn FnMut(Value),
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
    pub(super) fn begin(
        emit: &'a mut dyn FnMut(Value),
        plan: &'a WorldPlan,
        store: &LogStore,
        project: &Path,
        image: &str,
        command: &str,
    ) -> Result<Self, Failure> {
        let position = plan
            .steps
            .iter()
            .position(|step| step.kind.as_str() == "image" && step.subject == image);
        let step = position.map_or(1, |index| index + 1);
        let label = position.map_or_else(
            || format!("image {image}"),
            |index| plan.steps[index].label.clone(),
        );
        let run = RunTarget::open(store, project, "image", image, command)
            .map_err(|failure| Failure::new("image-log", failure.message))?;
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
            "total": plan.steps.len().max(1),
            "kind": "image",
            "name": image,
            "label": label,
            "state": "running",
            "outcome": Outcome::Running,
            "run_id": run_id,
        }));
        Ok(Self {
            emit,
            total: plan.steps.len().max(1),
            step,
            label,
            image: image.to_owned(),
            index: None,
            inner_total: None,
            run: Some(run),
            notes: RunNotes::new(store),
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

    /// Settles the image row and its capture; a cached attempt with no output drops its capture.
    ///
    /// A kept capture names the last instruction this attempt applied (or the `FROM` reference it
    /// pulled), so a replay never has to guess which instruction produced its rows.
    pub(super) fn finish(
        &mut self,
        settle: ImageSettle,
        elapsed_ms: u64,
        last: Option<&StepTrace>,
        executed: usize,
    ) {
        let state = settle.as_str();
        let discard = !self.wrote && state == "cached";
        if !discard {
            let note = success_note(self.nodes, &self.image, last);
            if let Some(run_id) = self.run.as_ref().and_then(RunTarget::run_id) {
                let _ = self.notes.succeeded(Some(run_id), &note);
            }
        }
        let run_id = self.run.as_mut().and_then(|run| {
            if discard {
                run.discard();
                None
            } else {
                run.finish(state, elapsed_ms)
            }
        });
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
        let note = failure
            .note
            .map(|note| *note)
            .or_else(|| self.started.take())
            .or_else(|| self.dockerfile.as_deref().map(failed_note));
        if let Some(run_id) = self.run.as_ref().and_then(RunTarget::run_id) {
            let _ = self.notes.failed(
                Some(run_id),
                note.as_ref(),
                failure.exit_code,
                failure.reason.as_deref(),
            );
        }
        let run_id = self
            .run
            .as_mut()
            .and_then(|run| run.finish("failed", elapsed_ms));
        let (reason, next) = failure_parts(&failure.message);
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
            next: next.clone(),
        };
        let cause = failure.message.lines().next().unwrap_or(&failure.message);
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
        Failure::new("image-build", failure.message)
    }

    fn observe(&mut self, event: BuildEvent<'_>) {
        match event {
            BuildEvent::Step(step) => {
                if step.state == StepState::Started {
                    self.index = Some(step.index);
                    self.inner_total = Some(step.total);
                    self.wrote = true;
                    let note = started_note(self.dockerfile.as_deref(), &step);
                    if let Some(run_id) = self.run.as_ref().and_then(RunTarget::run_id) {
                        let _ = self.notes.started(Some(run_id), &note);
                    }
                    self.started = Some(note);
                    if let Some(run) = self.run.as_mut() {
                        run.step_row(&step_label(&step));
                    }
                }
                (self.emit)(step_frame(&self.image, &step));
            }
            BuildEvent::Line { stream, text } => {
                // Stored and streamed raw: the viewer strips when its output cannot take colour.
                let text = text.trim_end_matches('\r');
                if text.trim().is_empty() {
                    return;
                }
                self.wrote = true;
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
                (self.emit)(frame);
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
                (self.emit)(frame);
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

/// Splits an engine message into its body and the one `next:` remedy it already carries.
fn failure_parts(message: &str) -> (String, String) {
    let (body, next) = crate::ui::report::split_remedy(message);
    (
        body,
        next.unwrap_or_else(|| "fix the failing step and retry `slipfork up`".to_owned()),
    )
}

/// The exact settlement one finished attempt publishes; zero executed steps stay `cached`.
fn settlement(settle: ImageSettle, duration_ms: u64, run_id: Option<&str>) -> StepSettlement {
    match settle {
        ImageSettle::Cached => StepSettlement::Cached { duration_ms },
        ImageSettle::Pulled => StepSettlement::Completed {
            done: Done::Pulled,
            duration_ms,
            run_id: run_id.map(str::to_owned),
        },
        ImageSettle::Built(steps) => StepSettlement::Completed {
            done: Done::Built { steps },
            duration_ms,
            run_id: run_id.map(str::to_owned),
        },
    }
}

/// One failed attempt: the user message plus the typed facts its run record still needs.
pub(super) struct AttemptFailure {
    /// `1 line + next:` message printed by `up`; never appended to the capture.
    pub(super) message: String,
    /// The failing instruction when the attempt already knows it (`FROM`, a parsed pull).
    pub(super) note: Option<Box<RunNote>>,
    /// Typed child exit code the step carried, when it carried one.
    pub(super) exit_code: Option<i32>,
    /// The engine's raw one-line cause, so a comment-less replay explains itself.
    pub(super) reason: Option<String>,
}

impl AttemptFailure {
    /// A failure with no typed engine facts (a missing `Dockerfile`, an unreadable context).
    pub(super) fn bare(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            note: None,
            exit_code: None,
            reason: None,
        }
    }

    /// A failure the image engine reported, with its typed exit code and one-line cause.
    pub(super) fn engine(
        message: impl Into<String>,
        error: &slipfork_image::Error,
        note: Option<RunNote>,
    ) -> Self {
        Self {
            message: message.into(),
            note: note.map(Box::new),
            exit_code: error.exit_code(),
            reason: Some(error.cause()),
        }
    }
}

/// One instruction's capture row: `<index>/<total> <KEYWORD> <args>`, ANSI-free and single-line.
fn step_label(event: &StepEvent<'_>) -> String {
    let raw = format!(
        "{}/{} {} {}",
        event.index, event.total, event.instruction, event.args
    );
    let raw = crate::ui::strip(&raw);
    let mut label = String::with_capacity(raw.len());
    let mut gap = false;
    for ch in raw.chars() {
        if matches!(ch, '\n' | '\r' | '\t') {
            gap = true;
            continue;
        }
        if gap {
            label.push(' ');
            gap = false;
        }
        label.push(ch);
    }
    label.trim_end().to_owned()
}

/// The instruction a build announced at START, so a failure always leaves the right header.
///
/// `line` is the instruction's `Dockerfile` source line; a step with no source line (an OCI pull)
/// keeps its phase and command and simply carries no `source`.
fn started_note(dockerfile: Option<&Path>, event: &StepEvent<'_>) -> RunNote {
    RunNote {
        phase: Some(event.instruction.to_ascii_lowercase()),
        instruction: (!event.args.is_empty()).then(|| event.args.to_owned()),
        source: dockerfile
            .filter(|_| event.line > 0)
            .map(|dockerfile| format!("{}:{}", file_label(dockerfile), event.line)),
        ..RunNote::default()
    }
}

/// The note for a failure that ran no instruction at all: it names only the `Dockerfile`, so the
/// verdict — never an invented step row — carries the reason.
fn failed_note(dockerfile: &Path) -> RunNote {
    RunNote {
        source: Some(file_label(dockerfile)),
        ..RunNote::default()
    }
}

/// A `from:` image pull records the reference it fetched, under the one `FROM` verb.
fn from_note(reference: &str) -> RunNote {
    RunNote {
        phase: Some("from".to_owned()),
        instruction: Some(reference.to_owned()),
        ..RunNote::default()
    }
}

/// A successful build names the last instruction it applied, or its Dockerfile when it applied
/// none, or the pulled reference.
fn success_note(nodes: &[ImageNode], image: &str, last: Option<&StepTrace>) -> RunNote {
    let Some(node) = nodes.iter().find(|node| node.id == image) else {
        return RunNote::default();
    };
    if let Some(reference) = &node.from {
        return from_note(reference);
    }
    let dockerfile = node
        .dockerfile
        .clone()
        .unwrap_or_else(|| PathBuf::from("Dockerfile"));
    let label = file_label(&dockerfile);
    match last {
        Some(last) => RunNote {
            phase: Some(last.phase.to_owned()),
            instruction: Some(last.command.clone()),
            source: Some(format!("{label}:{}", last.line)),
            exit_code: Some(0),
            reason: None,
        },
        None => RunNote {
            phase: Some(label.clone()),
            instruction: None,
            source: Some(label),
            exit_code: Some(0),
            reason: None,
        },
    }
}

/// The bare file name a source location starts with (`Dockerfile`, not `/ctx/Dockerfile`).
fn file_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use slipfork_image::{StepEvent, StepPlace, StepState, StepTrace};

    use serde_json::Value;
    use slipfork_world::LogStore;

    use super::{
        AttemptFailure, ImageAttempt, ImageSettle, failed_note, settlement, started_note,
        step_label, success_note,
    };
    use crate::ui::report::StepSettlement;
    use crate::world::RunNotes;

    /// A capture divider names the instruction's position, keyword, and args as the author wrote
    /// them, with no trailing space when the instruction takes none.
    #[test]
    fn a_step_label_is_one_position_keyword_and_args_line() {
        let copy = StepEvent::new(
            StepPlace::new(2, 3, 2),
            "COPY",
            ". /app",
            StepState::Started,
            Duration::ZERO,
            None,
        );
        assert_eq!(step_label(&copy), "2/3 COPY . /app");

        let bare = StepEvent::new(
            StepPlace::new(1, 1, 1),
            "RUN",
            "",
            StepState::Started,
            Duration::ZERO,
            None,
        );
        assert_eq!(step_label(&bare), "1/1 RUN");
    }

    /// A heredoc `RUN` carries ANSI, newlines, and tabs: the row strips the escapes and stays one
    /// physical line, because the capture frames rows on `\t`.
    #[test]
    fn a_step_label_collapses_control_space_onto_one_line() {
        let heredoc = StepEvent::new(
            StepPlace::new(3, 3, 9),
            "RUN",
            "\u{1b}[1msh -c 'echo one\necho two\t\techo three'\u{1b}[0m",
            StepState::Started,
            Duration::ZERO,
            None,
        );
        let label = step_label(&heredoc);
        assert_eq!(label, "3/3 RUN sh -c 'echo one echo two echo three'");
        assert!(
            !label.contains(&['\u{1b}', '\n', '\r', '\t'][..]),
            "a step row must never carry ANSI or a row break: {label:?}"
        );
    }

    /// The note a build publishes when a step STARTS names its instruction, line, and phase.
    #[test]
    fn a_started_step_note_names_its_instruction_and_source_line() {
        let dockerfile = PathBuf::from("/ctx/Dockerfile");
        let event = StepEvent::new(
            StepPlace::new(2, 4, 7),
            "COPY",
            "--from=missing-stage /x /y",
            StepState::Started,
            Duration::ZERO,
            None,
        );
        let note = started_note(Some(&dockerfile), &event);
        assert_eq!(note.phase.as_deref(), Some("copy"));
        assert_eq!(
            note.instruction.as_deref(),
            Some("--from=missing-stage /x /y")
        );
        assert_eq!(note.source.as_deref(), Some("Dockerfile:7"));
        assert_eq!(note.exit_code, None);
        assert_eq!(note.reason, None);

        let step_less = failed_note(&dockerfile);
        assert_eq!(step_less.phase, None);
        assert_eq!(step_less.instruction, None);
        assert_eq!(step_less.source.as_deref(), Some("Dockerfile"));
    }

    /// A successful build names its last instruction rather than the Dockerfile label.
    #[test]
    fn a_successful_build_names_its_last_instruction_not_the_dockerfile_label() {
        let node = slipfork_world::ImageNode {
            id: "web".to_owned(),
            directory: PathBuf::from("."),
            dockerfile: Some(PathBuf::from("docker/Dockerfile")),
            from: None,
            line: 3,
        };
        let last = StepTrace {
            phase: "run",
            command: "npm run build".to_owned(),
            line: 7,
        };
        let note = success_note(std::slice::from_ref(&node), "web", Some(&last));
        assert_eq!(note.phase.as_deref(), Some("run"));
        assert_eq!(note.instruction.as_deref(), Some("npm run build"));
        assert_eq!(note.source.as_deref(), Some("Dockerfile:7"));
        assert_eq!(note.exit_code, Some(0));

        let unused = success_note(std::slice::from_ref(&node), "web", None);
        assert_eq!(unused.phase.as_deref(), Some("Dockerfile"));
        assert_eq!(unused.source.as_deref(), Some("Dockerfile"));
        assert_eq!(unused.instruction, None);
    }

    #[test]
    fn image_settlements_publish_exact_stable_json_without_fabricated_steps() {
        let value = |settlement: &StepSettlement| {
            serde_json::to_value(settlement).expect("serialize a settlement")
        };
        assert_eq!(
            value(&settlement(ImageSettle::Cached, 7, None)),
            serde_json::json!({ "state": "cached", "duration_ms": 7 })
        );
        assert_eq!(
            value(&settlement(ImageSettle::Pulled, 7, None)),
            serde_json::json!({
                "state": "completed",
                "done": "pulled",
                "duration_ms": 7,
                "run_id": null,
            })
        );
        assert_eq!(
            value(&settlement(ImageSettle::Pulled, 7, Some("abc"))),
            serde_json::json!({
                "state": "completed",
                "done": "pulled",
                "duration_ms": 7,
                "run_id": "abc",
            })
        );
        assert_eq!(
            value(&settlement(
                ImageSettle::Built(std::num::NonZeroUsize::new(2).unwrap()),
                9,
                Some("abc")
            )),
            serde_json::json!({
                "state": "completed",
                "done": { "built": { "steps": 2 } },
                "duration_ms": 9,
                "run_id": "abc",
            })
        );
    }

    #[test]
    fn emitted_attempt_frames_carry_typed_objects() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let dir = tempfile::Builder::new()
            .prefix("slipfork-progress-frame-")
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory");
        let store = LogStore::at(
            dir.path().join("store"),
            slipfork_world::WorldName::parse("demo").expect("valid world name"),
        );
        let frames = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&frames);
        let mut emit = move |frame: Value| sink.borrow_mut().push(frame);
        let mut attempt = ImageAttempt {
            emit: &mut emit,
            total: 1,
            step: 1,
            label: "image api".to_owned(),
            image: "api".to_owned(),
            index: None,
            inner_total: None,
            run: None,
            notes: RunNotes::new(&store),
            nodes: &[],
            dockerfile: None,
            started: None,
            wrote: true,
        };
        attempt.finish(
            ImageSettle::Built(std::num::NonZeroUsize::new(2).unwrap()),
            9,
            None,
            2,
        );
        let finished = frames
            .borrow()
            .last()
            .cloned()
            .expect("finish emits one frame");
        assert_eq!(
            finished["settlement"],
            serde_json::json!({
                "state": "completed",
                "done": { "built": { "steps": 2 } },
                "duration_ms": 9,
                "run_id": null,
            })
        );
        assert!(finished["settlement"].is_object());

        attempt.fail(
            AttemptFailure::bare("build `api` failed: boom\nnext: fix it"),
            5,
        );
        let failed = frames
            .borrow()
            .last()
            .cloned()
            .expect("fail emits one frame");
        assert!(failed["failure"].is_object());
        assert_eq!(failed["failure"]["code"], "image-build");
        assert_eq!(failed["failure"]["kind"], "image");
        assert_eq!(failed["failure"]["next"], "fix it");
    }
}
