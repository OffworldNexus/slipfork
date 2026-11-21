//! One `\r`-rewritten colourless status line for `NO_COLOR`/`TERM=dumb` terminals.
//!
//! The line is redrawn in place by the ticker and clears itself before any printed row. It never
//! emits a single `ESC[` byte.

use std::io::Write;
use std::time::Instant;

use super::render::status_line;

/// ASCII spinner for a terminal that forbids escapes.
const ASCII_SPIN: [char; 4] = ['|', '/', '-', '\\'];

pub(super) struct AsciiLine {
    title: String,
    width: usize,
    started: Instant,
    total: usize,
    done: usize,
    current: String,
    frame: usize,
    line_len: usize,
}

impl AsciiLine {
    pub(super) fn new(title: &str, width: usize) -> Self {
        Self {
            title: title.to_owned(),
            width,
            started: Instant::now(),
            total: 0,
            done: 0,
            current: String::new(),
            frame: 0,
            line_len: 0,
        }
    }

    /// Pushes the scheduler's counters into the line and redraws it immediately.
    pub(super) fn show(&mut self, done: usize, total: usize, current: &str) {
        self.done = done;
        self.total = total;
        self.current.clear();
        self.current.push_str(current);
        self.draw();
    }

    pub(super) fn draw(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        let spinner = ASCII_SPIN[self.frame % ASCII_SPIN.len()];
        let line = status_line(
            spinner,
            self.done,
            self.total,
            &self.title,
            &self.current,
            self.started.elapsed(),
            self.width,
        );
        let visible = line.chars().count();
        if visible < self.line_len {
            print!("\r{line}{}", " ".repeat(self.line_len - visible));
        } else {
            print!("\r{line}");
        }
        self.line_len = visible;
        let _ = std::io::stdout().flush();
    }

    pub(super) fn clear(&mut self) {
        if self.line_len > 0 {
            print!("\r{}\r", " ".repeat(self.line_len));
            let _ = std::io::stdout().flush();
            self.line_len = 0;
        }
    }
}
