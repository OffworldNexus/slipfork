//! Live build reporting: child lines, engine notes, and instruction `started`/`done`/`failed` rows.

use std::time::Duration;

use slipfork_world::snapshot::TailPhase;

use crate::runtime::Stream;

/// The `-v`-only container-runtime detail for a `RUN` that exited with `code`.
#[must_use]
pub fn exit_detail(code: Option<i32>) -> String {
    format!(
        "crun exited with {}",
        code.map_or_else(|| "a signal".to_owned(), |code| format!("code {code}"))
    )
}

/// One instruction's lifecycle state on the live build wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    /// The instruction is executing now.
    Started,
    /// The instruction settled its work and is in one post-execution tail phase (seal/record/publish)
    /// or waiting on the next slot clone; the live row keeps its clock.
    Tail,
    /// The instruction applied, or was served from the sealed step cache.
    Done,
    /// The instruction failed.
    Failed,
}

impl StepState {
    /// The `--json` state string shared by every live build row.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Tail => "tail",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

/// Where one instruction sits: its 1-based index of the walked total, and its `Dockerfile` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepPlace {
    /// 1-based instruction index within the image's `Dockerfile`.
    pub index: usize,
    /// Instructions in the walked `Dockerfile`.
    pub total: usize,
    /// 1-based `Dockerfile` line the instruction starts on; `0` when it has no source line.
    pub line: usize,
}

impl StepPlace {
    /// Builds the position of `index`/`total`, sourced from `line`.
    #[must_use]
    pub const fn new(index: usize, total: usize, line: usize) -> Self {
        Self { index, total, line }
    }
}

/// One instruction row: which instruction ran, where it sits, and how it settled.
#[derive(Debug, Clone, Copy)]
pub struct StepEvent<'a> {
    /// 1-based instruction index within the image's `Dockerfile`.
    pub index: usize,
    /// Instructions in the walked `Dockerfile`.
    pub total: usize,
    /// Uppercase instruction keyword (`RUN`, `COPY`, `FROM`, …).
    pub instruction: &'a str,
    /// Arguments exactly as the author wrote them.
    pub args: &'a str,
    /// 1-based `Dockerfile` line the instruction starts on; `0` when it has no source line.
    pub line: usize,
    /// Lifecycle state.
    pub state: StepState,
    /// Time the instruction executed; zero while it is still running.
    pub elapsed: Duration,
    /// Child process exit code, when a failed `RUN` carried one.
    pub exit_code: Option<i32>,
    /// The tail phase this row announces; `Some` exactly when `state` is [`StepState::Tail`].
    pub phase: Option<TailPhase>,
    /// Widest instruction keyword in the image's whole walked plan, so every args cell aligns.
    pub instruction_width: usize,
    /// A pre-build external-pull announcement, not a real planned instruction: renderers buffer
    /// but never release it, because the real instruction at the same index supersedes it.
    pub provisional: bool,
}

impl<'a> StepEvent<'a> {
    /// Builds one row for `instruction` at `place`, in `state`.
    ///
    /// `instruction_width` defaults to this keyword's own width; a [`StepLog`] with a planned
    /// width overrides it so every row in one image shares the args column.
    #[must_use]
    pub fn new(
        place: StepPlace,
        instruction: &'a str,
        args: &'a str,
        state: StepState,
        elapsed: Duration,
        exit_code: Option<i32>,
    ) -> Self {
        Self {
            index: place.index,
            total: place.total,
            instruction,
            args,
            line: place.line,
            state,
            elapsed,
            exit_code,
            phase: None,
            instruction_width: instruction.chars().count(),
            provisional: false,
        }
    }
}

/// Everything one build reports while it walks a `Dockerfile` or pulls an OCI image.
#[derive(Debug)]
pub enum BuildEvent<'a> {
    /// One line of child output, tagged with the stream that produced it.
    Line {
        /// The stream the line came from.
        stream: Stream,
        /// The line without its terminator.
        text: &'a str,
    },
    /// Engine narration (seal/cache/digest bookkeeping); never step output, never a capture line.
    Note {
        /// The one engine sentence.
        text: &'a str,
    },
    /// One instruction's lifecycle transition.
    Step(StepEvent<'a>),
}

/// Identity of the instruction currently executing, so its settle row repeats it.
struct StepIdentity {
    place: StepPlace,
    instruction: String,
    args: String,
    /// A pre-build pull announcement: its rows never count as a real instruction.
    provisional: bool,
}

/// Typed sink for one build or pull: child lines, engine notes, and instruction lifecycle rows
/// travel on distinct variants so bookkeeping never masquerades as child output.
pub struct StepLog<'a> {
    events: Option<&'a mut dyn FnMut(BuildEvent<'_>)>,
    current: Option<StepIdentity>,
    planned_width: usize,
}

impl StepLog<'static> {
    /// A silent sink for headless pulls and tests that do not inspect live events.
    #[must_use]
    pub const fn discard() -> Self {
        Self {
            events: None,
            current: None,
            planned_width: 0,
        }
    }
}

impl<'a> StepLog<'a> {
    /// Wraps the caller's typed sink, which receives child lines, engine notes, and step rows.
    pub fn with_events(events: &'a mut dyn FnMut(BuildEvent<'_>)) -> Self {
        Self {
            events: Some(events),
            current: None,
            planned_width: 0,
        }
    }

    /// Pins the widest instruction keyword in the image's walked plan so every row shares one
    /// args column; `0` keeps each row at its own keyword width.
    pub fn plan_width(&mut self, width: usize) {
        self.planned_width = width;
    }

    /// Emits one child line tagged with the stream that produced it.
    pub fn line(&mut self, stream: Stream, text: &str) {
        self.emit(BuildEvent::Line { stream, text });
    }

    /// Emits one engine narration line: the caller's renderer keeps it behind `-v`.
    pub fn note(&mut self, text: &str) {
        self.emit(BuildEvent::Note { text });
    }

    /// Re-opens the instruction identity a caller announced on an earlier [`StepLog`] for the same
    /// row, without emitting a second `started` frame.
    ///
    /// A caller that pauses around the log (probe a sealed cache, then pull) must settle on the
    /// same [`StepLog`] instance it announced on; this restores that identity so the pause never
    /// changes the row the later `done`/`failed` settles.
    pub fn resume_at(
        &mut self,
        place: StepPlace,
        instruction: &str,
        args: &str,
        provisional: bool,
    ) {
        self.current = Some(StepIdentity {
            place,
            instruction: instruction.to_owned(),
            args: args.to_owned(),
            provisional,
        });
    }

    /// Announces `instruction` before it executes, so a pull or context walk never looks frozen.
    ///
    /// `place` carries the instruction's index and `Dockerfile` line, so the caller can publish the
    /// step's record the moment it starts and a crash still leaves the right header behind.
    pub(crate) fn started(&mut self, place: StepPlace, instruction: &str, args: &str) {
        self.start(place, instruction, args, false);
    }

    /// Announces one standalone step (a `from:` pull) whose total is itself, before it is tried.
    pub fn announce(&mut self, instruction: &str, args: &str) {
        self.announce_at(StepPlace::new(1, 1, 0), instruction, args);
    }

    /// Announces a pre-build row at `place`, so an external `COPY --from` pull still carries the
    /// `Dockerfile` line it serves.
    ///
    /// The row is provisional: it opens the live row early but never counts as a built
    /// instruction, and the real instruction at `place` later supersedes it.
    pub fn announce_at(&mut self, place: StepPlace, instruction: &str, args: &str) {
        self.start(place, instruction, args, true);
    }

    fn start(&mut self, place: StepPlace, instruction: &str, args: &str, provisional: bool) {
        let mut event = StepEvent::new(
            place,
            instruction,
            args,
            StepState::Started,
            Duration::ZERO,
            None,
        );
        event.provisional = provisional;
        event.instruction_width = event.instruction_width.max(self.planned_width);
        self.emit(BuildEvent::Step(event));
        self.current = Some(StepIdentity {
            place,
            instruction: instruction.to_owned(),
            args: args.to_owned(),
            provisional,
        });
    }

    /// Announces one post-execution tail phase without settling or restarting the running row.
    ///
    /// The identity is retained, so the following `done`/`failed` still repeats this instruction;
    /// the caller's renderer keeps the row's own clock ticking across the phase.
    pub fn tail(&mut self, phase: TailPhase) {
        let Some(identity) = self.current.take() else {
            return;
        };
        let mut event = StepEvent::new(
            identity.place,
            &identity.instruction,
            &identity.args,
            StepState::Tail,
            Duration::ZERO,
            None,
        );
        event.phase = Some(phase);
        event.provisional = identity.provisional;
        event.instruction_width = event.instruction_width.max(self.planned_width);
        self.emit(BuildEvent::Step(event));
        self.current = Some(identity);
    }

    /// Settles the running instruction with the wall time it took.
    pub fn done(&mut self, elapsed: Duration) {
        self.settle(StepState::Done, elapsed, None);
    }

    /// Settles the running instruction as failed, keeping a typed child exit code.
    pub fn failed(&mut self, elapsed: Duration, exit_code: Option<i32>) {
        self.settle(StepState::Failed, elapsed, exit_code);
    }

    fn settle(&mut self, state: StepState, elapsed: Duration, exit_code: Option<i32>) {
        debug_assert!(
            self.current.is_some(),
            "a build step settled before it started"
        );
        let Some(identity) = self.current.take() else {
            return;
        };
        let mut event = StepEvent::new(
            identity.place,
            &identity.instruction,
            &identity.args,
            state,
            elapsed,
            exit_code,
        );
        event.provisional = identity.provisional;
        event.instruction_width = event.instruction_width.max(self.planned_width);
        self.emit(BuildEvent::Step(event));
    }

    fn emit(&mut self, event: BuildEvent<'_>) {
        if let Some(events) = self.events.as_deref_mut() {
            events(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{BuildEvent, StepLog, StepPlace, StepState};
    use crate::runtime::Stream;
    use slipfork_world::snapshot::TailPhase;

    #[test]
    fn engine_notes_and_child_lines_stay_on_distinct_variants() {
        let mut notes = Vec::new();
        let mut lines = Vec::new();
        let mut steps = Vec::new();
        {
            let mut bus = |event: BuildEvent<'_>| match event {
                BuildEvent::Note { text } => notes.push(text.to_owned()),
                BuildEvent::Line { stream, text } => lines.push((stream, text.to_owned())),
                BuildEvent::Step(step) => {
                    steps.push((
                        step.instruction.to_owned(),
                        step.args.to_owned(),
                        step.line,
                        step.state,
                        step.phase,
                    ));
                }
            };
            let mut log = StepLog::with_events(&mut bus);
            log.started(StepPlace::new(1, 1, 1), "FROM", "python:3.13-slim");
            log.note("sealed sha256:deadbeef: reusing cache");
            log.line(Stream::Stdout, "child-out");
            log.tail(TailPhase::Sealing);
            log.done(Duration::from_millis(12));
        }
        assert_eq!(notes, ["sealed sha256:deadbeef: reusing cache"]);
        assert_eq!(lines, [(Stream::Stdout, "child-out".to_owned())]);
        assert_eq!(
            steps,
            [
                (
                    "FROM".to_owned(),
                    "python:3.13-slim".to_owned(),
                    1,
                    StepState::Started,
                    None
                ),
                (
                    "FROM".to_owned(),
                    "python:3.13-slim".to_owned(),
                    1,
                    StepState::Tail,
                    Some(TailPhase::Sealing)
                ),
                (
                    "FROM".to_owned(),
                    "python:3.13-slim".to_owned(),
                    1,
                    StepState::Done,
                    None
                ),
            ]
        );
        assert_eq!(StepState::Tail.as_str(), "tail");

        let mut silent = StepLog::discard();
        silent.started(StepPlace::new(1, 1, 1), "FROM", "alpine:3.20");
        silent.note("sealed sha256:00: reusing cache");
        silent.tail(TailPhase::Publishing);
        silent.done(Duration::ZERO);
    }
}
