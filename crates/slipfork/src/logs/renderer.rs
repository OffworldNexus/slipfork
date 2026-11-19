//! The line renderer: dynamic gutters, the dimmed restart divider, and partial-line flushing.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

use owo_colors::{AnsiColors, OwoColorize};
use slipfork_world::{LogChunk, LogStream};

/// Render `slipforkd` log chunks to the caller's two inherited streams.
pub struct Renderer {
    json: bool,
    prefix: bool,
    color: bool,
    out: Box<dyn Write>,
    err: Box<dyn Write>,
    pending: BTreeMap<String, PendingLine>,
    seen_targets: BTreeSet<String>,
    /// Widest target seen so far (visible columns, never ANSI bytes).
    width: usize,
}

/// The not-yet-newline-terminated tail of one source, per child stream.
#[derive(Default)]
struct PendingLine {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Renderer {
    /// Build the renderer over the caller's already-resolved streams (the composition root owns
    /// `stdout`/`stderr` and the colour decision; this module never probes either).
    ///
    /// `prefix` is `true` for a fan-out request (or `-v` on a named target). `json` selects the
    /// NDJSON document mode; `color` permits the dimmed divider and the stable target palette.
    pub fn new(
        json: bool,
        prefix: bool,
        color: bool,
        out: Box<dyn Write>,
        err: Box<dyn Write>,
    ) -> Self {
        Self {
            json,
            prefix,
            color,
            out,
            err,
            pending: BTreeMap::new(),
            seen_targets: BTreeSet::new(),
            width: 0,
        }
    }

    /// Render one attributed chunk or daemon event.
    pub fn chunk(&mut self, log: LogChunk) {
        if self.json {
            let (stream, text) = match log.stream {
                LogStream::Event => ("event", divider_text(log.ts_ms)),
                LogStream::Stdout => ("stdout", String::from_utf8_lossy(&log.bytes).into_owned()),
                LogStream::Stderr => ("stderr", String::from_utf8_lossy(&log.bytes).into_owned()),
            };
            let document = serde_json::json!({
                "ts_ms": log.ts_ms,
                "target": log.target,
                "stream": stream,
                "text": text,
            });
            let _ = writeln!(self.out, "{document}");
            let _ = self.out.flush();
            return;
        }
        if log.is_event() {
            // A raw single-target pipe must stay byte-exact: the epoch divider is a fan-out marker.
            if self.prefix {
                // Pre-restart bytes are never allowed to surface after the divider. A partial line
                // is terminated here (and only here), so `finish` stays byte-exact.
                if self.flush_pending() {
                    let _ = self.out.write_all(b"\n");
                }
                let line = self.style_divider(&divider_text(log.ts_ms));
                let _ = self.out.write_all(line.as_bytes());
                let _ = self.out.write_all(b"\n");
                let _ = self.out.flush();
            }
            return;
        }
        if !self.prefix {
            let _ = self.write(log.stream, &log.bytes);
            return;
        }
        let mut lines: Vec<Vec<u8>> = Vec::new();
        {
            let pending = self.pending.entry(log.target.clone()).or_default();
            let tail = if log.stream == LogStream::Stderr {
                &mut pending.stderr
            } else {
                &mut pending.stdout
            };
            tail.extend_from_slice(&log.bytes);
            while let Some(end) = tail.iter().position(|byte| *byte == b'\n') {
                lines.push(tail.drain(..=end).collect());
            }
        }
        for line in lines {
            let _ = self.write_prefixed(&log.target, log.stream, &line);
        }
    }

    /// Flush every trailing partial line at EOF, each introduced exactly once.
    pub fn finish(&mut self) {
        if self.json {
            return;
        }
        let _ = self.flush_pending();
    }

    /// Write every buffered partial line; returns `true` when at least one was written (so a
    /// divider can terminate it with the newline the stream has not delivered yet).
    fn flush_pending(&mut self) -> bool {
        let mut wrote = false;
        for (target, pending) in std::mem::take(&mut self.pending) {
            for (stream, bytes) in [
                (LogStream::Stdout, pending.stdout),
                (LogStream::Stderr, pending.stderr),
            ] {
                if !bytes.is_empty() {
                    let _ = self.write_prefixed(&target, stream, &bytes);
                    wrote = true;
                }
            }
        }
        wrote
    }

    /// The gutter width grows to the widest target seen; ANSI never counts as a column.
    fn target_width(&mut self, target: &str) -> usize {
        if self.seen_targets.insert(target.to_owned()) {
            self.width = self.width.max(target.chars().count());
        }
        self.width
    }

    fn write_prefixed(&mut self, target: &str, stream: LogStream, line: &[u8]) -> io::Result<()> {
        let width = self.target_width(target);
        let pad = width.saturating_sub(target.chars().count());
        let gutter = if self.color {
            format!(
                "{} {}│ ",
                target.color(target_colour(target)),
                " ".repeat(pad)
            )
        } else {
            format!("{target:<width$} │ ")
        };
        self.write(stream, gutter.as_bytes())?;
        self.write(stream, line)
    }

    /// Dim the divider only when colour was injected; otherwise pass it through untouched.
    fn style_divider(&self, text: &str) -> String {
        if self.color {
            text.dimmed().to_string()
        } else {
            text.to_owned()
        }
    }

    fn write(&mut self, stream: LogStream, bytes: &[u8]) -> io::Result<()> {
        let stream: &mut Box<dyn Write> = match stream {
            LogStream::Stdout | LogStream::Event => &mut self.out,
            LogStream::Stderr => &mut self.err,
        };
        stream.write_all(bytes)?;
        stream.flush()
    }
}

/// One stable colour per target id (FNV-1a over the id), so a source keeps its colour across lines
/// and restarts and hook ids containing `:` remain readable.
fn target_colour(target: &str) -> AnsiColors {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in target.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    match hash % 6 {
        0 => AnsiColors::Red,
        1 => AnsiColors::Green,
        2 => AnsiColors::Yellow,
        3 => AnsiColors::Blue,
        4 => AnsiColors::Magenta,
        _ => AnsiColors::Cyan,
    }
}

/// `── restarted HH:MM:SSZ ──` in UTC (the daemon stamps `ts_ms` from the Unix epoch).
fn divider_text(ts_ms: u64) -> String {
    let seconds = ts_ms / 1000;
    let (hour, minute, second) = (seconds / 3600 % 24, seconds / 60 % 60, seconds % 60);
    format!("── restarted {hour:02}:{minute:02}:{second:02}Z ──")
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;

    use slipfork_world::{LogChunk, StreamSource};

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

    fn renderer(prefix: bool) -> (Renderer, Capture, Capture) {
        let out = Capture::default();
        let err = Capture::default();
        (
            Renderer::new(
                false,
                prefix,
                false,
                Box::new(out.clone()),
                Box::new(err.clone()),
            ),
            out,
            err,
        )
    }

    #[test]
    fn a_live_follow_merges_one_service_and_one_hook_by_capture_time() {
        use std::ops::ControlFlow;
        use std::path::PathBuf;
        use std::sync::atomic::AtomicBool;
        use std::time::Duration;

        use crate::daemon::HookLog;
        use crate::daemon::fan_in::{FanIn, LogSource};
        use slipfork_world::ServiceLogs;

        // 300 ms writer gaps: capture stamps are read at push, so neighbouring chunks can never tie
        // and the merged order is the chronological one regardless of scheduler jitter.
        const GAP: Duration = Duration::from_millis(300);

        let service = ServiceLogs::new("api");
        let hook = Arc::new(HookLog::new(
            "api:pre-deploy".to_owned(),
            PathBuf::from("/var/tmp/slipfork-renderer-interleave.log"),
        ));
        let (sender, receiver) = mpsc::channel();
        let aborted = Arc::new(AtomicBool::new(false));
        let follower = {
            let service = service.clone();
            let hook = Arc::clone(&hook);
            let aborted = Arc::clone(&aborted);
            thread::spawn(move || {
                let mut fan = FanIn::new(
                    vec![LogSource::Service(service), LogSource::Hook(hook)],
                    false,
                );
                let _ = fan.follow(&aborted, &mut |chunk| {
                    let _ = sender.send(chunk);
                    ControlFlow::Continue(())
                });
            })
        };

        // Two real retained windows, each writing stdout AND stderr, strictly interleaved.
        service.push(StreamSource::Stdout, b"api-out-1\n".to_vec());
        thread::sleep(GAP);
        hook.push(slipfork_world::StreamChunk {
            source: StreamSource::Stderr,
            bytes: b"hook-err-1\n".to_vec(),
        });
        thread::sleep(GAP);
        service.push(StreamSource::Stderr, b"api-err-1\n".to_vec());
        thread::sleep(GAP);
        hook.push(slipfork_world::StreamChunk {
            source: StreamSource::Stdout,
            bytes: b"hook-out-1\n".to_vec(),
        });
        thread::sleep(GAP);
        service.close();
        hook.close();
        follower.join().expect("the fan-in reaped both readers");

        let emitted: Vec<LogChunk> = receiver.into_iter().collect();
        let emitted_position = |needle: &str| {
            emitted
                .iter()
                .position(|chunk| String::from_utf8_lossy(&chunk.bytes).starts_with(needle))
                .unwrap_or_else(|| panic!("{needle} was never emitted: {emitted:?}"))
        };
        assert!(emitted_position("api-out-1") < emitted_position("hook-err-1"));
        assert!(emitted_position("hook-err-1") < emitted_position("api-err-1"));
        assert!(emitted_position("api-err-1") < emitted_position("hook-out-1"));
        assert!(
            emitted
                .windows(2)
                .all(|pair| pair[0].ts_ms <= pair[1].ts_ms),
            "capture stamps never run backwards: {emitted:?}"
        );

        // Both child streams go to one sink here, so stdout/stderr interleaving stays visible.
        let sink = Capture::default();
        let mut renderer = Renderer::new(
            false,
            true,
            false,
            Box::new(sink.clone()),
            Box::new(sink.clone()),
        );
        for chunk in emitted {
            renderer.chunk(chunk);
        }
        renderer.finish();
        let text = sink.text();
        let position = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("missing {needle} in {text}"))
        };
        assert!(position("api-out-1") < position("hook-err-1"), "{text}");
        assert!(position("hook-err-1") < position("api-err-1"), "{text}");
        assert!(position("api-err-1") < position("hook-out-1"), "{text}");
        assert!(text.contains("api │ api-out-1\n"), "{text}");
        assert!(
            text.contains("api:pre-deploy │ hook-err-1\n"),
            "hook lines carry their own gutter: {text}"
        );
        assert!(
            text.contains("api            │ api-err-1\n"),
            "the gutter widens to the widest target: {text}"
        );
    }

    #[test]
    fn logs_a_restart_divider_is_dimmed_text_with_no_ansi_without_color() {
        let (mut renderer, out, _) = renderer(true);
        renderer.chunk(chunk("api", b"before\n"));
        renderer.chunk(LogChunk::divider(3_600_000));
        renderer.chunk(chunk("api", b"after\n"));
        renderer.finish();
        assert_eq!(
            out.text(),
            "api │ before\n── restarted 01:00:00Z ──\napi │ after\n"
        );
        assert!(
            !out.text().contains("\u{1b}["),
            "no ANSI without injected color"
        );
    }

    #[test]
    fn logs_flushes_a_partial_line_before_the_divider() {
        let (mut renderer, out, _) = renderer(true);
        renderer.chunk(chunk("api", b"pre-restart bytes"));
        renderer.chunk(LogChunk::divider(3_600_000));
        renderer.chunk(chunk("api", b"after\n"));
        renderer.finish();
        let text = out.text();
        assert_eq!(
            text,
            "api │ pre-restart bytes\n── restarted 01:00:00Z ──\napi │ after\n"
        );
        assert!(
            text.find("pre-restart").expect("partial line")
                < text.find("restarted").expect("divider"),
            "pre-restart bytes must precede the divider: {text}"
        );
    }

    #[test]
    fn logs_a_colored_terminal_paints_targets_and_the_divider() {
        let out = Capture::default();
        let mut renderer = Renderer::new(
            false,
            true,
            true,
            Box::new(out.clone()),
            Box::new(Capture::default()),
        );
        renderer.chunk(chunk("api:pre-deploy", b"migrated\n"));
        renderer.chunk(LogChunk::divider(3_600_000));
        renderer.finish();
        let text = out.text();
        assert!(text.contains("\u{1b}["), "color is injected: {text:?}");
        assert!(
            text.contains("api:pre-deploy"),
            "hook ids stay readable: {text:?}"
        );
        assert!(text.contains("── restarted 01:00:00Z ──"), "{text:?}");
    }

    #[test]
    fn logs_two_sources_prefix_every_line_with_its_own_source() {
        let (mut renderer, out, _) = renderer(true);
        renderer.chunk(chunk("worker", b"job 1 done\njob 2 done\n"));
        renderer.chunk(chunk("api", b"listening on :8000\nready\n"));
        renderer.chunk(chunk("api", b"GET / 200\n"));
        renderer.finish();
        assert_eq!(
            out.text(),
            "worker │ job 1 done\nworker │ job 2 done\napi    │ listening on :8000\napi    │ ready\napi    │ GET / 200\n"
        );
    }

    #[test]
    fn logs_a_line_split_across_chunks_is_prefixed_exactly_once() {
        let (mut renderer, out, _) = renderer(true);
        renderer.chunk(chunk("worker", b"other\n"));
        renderer.chunk(chunk("api", b"par"));
        renderer.chunk(chunk("api", b"tial "));
        renderer.chunk(chunk("api", b"line\n"));
        renderer.finish();
        assert_eq!(out.text(), "worker │ other\napi    │ partial line\n");
    }

    #[test]
    fn logs_flushes_a_trailing_partial_line_at_eof() {
        let (mut renderer, out, _) = renderer(true);
        renderer.chunk(chunk("api", b"no newline yet"));
        assert_eq!(out.text(), "", "a partial line is never emitted mid-line");
        renderer.finish();
        assert_eq!(out.text(), "api │ no newline yet");
    }

    #[test]
    fn logs_routes_stderr_lines_to_stderr_with_the_same_gutter() {
        let (mut renderer, out, err) = renderer(true);
        renderer.chunk(chunk("api", b"to stdout\n"));
        renderer.chunk(LogChunk::output(
            "api",
            2,
            slipfork_world::StreamChunk {
                source: StreamSource::Stderr,
                bytes: b"to stderr\n".to_vec(),
            },
        ));
        renderer.finish();
        assert_eq!(out.text(), "api │ to stdout\n");
        assert_eq!(err.text(), "api │ to stderr\n");
    }
}
