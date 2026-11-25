//! The one in-place status line of the append-only boot renderer.
//!
//! Completed rows are ordinary newline-terminated scrollback lines; the in-place byte mechanic
//! itself lives in [`crate::ui::report::live::Pane`], the workspace's only `\r\x1b[2K` writer.
//! This module is the content adapter: the live vocabulary (spinner, `[i/n]`, verb/target,
//! elapsed, and the tail phase suffix) comes from [`super::phase::Phase::live_row`] for the most
//! recently active step; with no live step it falls back to the global
//! [`super::progress::BootProgress`] bar label.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::ui::Palette;
use crate::ui::report::Pane;

use super::build::LiveSteps;
use super::progress::BootProgress;
use super::render::panel_line;

mod ticker;

pub(super) use ticker::Ticker;

/// A status line exists only for a colour terminal, so its frame is always styled.
const PALETTE: Palette = Palette::new(true);

/// The one in-place status line: its content model plus the shared [`Pane`] mechanic.
pub(super) struct StatusLine {
    progress: BootProgress,
    started: Instant,
    steps: Arc<Mutex<LiveSteps>>,
    pane: Pane,
}

impl StatusLine {
    pub(super) fn new(progress: BootProgress, steps: Arc<Mutex<LiveSteps>>, pane: Pane) -> Self {
        Self {
            progress,
            started: Instant::now(),
            steps,
            pane,
        }
    }

    /// Replaces the progress model the fallback frame paints.
    pub(super) fn set_progress(&mut self, progress: BootProgress) {
        self.progress = progress;
    }

    /// Re-measures the terminal after a resize; never emits a row or erases anything.
    pub(super) fn resize(&mut self, cols: usize) {
        self.pane.resize(cols);
    }

    /// One full status frame: [`Pane::PREFIX`] plus the rendered line.
    #[cfg(test)]
    pub(super) fn frame_text(&self, spinner: char) -> String {
        Pane::frame(&self.line(spinner))
    }

    fn line(&self, spinner: char) -> String {
        self.steps
            .lock()
            .ok()
            .and_then(|steps| steps.live_row(spinner, self.pane.cols(), PALETTE))
            .unwrap_or_else(|| {
                panel_line(
                    spinner,
                    self.progress.bar(),
                    &self.progress.label(),
                    self.started.elapsed(),
                    self.pane.cols(),
                )
            })
    }

    /// Writes one status frame without flushing; the caller owns the lock and the flush.
    fn paint_to(&mut self, out: &mut dyn Write) {
        let spinner = self.pane.spinner();
        let line = self.line(spinner);
        self.pane.paint_to(&line, out);
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
        let spinner = self.pane.spinner();
        let line = self.line(spinner);
        self.pane.emit_to(rows, &line, out);
    }

    /// Emits completed rows through stdout and repaints the status line.
    pub(super) fn emit(&mut self, rows: &[String]) {
        let mut stdout = std::io::stdout().lock();
        self.emit_to(rows, &mut stdout);
        let _ = stdout.flush();
    }

    /// Erases the status line exactly once into `out`; idempotent when called twice.
    pub(super) fn erase_to(&mut self, out: &mut dyn Write) {
        self.pane.erase_to(out);
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
