//! The `DECSTBM` fixed bottom bar: rows `1..rows-1` scroll finished tasks while row `rows`
//! stays a one-line `apt`-style panel, refreshed by a 120ms ticker and re-applied on `SIGWINCH`.
//!
//! Every escape is written here only after the composition root resolved [`UiMode::ColorTty`];
//! this module holds only terminal state and delegates every string to [`super::render`].
//!
//! Task rows follow a real flow cursor seeded from the shell prompt's line: the first task lands
//! one row below the cursor, and a prompt found on the bottom row is scrolled into the region
//! before anything is cleared. The panel is always painted inside `ESC 7 … ESC 8`, so drawing it
//! never moves the flow cursor that the next task row depends on.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::ascii::AsciiLine;
use super::render::panel_line;

/// Braille spinner for a terminal that accepts ANSI.
const BRAILLE: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

static RESIZED: AtomicBool = AtomicBool::new(false);
static REGION_ROWS: AtomicUsize = AtomicUsize::new(0);
static PREVIOUS_SIGINT: AtomicUsize = AtomicUsize::new(0);
static PREVIOUS_SIGWINCH: AtomicUsize = AtomicUsize::new(0);

extern "C" fn on_sigwinch(_: libc::c_int) {
    RESIZED.store(true, Ordering::Relaxed);
}

extern "C" fn on_sigint(signal: libc::c_int) {
    let rows = REGION_ROWS.load(Ordering::Relaxed);
    if rows >= 3 {
        let mut buffer = [0_u8; 32];
        let mut length = 0;
        buffer[length] = 0x1b;
        length += 1;
        buffer[length] = b'[';
        length += 1;
        length = push_decimal(&mut buffer, length, rows);
        for byte in b";1H\x1b[2K\x1b[r" {
            buffer[length] = *byte;
            length += 1;
        }
        // SAFETY: `write(2)` only reads `buffer`; escape restoration is async-signal-safe.
        unsafe {
            libc::write(libc::STDOUT_FILENO, buffer.as_ptr().cast(), length);
        }
    }
    // SAFETY: `_exit(2)` ends the process without touching shared state or unwinding.
    unsafe { libc::_exit(128 + signal) };
}

fn push_decimal(buffer: &mut [u8], mut at: usize, mut value: usize) -> usize {
    let start = at;
    if value == 0 {
        buffer[at] = b'0';
        return at + 1;
    }
    let mut digits = [0_u8; 8];
    let mut count = 0;
    while value > 0 {
        digits[count] = b'0' + (value % 10) as u8;
        value /= 10;
        count += 1;
    }
    while count > 0 {
        count -= 1;
        buffer[at] = digits[count];
        at += 1;
    }
    let _ = start;
    at
}

/// The one shared live region: the panel on row `rows` and its scroll-region guard.
pub(super) struct Pane {
    cols: usize,
    rows: usize,
    guard: Option<ScrollRegionGuard>,
    total: usize,
    done: usize,
    summary: String,
    started: Instant,
    frame: usize,
    line_len: usize,
    /// The row the next task row will be painted on; always inside `1..=rows-1`.
    flow_row: usize,
}

impl Pane {
    pub(super) fn new(title: &str, cols: usize, prompt_row: Option<usize>) -> Self {
        let (width, rows) = terminal_size();
        let cols = cols.max(width);
        let title = title.to_owned();
        let mut pane = Self {
            cols,
            rows,
            guard: None,
            total: 0,
            done: 0,
            summary: title.clone(),
            started: Instant::now(),
            frame: 0,
            line_len: 0,
            flow_row: rows.saturating_sub(1),
        };
        // A missing probe (`None`) or one reporting the panel row cannot rule out a prompt hiding
        // there, and `install` clears that row; one full-screen scroll while no region is set
        // keeps the prompt visible. The region scroll in `seed_flow` then frees the margin.
        if rows >= 3 && prompt_row.is_none_or(|row| row >= rows) {
            let mut stdout = std::io::stdout().lock();
            let _ = write!(stdout, "\r\n");
            let _ = stdout.flush();
        }
        pane.guard = ScrollRegionGuard::install(rows);
        pane.seed_flow(prompt_row);
        pane.draw();
        pane
    }

    /// Re-queries the terminal after a resize and re-applies the scroll region.
    pub(super) fn set_size(&mut self, cols: usize, rows: usize) {
        if self.cols == cols && self.rows == rows {
            return;
        }
        self.guard = None;
        self.cols = cols;
        self.rows = rows;
        self.line_len = 0;
        if rows >= 3 {
            self.flow_row = self.flow_row.clamp(1, rows - 1);
        }
        self.guard = ScrollRegionGuard::install(rows);
        if self.guard.is_some() {
            let mut stdout = std::io::stdout().lock();
            let _ = write!(stdout, "\x1b[{};1H", self.flow_row);
            let _ = stdout.flush();
        }
        self.draw();
    }

    /// Prints one completed row at the natural flow cursor, one line below the shell prompt.
    pub(super) fn println(&mut self, text: &str) {
        let mut stdout = std::io::stdout().lock();
        if self.guard.is_some() {
            let row = self.flow_row;
            let _ = write!(stdout, "\x1b[{row};1H\r\x1b[2K{text}\r\n");
            self.flow_row = (row + 1).min(self.rows - 1);
        } else if self.line_len > 0 {
            let _ = write!(stdout, "\r{}\r", " ".repeat(self.line_len));
            self.line_len = 0;
            let _ = writeln!(stdout, "{text}");
        } else {
            let _ = writeln!(stdout, "{text}");
        }
        let _ = stdout.flush();
        drop(stdout);
        self.draw();
    }

    pub(super) fn start(&mut self, name: &str, total: usize) {
        self.total = self.total.max(total);
        self.summary = format!("Starting {name}…");
        self.draw();
    }

    pub(super) fn finish(&mut self, done: usize) {
        self.done = done;
        self.total = self.total.max(done);
        self.draw();
    }

    /// Clears the panel row, restores the scrolling region, and parks one line under the last row.
    pub(super) fn restore(&mut self) {
        let had_region = self.guard.take().is_some();
        let mut stdout = std::io::stdout().lock();
        if had_region {
            let _ = write!(stdout, "\x1b[{};1H\x1b[2K", self.flow_row);
        } else if self.line_len > 0 {
            let _ = write!(stdout, "\r{}\r", " ".repeat(self.line_len));
            self.line_len = 0;
        }
        let _ = stdout.flush();
    }

    /// Seeds the flow cursor from the prompt line and scrolls a bottom prompt out of the way.
    fn seed_flow(&mut self, prompt_row: Option<usize>) {
        if self.rows < 3 {
            return;
        }
        let bottom = self.rows - 1;
        let mut scrolled = false;
        match prompt_row {
            // The prompt shares the region's bottom margin (or sat on the panel row, already
            // scrolled by `new`): a real region scroll pushes it up before any task row writes,
            // so a task can never overwrite the prompt cells.
            Some(row) if row >= bottom => {
                let mut stdout = std::io::stdout().lock();
                let _ = write!(stdout, "\x1b[{bottom};1H\r\n");
                let _ = stdout.flush();
                scrolled = true;
            }
            // The probe reports the cursor, already one line below the prompt: no blank gap.
            Some(row) => self.flow_row = row.min(bottom),
            None => {
                // No DSR responder: never assume the last line is blank. Scroll one line at the
                // region's bottom margin so existing content stays visible above the tasks.
                let mut stdout = std::io::stdout().lock();
                let _ = write!(stdout, "\x1b[{bottom};1H\r\n");
                let _ = stdout.flush();
                scrolled = true;
            }
        }
        if scrolled {
            self.flow_row = bottom;
        }
        let mut stdout = std::io::stdout().lock();
        let _ = write!(stdout, "\x1b[{};1H", self.flow_row);
        let _ = stdout.flush();
    }

    fn draw(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        let spinner = BRAILLE.chars().nth(self.frame % 10).unwrap_or('⠋');
        let line = panel_line(
            spinner,
            self.done,
            self.total,
            &self.summary,
            self.started.elapsed(),
            self.cols,
        );
        let mut stdout = std::io::stdout().lock();
        if self.guard.is_some() {
            // `ESC 7`/`ESC 8` keep the flow cursor untouched while the panel repaints.
            let _ = write!(stdout, "\x1b7\x1b[{};1H\x1b[2K{line}\x1b[K\x1b8", self.rows);
        } else {
            let visible = console::measure_text_width(&line);
            if visible < self.line_len {
                let _ = write!(stdout, "\r{line}{}", " ".repeat(self.line_len - visible));
            } else {
                let _ = write!(stdout, "\r{line}");
            }
            self.line_len = visible;
        }
        let _ = stdout.flush();
    }
}

/// The RAII `DECSTBM` guard: applies `\x1b[1;{rows-1}r` once and always restores `\x1b[r`.
///
/// It is dropped on success, error, panic unwind, and the `SIGINT` handler; `rows < 3` creates no
/// region at all, so `\x1b[1;1r` and `\x1b[1;2r` can never be written.
pub(super) struct ScrollRegionGuard {
    rows: usize,
    active: bool,
}

impl ScrollRegionGuard {
    fn install(rows: usize) -> Option<Self> {
        if rows < 3 {
            return None;
        }
        let mut stdout = std::io::stdout().lock();
        let _ = write!(stdout, "\x1b[1;{}r\x1b[{};1H\x1b[2K", rows - 1, rows);
        let _ = stdout.flush();
        REGION_ROWS.store(rows, Ordering::Relaxed);
        RESIZED.store(false, Ordering::Relaxed);
        // SAFETY: signal handlers only set atomics or issue one async-signal-safe `write`.
        unsafe {
            PREVIOUS_SIGINT.store(
                libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t),
                Ordering::Relaxed,
            );
            PREVIOUS_SIGWINCH.store(
                libc::signal(
                    libc::SIGWINCH,
                    on_sigwinch as *const () as libc::sighandler_t,
                ),
                Ordering::Relaxed,
            );
        }
        Some(Self { rows, active: true })
    }
}

impl Drop for ScrollRegionGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let mut stdout = std::io::stdout().lock();
        let _ = write!(stdout, "\x1b[{};1H\x1b[2K\x1b[r", self.rows);
        let _ = stdout.flush();
        REGION_ROWS.store(0, Ordering::Relaxed);
        // SAFETY: restoring the handlers captured in `install` only rewrites process handlers.
        unsafe {
            let previous = PREVIOUS_SIGINT.load(Ordering::Relaxed);
            if previous != 0 && previous != libc::SIG_ERR {
                libc::signal(libc::SIGINT, previous);
            }
            let previous = PREVIOUS_SIGWINCH.load(Ordering::Relaxed);
            if previous != 0 && previous != libc::SIG_ERR {
                libc::signal(libc::SIGWINCH, previous);
            }
        }
    }
}

fn terminal_size() -> (usize, usize) {
    crossterm::terminal::size().map_or((80, 0), |(cols, rows)| {
        (usize::from(cols), usize::from(rows))
    })
}

/// Which live region the ticker refreshes.
pub(super) enum Screen {
    Bars(Arc<Mutex<Pane>>),
    Ascii(Arc<Mutex<AsciiLine>>),
}

/// Refreshes the visible pane every 120ms so the spinner and elapsed time stay live even when the
/// daemon is quiet: the colour panel, or the ASCII line of a `NO_COLOR` terminal.
pub(super) struct Ticker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Ticker {
    pub(super) fn start(screen: Screen) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                if RESIZED.swap(false, Ordering::Relaxed) {
                    let (cols, rows) = terminal_size();
                    match &screen {
                        Screen::Bars(pane) => {
                            if let Ok(mut pane) = pane.lock() {
                                pane.set_size(cols, rows);
                            }
                        }
                        Screen::Ascii(_) => {}
                    }
                }
                match &screen {
                    Screen::Bars(pane) => {
                        if let Ok(mut pane) = pane.lock() {
                            pane.draw();
                        }
                    }
                    Screen::Ascii(line) => {
                        if let Ok(mut line) = line.lock() {
                            line.draw();
                        }
                    }
                }
                thread::sleep(Duration::from_millis(120));
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
