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

use std::sync::{Arc, Mutex};
use std::time::Instant;

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
        for row in self.verdict.drain(..) {
            println!("{row}");
        }
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
        let sentence = text::failure_sentence(kind, name);
        if self.pane.is_some() {
            self.verdict.push(row);
            self.diagnostics.push(sentence);
            return;
        }
        self.line(&row);
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
            if event["fork"].is_null() {
                self.verdict.push(text::hint_line(color));
            }
            return;
        }
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
