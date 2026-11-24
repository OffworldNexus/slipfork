//! Build-log tee: every image build line reaches the live boot sink **and**
//! `<store>/logs/<world>/<image>.log`, so `slipfork logs <image>` reads failed-build output.

use std::path::{Path, PathBuf};

use slipfork_image::{BuildEvent, Error as ImageError, StepEvent, StepLog, StepState, StepTrace};
use slipfork_world::LogStore;
use slipfork_world::run_log::RunNote;
use slipfork_world::{ImageNode, pod_log};

use super::record::RunNotes;
use crate::world::runtime::capture::strip_ansi;
use crate::world::supervise::BootSink;

/// A build-log tee: build progress goes to the live boot sink **and** to
/// `<store>/logs/<world>/<image>.log`, so `slipfork logs <image>` reads failed-build output.
pub(super) struct BuildLog<'a> {
    sink: &'a mut BootSink,
    notes: RunNotes,
    run_id: Option<String>,
    dir: PathBuf,
    id: String,
}

impl<'a> BuildLog<'a> {
    /// Taps `store`'s world capture directory for `id` and the minted `run_id` it may annotate.
    pub(super) fn new(
        sink: &'a mut BootSink,
        store: &LogStore,
        run_id: Option<&str>,
        id: &str,
    ) -> Self {
        Self {
            sink,
            notes: RunNotes::new(store),
            run_id: run_id.map(str::to_owned),
            dir: store.world_logs(),
            id: id.to_owned(),
        }
    }

    /// Runs `task` with a typed bus: child lines are teed, each step's record is published at
    /// START, and the running instruction is remembered so an engine failure can explain itself.
    pub(super) fn run<T>(
        &mut self,
        dockerfile: Option<&Path>,
        task: impl FnOnce(&mut StepLog<'_>) -> slipfork_image::Result<T>,
    ) -> BuildProgress<T> {
        let mut started = None;
        let mut captured = false;
        let outcome = {
            let mut bus = |event: BuildEvent<'_>| match event {
                BuildEvent::Line { stream, text } => {
                    captured = true;
                    self.append_stream(stream, text);
                }
                BuildEvent::Note { text } => self.note(text),
                BuildEvent::Step(step) => {
                    if step.state == StepState::Started {
                        self.divider(&step);
                        let note = started_note(dockerfile, &step);
                        let _ = self.notes.started(self.run_id.as_deref(), &note);
                        started = Some(note);
                        captured = false;
                    }
                    self.step(&step);
                }
            };
            task(&mut StepLog::with_events(&mut bus))
        };
        BuildProgress {
            outcome,
            started,
            captured,
        }
    }

    /// Writes the engine's raw cause as the failing step's one `┃` line, when a step started and
    /// captured nothing of its own.
    ///
    /// A child process that printed (or exited with a code) already explains itself; an engine-side
    /// failure — a `FROM` pull, `COPY`, context walk, a missing `crun`, a spawn error — has no child
    /// to do that, so its cause becomes the step's stderr line. A failure before step 1 (a parse or
    /// stage-graph error) has no step row at all, so it writes nothing and the verdict carries the
    /// reason instead.
    pub(super) fn explain<T>(
        &mut self,
        error: &ImageError,
        progress: &BuildProgress<T>,
    ) -> Option<String> {
        if progress.started.is_none()
            || progress.captured
            || error.exit_code().is_some()
            || error.runtime().is_some()
        {
            return None;
        }
        let reason = error.cause();
        self.append_stream(slipfork_image::Stream::Stderr, &reason);
        Some(reason)
    }

    /// Forwards one engine narration line to the live sink; it never enters a capture.
    pub(super) fn note(&mut self, text: &str) {
        self.sink.engine_note(text);
    }

    /// Forwards one instruction transition to the live sink, pinned to this image id.
    pub(super) fn step(&mut self, event: &StepEvent<'_>) {
        self.sink.step_event(&self.id, event);
    }

    /// Writes one cleaned line tagged with its `crun` stream to the live sink and the capture.
    pub(super) fn append_stream(&mut self, stream: slipfork_image::Stream, line: &str) {
        let Some(text) = clean_line(line.as_bytes()) else {
            return;
        };
        let stream = world_stream(stream);
        self.sink.append_stream(stream, &text);
        let _ = pod_log::append_stream(&self.dir, &self.id, stream, text.as_bytes());
    }

    /// Writes one instruction row to the image capture and the run capture at START.
    ///
    /// A build capture holds child output only, so without this row the replay of a multi-step
    /// build is one unexplained block under the failing instruction's header. The row is engine
    /// bookkeeping, never child output: it never sets the `captured` flag, and no row is written
    /// when the step settles. The run capture already owns its live sink, so the row goes through
    /// [`BootSink::step_row`] instead of opening a second writer on the same file.
    fn divider(&mut self, event: &StepEvent<'_>) {
        let text = step_label(event);
        if let Ok(mut sink) = pod_log::LogSink::open(&self.dir, &self.id) {
            let _ = sink.append(pod_log::LogKind::Step, &text);
        }
        if self.run_id.is_some() {
            self.sink.step_row(&text);
        }
    }
}

/// One instruction's capture row: `<index>/<total> <KEYWORD> <args>`, ANSI-free and single-line.
fn step_label(event: &StepEvent<'_>) -> String {
    let raw = format!(
        "{}/{} {} {}",
        event.index, event.total, event.instruction, event.args
    );
    let stripped = strip_ansi(&raw);
    // A heredoc `RUN` carries newlines and tabs; the capture frames rows on `\t`, so every
    // control-space character collapses to one space and the row stays physical.
    let mut label = String::with_capacity(stripped.len());
    let mut gap = false;
    for ch in stripped.chars() {
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
pub(super) fn started_note(dockerfile: Option<&Path>, event: &StepEvent<'_>) -> RunNote {
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
pub(super) fn failed_note(dockerfile: &Path) -> RunNote {
    RunNote {
        source: Some(file_label(dockerfile)),
        ..RunNote::default()
    }
}

/// A `from:` image pull records the reference it fetched, under the one `FROM` verb.
pub(super) fn from_note(reference: &str) -> RunNote {
    RunNote {
        phase: Some("from".to_owned()),
        instruction: Some(reference.to_owned()),
        ..RunNote::default()
    }
}

/// A successful build names the last instruction it applied, or its Dockerfile when it applied
/// none, or the pulled reference.
pub(super) fn success_note(nodes: &[ImageNode], image: &str, last: Option<&StepTrace>) -> RunNote {
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

/// What one build produced: its own result plus the step facts a failure needs to explain itself.
pub(super) struct BuildProgress<T> {
    /// The build's own result.
    pub(super) outcome: slipfork_image::Result<T>,
    /// The note the last started step published at START, so a failure names it.
    pub(super) started: Option<RunNote>,
    /// Whether the running step captured any child output of its own.
    pub(super) captured: bool,
}

/// The bare file name a source location starts with (`Dockerfile`, not `/ctx/Dockerfile`).
fn file_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Strips CSI escapes and bare `\r` progress from one raw child line, so neither the `--json`
/// stream nor `<image>.log` ever carries terminal control bytes.
fn clean_line(bytes: &[u8]) -> Option<String> {
    let raw = String::from_utf8_lossy(bytes);
    let text = strip_ansi(&raw).replace('\r', "");
    (!text.trim().is_empty()).then_some(text)
}

/// Maps the image crate's stream onto the world's wire/capture stream.
fn world_stream(stream: slipfork_image::Stream) -> slipfork_world::Stream {
    match stream {
        slipfork_image::Stream::Stdout => slipfork_world::Stream::Stdout,
        slipfork_image::Stream::Stderr => slipfork_world::Stream::Stderr,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use slipfork_image::{StepEvent, StepPlace, StepState};
    use slipfork_world::{LogStore, WorldName};

    use super::{BuildLog, failed_note, started_note, step_label, success_note};
    use crate::world::supervise::{BootSink, Emitter};

    fn temp(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory")
    }

    #[test]
    fn a_run_capture_keeps_only_the_streams_it_saw() {
        let root = temp("image-log");
        let store = LogStore::at(
            root.path(),
            WorldName::parse("demo").expect("valid world name"),
        );
        let mut sink = BootSink::for_step(
            Arc::new(Mutex::new(Emitter::closed())),
            1,
            2,
            "image",
            "frontend",
        );
        let mut tee = BuildLog::new(&mut sink, &store, None, "frontend");
        tee.append_stream(slipfork_image::Stream::Stdout, "step 1/2 npm ci");
        tee.append_stream(slipfork_image::Stream::Stdout, "step 2/2 npm run build");
        tee.append_stream(
            slipfork_image::Stream::Stderr,
            "npm ERR! missing script: build",
        );
        drop(tee);

        let captured = fs::read_to_string(store.world_logs().join("frontend.log"))
            .expect("the failed build must leave a log capture");
        for fragment in ["step 2/2 npm run build", "npm ERR! missing script: build"] {
            assert!(
                captured.contains(fragment),
                "missing `{fragment}` in:\n{captured}"
            );
        }
        assert!(
            captured.contains("\tO\tstep 1/2 npm ci"),
            "stdout rows keep their `O` marker:\n{captured}"
        );
        assert!(
            captured.contains("\tE\tnpm ERR! missing script: build"),
            "stderr rows keep their `E` marker:\n{captured}"
        );
        for absent in ["failed:", "next:", "Dockerfile:"] {
            assert!(
                !captured.contains(absent),
                "a capture is command output only, saw `{absent}`:\n{captured}"
            );
        }
    }

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
            std::time::Duration::ZERO,
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

    #[test]
    fn a_successful_build_names_its_last_instruction_not_the_dockerfile_label() {
        let node = slipfork_world::ImageNode {
            id: "web".to_owned(),
            directory: std::path::PathBuf::from("."),
            dockerfile: Some(std::path::PathBuf::from("docker/Dockerfile")),
            from: None,
            line: 3,
        };
        let last = slipfork_image::StepTrace {
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
}
