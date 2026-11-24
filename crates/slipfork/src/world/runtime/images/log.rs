//! Build-log tee: every image build line reaches the live boot sink **and**
//! `<store>/logs/<world>/<image>.log`, so `slipfork logs <image>` reads failed-build output.

use std::io::{self, Write};
use std::path::PathBuf;

use slipfork_world::{LogStore, pod_log};

use crate::world::supervise::strip_ansi;

/// A `Write` tee: build progress goes to the live boot sink **and** to
/// `<store>/logs/<world>/<image>.log`, so `slipfork logs <image>` reads failed-build output.
pub(super) struct BuildLog<'a> {
    sink: &'a mut dyn Write,
    dir: PathBuf,
    id: String,
    pending: Vec<u8>,
}

impl<'a> BuildLog<'a> {
    /// Taps `store`'s world capture directory for `id`, so no caller recomputes a logs path.
    pub(super) fn new(sink: &'a mut dyn Write, store: &LogStore, id: &str) -> Self {
        Self {
            sink,
            dir: store.world_logs(),
            id: id.to_owned(),
            pending: Vec::new(),
        }
    }

    /// Writes one cleaned, newline-terminated line to the live sink and the `slipfork logs`
    /// capture. The sink (`EventLog`) is line-buffered: without `\n` it would hold every build
    /// line — including the failure cause — until an unrelated flush, so the run capture stays empty.
    fn emit(&mut self, line: &[u8]) -> io::Result<()> {
        let Some(text) = clean_line(line) else {
            return Ok(());
        };
        self.sink.write_all(text.as_bytes())?;
        self.sink.write_all(b"\n")?;
        let _ = pod_log::append(&self.dir, &self.id, text.as_bytes());
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use slipfork_world::{LogStore, WorldName};

    use super::BuildLog;

    fn temp(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory")
    }

    #[test]
    fn a_failed_run_tees_its_output_and_error_into_the_image_log() {
        let root = temp("image-log");
        let store = LogStore::at(
            root.path(),
            WorldName::parse("demo").expect("valid world name"),
        );
        let mut mirror = Vec::new();
        let mut tee = BuildLog::new(&mut mirror, &store, "frontend");
        tee.write_all(b"step 1/2 npm ci\nstep 2/2 npm run build\nnpm ERR! missing script: build")
            .expect("stream the failing RUN output");
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
            !captured.contains("next:"),
            "the capture carries no embedded remedy:\n{captured}"
        );
        assert_eq!(
            captured.matches("Dockerfile:12").count(),
            1,
            "the error lands once:\n{captured}"
        );

        let mirror = String::from_utf8(mirror).expect("the live sink mirrors UTF-8 output");
        assert!(
            mirror.contains("npm ERR! missing script: build\n"),
            "the sink receives a newline-terminated line: {mirror:?}"
        );
        assert!(
            mirror
                .contains("Dockerfile:12: `RUN npm run build` failed: `crun` exited with code 1\n"),
            "the failure cause reaches the sink once, framed: {mirror:?}"
        );
        assert!(
            !mirror.contains("next:"),
            "a remedy never reaches the sink: {mirror:?}"
        );
        assert!(
            !captured.trim().is_empty(),
            "the capture is never empty after a failure"
        );
    }
}
