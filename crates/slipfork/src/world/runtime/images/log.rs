//! Build-log tee: every image build line reaches the live boot sink **and**
//! `<store>/logs/<world>/<image>.log`, so `slipfork logs <image>` reads failed-build output.

use std::io::{self, Write};
use std::path::PathBuf;

use slipfork_world::{LogStore, pod_log};

use crate::world::supervise::{BootSink, strip_ansi};

/// A build-log tee: build progress goes to the live boot sink **and** to
/// `<store>/logs/<world>/<image>.log`, so `slipfork logs <image>` reads failed-build output.
pub(super) struct BuildLog<'a> {
    sink: &'a mut BootSink,
    dir: PathBuf,
    id: String,
    pending: Vec<u8>,
}

impl<'a> BuildLog<'a> {
    /// Taps `store`'s world capture directory for `id`, so no caller recomputes a logs path.
    pub(super) fn new(sink: &'a mut BootSink, store: &LogStore, id: &str) -> Self {
        Self {
            sink,
            dir: store.world_logs(),
            id: id.to_owned(),
            pending: Vec::new(),
        }
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

    /// Writes one untagged line as stdout; the `Write` path keeps piping raw bytes.
    fn emit(&mut self, line: &[u8]) -> io::Result<()> {
        self.append_stream(
            slipfork_image::Stream::Stdout,
            &String::from_utf8_lossy(line),
        );
        Ok(())
    }

    /// Flushes a trailing unterminated line into the capture file.
    pub(super) fn finish(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        let _ = self.emit(&pending);
    }

    /// Flushes the buffered tail and appends every line of a failed build's error, so the run
    /// capture is never empty after a failure. A `next:` remedy is not captured output: skipping it
    /// keeps `logs <image>` from printing a second, circular hint.
    pub(super) fn fail(&mut self, error: &str) {
        self.finish();
        for line in error.lines() {
            if line.trim_start().starts_with("next:") {
                continue;
            }
            let _ = self.emit(line.as_bytes());
        }
    }
}

impl Write for BuildLog<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buffer);
        let mut start = 0;
        while let Some(offset) = self.pending[start..].iter().position(|byte| *byte == b'\n') {
            let end = start + offset;
            let line = self.pending[start..end].to_vec();
            self.emit(&line)?;
            start = end + 1;
        }
        if start > 0 {
            self.pending.drain(..start);
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush()
    }
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

    use super::BuildLog;
    use crate::world::supervise::{BootSink, Emitter};

    fn temp(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory")
    }

    #[test]
    fn a_failed_run_tees_its_streams_and_error_into_the_image_log() {
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
        tee.fail(
            "build `frontend` failed: Dockerfile:12: `RUN npm run build` failed: `crun` exited with code 1\nnext: fix the command above; then rerun `slipfork up`",
        );

        drop(tee);

        let captured = fs::read_to_string(store.world_logs().join("frontend.log"))
            .expect("the failed build must leave a log capture");
        for fragment in [
            "step 2/2 npm run build",
            "npm ERR! missing script: build",
            "Dockerfile:12: `RUN npm run build` failed",
        ] {
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
        assert!(
            !captured.contains("next:"),
            "the capture carries no embedded remedy:\n{captured}"
        );
        assert_eq!(
            captured.matches("Dockerfile:12").count(),
            1,
            "the error lands once:\n{captured}"
        );
        assert!(
            !captured.trim().is_empty(),
            "the capture is never empty after a failure"
        );
    }
}
