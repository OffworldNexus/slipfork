//! Build-log tee: every image build line reaches the live boot sink **and**
//! `<store>/logs/<world>/<image>.log`, so `slipfork logs <image>` reads failed-build output.

use std::path::{Path, PathBuf};

use slipfork_image::{Error as ImageError, StepEvent, StepTrace};
use slipfork_world::run_log::RunNote;
use slipfork_world::{ImageNode, LogStore, pod_log};

use crate::world::runtime::capture::strip_ansi;
use crate::world::supervise::BootSink;

/// A build-log tee: build progress goes to the live boot sink **and** to
/// `<store>/logs/<world>/<image>.log`, so `slipfork logs <image>` reads failed-build output.
pub(super) struct BuildLog<'a> {
    sink: &'a mut BootSink,
    dir: PathBuf,
    id: String,
}

impl<'a> BuildLog<'a> {
    /// Taps `store`'s world capture directory for `id`, so no caller recomputes a logs path.
    pub(super) fn new(sink: &'a mut BootSink, store: &LogStore, id: &str) -> Self {
        Self {
            sink,
            dir: store.world_logs(),
            id: id.to_owned(),
        }
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
}

/// The failing instruction named by a typed image error; the engine already carries the phase,
/// the as-written command, and the exit code, so nothing is re-parsed here.
pub(super) fn failed_note(dockerfile: &Path, error: &ImageError) -> RunNote {
    let label = file_label(dockerfile);
    match error {
        ImageError::StepFailed {
            line,
            phase,
            instruction,
            exit_code,
            ..
        } if *line > 0 => RunNote {
            phase: Some((*phase).to_owned()),
            instruction: Some(instruction.to_string()),
            source: Some(format!("{label}:{line}")),
            exit_code: *exit_code,
        },
        _ => RunNote {
            phase: Some(label.clone()),
            instruction: None,
            source: Some(label),
            exit_code: None,
        },
    }
}

/// A registry pull records the reference it fetched instead of a Dockerfile line.
pub(super) fn pull_note(reference: &str) -> RunNote {
    RunNote {
        phase: Some("pull".to_owned()),
        instruction: Some(reference.to_owned()),
        source: None,
        exit_code: None,
    }
}

/// A successful build names the last instruction it applied, or its Dockerfile when it applied
/// none, or the pulled reference.
pub(super) fn success_note(nodes: &[ImageNode], image: &str, last: Option<&StepTrace>) -> RunNote {
    let Some(node) = nodes.iter().find(|node| node.id == image) else {
        return RunNote::default();
    };
    if let Some(reference) = &node.from {
        return pull_note(reference);
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
        },
        None => RunNote {
            phase: Some(label.clone()),
            instruction: None,
            source: Some(label),
            exit_code: Some(0),
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
    use std::sync::{Arc, Mutex};

    use slipfork_world::{LogStore, WorldName};

    use super::{BuildLog, failed_note, success_note};
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
        let mut tee = BuildLog::new(&mut sink, &store, "frontend");
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

    #[test]
    fn a_failing_run_names_its_instruction_and_source_line() {
        let root = temp("image-note");
        let dockerfile = root.path().join("Dockerfile");
        fs::write(
            &dockerfile,
            "FROM scratch\nRUN sh -c 'echo out; exit 3'\nWORKDIR /app\n",
        )
        .expect("write a Dockerfile");
        let note = failed_note(
            &dockerfile,
            &slipfork_image::Error::StepFailed {
                line: 2,
                phase: "run",
                instruction: "sh -c 'echo out; exit 3'".into(),
                message: "the command exited with a non-zero status".to_owned(),
                exit_code: Some(3),
                runtime: Some("crun exited with code 3".into()),
                next: "fix the command".to_owned(),
            },
        );
        assert_eq!(note.phase.as_deref(), Some("run"));
        assert_eq!(
            note.instruction.as_deref(),
            Some("sh -c 'echo out; exit 3'")
        );
        assert_eq!(note.source.as_deref(), Some("Dockerfile:2"));
        assert_eq!(note.exit_code, Some(3));
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
