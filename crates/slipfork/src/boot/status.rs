//! The one in-place status line of the append-only boot renderer.
//!
//! Completed rows are ordinary newline-terminated scrollback lines; this module owns the only
//! bytes that may ever use `\r` or `ESC[2K`: one status frame written as `\r\x1b[2K` plus the
//! rendered line. The live vocabulary (spinner, `[i/n]`, verb/target, elapsed, and the tail
//! phase suffix) comes from [`super::phase::Phase::live_row`] for the most recently active step;
//! with no live step it falls back to the global [`super::progress::BootProgress`] bar label.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::ui::Palette;

use super::build::LiveSteps;
use super::progress::BootProgress;
use super::render::panel_line;

mod ticker;

pub(super) use ticker::Ticker;

/// Braille spinner for a terminal that accepts ANSI.
const BRAILLE: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

/// A status line exists only for a colour terminal, so its frame is always styled.
const PALETTE: Palette = Palette::new(true);

/// The one in-place status line: its progress model, live-step registry, and paint state.
pub(super) struct StatusLine {
    progress: BootProgress,
    started: Instant,
    cols: usize,
    steps: Arc<Mutex<LiveSteps>>,
    frame: usize,
    /// Whether a status frame currently owns the cursor's line.
    painted: bool,
}

impl StatusLine {
    pub(super) fn new(progress: BootProgress, cols: usize, steps: Arc<Mutex<LiveSteps>>) -> Self {
        Self {
            progress,
            started: Instant::now(),
            cols,
            steps,
            frame: 0,
            painted: false,
        }
    }

    /// Replaces the progress model the fallback frame paints.
    pub(super) fn set_progress(&mut self, progress: BootProgress) {
        self.progress = progress;
    }

    /// Re-measures the terminal after a resize; never emits a row or erases anything.
    pub(super) fn resize(&mut self, cols: usize) {
        if cols > 0 {
            self.cols = cols;
        }
    }

    /// The status frame prefix; the only place `\r`/`ESC[2K` may be written.
    pub(super) const fn prefix() -> &'static str {
        "\r\x1b[2K"
    }

    /// One full status frame: the prefix plus the rendered line.
    pub(super) fn frame_text(&self, spinner: char) -> String {
        let line = self.line(spinner);
        format!("{}{line}", Self::prefix())
    }

    fn line(&self, spinner: char) -> String {
        self.steps
            .lock()
            .ok()
            .and_then(|steps| steps.live_row(spinner, self.cols, PALETTE))
            .unwrap_or_else(|| {
                panel_line(
                    spinner,
                    self.progress.bar(),
                    &self.progress.label(),
                    self.started.elapsed(),
                    self.cols,
                )
            })
    }

    fn next_spinner(&mut self) -> char {
        let spinner = BRAILLE
            .chars()
            .nth(self.frame % BRAILLE.chars().count())
            .unwrap_or('⠋');
        self.frame = self.frame.wrapping_add(1);
        spinner
    }

    /// Writes one status frame without flushing; the caller owns the lock and the flush.
    fn paint_to(&mut self, out: &mut dyn Write) {
        let spinner = self.next_spinner();
        let frame = self.frame_text(spinner);
        let _ = out.write_all(frame.as_bytes());
        self.painted = true;
    }

    /// Repaints the status line in place, flushed.
    pub(super) fn paint(&mut self) {
        let mut stdout = std::io::stdout().lock();
        self.paint_to(&mut stdout);
        let _ = stdout.flush();
    }

    /// Emits completed rows as ordinary lines, then repaints the status line.
    ///
    /// While a status frame is painted the cursor sits on the status line, so one `\r\x1b[2K`
    /// clears it before the first row; the rows then advance the cursor normally and the final
    /// repaint owns the new bottom line.
    pub(super) fn emit_to(&mut self, rows: &[String], out: &mut dyn Write) {
        if rows.is_empty() {
            return;
        }
        if self.painted {
            let _ = out.write_all(Self::prefix().as_bytes());
        }
        for row in rows {
            let _ = writeln!(out, "{row}");
        }
        self.painted = false;
        self.paint_to(out);
    }

    /// Emits completed rows through stdout and repaints the status line.
    pub(super) fn emit(&mut self, rows: &[String]) {
        let mut stdout = std::io::stdout().lock();
        self.emit_to(rows, &mut stdout);
        let _ = stdout.flush();
    }

    /// Erases the status line exactly once into `out`; idempotent when called twice.
    pub(super) fn erase_to(&mut self, out: &mut dyn Write) {
        if !self.painted {
            return;
        }
        let _ = out.write_all(Self::prefix().as_bytes());
        self.painted = false;
    }

    /// Erases the status line exactly once; idempotent when called twice.
    pub(super) fn erase(&mut self) {
        let mut stdout = std::io::stdout().lock();
        self.erase_to(&mut stdout);
        let _ = stdout.flush();
    }
}

#[cfg(test)]
mod tests;
