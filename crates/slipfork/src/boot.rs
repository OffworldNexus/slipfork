//! The one append-only boot narrative shared by `up`, `restart`, `fork`, `down`, and `drop`.
//!
//! Completed rows are ordinary newline-terminated scrollback lines in event order; a colour TTY
//! additionally owns one in-place status line (`\r\x1b[2K` + the rendered line) carrying the
//! spinner, counter, verb/target, elapsed clock, and tail phase. A pipe, `NO_COLOR` terminal, or
//! unmeasurable terminal gets plain `println!` lines with zero escapes and zero `\r`.
//!
//! The daemon's `plan` frame latches the plan positions before row 1; `--json` passes through.

mod build;
mod failure;
mod layout;
mod phase;
mod progress;
mod render;
mod status;

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::Value;

use crate::ui::Palette;
use crate::ui::UiMode;
use crate::ui::normalize_size;

use self::build::Narrative;
use self::progress::BootProgress;
use self::render as text;
pub use self::render::{Node, OuterWidths, completed_row};
use self::status::{StatusLine, Ticker};

pub use self::render::dropped_row;
#[allow(unused_imports)]
pub use self::render::{BarProgress, outer_verb, outer_widths, pane_geometry, status_line};
pub use crate::ui::elapsed_text;

/// One live boot pane; drop it (via [`BootTui::finish`]) before printing the final result.
pub struct BootTui {
    mode: UiMode,
    width: usize,
    started: Instant,
    status: Option<Arc<Mutex<StatusLine>>>,
    ticker: Option<Ticker>,
    failed: bool,
    /// The final verdict rows held until the status line is erased.
    verdict: Vec<String>,
    /// The verdict envelope held for stderr: `✗ …`, optional `runtime:`, one `next:`.
    diagnosis: Vec<String>,
    /// The plan-derived outer `kind` and `target` column widths.
    widths: OuterWidths,
    /// The announced plan, both progress levels, and the bar label.
    progress: BootProgress,
    /// The live build/hook narrative: phase rows, collapse, and the failure block.
    narrative: Narrative,
}

/// The measured terminal size, or `None` when it cannot be measured (zero rows included).
fn terminal_size() -> Option<(usize, usize)> {
    let (cols, rows) = crossterm::terminal::size().ok()?;
    let (cols, rows) = normalize_size(usize::from(cols), usize::from(rows));
    (rows > 0).then_some((cols, rows))
}

impl BootTui {
    /// A pane with the anchored progress bar (`up`, `restart`, `fork`).
    #[must_use]
    pub fn boot(mode: UiMode, width: usize, title: &str, verbose: bool) -> Self {
        Self::new(mode, width, title, true, verbose)
    }

    /// Rows only, no status line (`down`, `drop`).
    #[must_use]
    pub fn rows(mode: UiMode, width: usize, title: &str) -> Self {
        Self::new(mode, width, title, false, false)
    }

    fn new(mode: UiMode, width: usize, title: &str, summary: bool, verbose: bool) -> Self {
        // The only ambient-terminal probe lives here, at the composition root.
        let progress = BootProgress::new(title);
        let narrative = Narrative::new(verbose);
        let measured = terminal_size();
        let live = summary && mode.color() && measured.is_some();
        let cols = measured.map_or(width, |(cols, _)| cols).max(width);
        let status = live.then(|| {
            Arc::new(Mutex::new(StatusLine::new(
                progress.clone(),
                cols,
                narrative.steps(),
            )))
        });
        let ticker = status
            .clone()
            .map(|status| Ticker::start(status, || terminal_size().map(|(cols, _)| cols)));
        Self {
            mode,
            width,
            started: Instant::now(),
            status,
            ticker,
            failed: false,
            verdict: Vec::new(),
            diagnosis: Vec::new(),
            widths: OuterWidths::default(),
            progress,
            narrative,
        }
    }

    /// Whether a `failed` frame has been rendered.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.failed
    }

    /// Renders one typed daemon `event` frame; `--json` frames never reach the renderer.
    pub fn observe(&mut self, event: &Value) {
        if self.mode.json() {
            return;
        }
        let palette = Palette::new(self.mode.color());
        match event["event"].as_str().unwrap_or_default() {
            "plan" => self.plan_event(event),
            "started" => self.started_node(event, palette),
            "step" => {
                self.progress.observe_step(event);
                let actions = self.narrative.step(event, palette);
                self.sync_status(actions.is_empty());
                self.apply(actions, palette);
            }
            "log" => {
                let actions = self.narrative.log(event, self.width, palette);
                self.apply(actions, palette);
            }
            "note" => {
                let actions = self.narrative.note(event, palette);
                self.apply(actions, palette);
            }
            "finished" | "up-to-date" => self.finished_node(event, palette),
            "failed" => self.failed_node(event, palette),
            "ready" => self.ready_event(event),
            "stopped" => self.stopped_event(event),
            _ => {}
        }
    }

    /// Prints one styled free-form line through the active driver.
    pub fn line(&self, text: &str) {
        if self.mode.json() {
            return;
        }
        if let Some(status) = &self.status
            && let Ok(mut status) = status.lock()
        {
            status.emit(&[text.to_owned()]);
            return;
        }
        println!("{text}");
    }

    /// Stops the ticker, erases the status line, then prints the held verdict rows.
    pub fn finish(&mut self) {
        self.ticker = None;
        if let Some(status) = &self.status
            && let Ok(mut status) = status.lock()
        {
            status.erase();
        }
        self.status = None;
        let mut stdout = std::io::stdout().lock();
        for row in self.verdict.drain(..) {
            let _ = writeln!(stdout, "{row}");
        }
        let _ = stdout.flush();
        let mut stderr = std::io::stderr().lock();
        for row in self.diagnosis.drain(..) {
            let _ = writeln!(stderr, "{row}");
        }
        let _ = stderr.flush();
    }

    /// Routes one narrative action through the one status-line emitter or plain stdout.
    fn apply(&mut self, actions: Vec<build::Action>, palette: Palette) {
        for action in actions {
            match action {
                build::Action::Output {
                    stream,
                    text,
                    indent,
                } => {
                    let rows = build::wrap_gutter(&stream, &text, self.width, palette, indent);
                    self.emit_rows(&rows);
                }
                build::Action::Settle(row) | build::Action::Print(row) => {
                    self.emit_rows(&[row]);
                }
                build::Action::Diagnose(row) => {
                    if self.status.is_some() {
                        self.diagnosis.push(row);
                    } else {
                        eprintln!("{row}");
                    }
                }
            }
        }
    }

    /// Emits completed rows as ordinary lines (repainting the status line when one is painted).
    fn emit_rows(&mut self, rows: &[String]) {
        if rows.is_empty() {
            return;
        }
        if let Some(status) = &self.status
            && let Ok(mut status) = status.lock()
        {
            status.emit(rows);
            return;
        }
        for row in rows {
            println!("{row}");
        }
    }

    /// Latches the announced rows and their plan positions from the one `plan` frame.
    fn plan_event(&mut self, event: &Value) {
        if self.progress.announced() {
            return;
        }
        let Some(rows) = event["rows"].as_array() else {
            return;
        };
        let announced: Vec<(String, String)> = rows
            .iter()
            .filter_map(|row| {
                Some((
                    row["kind"].as_str()?.to_owned(),
                    row["name"].as_str()?.to_owned(),
                ))
            })
            .collect();
        if announced.is_empty() {
            return;
        }
        let total = event["total"]
            .as_u64()
            .and_then(|total| usize::try_from(total).ok())
            .unwrap_or(announced.len());
        self.progress.announce(&announced, total);
        self.refresh_widths();
        self.sync_status(true);
    }

    fn refresh_widths(&mut self) {
        self.widths = text::outer_widths(self.progress.plan());
        self.narrative.set_outer(self.widths);
    }

    fn sync_status(&self, redraw: bool) {
        if let Some(status) = &self.status
            && let Ok(mut status) = status.lock()
        {
            status.set_progress(self.progress.clone());
            if redraw {
                status.paint();
            }
        }
    }

    fn started_node(&mut self, event: &Value, palette: Palette) {
        let kind = event["kind"].as_str().unwrap_or("step").to_owned();
        let name = event["name"]
            .as_str()
            .or_else(|| event["label"].as_str())
            .unwrap_or("step")
            .to_owned();
        let step = event["step"].as_u64().unwrap_or(0);
        if !self.progress.announced() {
            self.progress.remember(&kind, &name);
            self.refresh_widths();
        }
        let (index, outer_total) =
            self.progress
                .position_at(&kind, &name, usize::try_from(step).unwrap_or(0));
        let position = (index > 0 && outer_total > 0).then_some((index, outer_total));
        if kind != "image" && index > 0 {
            self.progress
                .set_active(index, outer_total, text::outer_verb(&kind), &name);
        }
        let actions = self.narrative.started(event, position);
        // No eager `→` row: an outer step that streams nothing earns only its later verdict, and a
        // streaming one earns its single header from the first `log` line.
        self.sync_status(true);
        self.apply(actions, palette);
    }

    fn finished_node(&mut self, event: &Value, palette: Palette) {
        let Some(node) = Node::from_event(event) else {
            return;
        };
        self.progress.advance(node.kind, node.name);
        if node.kind == "image" {
            self.progress.finish_build();
        } else {
            self.progress.clear_active();
        }
        if !self.progress.announced() {
            self.progress.remember(node.kind, node.name);
            self.refresh_widths();
        }
        if let Some(actions) = self
            .narrative
            .finish_node(node.kind, node.name, event, palette)
        {
            self.sync_status(actions.is_empty());
            self.apply(actions, palette);
            return;
        }
        let frame = build::counter(event).unwrap_or((0, 0));
        let (index, total) = self.progress.settled(node.kind, node.name, frame);
        let row = completed_row(&node, index, total, &self.widths, self.mode.color());
        self.emit_rows(&[row]);
    }

    fn failed_node(&mut self, event: &Value, palette: Palette) {
        self.failed = true;
        // Stop the repaint thread first, so no tick can restore a live status after the verdict.
        self.ticker = None;
        self.progress.clear_active();
        self.progress.finish_build();
        let actions = self.narrative.failed(event, palette);
        self.sync_status(true);
        self.apply(actions, palette);
    }

    fn ready_event(&mut self, event: &Value) {
        let url = event["url"].as_str().unwrap_or_default();
        let color = self.mode.color();
        let text = match event["fork"].as_str() {
            Some(fork) => text::forked_row(
                fork,
                u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
                url,
                color,
            ),
            None => text::ready_row(url, color),
        };
        if self.status.is_some() {
            self.verdict.push(text);
            return;
        }
        println!("{text}");
    }

    fn stopped_event(&mut self, event: &Value) {
        let world = event["world"].as_str().unwrap_or("world");
        let row = text::stopped_row(
            world,
            event["elapsed_ms"].as_u64().unwrap_or(0),
            self.mode.color(),
        );
        self.line(&row);
    }
}

impl Drop for BootTui {
    fn drop(&mut self) {
        self.finish();
    }
}
