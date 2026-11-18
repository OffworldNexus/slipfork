//! The one `slipfork logs` renderer: attributed, line-buffered output over daemon
//! [`slipfork_engine::LogChunk`]s.
//!
//! Attribution is **data from `slipforkd`**, never a renderer guess: every chunk names the
//! component or hook id that produced its bytes (`LogChunk::target`), and this module only decides
//! where that name is shown (`docs/design/dx-0.1.0-ux.md` §8.3):
//!
//! - **Fan-out** (`slipfork logs`, no explicit target) — every line is prefixed with
//!   `<source>: ` (`api: listening`, `api:pre-deploy: migrated`), so interleaved sources stay
//!   attributable.
//! - **One named target** (`slipfork logs api`) — the bytes are written through unchanged: no
//!   prefix, byte-exact, so a pipe into `grep` / `jq` sees exactly what the service wrote.
//!
//! Prefixing is line-buffered per source: a line that spans several chunks is prefixed exactly
//! once, whole lines are emitted as soon as their newline arrives, and the trailing partial line is
//! flushed by [`Renderer::finish`] at EOF. A prefix is therefore never split from, or duplicated
//! on, the bytes it labels. `--json` bypasses the line buffer entirely: it keeps the one-document-
//! per-chunk NDJSON shape and carries the same structured `source` field.

use std::collections::BTreeMap;
use std::io::{self, Write};

use slipfork_engine::{LogChunk, StreamSource};

/// Render `slipforkd` log chunks to the caller's two inherited streams.
pub struct Renderer {
    json: bool,
    prefix: bool,
    out: Box<dyn Write>,
    err: Box<dyn Write>,
    pending: BTreeMap<String, PendingLine>,
}

/// The not-yet-newline-terminated tail of one source, per child stream.
#[derive(Default)]
struct PendingLine {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Renderer {
    /// Build the renderer over the caller's already-resolved streams (the composition root owns
    /// `stdout`/`stderr`; this module never touches them itself).
    ///
    /// `prefix` is `true` for a fan-out request: the daemon streams every retained source, so each
    /// line must say which one produced it. `json` selects the unchanged NDJSON document mode.
    pub fn new(json: bool, prefix: bool, out: Box<dyn Write>, err: Box<dyn Write>) -> Self {
        Self {
            json,
            prefix,
            out,
            err,
            pending: BTreeMap::new(),
        }
    }

    /// Render one attributed chunk.
    pub fn chunk(&mut self, log: LogChunk) {
        if self.json {
            let stream = match log.source {
                StreamSource::Stdout => "stdout",
                StreamSource::Stderr => "stderr",
            };
            let document = serde_json::json!({
                "stream": stream,
                "source": log.target,
                "text": String::from_utf8_lossy(&log.bytes),
            });
            let _ = writeln!(self.out, "{document}");
            let _ = self.out.flush();
            return;
        }
        if !self.prefix {
            let _ = self.write(log.source, &log.bytes);
            return;
        }
        let mut lines: Vec<Vec<u8>> = Vec::new();
        {
            let pending = self.pending.entry(log.target.clone()).or_default();
            let tail = match log.source {
                StreamSource::Stdout => &mut pending.stdout,
                StreamSource::Stderr => &mut pending.stderr,
            };
            tail.extend_from_slice(&log.bytes);
            while let Some(end) = tail.iter().position(|byte| *byte == b'\n') {
                lines.push(tail.drain(..=end).collect());
            }
        }
        for line in lines {
            let _ = self.write_prefixed(&log.target, log.source, &line);
        }
    }

    /// Flush every trailing partial line at EOF, each prefixed exactly once.
    pub fn finish(&mut self) {
        if self.json {
            return;
        }
        for (target, pending) in std::mem::take(&mut self.pending) {
            for (source, bytes) in [
                (StreamSource::Stdout, pending.stdout),
                (StreamSource::Stderr, pending.stderr),
            ] {
                if !bytes.is_empty() {
                    let _ = self.write_prefixed(&target, source, &bytes);
                }
            }
        }
    }

    fn write_prefixed(
        &mut self,
        target: &str,
        source: StreamSource,
        line: &[u8],
    ) -> io::Result<()> {
        self.write(source, target.as_bytes())?;
        self.write(source, b": ")?;
        self.write(source, line)
    }

    fn write(&mut self, source: StreamSource, bytes: &[u8]) -> io::Result<()> {
        let stream: &mut Box<dyn Write> = match source {
            StreamSource::Stdout => &mut self.out,
            StreamSource::Stderr => &mut self.err,
        };
        stream.write_all(bytes)?;
        stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    use slipfork_engine::StreamSource;

    use super::Renderer;

    /// A write handle every renderer test reads back after the fact.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().expect("capture").clone()).expect("utf-8 output")
        }
    }

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("capture").extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn chunk(target: &str, bytes: &[u8]) -> slipfork_engine::LogChunk {
        slipfork_engine::LogChunk::attribute(
            target,
            slipfork_engine::StreamChunk {
                source: StreamSource::Stdout,
                bytes: bytes.to_vec(),
            },
        )
    }

    fn renderer(prefix: bool) -> (Renderer, Capture, Capture) {
        let out = Capture::default();
        let err = Capture::default();
        (
            Renderer::new(false, prefix, Box::new(out.clone()), Box::new(err.clone())),
            out,
            err,
        )
    }

    #[test]
    fn logs_two_sources_prefix_every_line_with_its_own_source() {
        let (mut renderer, out, _) = renderer(true);
        renderer.chunk(chunk("api", b"listening on :8000\nready\n"));
        renderer.chunk(chunk("worker", b"job 1 done\njob 2 done\n"));
        renderer.chunk(chunk("api", b"GET / 200\n"));
        renderer.finish();
        assert_eq!(
            out.text(),
            "api: listening on :8000\napi: ready\nworker: job 1 done\nworker: job 2 done\napi: GET / 200\n"
        );
    }

    #[test]
    fn logs_one_named_source_writes_raw_bytes_unchanged() {
        let (mut renderer, out, _) = renderer(false);
        renderer.chunk(chunk("api", b"no trailing newline"));
        renderer.chunk(chunk("api", b"\nsecond\rline\n"));
        renderer.finish();
        assert_eq!(out.text(), "no trailing newline\nsecond\rline\n");
    }

    #[test]
    fn logs_a_line_split_across_chunks_is_prefixed_exactly_once() {
        let (mut renderer, out, _) = renderer(true);
        renderer.chunk(chunk("api", b"par"));
        renderer.chunk(chunk("api", b"tial "));
        renderer.chunk(chunk("worker", b"other\n"));
        renderer.chunk(chunk("api", b"line\n"));
        renderer.finish();
        assert_eq!(out.text(), "worker: other\napi: partial line\n");
    }

    #[test]
    fn logs_flushes_a_trailing_partial_line_at_eof() {
        let (mut renderer, out, _) = renderer(true);
        renderer.chunk(chunk("api", b"no newline yet"));
        assert_eq!(out.text(), "", "a partial line is never emitted mid-line");
        renderer.finish();
        assert_eq!(out.text(), "api: no newline yet");
    }

    #[test]
    fn logs_routes_stderr_lines_to_stderr_with_the_same_prefix() {
        let (mut renderer, out, err) = renderer(true);
        renderer.chunk(chunk("api", b"to stdout\n"));
        renderer.chunk(slipfork_engine::LogChunk {
            target: "api".to_owned(),
            source: StreamSource::Stderr,
            bytes: b"to stderr\n".to_vec(),
        });
        renderer.finish();
        assert_eq!(out.text(), "api: to stdout\n");
        assert_eq!(err.text(), "api: to stderr\n");
    }

    #[test]
    fn logs_json_mode_is_the_unchanged_ndjson_document_per_chunk() {
        let out = Capture::default();
        let err = Capture::default();
        let mut renderer = Renderer::new(true, true, Box::new(out.clone()), Box::new(err.clone()));
        renderer.chunk(chunk("api", b"one\ntwo\n"));
        renderer.finish();
        let document: serde_json::Value =
            serde_json::from_str(out.text().trim_end()).expect("one JSON document");
        assert_eq!(document["stream"], "stdout");
        assert_eq!(document["source"], "api");
        assert_eq!(document["text"], "one\ntwo\n");
        assert!(err.text().is_empty());
    }
}
