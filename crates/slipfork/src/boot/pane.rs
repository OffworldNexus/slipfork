//! The `DECSTBM` fixed bottom bar with one live row per running task.
//!
//! Rows `1..rows-1` hold settled rows above [`LiveSteps`]'s bottom block; row `rows` is the
//! 120ms-refreshed panel, re-applied on `SIGWINCH`. Every escape is emitted only for a colour TTY.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::ui::Palette;
use crate::ui::table::ColumnWidths;

use super::build::LiveSteps;
use super::region::{ScrollRegionGuard, take_resized};
use super::render::{panel_line, running_row, status_line};

/// Braille spinner for a terminal that accepts ANSI.
const BRAILLE: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

/// A pane exists only for a colour terminal, so its block is always styled.
const PALETTE: Palette = Palette::new(true);

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

    /// Lowers the grid's bottom when `block` is empty, without scrolling settled rows.
    pub(super) fn set_bottom(&mut self, bottom: usize) {
        self.bottom = bottom;
    }

    /// Reserves rows below `bottom` for a non-empty narrative block, returning the physical region
    /// scrolls needed so settled rows at `bottom + 1 .. self.next` move up instead of being
    /// overwritten by `block`.
    pub(super) fn reserve_below(&mut self, bottom: usize) -> usize {
        self.bottom = bottom;
        let occupied = self.next.saturating_sub(1);
        if occupied <= bottom {
            return 0;
        }
        let shift = occupied - bottom;
        for task in &mut self.tasks {
            task.slot = task.slot.saturating_sub(shift);
        }
        self.tasks.retain(|task| task.slot >= 1);
        self.next = bottom + 1;
        shift
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

/// The one shared live region: task rows, the injected [`LiveSteps`], the panel, and its guard.
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
    /// The region's own bottom row (`rows - 1`), independent of the narrative block.
    region_bottom: usize,
    widths: ColumnWidths,
    /// The grid was full, so the next paint must scroll the region one line first.
    pending_scroll: bool,
    /// Shared single source of truth for running build/hook steps and their output lines.
    steps: Arc<Mutex<LiveSteps>>,
    /// Rows the narrative block painted last frame, so a shrunk block clears its leftovers.
    block_rows: usize,
}

impl Pane {
    pub(super) fn new(
        title: &str,
        cols: usize,
        prompt_row: Option<usize>,
        steps: Arc<Mutex<LiveSteps>>,
    ) -> Self {
        let (width, rows) = terminal_size();
        let cols = cols.max(width);
        let title = title.to_owned();
        let region_bottom = if rows >= 3 { rows - 1 } else { 0 };
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
            region_bottom,
            widths: ColumnWidths::default(),
            pending_scroll: false,
            steps,
            block_rows: 0,
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
        pane.tasks = TaskRows::new(flow, region_bottom);
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
        self.region_bottom = if rows >= 3 { rows - 1 } else { 0 };
        self.tasks.fit(self.tasks.next_row(), self.region_bottom);
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

    /// Latches `done` and `total` onto the bottom bar, including for narrated image/hook/service steps.
    pub(super) fn set_progress(&mut self, done: usize, total: usize) {
        self.done = done;
        self.total = self.total.max(total).max(done);
        self.refresh_summary();
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
        self.set_progress(done, self.total);
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

    /// Writes `text` at one allocated slot, scrolling `1..=region_bottom` first when it was full.
    fn write_slot(&mut self, slot: Slot, text: &str) {
        if self.guard.is_none() {
            self.clear_line();
            println!("{text}");
            return;
        }
        let mut stdout = std::io::stdout().lock();
        if slot.scrolled() {
            self.scroll_up(&mut stdout, 1);
        }
        if let Some(row) = slot.row() {
            let _ = write!(stdout, "\x1b[{row};1H\r\x1b[2K{text}");
        }
        let _ = stdout.flush();
    }

    /// Scrolls `1..=region_bottom` up `lines` times at the real `DECSTBM` bottom margin, clearing
    /// any live narrative block first so temporary spinner/gutter rows never scroll into history.
    fn scroll_up(&mut self, stdout: &mut std::io::StdoutLock<'_>, lines: usize) {
        if lines == 0 || self.region_bottom == 0 {
            return;
        }
        for offset in 0..self.block_rows {
            let row = self.region_bottom.saturating_sub(offset);
            if row > 0 {
                let _ = write!(stdout, "\x1b[{row};1H\r\x1b[2K");
            }
        }
        self.block_rows = 0;
        for _ in 0..lines {
            let _ = write!(stdout, "\x1b[{};1H\r\n", self.region_bottom);
        }
    }

    /// Clears the panel row and the narrative block, restores the region, and parks the cursor.
    pub(super) fn restore(&mut self) {
        if let Ok(mut steps) = self.steps.lock() {
            steps.clear();
        }
        let had_region = self.guard.take().is_some();
        let mut stdout = std::io::stdout().lock();
        if had_region {
            for offset in 0..self.block_rows {
                let row = self.region_bottom.saturating_sub(offset);
                if row > 0 {
                    let _ = write!(stdout, "\x1b[{row};1H\r\x1b[2K");
                }
            }
            self.block_rows = 0;
            // A full grid has no blank task row left; park on the panel row below the region
            // instead of leaving the cursor at (1,1) after the `ESC[r` reset.
            let row = self.tasks.free_row().unwrap_or(self.rows);
            let _ = write!(stdout, "\x1b[{row};1H\x1b[2K");
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

    pub(super) fn draw(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        let spinner = BRAILLE.chars().nth(self.frame % 10).unwrap_or('⠋');
        let mut block = self
            .steps
            .lock()
            .map(|steps| steps.block_rows(spinner, self.cols, PALETTE))
            .unwrap_or_default();
        let max_block = self.region_bottom.saturating_sub(1);
        if max_block > 0 && block.len() > max_block {
            block.drain(..block.len() - max_block);
        }
        // The narrative block owns the region's last rows; settled rows above it scroll up before
        // `block` paints so a newly opened phase row never overwrites a settled row on row `rows-1`.
        let new_bottom = self.region_bottom.saturating_sub(block.len()).max(1);
        let shift = if block.is_empty() {
            self.tasks.set_bottom(new_bottom);
            0
        } else {
            self.tasks.reserve_below(new_bottom)
        };
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
            let scrolls = shift + usize::from(std::mem::take(&mut self.pending_scroll));
            self.scroll_up(&mut stdout, scrolls);
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
            // Paint the block bottom-up and clear any rows a previous, longer block owned.
            let span = self.block_rows.max(block.len());
            for offset in 0..span {
                let Some(row) = self.region_bottom.checked_sub(offset) else {
                    break;
                };
                if row == 0 {
                    break;
                }
                let text = block
                    .len()
                    .checked_sub(offset + 1)
                    .and_then(|index| block.get(index))
                    .map_or("", String::as_str);
                let _ = write!(stdout, "\x1b[{row};1H\r\x1b[2K{text}");
            }
            self.block_rows = block.len();
            // The panel is always painted inside the saved cursor, so no task row or panel draw
            // ever moves the flow cursor the next allocation depends on.
            let _ = write!(stdout, "\x1b[{};1H\x1b[2K{line}\x1b[K\x1b8", self.rows);
        } else {
            // No region (a terminal shorter than three rows): degrade to one `\r` line, the one
            // colourless shape that never assumes cursor addressing.
            let line = status_line(
                spinner,
                self.done,
                self.total,
                &self.summary,
                self.started.elapsed(),
                self.cols,
            );
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

fn terminal_size() -> (usize, usize) {
    crossterm::terminal::size().map_or((80, 0), |(cols, rows)| {
        (usize::from(cols), usize::from(rows))
    })
}

/// Refreshes the visible pane every 120ms so every spinner and elapsed time stays live even when
/// the daemon is quiet.
pub(super) struct Ticker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Ticker {
    pub(super) fn start(pane: Arc<Mutex<Pane>>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                if take_resized() {
                    let (cols, rows) = terminal_size();
                    if let Ok(mut pane) = pane.lock() {
                        pane.set_size(cols, rows);
                    }
                }
                if let Ok(mut pane) = pane.lock() {
                    pane.draw();
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
                run_id: None,
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

    /// The narrative block shrinks the grid without dropping a settled row's slot, and shifts
    /// bottom-row settled allocations upward when a block opens over `region_bottom`.
    #[test]
    fn narrative_block_reserves_rows_by_scrolling_settled_slots_upward() {
        let mut tasks = TaskRows::new(5, 20);
        assert_eq!(tasks.begin("service", "api"), Slot::Row(5));
        assert_eq!(tasks.reserve_below(12), 0);
        assert_eq!(tasks.next_row(), 6);
        tasks.set_bottom(20);
        assert_eq!(tasks.next_row(), 6, "restored bottom keeps the next slot");

        let mut bottom_seeded = TaskRows::new(39, 39);
        assert_eq!(bottom_seeded.place(), Slot::Row(39));
        assert_eq!(bottom_seeded.reserve_below(38), 1);
        assert_eq!(bottom_seeded.next_row(), 38);
    }
}
