//! The `DECSTBM` fixed bottom bar with one live row per running task.
//!
//! Rows `1..rows-1` are a 2D content grid: every `Started` task allocates its own fixed slot and
//! keeps it for its whole life, spinning in place with a cyan Braille spinner and live elapsed
//! time until its `Finished`/`UpToDate`/`Failed` row rewrites that same slot. When the grid is
//! full, the scroll region moves up exactly one line and the top task leaves the grid; nothing
//! else moves. Row `rows` stays a one-line `apt`-style panel, refreshed by a 120ms ticker and
//! re-applied on `SIGWINCH`.
//!
//! Every escape is written here only after the composition root resolved [`UiMode::ColorTty`];
//! this module holds only terminal state and delegates every string to [`super::render`].
//!
//! The flow cursor is seeded from the shell prompt's line and every paint happens inside
//! `ESC 7 … ESC 8`, so drawing task rows and the panel never moves the cursor the shell expects.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::ui::table::ColumnWidths;

use super::ascii::AsciiLine;
use super::render::{panel_line, running_row};

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
    at
}

/// One live task row: its fixed slot, identity, and the clock its spinner reads.
#[derive(Debug, Clone)]
pub(super) struct Task {
    pub(super) slot: usize,
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) started: Instant,
    pub(super) done: bool,
}

/// Where one allocation landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Slot {
    /// A free content row.
    Row(usize),
    /// A free content row that required one physical region scroll first.
    Scrolled(usize),
    /// No region (`rows < 3`): counters only.
    Unavailable,
}

impl Slot {
    fn row(self) -> Option<usize> {
        match self {
            Self::Row(row) | Self::Scrolled(row) => Some(row),
            Self::Unavailable => None,
        }
    }

    fn scrolled(self) -> bool {
        matches!(self, Self::Scrolled(_))
    }
}

/// The 2D content grid: one fixed slot per task, scrolled one line at a time when it fills up.
#[derive(Debug)]
pub(super) struct TaskRows {
    tasks: Vec<Task>,
    /// The next free content row; `bottom + 1` means the grid is full.
    next: usize,
    bottom: usize,
}

impl TaskRows {
    pub(super) fn new(next: usize, bottom: usize) -> Self {
        Self {
            tasks: Vec::new(),
            next,
            bottom,
        }
    }

    /// Allocates a fixed row for one started task.
    pub(super) fn begin(&mut self, kind: &str, name: &str) -> Slot {
        let slot = self.take();
        if let Some(row) = slot.row() {
            self.tasks.push(Task {
                slot: row,
                kind: kind.to_owned(),
                name: name.to_owned(),
                started: Instant::now(),
                done: false,
            });
        }
        slot
    }

    /// Allocates a free row for one free-form line.
    pub(super) fn place(&mut self) -> Slot {
        self.take()
    }

    /// Marks the matching task finished and returns the fixed slot its row must be rewritten in.
    pub(super) fn complete(&mut self, kind: &str, name: &str) -> Option<usize> {
        let task = self
            .tasks
            .iter_mut()
            .find(|task| !task.done && task.kind == kind && task.name == name)?;
        task.done = true;
        Some(task.slot)
    }

    /// Tasks that still own a spinning row, top to bottom.
    pub(super) fn live(&self) -> impl Iterator<Item = &Task> {
        self.tasks.iter().filter(|task| !task.done)
    }

    /// The names of every running task, in slot order.
    pub(super) fn running(&self) -> Vec<&str> {
        self.live().map(|task| task.name.as_str()).collect()
    }

    /// The blank row under the last task, when one is still free.
    pub(super) fn free_row(&self) -> Option<usize> {
        (self.next <= self.bottom).then_some(self.next)
    }

    /// The next allocation row, clamped to the content region.
    pub(super) fn next_row(&self) -> usize {
        self.next.min(self.bottom.max(1))
    }

    /// Re-seeds the grid after a resize, dropping rows the terminal no longer owns.
    pub(super) fn fit(&mut self, next: usize, bottom: usize) {
        self.bottom = bottom;
        if bottom == 0 {
            self.tasks.clear();
            self.next = 1;
            return;
        }
        self.next = next.clamp(1, bottom);
        self.tasks.retain(|task| task.slot <= bottom);
        for task in &mut self.tasks {
            task.slot = task.slot.clamp(1, bottom);
        }
    }

    fn take(&mut self) -> Slot {
        if self.bottom == 0 {
            return Slot::Unavailable;
        }
        let scrolled = self.next > self.bottom;
        if scrolled {
            // One line leaves the top of the region: every surviving slot moves up one, and a
            // task that lived on row 1 is gone from the grid for good.
            for task in &mut self.tasks {
                task.slot = task.slot.saturating_sub(1);
            }
            self.tasks.retain(|task| task.slot >= 1);
            self.next = self.bottom;
        }
        let slot = self.next;
        self.next += 1;
        if scrolled {
            Slot::Scrolled(slot)
        } else {
            Slot::Row(slot)
        }
    }
}

/// The one shared live region: task rows, the panel on row `rows`, and its scroll-region guard.
pub(super) struct Pane {
    cols: usize,
    rows: usize,
    guard: Option<ScrollRegionGuard>,
    total: usize,
    done: usize,
    title: String,
    summary: String,
    started: Instant,
    frame: usize,
    line_len: usize,
    tasks: TaskRows,
    widths: ColumnWidths,
    /// The grid was full, so the next paint must scroll the region one line first.
    pending_scroll: bool,
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
            title,
            started: Instant::now(),
            frame: 0,
            line_len: 0,
            tasks: TaskRows::new(0, 0),
            widths: ColumnWidths::default(),
            pending_scroll: false,
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
        let flow = pane.seed_flow(prompt_row);
        let bottom = if pane.rows >= 3 { pane.rows - 1 } else { 0 };
        pane.tasks = TaskRows::new(flow, bottom);
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
        let bottom = if rows >= 3 { rows - 1 } else { 0 };
        self.tasks.fit(self.tasks.next_row(), bottom);
        self.guard = ScrollRegionGuard::install(rows);
        if self.guard.is_some() {
            let mut stdout = std::io::stdout().lock();
            let _ = write!(stdout, "\x1b[{};1H", self.tasks.next_row());
            let _ = stdout.flush();
        }
        self.draw();
    }

    /// Pins the six-column grid the running and finished rows share.
    pub(super) fn set_widths(&mut self, widths: ColumnWidths) {
        self.widths = widths;
    }

    /// Allocates a fixed row for one started task and paints its first spinner frame.
    pub(super) fn start(&mut self, kind: &str, name: &str, total: usize, live: bool) {
        self.total = self.total.max(total);
        if live {
            let slot = self.tasks.begin(kind, name);
            self.pending_scroll |= slot.scrolled();
        }
        self.refresh_summary();
        self.draw();
    }

    /// Rewrites one finished task's own slot in place and never moves another row.
    pub(super) fn complete(&mut self, kind: &str, name: &str, row: &str, done: usize) {
        self.done = done;
        self.total = self.total.max(done);
        self.refresh_summary();
        // A finished row that never allocated a live slot (a phase header the plan did not
        // announce, such as `restart`'s) still prints once, at the next free row.
        let slot = match self.tasks.complete(kind, name) {
            Some(slot) => Slot::Row(slot),
            None => self.tasks.place(),
        };
        self.write_slot(slot, row);
        self.draw();
    }

    /// Prints one free-form row at the next free slot.
    pub(super) fn println(&mut self, text: &str) {
        let slot = self.tasks.place();
        self.write_slot(slot, text);
        self.draw();
    }

    /// Writes `text` at one allocated slot, scrolling the region first when it was full.
    fn write_slot(&mut self, slot: Slot, text: &str) {
        if self.guard.is_none() {
            self.clear_line();
            println!("{text}");
            return;
        }
        let mut stdout = std::io::stdout().lock();
        if slot.scrolled() {
            let _ = write!(stdout, "\x1b[{};1H\r\n", self.tasks.bottom);
        }
        if let Some(row) = slot.row() {
            let _ = write!(stdout, "\x1b[{row};1H\r\x1b[2K{text}");
        }
        let _ = stdout.flush();
    }

    /// Clears the panel row, restores the scrolling region, and parks on the next free row.
    pub(super) fn restore(&mut self) {
        let had_region = self.guard.take().is_some();
        let mut stdout = std::io::stdout().lock();
        if had_region {
            if let Some(row) = self.tasks.free_row() {
                let _ = write!(stdout, "\x1b[{row};1H\x1b[2K");
            }
        } else if self.line_len > 0 {
            let _ = write!(stdout, "\r{}\r", " ".repeat(self.line_len));
            self.line_len = 0;
        }
        let _ = stdout.flush();
    }

    /// Seeds the flow cursor from the prompt line and scrolls a bottom prompt out of the way.
    fn seed_flow(&mut self, prompt_row: Option<usize>) -> usize {
        let mut flow = self.rows.saturating_sub(1);
        if self.rows < 3 {
            return flow;
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
            Some(row) => flow = row.min(bottom),
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
            flow = bottom;
        }
        let mut stdout = std::io::stdout().lock();
        let _ = write!(stdout, "\x1b[{flow};1H");
        let _ = stdout.flush();
        flow
    }

    fn refresh_summary(&mut self) {
        let running = self.tasks.running();
        self.summary = match running.as_slice() {
            [] => self.title.clone(),
            [only] => format!("Starting {only}…"),
            names => super::render::running_summary(names),
        };
    }

    fn clear_line(&mut self) {
        if self.line_len > 0 {
            print!("\r{}\r", " ".repeat(self.line_len));
            self.line_len = 0;
            let _ = std::io::stdout().flush();
        }
    }

    fn draw(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        let spinner = BRAILLE.chars().nth(self.frame % 10).unwrap_or('⠋');
        let summary = self.summary.clone();
        let line = panel_line(
            spinner,
            self.done,
            self.total,
            &summary,
            self.started.elapsed(),
            self.cols,
        );
        let mut stdout = std::io::stdout().lock();
        if self.guard.is_some() {
            let _ = write!(stdout, "\x1b7");
            if std::mem::take(&mut self.pending_scroll) {
                let _ = write!(stdout, "\x1b[{};1H\r\n", self.tasks.bottom);
            }
            let live: Vec<(usize, String)> = self
                .tasks
                .live()
                .map(|task| {
                    (
                        task.slot,
                        running_row(
                            &task.kind,
                            &task.name,
                            spinner,
                            u64::try_from(task.started.elapsed().as_millis()).unwrap_or(u64::MAX),
                            &self.widths,
                        ),
                    )
                })
                .collect();
            for (slot, row) in live {
                let _ = write!(stdout, "\x1b[{slot};1H\r\x1b[2K{row}");
            }
            // The panel is always painted inside the saved cursor, so no task row or panel draw
            // ever moves the flow cursor the next allocation depends on.
            let _ = write!(stdout, "\x1b[{};1H\x1b[2K{line}\x1b[K\x1b8", self.rows);
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

/// Refreshes the visible pane every 120ms so every spinner and elapsed time stays live even when
/// the daemon is quiet: the colour panel, or the ASCII line of a `NO_COLOR` terminal.
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

#[cfg(test)]
mod tests {
    use super::{Slot, TaskRows};
    use crate::boot::render::{Node, completed_row, plan_widths};

    /// Two `Started` events in one tick own two distinct rows, spin together, and finish in place.
    #[test]
    fn two_started_tasks_spin_on_their_own_rows_and_finish_in_place() {
        let mut tasks = TaskRows::new(4, 10);
        assert_eq!(tasks.begin("image", "base"), Slot::Row(4));
        assert_eq!(tasks.begin("service", "front"), Slot::Row(5));
        assert_eq!(tasks.running(), ["base", "front"]);

        let widths = plan_widths(&[
            ("image".to_owned(), "base".to_owned()),
            ("service".to_owned(), "front".to_owned()),
        ]);
        let spinning: Vec<(usize, String)> = tasks
            .live()
            .map(|task| {
                (
                    task.slot,
                    crate::boot::render::running_row(&task.kind, &task.name, '⠋', 900, &widths),
                )
            })
            .collect();
        assert_eq!(spinning[0].0, 4);
        assert_eq!(spinning[1].0, 5);
        for (slot, row) in &spinning {
            assert!(row.contains('⠋'), "slot {slot} must spin: {row:?}");
            assert!(row.contains("running"), "slot {slot}: {row:?}");
        }

        let finished = completed_row(
            &Node {
                kind: "image",
                name: "base",
                state: "Cached",
                millis: 120,
                success: true,
            },
            &widths,
            false,
        );
        assert!(finished.contains('✓'));
        assert_eq!(
            tasks.complete("image", "base"),
            Some(4),
            "the finished row rewrites slot 4"
        );
        assert_eq!(tasks.running(), ["front"], "the other row keeps spinning");
        assert_eq!(
            tasks.live().next().map(|task| task.slot),
            Some(5),
            "the live row never moves"
        );
        assert_eq!(tasks.complete("service", "front"), Some(5));
        assert!(tasks.running().is_empty());
    }

    /// A full grid scrolls exactly one line and the task that leaves the top is gone for good.
    #[test]
    fn a_full_grid_scrolls_one_line_and_drops_the_top_task() {
        let mut tasks = TaskRows::new(2, 2);
        assert_eq!(tasks.begin("image", "one"), Slot::Row(2));
        assert_eq!(tasks.begin("service", "two"), Slot::Scrolled(2));
        let slots: Vec<usize> = tasks.live().map(|task| task.slot).collect();
        assert_eq!(slots, [1, 2], "every surviving task moves up one row");

        assert_eq!(tasks.begin("setup", "three"), Slot::Scrolled(2));
        let names: Vec<&str> = tasks.running();
        assert_eq!(names, ["two", "three"], "the row-1 task left the grid");
        assert_eq!(tasks.complete("one", "gone"), None);
    }
}
