//! The one live boot pane shared by `up`, `restart`, `fork`, `down`, and `drop`.
//!
//! Three drivers, one event model: a colour terminal establishes a `DECSTBM` scroll region with
//! one aligned row per finished node above a fixed `apt`-style panel on row `rows`; a
//! `NO_COLOR`/`TERM=dumb` terminal rewrites one `\r` ASCII line with zero `ESC[` bytes; a pipe
//! gets `→ [i/n] kind name` headers plus one `✓ …` row per node and zero `\r`.
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

use self::ascii::AsciiLine;
use self::pane::{Pane, Screen, Ticker};
use self::render as text;
use self::render::{Node, completed_row};

pub use self::render::elapsed_text;

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
        if let Some(pane) = &self.pane {
            if let Ok(mut pane) = pane.lock() {
                pane.start(&name, self.total);
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
        let row = completed_row(&node, self.mode.color());
        if let Some(pane) = &self.pane
            && let Ok(mut pane) = pane.lock()
        {
            pane.finish(self.done);
            pane.println(&row);
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
        let details = text::failure_details(
            event["message"].as_str().unwrap_or_default(),
            event["remedy"].as_str(),
            event["line"].as_u64(),
            self.width,
            self.mode.color(),
        );
        let sentence = text::failure_sentence(kind, name);
        if self.pane.is_some() {
            self.verdict.push(row);
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
