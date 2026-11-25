//! One renderer for native image build/pull frames, owned by the CLI.
//!
//! The printer owns every row's exact bytes: the live `[i/n]` header, the padded lowercase
//! instruction cell, child-line gutters, and NDJSON frames under `--json`. The daemon only sends
//! typed [`ImageFrame`]s, so no palette or terminal decision ever lives in the daemon.

use std::time::Duration;

use serde_json::json;
use slipfork_image::{StepEvent, StepPlace, StepState};

use crate::daemon::image::ImageFrame;
use crate::ui::{Palette, elapsed_text};

/// Renders one build or pull event stream into `sink`.
pub(crate) struct EventPrinter<'sink> {
    palette: Palette,
    interactive: bool,
    verbose: bool,
    json: bool,
    notes: bool,
    sink: &'sink mut dyn FnMut(&str),
}

impl<'sink> EventPrinter<'sink> {
    /// Binds one mode: `json` wins over every human row, and `interactive` gates the `→` row.
    pub(crate) fn new(
        palette: Palette,
        interactive: bool,
        verbose: bool,
        json: bool,
        notes: bool,
        sink: &'sink mut dyn FnMut(&str),
    ) -> Self {
        Self {
            palette,
            interactive,
            verbose,
            json,
            notes,
            sink,
        }
    }

    /// Renders one typed frame.
    pub(crate) fn frame(&mut self, frame: &ImageFrame) {
        match frame {
            ImageFrame::Step {
                index,
                total,
                instruction,
                args,
                state,
                elapsed_ms,
                exit_code,
                phase: _,
                width,
            } => {
                let state = match state.as_str() {
                    "started" => StepState::Started,
                    "tail" => StepState::Tail,
                    "failed" => StepState::Failed,
                    _ => StepState::Done,
                };
                let mut step = StepEvent::new(
                    StepPlace::new(*index, *total, 0),
                    instruction,
                    args,
                    state,
                    Duration::from_millis(*elapsed_ms),
                    *exit_code,
                );
                step.instruction_width = (*width).max(instruction.chars().count());
                self.step(step);
            }
            ImageFrame::Log { stream, line } => self.line(stream, line),
            ImageFrame::Note { text } => self.note(text),
        }
    }

    /// Writes one already-rendered chunk (the final `Built`/`Pulled`/JSON object row).
    pub(crate) fn chunk(&mut self, text: &str) {
        if !text.is_empty() {
            self.out(text);
        }
    }

    fn line(&mut self, stream: &str, text: &str) {
        let text = strip_ansi(text).replace('\r', "");
        if text.trim().is_empty() {
            return;
        }
        if self.json {
            self.out(&format!(
                "{}\n",
                json!({ "event": "log", "stream": stream, "line": text })
            ));
        } else {
            self.out(&format!("  {} {}\n", self.palette.gutter(stream), text));
        }
    }

    fn note(&mut self, text: &str) {
        if self.json {
            self.out(&format!("{}\n", json!({ "event": "note", "text": text })));
        } else if self.notes || self.verbose {
            self.out(&format!("  {}\n", self.palette.meta(text)));
        }
    }

    fn step(&mut self, step: StepEvent<'_>) {
        if self.json {
            let mut frame = json!({
                "event": "step",
                "index": step.index,
                "total": step.total,
                "instruction": step.instruction,
                "args": step.args,
                "state": step.state.as_str(),
                "elapsed_ms": u64::try_from(step.elapsed.as_millis()).unwrap_or(u64::MAX),
            });
            if let Some(code) = step.exit_code {
                frame["exit_code"] = json!(code);
            }
            self.out(&format!("{frame}\n"));
            return;
        }
        let header = format!("[{}/{}]", step.index, step.total);
        let keyword = keyword_cell(self.palette, &step);
        let args = if step.args.is_empty() {
            String::new()
        } else {
            format!(" {}", self.palette.value(step.args))
        };
        match step.state {
            StepState::Started => {
                if self.interactive {
                    self.out(&format!(
                        "{} {} {}{}\n",
                        self.palette.tint("→", "36"),
                        self.palette.meta(&header),
                        keyword,
                        args
                    ));
                }
            }
            StepState::Tail => {}
            StepState::Done | StepState::Failed => {
                let icon = if step.state == StepState::Done {
                    self.palette.icon_ok()
                } else {
                    self.palette.icon_bad()
                };
                let elapsed =
                    elapsed_text(u64::try_from(step.elapsed.as_millis()).unwrap_or(u64::MAX));
                let exit = step
                    .exit_code
                    .map_or_else(String::new, |code| format!(" (exit {code})"));
                self.out(&format!(
                    "{} {} {}{} {}{}\n",
                    icon,
                    self.palette.meta(&header),
                    keyword,
                    args,
                    self.palette.meta(&elapsed),
                    exit
                ));
            }
        }
    }

    fn out(&mut self, text: &str) {
        (self.sink)(text);
    }
}

/// The lowercase instruction keyword, in the substep role, padded outside the tint.
fn keyword_cell(palette: Palette, step: &StepEvent<'_>) -> String {
    let keyword = step.instruction.to_lowercase();
    let pad = step
        .instruction_width
        .saturating_sub(keyword.chars().count());
    format!("{}{}", palette.substep(&keyword), " ".repeat(pad))
}

/// Strips CSI escape sequences from child output, keeping the `\r` policy at the call site.
fn strip_ansi(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\u{1b}') {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for ch in chars.by_ref() {
                    if ('@'..='~').contains(&ch) {
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slipfork_image::{StepEvent, StepPlace, StepState};

    use super::{Palette, keyword_cell};

    fn cell(instruction: &'static str, width: usize, color: bool) -> String {
        let mut event = StepEvent::new(
            StepPlace::new(1, 1, 1),
            instruction,
            "",
            StepState::Done,
            Duration::ZERO,
            None,
        );
        event.instruction_width = width;
        keyword_cell(Palette::new(color), &event)
    }

    #[test]
    fn keyword_cell_pads_outside_the_tint_to_the_plan_width() {
        for instruction in ["FROM", "COPY", "RUN", "WORKDIR"] {
            let plain = cell(instruction, 7, false);
            let keyword = instruction.to_lowercase();
            assert_eq!(plain.len(), 7, "{plain:?}");
            assert_eq!(plain, format!("{keyword}{}", " ".repeat(7 - keyword.len())));
            assert!(!plain.contains('\u{1b}'), "plain cells are unstyled");
        }

        assert_eq!(cell("FROM", 7, true), "\u{1b}[38;5;132mfrom\u{1b}[0m   ");
        assert_eq!(cell("COPY", 7, true), "\u{1b}[38;5;132mcopy\u{1b}[0m   ");
        assert_eq!(cell("RUN", 7, true), "\u{1b}[38;5;132mrun\u{1b}[0m    ");
        assert_eq!(cell("WORKDIR", 7, true), "\u{1b}[38;5;132mworkdir\u{1b}[0m");
        let padded = cell("FROM", 7, true);
        assert!(
            padded.ends_with("\u{1b}[0m   "),
            "padding is outside the tint: {padded:?}"
        );
    }
}
