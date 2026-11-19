//! Presentation of `slipfork_world::LogChunk`s on the caller's two inherited streams.
//!
//! Attribution and ordering are **data from `slipforkd`**, never a renderer guess: every chunk
//! already carries the component/hook id that produced its bytes (`LogChunk::target`) and the
//! instant the engine read them (`LogChunk::ts_ms`), and the daemon merges every source by that
//! capture time. This module only decides presentation (`docs/design/dx-0.1.0-ux.md` §8.3):
//!
//! - **Fan-out** (`slipfork logs`, no explicit target) — every line is introduced by a gutter of
//!   the form `<target><pad> │ ` (`api    │ listening`, `api:pre-deploy │ migrated`), with the
//!   target coloured stably so interleaved sources stay attributable at a glance.
//! - **One named target** (`slipfork logs api`) — the bytes are written through unchanged: no
//!   gutter, byte-exact, so a pipe into `grep` / `jq` sees exactly what the service wrote.
//!   `-v` forces the gutter even for a single target.
//! - **Restart divider** — the daemon's `event` chunk renders as a dimmed
//!   `── restarted <time>Z ──` line, so a follower keeps its place across `restart` and
//!   `down` + `up`. Every target's partial line is flushed first, so pre-restart bytes can never
//!   appear after the divider.
//!
//! Colour is **injected**, never probed: the composition root resolves `NO_COLOR`/TTY once and
//! passes the decision in, so `--json`, a pipe, and `NO_COLOR=1` carry zero ANSI bytes and the
//! coloured branch is deterministically testable. `--json` bypasses the line buffer entirely and
//! emits the one NDJSON document `{"ts_ms","target","stream","text"}`.

mod renderer;

pub use renderer::Renderer;

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    use slipfork_world::{LogChunk, StreamSource};

    use super::Renderer;

    /// A write handle the document-contract tests read back after the fact.
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

    fn chunk(target: &str, bytes: &[u8]) -> LogChunk {
        LogChunk::output(
            target,
            1,
            slipfork_world::StreamChunk {
                source: StreamSource::Stdout,
                bytes: bytes.to_vec(),
            },
        )
    }

    fn json_renderer() -> (Renderer, Capture) {
        let out = Capture::default();
        let err = Capture::default();
        (
            Renderer::new(true, true, false, Box::new(out.clone()), Box::new(err)),
            out,
        )
    }

    #[test]
    fn logs_one_named_target_writes_raw_bytes_unchanged() {
        let out = Capture::default();
        let mut renderer = Renderer::new(
            false,
            false,
            false,
            Box::new(out.clone()),
            Box::new(Capture::default()),
        );
        renderer.chunk(chunk("api", b"no trailing newline"));
        renderer.chunk(chunk("api", b"\nsecond\xffline\n"));
        renderer.finish();
        let rendered = out.0.lock().expect("capture").clone();
        assert_eq!(rendered, b"no trailing newline\nsecond\xffline\n");
        assert!(!String::from_utf8_lossy(&rendered).contains("\u{1b}["));
    }

    #[test]
    fn logs_json_renders_a_divider_as_an_event_document() {
        let (mut renderer, out) = json_renderer();
        renderer.chunk(LogChunk::divider(3_600_000));
        renderer.finish();
        let document: serde_json::Value =
            serde_json::from_str(out.text().trim_end()).expect("one JSON document");
        assert_eq!(document["ts_ms"], 3_600_000_u64);
        assert_eq!(document["stream"], "event");
        assert_eq!(document["target"], "");
        assert_eq!(document["text"], "── restarted 01:00:00Z ──");
    }

    #[test]
    fn logs_json_mode_is_the_one_document_per_chunk_ndjson_shape() {
        let (mut renderer, out) = json_renderer();
        renderer.chunk(chunk("api", b"one\ntwo\n"));
        renderer.finish();
        let document: serde_json::Value =
            serde_json::from_str(out.text().trim_end()).expect("one JSON document");
        assert_eq!(document["ts_ms"], 1);
        assert_eq!(document["target"], "api");
        assert_eq!(document["stream"], "stdout");
        assert_eq!(document["text"], "one\ntwo\n");
        assert_eq!(document.as_object().expect("object").len(), 4);
    }
}
