//! The one live boot pane shared by `up`, `restart`, `fork`, `down`, and `drop`.
//!
//! Three drivers, one event model: a colour terminal establishes a `DECSTBM` scroll region where
//! every started task owns one fixed row (cyan Braille spinner plus live elapsed, rewritten in
//! place when it finishes) above a fixed `apt`-style panel on row `rows`; a `NO_COLOR`/`TERM=dumb`
//! terminal rewrites one `\r` ASCII line with zero `ESC[` bytes; a pipe gets `→ [i/n] kind name`
//! headers plus one `✓ …` row per node and zero `\r`.
//!
//! The daemon's `plan` frame arrives before the first row and latches the six-column grid, so a
//! long name or a later state can never shear a row that is already on screen.
//!
//! Every string lives in [`render`]; this module only schedules them. Mode and width arrive as
//! plain values from the composition root. Log frames are never rendered here: every build, hook,
//! and pod byte already tees to `<store>/logs/<world>/<id>.log` for `slipfork logs`.

mod ascii;
mod pane;
mod render;

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::ui::UiMode;
use crate::ui::table::ColumnWidths;

use self::ascii::AsciiLine;
use self::pane::{Pane, Screen, Ticker};
use self::render as text;
use self::render::{Node, completed_row};

pub use crate::ui::elapsed_text;

/// One live boot pane; drop it (via [`BootTui::finish`]) before printing the final result.
pub struct BootTui {
    mode: UiMode,
    width: usize,
    total: usize,
    done: usize,
    started: Instant,
    current: String,
    pane: Option<Arc<Mutex<Pane>>>,
    ascii: Option<Arc<Mutex<AsciiLine>>>,
    ticker: Option<Ticker>,
    failed: bool,
    /// Verdict rows held until the scroll region is restored, so the final line never scrolls.
    verdict: Vec<String>,
    /// The one failure sentence, printed to stderr after the region is restored.
    diagnostics: Vec<String>,
    /// The announced boot plan as raw `[icon, kind, name, state, duration, run-id]` cells, used
    /// only when a stream arrives without a `plan` frame.
    plan: Vec<Vec<String>>,
    /// Whether the `plan` frame already latched the grid and the operation count.
    announced: bool,
    /// Every announced `(kind, name)`, so a `Started` row only allocates a slot when it was planned.
    announced_rows: Vec<(String, String)>,
    /// The precomputed six-column grid shared by every running and completed row.
    widths: ColumnWidths,
}

/// Asks the real terminal for its cursor row (1-based) with a bounded `DSR` probe.
///
/// Raw mode is enabled only around the write, poll, and read of `ESC[6n`, then the exact captured
/// mode is restored. A pipe, a non-tty, a silent console, or a 60ms timeout yields `None` — the
/// pane then takes its region-scroll path instead of blocking on a terminal that never answers.
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
    let deadline = Instant::now() + Duration::from_millis(60);
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
    pub fn boot(mode: UiMode, width: usize, title: &str) -> Self {
        Self::new(mode, width, title, true)
    }

    /// Rows only, no progress bar (`down`, `drop`).
    #[must_use]
    pub fn rows(mode: UiMode, width: usize, title: &str) -> Self {
        Self::new(mode, width, title, false)
    }

    fn new(mode: UiMode, width: usize, title: &str, summary: bool) -> Self {
        // The only ambient-terminal probe lives here, at the composition root: a bounded 60ms
        // `DSR` ask so the anchored pane can seed its flow cursor from the shell prompt's row.
        let live = summary && mode.color();
        let prompt_row = if live { probe_cursor_row() } else { None };
        let pane = live.then(|| Arc::new(Mutex::new(Pane::new(title, width, prompt_row))));
        let ascii = (summary && mode.interactive() && !mode.color())
            .then(|| Arc::new(Mutex::new(AsciiLine::new(title, width))));
        let screen = pane
            .clone()
            .map(Screen::Bars)
            .or_else(|| ascii.clone().map(Screen::Ascii));
        Self {
            mode,
            width,
            total: 0,
            done: 0,
            started: Instant::now(),
            current: String::new(),
            pane,
            ascii,
            ticker: screen.map(Ticker::start),
            failed: false,
            verdict: Vec::new(),
            diagnostics: Vec::new(),
            plan: Vec::new(),
            announced: false,
            announced_rows: Vec::new(),
            widths: text::boot_widths(&[]),
        }
    }

    /// Whether a `failed` frame has been rendered.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.failed
    }

    /// Renders one typed daemon `event` frame.
    pub fn observe(&mut self, event: &Value) {
        match event["event"].as_str().unwrap_or_default() {
            "plan" => self.plan_event(event),
            "started" => self.started_node(event),
            // Log frames are captured under `<store>/logs` and read with `slipfork logs`; they
            // never reach the terminal.
            "log" => {}
            "finished" | "up-to-date" => self.finished_node(event),
            "failed" => self.failed_node(event),
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
        if let Some(ascii) = &self.ascii {
            if let Ok(mut line) = ascii.lock() {
                line.clear();
                // Keep the ticker's lock until this row is on the wire: it redraws the same
                // `\r` line from another thread and would otherwise erase characters mid-row.
                println!("{text}");
                return;
            }
        }
        println!("{text}");
    }

    /// Stops the ticker, restores the scroll region, then prints the held verdict.
    pub fn finish(&mut self) {
        self.ticker = None;
        if let Some(ascii) = &self.ascii
            && let Ok(mut line) = ascii.lock()
        {
            line.clear();
        }
        if let Some(pane) = &self.pane
            && let Ok(mut pane) = pane.lock()
        {
            pane.restore();
        }
        // Post-finish rows (`✓ Restarted …`, verdicts) must go through plain `println!`: keeping
        // the live pane would re-enter its region and leave a stale `⠋` row above the shell.
        self.pane = None;
        self.ascii = None;
        // The ticker is joined and the pane mutex is free, so one stdout lock now serializes
        // every remaining byte (verdicts and diagnostics) with no other writer alive.
        let mut stdout = std::io::stdout().lock();
        for row in self.verdict.drain(..) {
            let _ = writeln!(stdout, "{row}");
        }
        let _ = stdout.flush();
        drop(stdout);
        for row in self.diagnostics.drain(..) {
            eprintln!("{row}");
        }
    }

    /// Latches the six-column grid and the operation count from the one `plan` frame.
    ///
    /// This happens before row 1 is printed, so a name or a state that arrives later can never
    /// shift a row that is already on screen. A second plan frame is ignored.
    fn plan_event(&mut self, event: &Value) {
        if self.announced {
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
        self.announced = true;
        self.widths = text::plan_widths(&announced);
        let total = event["total"]
            .as_u64()
            .and_then(|total| usize::try_from(total).ok())
            .unwrap_or(announced.len());
        self.total = self.total.max(total);
        self.announced_rows = announced;
        if let Some(pane) = &self.pane
            && let Ok(mut pane) = pane.lock()
        {
            pane.set_widths(self.widths.clone());
        }
    }

    fn started_node(&mut self, event: &Value) {
        let kind = event["kind"].as_str().unwrap_or("step").to_owned();
        let name = event["name"]
            .as_str()
            .or_else(|| event["label"].as_str())
            .unwrap_or("step")
            .to_owned();
        let step = event["step"].as_u64().unwrap_or(0);
        let total = event["total"].as_u64().unwrap_or(0);
        self.total = self.total.max(total as usize).max(step as usize);
        self.current.clone_from(&name);
        // A `Started` for a row the plan never announced (a phase header such as `down`'s
        // `stopping`) only updates the counters: it allocates no slot and no column width.
        let live = !self.announced
            || self
                .announced_rows
                .iter()
                .any(|(planned_kind, planned_name)| planned_kind == &kind && planned_name == &name);
        if !self.announced {
            // No plan frame: fall back to the legacy growing row set, six cells wide.
            self.plan.push(vec![
                String::new(),
                kind.clone(),
                name.clone(),
                String::new(),
                String::new(),
                String::new(),
            ]);
            self.widths = text::boot_widths(&self.plan);
        }
        if let Some(pane) = &self.pane {
            if let Ok(mut pane) = pane.lock() {
                pane.set_widths(self.widths.clone());
                pane.start(&kind, &name, self.total, live);
            }
            return;
        }
        if self.mode.json() {
            return;
        }
        if self.ascii.is_some() {
            self.ascii_update();
        } else {
            println!("{}", text::header_row(step, total, &kind, &name));
        }
    }

    fn finished_node(&mut self, event: &Value) {
        let Some(node) = Node::from_event(event) else {
            return;
        };
        self.done += 1;
        if !self.announced {
            let state = node.state.to_lowercase();
            match self
                .plan
                .iter_mut()
                .rev()
                .find(|row| row[1].as_str() == node.kind && row[2].as_str() == node.name)
            {
                Some(row) => row[3].clone_from(&state),
                None => self.plan.push(vec![
                    String::new(),
                    node.kind.to_owned(),
                    node.name.to_owned(),
                    state,
                    String::new(),
                    String::new(),
                ]),
            }
            self.widths = text::boot_widths(&self.plan);
        }
        let row = completed_row(&node, &self.widths, self.mode.color());
        if let Some(pane) = &self.pane
            && let Ok(mut pane) = pane.lock()
        {
            pane.set_widths(self.widths.clone());
            pane.complete(node.kind, node.name, &row, self.done);
            return;
        }
        if self.mode.json() {
            return;
        }
        self.line(&row);
        if self.ascii.is_some() {
            self.ascii_update();
        }
    }

    fn failed_node(&mut self, event: &Value) {
        self.failed = true;
        let kind = event["kind"].as_str().unwrap_or("step");
        let name = event["name"]
            .as_str()
            .or_else(|| event["label"].as_str())
            .unwrap_or("step");
        if self.mode.json() {
            return;
        }
        let row = text::failed_row(kind, name, self.mode.color());
        // Exactly one `next:` per failure: the catalog run id supplies a styled replay command, or
        // the step's own remedy is used when it never opened a capture.
        let (details, sentence) = text::failure_block(event, self.width, self.mode.color());
        if self.pane.is_some() {
            if let Some(pane) = &self.pane
                && let Ok(mut pane) = pane.lock()
            {
                pane.set_widths(self.widths.clone());
                pane.complete(kind, name, &row, self.done);
            }
            self.verdict.extend(details);
            self.diagnostics.push(sentence);
            return;
        }
        self.line(&row);
        for detail in details {
            self.line(&detail);
        }
        eprintln!("{sentence}");
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
        self.line(&text);
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

    /// Pushes the current counters into the ASCII line and redraws it immediately.
    fn ascii_update(&self) {
        if let Some(ascii) = &self.ascii
            && let Ok(mut line) = ascii.lock()
        {
            line.show(self.done, self.total, &self.current);
        }
    }
}

impl Drop for BootTui {
    fn drop(&mut self) {
        self.finish();
    }
}
