//! The typed-event renderer shared by `logs`, `logs -r`, `shell`, and `exec`.
//!
//! Kept beside the verbs instead of inside them so the parent module stays a thin command surface.
//! Every write goes through [`Renderer::write`], which remembers a non-EPIPE failure instead of
//! discarding it: a reader that stops early is a clean stream end, any other write failure is the
//! command's.

use std::process::ExitCode;

use serde_json::{Value, json};

use crate::commands::LocalClock;
use crate::commands::emit;
use crate::ui::strip;
use crate::ui::table::{Column, Table};
use crate::ui::{Palette, Verdict};

/// Renders typed daemon events as gutters, instruction rows, or NDJSON.
pub(crate) struct Renderer {
    palette: Palette,
    json: bool,
    /// `logs` prefixes every line with its pod gutter; `exec` stays verbatim like `docker exec`.
    gutter: bool,
    /// Prefix a human timestamp before the pod gutter (`-t`).
    timestamps: bool,
    /// Replay rows carry a `HH:MM:SS.mmm` clock and the v3 stream gutter.
    clocked: bool,
    /// The one wall clock resolved by the composition root.
    clock: LocalClock,
    width: usize,
    /// Human rows emitted in this replay, so an empty capture prints no double blank line.
    rows: usize,
    /// The replay's verdict, assembled from the run record.
    verdict: Option<Verdict>,
    /// The record's one-line reason, used by the verdict when no row above carries it.
    reason: Option<String>,
    /// Set when a non-EPIPE write failed, so the caller can exit non-zero.
    failed: bool,
}

/// The stored line as this viewer may render it.
///
/// The capture stores the child's raw bytes; only a colour-capable human viewer keeps them. A
/// pipe, `NO_COLOR`, and `--json` strip every escape before rendering or measuring.
fn visible(stored: &str, color: bool, json: bool) -> std::borrow::Cow<'_, str> {
    if color && !json {
        crate::ui::colour_only(stored)
    } else {
        strip(stored)
    }
}

/// One v3 replay row: `  HH:MM:SS.mmm │ line`, with the clock resolved once for the invocation.
fn replay_row(
    clock: &LocalClock,
    palette: Palette,
    ts_ms: Option<u64>,
    glyph: &str,
    line: &str,
) -> String {
    ts_ms.map_or_else(
        || format!("  {glyph} {line}"),
        |ts| format!("  {} {glyph} {line}", palette.meta(&clock.clock(ts))),
    )
}

impl Renderer {
    /// One renderer with only the mode flags set; every constructor narrows it.
    fn blank(
        color: bool,
        json: bool,
        gutter: bool,
        timestamps: bool,
        clocked: bool,
        clock: &LocalClock,
    ) -> Self {
        Self {
            palette: Palette::new(color),
            json,
            gutter,
            timestamps,
            clocked,
            clock: clock.clone(),
            width: 0,
            rows: 0,
            verdict: None,
            reason: None,
            failed: false,
        }
    }

    pub(super) fn new(color: bool, json: bool, timestamps: bool, clock: &LocalClock) -> Self {
        Self::blank(color, json, true, timestamps, false, clock)
    }

    /// The `exec`/`shell` renderer: the command's own stdout and stderr, byte for byte.
    pub(crate) fn exec(color: bool, json: bool, clock: &LocalClock) -> Self {
        Self::blank(color, json, false, false, false, clock)
    }

    /// The `logs -r <id>` renderer: one recorded run's clocked stream-gutter rows.
    pub(super) fn replay(color: bool, json: bool, timestamps: bool, clock: &LocalClock) -> Self {
        Self::blank(color, json, false, timestamps, true, clock)
    }

    /// Writes one rendered chunk, remembering a non-EPIPE failure (`emit` treats EPIPE as clean).
    fn write(&mut self, text: &str) {
        if matches!(emit(text), ExitCode::FAILURE) {
            self.failed = true;
        }
    }

    /// Whether any write failed for a reason other than the reader closing the pipe.
    pub(crate) fn failed(&self) -> bool {
        self.failed
    }

    pub(crate) fn event(&mut self, event: &Value) {
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
        let line = visible(stored, self.palette.color(), self.json);
        let ts_ms = event.get("ts_ms").and_then(Value::as_u64);
        if kind == "roster" {
            // `logs -r --json` mirrors the catalog record before its captured lines.
            if self.json
                && let Some(run) = event.get("run")
            {
                self.write(&format!("{run}\n"));
            }
            self.roster(event);
            return;
        }
        if self.json {
            let mut payload = json!({
                "pod": pod,
                "stream": stream,
                "line": line,
                "kind": kind,
            });
            if let Some(ts_ms) = ts_ms {
                payload["ts_ms"] = json!(ts_ms);
            }
            self.write(&format!("{payload}\n"));
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
                            self.palette.meta(&self.clock.timestamp(ts))
                        } else {
                            self.palette.meta(&self.clock.clock(ts))
                        };
                        format!("  {clock}  {line}")
                    },
                );
                self.rows += 1;
                self.write(&format!("{row}\n"));
            }
            "divider" => {
                self.write(&format!("{}\n", self.palette.meta(&line)));
            }
            _ if self.clocked => {
                // The v3 replay row: 2-space indent, a dim clock in the gutter, then the glyph.
                let glyph = self.palette.gutter(stream);
                let row = replay_row(&self.clock, self.palette, ts_ms, &glyph, &line);
                self.rows += 1;
                self.write(&format!("{row}\n"));
            }
            _ if !self.gutter => {
                let stamp = if self.timestamps {
                    ts_ms
                        .map(|ts| format!("{} ", self.palette.meta(&self.clock.timestamp(ts))))
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                self.write(&format!("{stamp}{line}\n"));
            }
            _ => {
                let stamp = if self.timestamps {
                    ts_ms
                        .map(|ts| format!("{} ", self.palette.meta(&self.clock.timestamp(ts))))
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
                let cells = [
                    self.palette.tint(pod, "1"),
                    self.palette.gutter(stream),
                    line.to_string(),
                ];
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
        if self.json {
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
            self.gutter = false;
            self.clocked = true;
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
            .map(|ts| self.clock.timestamp(ts));
        let header =
            self.palette
                .header_line(run_id, &header_verb, target, command, started.as_deref());
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

    pub(crate) fn finish(&mut self, code: i64) {
        if self.json {
            self.write(&format!("{}\n", json!({ "exit": code })));
            return;
        }
        if let Some(verdict) = &self.verdict {
            // A failure with no replayable row at all explains itself in the verdict itself.
            let line = match &self.reason {
                Some(reason) if self.rows == 0 && verdict.exit_code.is_none() => {
                    self.palette.verdict_detail(verdict, reason)
                }
                _ => self.palette.verdict(verdict),
            };
            // Exactly one blank line separates rows from the verdict; an empty capture has none.
            let gap = if self.rows > 0 { "\n" } else { "" };
            self.write(&format!("{gap}{line}\n"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{replay_row, visible};
    use crate::commands::LocalClock;
    use crate::ui::Palette;
    use crate::ui::table::{Column, Table};

    const RED: &str = "\u{1b}[31mred\u{1b}[0m";

    #[test]
    fn a_colour_terminal_keeps_the_child_bytes_and_every_other_viewer_strips_them() {
        assert_eq!(visible(RED, true, false), RED);
        assert_eq!(visible(RED, false, false), "red", "a pipe carries no ESC");
        assert_eq!(visible(RED, true, true), "red", "`--json` is escape-free");
        assert_eq!(visible(RED, false, true), "red");
    }

    #[test]
    fn a_colour_line_measures_by_its_plain_width_so_the_gutter_never_shears() {
        let columns = [Column::left().at_least(6), Column::left()];
        let table = Table::new(&columns).gap(" ");
        let plain = table.widths(&[vec!["api".to_owned(), "red".to_owned()]]);
        let coloured = table.widths(&[vec!["api".to_owned(), RED.to_owned()]]);
        assert_eq!(plain, coloured);
        assert_eq!(
            crate::ui::strip(&table.line(&coloured, &["api".to_owned(), RED.to_owned()])),
            table.line(&plain, &["api".to_owned(), "red".to_owned()]),
            "the visible gutter never shears on a coloured line"
        );
    }

    #[test]
    fn replay_clock_gutter_uses_the_injected_local_clock() {
        let clock = LocalClock::fixed(2 * 3600);
        assert_eq!(
            replay_row(
                &clock,
                Palette::new(false),
                Some(1_782_907_200_000),
                "│",
                "line"
            ),
            "  14:00:00.000 │ line"
        );
        assert_eq!(
            replay_row(&clock, Palette::new(false), None, "┃", "line"),
            "  ┃ line"
        );
    }
}
