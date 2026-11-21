//! The one live boot pane shared by `up`, `restart`, `fork`, `down`, and `drop`.
//!
//! Three drivers, one event model: a colour terminal gets a frozen `indicatif` `MultiProgress`
//! with one Braille spinner per node, a live rolling tail, and an anchored 16-cell bar refreshed
//! every 120ms; `NO_COLOR`/`TERM=dumb` gets one `\r`-rewritten ASCII line with zero `ESC[` bytes;
//! a pipe gets `→ [i/n] kind name` headers plus one `✓ …` row per node and zero `\r`.
//!
//! Every string lives in [`render`]; this module only schedules them. Mode and width arrive as
//! plain values from the composition root: nothing here probes `/dev/tty`.

mod ascii;
mod pane;
mod render;

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::Value;

use crate::ui::UiMode;

use self::ascii::AsciiLine;
use self::pane::{Pane, Screen, Ticker};
use self::render as text;
use self::render::{Node, TailLine, completed_row};

pub use self::render::elapsed_text;

/// One live boot pane; drop it (via [`BootTui::finish`]) before printing the final result.
pub struct BootTui {
    mode: UiMode,
    total: usize,
    done: usize,
    started: Instant,
    current: String,
    /// Tails of the drivers without an `indicatif` pane (ASCII line and pipes).
    tails: BTreeMap<String, VecDeque<TailLine>>,
    pane: Option<Arc<Mutex<Pane>>>,
    ascii: Option<Arc<Mutex<AsciiLine>>>,
    ticker: Option<Ticker>,
    width: usize,
    /// `up`/`restart`/`fork` draw an anchored bar; `down`/`drop` stream rows only.
    summary: bool,
    failed: bool,
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
        let pane = (summary && mode.color()).then(|| Arc::new(Mutex::new(Pane::new(title, width))));
        let ascii = (summary && mode.interactive() && !mode.color())
            .then(|| Arc::new(Mutex::new(AsciiLine::new(title, width))));
        let screen = pane
            .clone()
            .map(Screen::Bars)
            .or_else(|| ascii.clone().map(Screen::Ascii));
        Self {
            mode,
            total: 0,
            done: 0,
            started: Instant::now(),
            current: String::new(),
            tails: BTreeMap::new(),
            pane,
            ascii,
            ticker: screen.map(Ticker::start),
            width,
            summary,
            failed: false,
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
            "log" => self.stream_line(event),
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
            if let Ok(pane) = pane.lock() {
                pane.println(text);
            }
            return;
        }
        if let Some(ascii) = &self.ascii {
            if let Ok(mut line) = ascii.lock() {
                line.clear();
                println!("{text}");
            }
            return;
        }
        if self.mode.interactive() {
            println!("{text}");
        } else {
            eprintln!("{text}");
        }
    }

    /// Stops the ticker and clears the status line; the pane's printed rows stay visible.
    pub fn finish(&mut self) {
        self.ticker = None;
        if let Some(ascii) = &self.ascii
            && let Ok(mut line) = ascii.lock()
        {
            line.clear();
        }
        if let Some(pane) = &self.pane
            && let Ok(pane) = pane.lock()
        {
            pane.finish_and_clear();
        }
        self.tails.clear();
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
                pane.start(&name, &kind, self.total);
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

    /// Keeps a bounded rolling tail per active node instead of an unbounded stream of lines.
    fn stream_line(&mut self, event: &Value) {
        if !self.summary || self.mode.json() {
            return;
        }
        let Some(line) = TailLine::from_event(event) else {
            return;
        };
        let tail = self.tails.entry(self.current.clone()).or_default();
        tail.push_back(line);
        while tail.len() > text::TAIL_LINES {
            tail.pop_front();
        }
        if let Some(pane) = &self.pane
            && let Ok(mut pane) = pane.lock()
        {
            pane.set_tail(&self.current, tail);
        }
    }

    fn finished_node(&mut self, event: &Value) {
        let Some(node) = Node::from_event(event) else {
            return;
        };
        let name = node.name.to_owned();
        self.done += 1;
        let tail = self.tails.remove(&name);
        // Any other buffered tail is stale: a finished node must never bleed into the next one.
        self.tails.clear();
        let rows = text::tail_rows(tail, self.width, self.mode.color());
        if let Some(pane) = &self.pane
            && let Ok(mut pane) = pane.lock()
        {
            pane.finish(&name, self.done);
            for row in rows {
                pane.println(&row);
            }
            pane.println(&completed_row(&node, true));
            return;
        }
        if self.mode.json() {
            return;
        }
        for row in rows {
            self.line(&row);
        }
        self.line(&completed_row(&node, self.mode.color()));
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
        let line = event["line"].as_u64().unwrap_or(0);
        let message = event["message"].as_str().unwrap_or("step failed");
        let tail = self.tails.remove(name);
        let rows = text::tail_rows(tail, self.width, self.mode.color());
        // Every running bar is cleared before the verdict so the failure reads as one statement.
        if let Some(pane) = &self.pane
            && let Ok(mut pane) = pane.lock()
        {
            pane.clear_bars();
            for row in rows {
                pane.println(&row);
            }
            pane.println(&text::failed_row(kind, name, true));
        } else {
            for row in rows {
                self.line(&row);
            }
            self.line(&text::failed_row(kind, name, self.mode.color()));
        }
        if self.mode.json() {
            return;
        }
        for row in text::failure_lines(line, message, event["remedy"].as_str()) {
            eprintln!("{row}");
        }
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
        self.line(&text);
        if event["fork"].is_null() {
            self.line(&text::hint_line(color));
        }
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
