//! The one live-stream renderer shared by `logs`, `exec`, `shell`, and `docker`.
//!
//! Daemon frames stay typed values; this module owns every byte a human reads: the pod/stream
//! gutter, replay `HH:MM:SS.mmm` clock, `--last` ring, NDJSON frames, and the final verdict.
//! [`ReportRenderer`] owns the palette, mode, and wall clock, so a command never formats SGR.

use std::collections::VecDeque;
use std::io;

use serde_json::{Value, json};

use super::palette::Verdict;
use super::table::{Column, Table};

use super::{Entry, ReportRenderer, StreamKind};

/// How one stream lays out its lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// `logs`: aligned pod gutter (`api │ line`).
    Logs,
    /// `logs -r`: indented clocked rows (`  12:00:00.000 │ line`).
    Replay,
    /// `exec`/`shell`: the child's own bytes, no gutter.
    Verbatim,
}

/// Renders typed daemon events as gutters, instruction rows, or NDJSON.
pub struct StreamRenderer {
    renderer: ReportRenderer,
    layout: Layout,
    /// Prefix a human timestamp before the pod gutter (`-t`).
    timestamps: bool,
    /// The roster's widest pod id; latched once and never grown mid-stream.
    width: usize,
    /// Human rows emitted in this replay, so an empty capture prints no double blank line.
    rows: usize,
    /// The replay's verdict, assembled from the run record.
    verdict: Option<Verdict>,
    /// The record's one-line reason, used by the verdict when no row above carries it.
    reason: Option<String>,
    /// Set when a non-EPIPE write failed, so the caller can exit non-zero.
    failed: bool,
    /// `logs --last N` holds rendered chunks here until the daemon marks history caught up.
    ring: TailRing,
}

/// The bounded ring behind `logs --last`: it keeps the last N rendered lines, terminators and all.
#[derive(Debug, Default)]
struct TailRing {
    capacity: usize,
    lines: VecDeque<String>,
}

impl TailRing {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            lines: VecDeque::with_capacity(capacity),
        }
    }

    /// Buffers one rendered chunk, splitting multi-line chunks (a replay header plus its blank).
    fn push(&mut self, chunk: &str) {
        for line in chunk.split_inclusive('\n') {
            if self.lines.len() == self.capacity {
                self.lines.pop_front();
            }
            self.lines.push_back(line.to_owned());
        }
    }

    fn drain(&mut self) -> Vec<String> {
        self.lines.drain(..).collect()
    }
}

/// The stored line as this viewer may render it: ANSI and `\r` are stripped for a pipe,
/// `NO_COLOR`, or `--json`; a colour terminal keeps only safe SGR.
pub fn visible_line(stored: &str, color: bool, json: bool) -> std::borrow::Cow<'_, str> {
    let visible = if color && !json {
        crate::ui::colour_only(stored)
    } else {
        crate::ui::strip(stored)
    };
    match visible {
        std::borrow::Cow::Borrowed(text) if !text.contains('\r') => {
            std::borrow::Cow::Borrowed(text)
        }
        text => std::borrow::Cow::Owned(text.replace('\r', "")),
    }
}

/// The stored line as this viewer may render it.
///
/// The capture stores the child's raw bytes; only a colour-capable human viewer keeps them. A
/// pipe, `NO_COLOR`, and `--json` strip every escape before rendering or measuring.
fn visible(stored: &str, color: bool, json: bool) -> std::borrow::Cow<'_, str> {
    visible_line(stored, color, json)
}

impl StreamRenderer {
    fn blank(renderer: ReportRenderer, layout: Layout, timestamps: bool) -> Self {
        Self {
            renderer,
            layout,
            timestamps,
            width: 0,
            rows: 0,
            verdict: None,
            reason: None,
            failed: false,
            ring: TailRing::default(),
        }
    }

    /// `logs`: aligned pod gutters, optional `-t` timestamp prefix.
    #[must_use]
    pub fn logs(renderer: ReportRenderer, timestamps: bool) -> Self {
        Self::blank(renderer, Layout::Logs, timestamps)
    }

    /// `logs -r <id>`: one recorded run's clocked stream-gutter rows.
    #[must_use]
    pub fn replay(renderer: ReportRenderer, timestamps: bool) -> Self {
        Self::blank(renderer, Layout::Replay, timestamps)
    }

    /// `exec`/`shell`: the command's own stdout and stderr, byte for byte.
    #[must_use]
    pub fn verbatim(renderer: ReportRenderer) -> Self {
        Self::blank(renderer, Layout::Verbatim, false)
    }

    /// `logs --last N`: hold the last N rendered lines back until history is caught up.
    #[must_use]
    pub fn tail(mut self, lines: Option<usize>) -> Self {
        if let Some(capacity) = lines.filter(|count| *count > 0) {
            self.ring = TailRing::new(capacity);
        }
        self
    }

    /// Whether any write failed for a reason other than the reader closing the pipe.
    #[must_use]
    pub const fn failed(&self) -> bool {
        self.failed
    }

    /// Whether this invocation renders NDJSON frames.
    fn json(&self) -> bool {
        self.renderer.mode().json()
    }

    /// Writes one already-rendered chunk, holding it in the `--last` ring while history is buffered.
    fn write(&mut self, text: &str) {
        if self.ring.capacity > 0 {
            self.ring.push(text);
            return;
        }
        self.write_direct(text);
    }

    /// Writes one rendered chunk now, remembering a non-EPIPE failure (`BrokenPipe` is a clean end).
    fn write_direct(&mut self, text: &str) {
        if let Err(error) = self.renderer.emit_text(StreamKind::Stdout, text)
            && error.kind() != io::ErrorKind::BrokenPipe
        {
            self.failed = true;
        }
    }

    /// Writes one typed entry through the shared report renderer, keeping the tail ring intact.
    fn emit_entry(&mut self, entry: &Entry) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _ = self.renderer.emit_entry_to(entry, &mut out, &mut err);
        if !out.is_empty() {
            let text = String::from_utf8_lossy(&out).into_owned();
            self.write(&text);
        }
        if !err.is_empty()
            && let Err(error) = self
                .renderer
                .emit_text(StreamKind::Stderr, &String::from_utf8_lossy(&err))
            && error.kind() != io::ErrorKind::BrokenPipe
        {
            self.failed = true;
        }
    }

    /// Writes one JSON value as a single line, through the same ring as human rows.
    fn write_json(&mut self, value: &Value) {
        let text = serde_json::to_string(value).unwrap_or_default();
        self.write(&format!("{text}\n"));
    }

    /// Flushes the `--last` ring once and switches every later chunk to write-through.
    fn flush_tail(&mut self) {
        if self.ring.capacity == 0 {
            return;
        }
        let lines = self.ring.drain();
        self.ring = TailRing::default();
        for line in lines {
            self.write_direct(&line);
        }
    }

    /// Renders one daemon frame.
    pub fn event(&mut self, event: &Value) {
        let pod = event.get("pod").and_then(Value::as_str).unwrap_or_default();
        let kind = event.get("kind").and_then(Value::as_str).unwrap_or("line");
        let stream = event
            .get("stream")
            .and_then(Value::as_str)
            .unwrap_or("stdout");
        let stored = event
            .get("line")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let line = visible(stored, self.renderer.color(), self.json());
        let ts_ms = event.get("ts_ms").and_then(Value::as_u64);
        if kind == "history_end" {
            // The daemon's one marker: everything before it is history, everything after is live.
            self.flush_tail();
            return;
        }
        if kind == "eof" && self.ring.capacity > 0 {
            // `--last N` promises N line records; the world's end marker is not one of them.
            return;
        }
        if kind == "roster" {
            // `logs -r --json` mirrors the catalog record before its captured lines.
            if self.json()
                && let Some(run) = event.get("run")
            {
                self.write_json(run);
            }
            self.roster(event);
            return;
        }
        if self.json() {
            let mut payload = json!({
                "pod": pod,
                "stream": stream,
                "line": line,
                "kind": kind,
            });
            if let Some(ts_ms) = ts_ms {
                payload["ts_ms"] = json!(ts_ms);
            }
            self.write_json(&payload);
            return;
        }
        match kind {
            "eof" => {}
            // A captured build instruction row is its own clocked line; the restart divider the
            // init writes stays a dim one-off and never appears inside a replay.
            "step" => {
                let row = ts_ms.map_or_else(
                    || format!("  {line}"),
                    |ts| {
                        let clock = if self.timestamps {
                            self.renderer.wall().timestamp(ts)
                        } else {
                            self.renderer.wall().clock(ts)
                        };
                        format!("  {}  {line}", self.renderer.palette().meta(&clock))
                    },
                );
                self.rows += 1;
                self.write(&format!("{row}\n"));
            }
            "divider" => {
                self.write(&format!("{}\n", self.renderer.palette().meta(&line)));
            }
            _ if self.layout == Layout::Replay => {
                // The v3 replay row: 2-space indent, a dim clock in the gutter, then the glyph.
                let kind = match stream {
                    "stderr" => StreamKind::Stderr,
                    _ => StreamKind::Stdout,
                };
                self.emit_entry(&Entry::Stream {
                    indent: 2,
                    stream: kind,
                    line: line.into_owned(),
                    ts_ms: ts_ms.unwrap_or(0),
                });
                self.rows += 1;
            }
            _ if self.layout == Layout::Verbatim => {
                let stamp = if self.timestamps {
                    ts_ms
                        .map(|ts| {
                            format!(
                                "{} ",
                                self.renderer
                                    .palette()
                                    .meta(&self.renderer.wall().timestamp(ts))
                            )
                        })
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                self.write(&format!("{stamp}{line}\n"));
            }
            _ => {
                let stamp = if self.timestamps {
                    ts_ms
                        .map(|ts| {
                            format!(
                                "{} ",
                                self.renderer
                                    .palette()
                                    .meta(&self.renderer.wall().timestamp(ts))
                            )
                        })
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let columns = [
                    Column::left().at_least(self.width),
                    Column::left(),
                    Column::left(),
                ];
                let gutter = Table::new(&columns).gap(" ").indent(&stamp);
                let palette = self.renderer.palette();
                let cells = [palette.value(pod), palette.gutter(stream), line.to_string()];
                // Measure the real row, line included: the table engine skips a zero-width column,
                // and measuring an empty body would silently drop every log line.
                let widths = gutter.widths(&[cells.to_vec()]);
                let row = gutter.line(&widths, &cells);
                self.write(&format!("{row}\n"));
            }
        }
    }

    /// Fixes the gutter width from the daemon's pod roster; it never grows mid-stream.
    fn roster(&mut self, event: &Value) {
        if self.json() {
            // Even `--json` must not silently drop a replay record, but the record was written
            // above; a roster only fixes human alignment.
            if event.get("run").is_some() {
                self.layout = Layout::Replay;
            }
            return;
        }
        let pods: Vec<&str> = event
            .get("pods")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        self.width = pods
            .iter()
            .map(|pod| pod.chars().count())
            .max()
            .unwrap_or(0);
        if let Some(run) = event.get("run") {
            // A roster carrying a catalog record is a capture replay, so every following row takes
            // the clocked v3 stream-gutter path.
            self.layout = Layout::Replay;
            self.print_header(run);
        }
    }

    /// One replay header row: `#<run_id>  <identity>  <target>  <started_at>`.
    ///
    /// An image build is one catalog run over many instructions, so its header names the run's
    /// identity only; setup and hook records still carry their phase and command. The final verdict
    /// is a separate line and keeps naming the failing instruction.
    fn print_header(&mut self, run: &Value) {
        let run_id = run["run_id"].as_str().unwrap_or_default();
        let kind = run["kind"].as_str().unwrap_or_default();
        let phase = run["phase"].as_str().filter(|phase| !phase.is_empty());
        let target = run["target"].as_str().unwrap_or_default();
        let (header_verb, command, verdict_verb) = if kind == "image" {
            let verdict_verb = phase.map_or_else(|| "image".to_owned(), str::to_ascii_uppercase);
            ("image".to_owned(), "", verdict_verb)
        } else {
            let verb = kind.to_owned();
            let command = run["instruction"]
                .as_str()
                .filter(|instruction| !instruction.is_empty())
                .or_else(|| run["command"].as_str())
                .unwrap_or_default();
            (verb.clone(), command, verb)
        };
        let started = run["started_ms"]
            .as_u64()
            .map(|ts| self.renderer.wall().timestamp(ts));
        let header = self.renderer.palette().header_line(
            run_id,
            &header_verb,
            target,
            command,
            started.as_deref(),
        );
        self.write(&format!("{header}\n\n"));
        self.verdict = Some(Verdict {
            verb: verdict_verb,
            target: target.to_owned(),
            state: run["state"].as_str().unwrap_or_default().to_owned(),
            exit_code: run["exit_code"]
                .as_i64()
                .and_then(|code| i32::try_from(code).ok()),
            duration_ms: run["duration_ms"].as_u64(),
        });
        self.reason = run["reason"]
            .as_str()
            .filter(|reason| !reason.is_empty())
            .map(str::to_owned);
    }

    /// Closes the stream: the `--json` exit trailer or the one human verdict line.
    pub fn finish(&mut self, code: i64) {
        if self.json() {
            // History-first: exactly N held records, then the one exit trailer, never N-1.
            self.flush_tail();
            self.write_json(&json!({ "exit": code }));
            return;
        }
        if let Some(verdict) = &self.verdict {
            let palette = self.renderer.palette();
            // A failure with no replayable row at all explains itself in the verdict itself.
            let line = match &self.reason {
                Some(reason) if self.rows == 0 && verdict.exit_code.is_none() => {
                    palette.verdict_detail(verdict, reason)
                }
                _ => palette.verdict(verdict),
            };
            // Exactly one blank line separates rows from the verdict; an empty capture has none.
            let gap = if self.rows > 0 { "\n" } else { "" };
            self.write(&format!("{gap}{line}\n"));
        }
        self.flush_tail();
    }
}

#[cfg(test)]
mod tests;
