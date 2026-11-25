//! The one live boot pane shared by `up`, `restart`, `fork`, `down`, and `drop`.
//!
//! Two drivers, one event model: a colour terminal establishes a `DECSTBM` scroll region where a
//! running build step owns the bottom block (spinner phase row plus its streaming output) and every
//! settled step collapses to one scrollback row; a pipe or a `NO_COLOR` terminal is append-only —
//! `→ [i/n] KIND target`, then indented `n/N` inner Dockerfile rows, every output line in the
//! gutter shape, `✓` when it settles, zero `ESC[` bytes and no spinner frames.
//!
//! The daemon's `plan` frame latches the plan positions before row 1; `--json` passes through.

mod build;
mod failure;
mod layout;
mod pane;
mod phase;
mod progress;
mod region;
mod render;

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::Value;

use crate::ui::Palette;
use crate::ui::UiMode;

use self::build::Narrative;
use self::pane::{Pane, Ticker};
use self::progress::BootProgress;
use self::render as text;
use self::render::{Node, OuterWidths, completed_row};

pub use self::render::dropped_row;
pub use crate::ui::elapsed_text;

/// One live boot pane; drop it (via [`BootTui::finish`]) before printing the final result.
pub struct BootTui {
    mode: UiMode,
    width: usize,
    started: Instant,
    pane: Option<Arc<Mutex<Pane>>>,
    ticker: Option<Ticker>,
    failed: bool,
    /// Narrative rows held until the scroll region is restored, so the final line never scrolls.
    verdict: Vec<String>,
    /// The verdict envelope held for stderr: `✗ …`, optional `runtime:`, one `next:`.
    diagnosis: Vec<String>,
    /// The plan-derived outer `kind` and `target` column widths.
    widths: OuterWidths,
    /// The announced plan, both progress levels, and the bar label.
    progress: BootProgress,
    /// The live build/hook narrative: phase rows, output tails, collapse, and the failure block.
    narrative: Narrative,
}

/// Asks the real terminal for its cursor row (1-based) with a bounded 60ms `DSR` probe.
fn probe_cursor_row() -> Option<usize> {
    // SAFETY: `isatty(3)` only inspects descriptor flags.
    let tty =
        unsafe { libc::isatty(libc::STDIN_FILENO) == 1 && libc::isatty(libc::STDOUT_FILENO) == 1 };
    if !tty {
        return None;
    }
    let mut captured = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `tcgetattr` initializes the buffer for a live tty descriptor.
    if unsafe { libc::tcgetattr(libc::STDIN_FILENO, captured.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: the call above succeeded and fully initialized the buffer.
    let original = unsafe { captured.assume_init() };
    let mut raw = original;
    // SAFETY: `cfmakeraw` only mutates the local termios copy.
    unsafe { libc::cfmakeraw(&mut raw) };
    raw.c_cc[libc::VMIN] = 0;
    raw.c_cc[libc::VTIME] = 0;
    // SAFETY: `raw` is a fully initialized termios for this tty descriptor.
    if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) } != 0 {
        return None;
    }
    let asked = {
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(b"\x1b[6n")
            .and_then(|()| stdout.flush())
            .is_ok()
    };
    let row = if asked { read_cursor_row() } else { None };
    // SAFETY: restoring the exact termios captured before the probe.
    unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &original) };
    row
}

/// Reads `ESC[{row};{col}R` within 60ms; any silence, partial reply, or stray byte yields `None`.
fn read_cursor_row() -> Option<usize> {
    let deadline = Instant::now() + std::time::Duration::from_millis(60);
    let mut response = Vec::new();
    while response.len() < 32 {
        let left = deadline.saturating_duration_since(Instant::now());
        let millis = i32::try_from(left.as_millis()).unwrap_or(60).max(1);
        let mut poll = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid `pollfd` lives across the call and the wait is capped at 60ms.
        let ready = unsafe { libc::poll(&mut poll, 1, millis) };
        if ready <= 0 {
            return None;
        }
        let mut byte = 0_u8;
        // SAFETY: `byte` is one writable byte on a live tty descriptor.
        let read =
            unsafe { libc::read(libc::STDIN_FILENO, std::ptr::from_mut(&mut byte).cast(), 1) };
        if read != 1 {
            return None;
        }
        response.push(byte);
        if byte == b'R' {
            break;
        }
    }
    let text = std::str::from_utf8(&response).ok()?;
    let body = text.strip_prefix("\x1b[")?.strip_suffix('R')?;
    let (row, _) = body.split_once(';')?;
    row.parse().ok()
}

impl BootTui {
    /// A pane with the anchored progress bar (`up`, `restart`, `fork`).
    #[must_use]
    pub fn boot(mode: UiMode, width: usize, title: &str, verbose: bool) -> Self {
        Self::new(mode, width, title, true, verbose)
    }

    /// Rows only, no progress bar (`down`, `drop`).
    #[must_use]
    pub fn rows(mode: UiMode, width: usize, title: &str) -> Self {
        Self::new(mode, width, title, false, false)
    }

    fn new(mode: UiMode, width: usize, title: &str, summary: bool, verbose: bool) -> Self {
        // The only ambient-terminal probe lives here, at the composition root: a bounded 60ms
        // `DSR` ask so the anchored pane can seed its flow cursor from the shell prompt's row.
        let progress = BootProgress::new(title);
        let narrative = Narrative::new(verbose);
        let live = summary && mode.color();
        let prompt_row = if live { probe_cursor_row() } else { None };
        let pane = live.then(|| {
            Arc::new(Mutex::new(Pane::new(
                progress.clone(),
                width,
                prompt_row,
                narrative.steps(),
            )))
        });
        let ticker = pane.clone().map(Ticker::start);
        Self {
            mode,
            width,
            started: Instant::now(),
            pane,
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
                self.sync_pane(actions.is_empty());
                self.apply(actions, palette);
            }
            "log" => {
                let actions = self
                    .narrative
                    .log(event, self.width, palette, self.pane.is_some());
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
        if let Some(pane) = &self.pane {
            if let Ok(mut pane) = pane.lock() {
                pane.println(text);
            }
            return;
        }
        println!("{text}");
    }

    /// Stops the ticker, restores the scroll region, then prints the held verdict.
    pub fn finish(&mut self) {
        self.ticker = None;
        if let Some(pane) = &self.pane
            && let Ok(mut pane) = pane.lock()
        {
            pane.restore();
        }
        // Post-finish rows (`✓ Ready …`, the failure block) must go through plain `println!`:
        // keeping the live pane would re-enter its region and leave a stale spinner row.
        self.pane = None;
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

    /// Routes one narrative action to the anchored region or straight to stdout/stderr.
    fn apply(&mut self, actions: Vec<build::Action>, palette: Palette) {
        for action in actions {
            match action {
                build::Action::Begin(phase) => {
                    if let Some(pane) = &self.pane {
                        if let Ok(mut pane) = pane.lock() {
                            pane.draw();
                        }
                    } else {
                        println!("{}", phase.started_row(palette));
                    }
                }
                build::Action::Output {
                    stream,
                    text,
                    indent,
                } => {
                    if let Some(pane) = &self.pane {
                        if let Ok(mut pane) = pane.lock() {
                            pane.draw();
                        }
                    } else {
                        for row in build::wrap_gutter(&stream, &text, self.width, palette, indent) {
                            println!("{row}");
                        }
                    }
                }
                build::Action::Settle(row) => {
                    if let Some(pane) = &self.pane {
                        if let Ok(mut pane) = pane.lock() {
                            pane.draw();
                            pane.println(&row);
                        }
                    } else {
                        println!("{row}");
                    }
                }
                build::Action::Print(row) => {
                    if let Some(pane) = &self.pane {
                        if let Ok(mut pane) = pane.lock() {
                            pane.println(&row);
                        }
                    } else {
                        println!("{row}");
                    }
                }
                build::Action::Hold(row) => {
                    if self.pane.is_some() {
                        self.verdict.push(row);
                    } else {
                        println!("{row}");
                    }
                }
                build::Action::Diagnose(row) => {
                    if self.pane.is_some() {
                        self.diagnosis.push(row);
                    } else {
                        eprintln!("{row}");
                    }
                }
            }
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
        self.sync_pane(true);
    }

    fn refresh_widths(&mut self) {
        self.widths = text::outer_widths(self.progress.plan());
        self.narrative.set_outer(self.widths);
    }

    fn sync_pane(&self, redraw: bool) {
        if let Some(pane) = &self.pane
            && let Ok(mut pane) = pane.lock()
        {
            pane.set_widths(self.widths);
            pane.set_progress(self.progress.clone());
            if redraw {
                pane.draw();
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
        let total = event["total"].as_u64().unwrap_or(0);
        if !self.progress.announced() {
            self.progress.remember(&kind, &name);
            self.refresh_widths();
        }
        let live = !self.progress.announced() || self.progress.planned(&kind, &name);
        let (index, outer_total) =
            self.progress
                .position_at(&kind, &name, usize::try_from(step).unwrap_or(0));
        let position = (index > 0 && outer_total > 0).then_some((index, outer_total));
        if kind != "image" && index > 0 {
            self.progress
                .set_active(index, outer_total, text::outer_verb(&kind), &name);
        }
        let actions = self.narrative.started(event, position);
        let narrated = !actions.is_empty();
        // An image announces its instructions through `step` frames; it never needs its own row.
        let spinning = !narrated && kind != "image";
        if let Some(pane) = &self.pane {
            if let Ok(mut pane) = pane.lock() {
                pane.set_widths(self.widths);
                pane.set_progress(self.progress.clone());
                if spinning {
                    pane.start(&kind, &name, live);
                } else if !narrated {
                    pane.draw();
                }
            }
        } else if spinning {
            let (shown_step, shown_total) =
                position.map_or((step, total), |(idx, tot)| (idx as u64, tot as u64));
            println!(
                "{}",
                text::header_row(shown_step, shown_total, &kind, &name)
            );
        }
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
            self.sync_pane(actions.is_empty());
            self.apply(actions, palette);
            return;
        }
        let frame = build::counter(event).unwrap_or((0, 0));
        let (index, total) = self.progress.settled(node.kind, node.name, frame);
        let row = completed_row(&node, index, total, &self.widths, self.mode.color());
        if let Some(pane) = &self.pane
            && let Ok(mut pane) = pane.lock()
        {
            pane.set_widths(self.widths);
            pane.set_progress(self.progress.clone());
            pane.complete(node.kind, node.name, &row);
            return;
        }
        println!("{row}");
    }

    fn failed_node(&mut self, event: &Value, palette: Palette) {
        self.failed = true;
        // Stop the repaint thread first, so no tick can restore the live block after it clears.
        self.ticker = None;
        self.progress.clear_active();
        self.progress.finish_build();
        let anchored = self.pane.is_some();
        let actions = self.narrative.failed(event, self.width, palette, anchored);
        self.sync_pane(true);
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
        if self.pane.is_some() {
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
