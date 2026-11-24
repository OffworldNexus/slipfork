//! The `DECSTBM` fixed bottom bar with one live row per running task.
//!
//! Rows `1..rows-1` hold settled rows above [`LiveSteps`]'s bottom block; row `rows` is the
//! 120ms-refreshed panel, re-applied on `SIGWINCH`. Every escape is emitted only for a colour TTY.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::ui::Palette;
use crate::ui::table::ColumnWidths;

use super::build::LiveSteps;
use super::region::ScrollRegionGuard;
use super::render::{panel_line, running_row, status_line};

use self::rows::{Slot, TaskRows};

mod rows;
#[cfg(test)]
mod tests;
mod ticker;

pub(super) use ticker::Ticker;

/// Braille spinner for a terminal that accepts ANSI.
const BRAILLE: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

/// A pane exists only for a colour terminal, so its block is always styled.
const PALETTE: Palette = Palette::new(true);

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
