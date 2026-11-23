//! Build-log tee: every image build line reaches the live boot sink **and**
//! `<store>/logs/<world>/<image>.log`, so `slipfork logs <image>` reads failed-build output.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use slipfork_world::pod_log;

use crate::world::supervise::strip_ansi;

/// A `Write` tee: build progress goes to the live boot sink **and** to
/// `<store>/logs/<world>/<image>.log`, so `slipfork logs <image>` reads failed-build output.
pub(super) struct BuildLog<'a> {
    sink: &'a mut dyn Write,
    logs_root: PathBuf,
    id: String,
    pending: Vec<u8>,
}

impl<'a> BuildLog<'a> {
    pub(super) fn new(sink: &'a mut dyn Write, logs_root: &Path, id: &str) -> Self {
        Self {
            sink,
            logs_root: logs_root.to_path_buf(),
            id: id.to_owned(),
            pending: Vec::new(),
        }
    }

    /// Writes one cleaned line to the live sink and the `slipfork logs` capture.
    fn emit(&mut self, line: &[u8]) -> io::Result<()> {
        let Some(text) = clean_line(line) else {
            return Ok(());
        };
        self.sink.write_all(text.as_bytes())?;
        let _ = pod_log::append(&self.logs_root, &self.id, text.as_bytes());
        Ok(())
    }

    /// Flushes a trailing unterminated line into the capture file.
    pub(super) fn finish(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        let _ = self.emit(&pending);
    }

    /// Flushes the buffered tail and appends every line of a failed build's error, so
    /// `<store>/logs/<world>/<image>.log` is never empty after a failure.
    pub(super) fn fail(&mut self, error: &str) {
        self.finish();
        for line in error.lines() {
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
        let logs_root = root.path().join("logs");
        let mut mirror = Vec::new();
        let mut tee = BuildLog::new(&mut mirror, &logs_root, "frontend");
        tee.write_all(b"step 1/2 npm ci\nstep 2/2 npm run build\nnpm ERR! missing script: build")
            .expect("stream the failing RUN output");
        tee.fail(
            "build `frontend` failed: Dockerfile:12: `RUN npm run build` failed: `crun` exited with code 1\nnext: run `slipfork logs frontend`",
        );

        drop(tee);

        let captured = fs::read_to_string(logs_root.join("frontend.log"))
            .expect("the failed build must leave a log capture");
        for fragment in [
            "step 2/2 npm run build",
            "npm ERR! missing script: build",
            "Dockerfile:12: `RUN npm run build` failed",
            "next: run `slipfork logs frontend`",
        ] {
            assert!(
                captured.contains(fragment),
                "missing `{fragment}` in:\n{captured}"
            );
        }
        assert_eq!(
            captured.matches("Dockerfile:12").count(),
            1,
            "the error lands once:\n{captured}"
        );

        let mirror = String::from_utf8(mirror).expect("the live sink mirrors UTF-8 output");
        assert!(
            mirror.contains("npm ERR! missing script: build"),
            "{mirror}"
        );
        assert!(
            mirror.contains("Dockerfile:12: `RUN npm run build` failed"),
            "{mirror}"
        );
    }
}
