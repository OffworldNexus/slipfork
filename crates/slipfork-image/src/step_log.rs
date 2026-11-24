//! Live build reporting: child lines plus the `started`/`done`/`failed` row of every instruction.

use std::time::Duration;

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
            Self::Done => "done",
            Self::Failed => "failed",
        }
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
    /// Lifecycle state.
    pub state: StepState,
    /// Time the instruction executed; zero while it is still running.
    pub elapsed: Duration,
    /// Child process exit code, when a failed `RUN` carried one.
    pub exit_code: Option<i32>,
}

impl<'a> StepEvent<'a> {
    /// Builds one row for `instruction` at `index` of `total`.
    #[must_use]
    pub const fn new(
        index: usize,
        total: usize,
        instruction: &'a str,
        args: &'a str,
        state: StepState,
        elapsed: Duration,
        exit_code: Option<i32>,
    ) -> Self {
        Self {
            index,
            total,
            instruction,
            args,
            state,
            elapsed,
            exit_code,
        }
    }
}

/// Everything one build reports while it walks a `Dockerfile`.
#[derive(Debug)]
pub enum BuildEvent<'a> {
    /// One line of child output, tagged with the stream that produced it.
    Line {
        /// The stream the line came from.
        stream: Stream,
        /// The line without its terminator.
        text: &'a str,
    },
    /// One instruction's lifecycle transition.
    Step(StepEvent<'a>),
}

/// Identity of the instruction currently executing, so its settle row repeats it.
struct StepIdentity {
    index: usize,
    total: usize,
    instruction: String,
    args: String,
}

/// Typed sink for one step's live build: every line keeps its stream, and every instruction
/// reports `started` before it executes and `done`/`failed` when it settles.
pub struct StepLog<'a> {
    events: &'a mut dyn FnMut(BuildEvent<'_>),
    current: Option<StepIdentity>,
    pending: Vec<u8>,
}

impl<'a> StepLog<'a> {
    /// Wraps the caller's typed sink, which receives child lines **and** instruction rows.
    pub fn with_events(events: &'a mut dyn FnMut(BuildEvent<'_>)) -> Self {
        Self {
            events,
            current: None,
            pending: Vec::new(),
        }
    }

    /// Emits one line tagged with the stream that produced it.
    pub fn line(&mut self, stream: Stream, text: &str) {
        (self.events)(BuildEvent::Line { stream, text });
    }

    /// Announces `instruction` before it executes, so a pull or context walk never looks frozen.
    pub(crate) fn started(&mut self, index: usize, total: usize, instruction: &str, args: &str) {
        (self.events)(BuildEvent::Step(StepEvent::new(
            index,
            total,
            instruction,
            args,
            StepState::Started,
            Duration::ZERO,
            None,
        )));
        self.current = Some(StepIdentity {
            index,
            total,
            instruction: instruction.to_owned(),
            args: args.to_owned(),
        });
    }

    /// Settles the running instruction with the wall time it took.
    pub(crate) fn done(&mut self, elapsed: Duration) {
        self.settle(StepState::Done, elapsed, None);
    }

    /// Settles the running instruction as failed, keeping a typed child exit code.
    pub(crate) fn failed(&mut self, elapsed: Duration, exit_code: Option<i32>) {
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
        (self.events)(BuildEvent::Step(StepEvent::new(
            identity.index,
            identity.total,
            &identity.instruction,
            &identity.args,
            state,
            elapsed,
            exit_code,
        )));
    }
}

/// The puller's progress lines become ordinary stdout lines on the same typed sink.
impl std::io::Write for StepLog<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(buffer);
        while let Some(position) = self.pending.iter().position(|byte| *byte == b'\n') {
            let rest = self.pending.split_off(position + 1);
            let line = std::mem::replace(&mut self.pending, rest);
            let text = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            self.line(Stream::Stdout, text.trim_end_matches('\r'));
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            let text = String::from_utf8_lossy(&line).into_owned();
            self.line(Stream::Stdout, text.trim_end_matches('\r'));
        }
        Ok(())
    }
}
