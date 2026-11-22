//! The `indicatif` region: one spinner bar per running node, a live rolling tail per bar, and the
//! anchored summary line, all refreshed by a 120ms ticker.
//!
//! Every byte comes from [`super::render`]; this module holds only terminal state.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use super::ascii::AsciiLine;
use super::render::{self, TailLine};

/// Braille spinner for a terminal that accepts ANSI.
const BRAILLE: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

/// The shared `MultiProgress` region: one bar per running node plus the anchored summary.
pub(super) struct Pane {
    mp: MultiProgress,
    summary: ProgressBar,
    bars: BTreeMap<String, Run>,
    title: String,
    width: usize,
    total: usize,
    done: usize,
    started: Instant,
}

/// One running node's spinner bar, its own start instant, and its bounded rolling tail.
struct Run {
    bar: ProgressBar,
    kind: String,
    name: String,
    started: Instant,
    tail: VecDeque<TailLine>,
}

impl Pane {
    pub(super) fn new(title: &str, width: usize) -> Self {
        let mp = MultiProgress::new();
        let summary = mp.add(ProgressBar::new(0));
        summary.set_style(style("{msg}"));
        Self {
            mp,
            summary,
            bars: BTreeMap::new(),
            title: title.to_owned(),
            width,
            total: 0,
            done: 0,
            started: Instant::now(),
        }
    }

    /// Prints one completed row above the live region.
    pub(super) fn println(&self, text: &str) {
        let _ = self.mp.println(text);
    }

    /// Erases the anchored summary once the boot verdict is printed.
    pub(super) fn finish_and_clear(&self) {
        self.summary.finish_and_clear();
    }

    pub(super) fn start(&mut self, name: &str, kind: &str, total: usize) {
        let bar = ProgressBar::new(0);
        bar.set_style(style("{spinner} {msg}"));
        let bar = self.mp.insert_after(&self.summary, bar);
        self.bars.insert(
            name.to_owned(),
            Run {
                bar,
                kind: kind.to_owned(),
                name: name.to_owned(),
                started: Instant::now(),
                tail: VecDeque::new(),
            },
        );
        self.total = self.total.max(total);
        self.refresh();
    }

    /// Mirrors the scheduler's bounded tail so the active row draws it live instead of waiting for
    /// completion.
    pub(super) fn set_tail(&mut self, name: &str, tail: &VecDeque<TailLine>) {
        let Some(run) = self.bars.get_mut(name) else {
            return;
        };
        run.tail.clear();
        run.tail.extend(tail.iter().cloned());
        self.refresh();
    }

    pub(super) fn finish(&mut self, name: &str, done: usize) {
        self.done = done;
        if let Some(run) = self.bars.remove(name) {
            self.mp.remove(&run.bar);
        }
        self.total = self.total.max(done);
        self.refresh();
    }

    /// Clears every running bar (a failure renders as one verdict, not a field of stale spinners).
    pub(super) fn clear_bars(&mut self) {
        for (_, run) in std::mem::take(&mut self.bars) {
            self.mp.remove(&run.bar);
        }
    }

    /// Live elapsed time for the anchored bar and every running row, tails included.
    fn refresh(&self) {
        self.summary.set_message(render::summary_line(
            self.total,
            self.done,
            &self.title,
            self.started.elapsed(),
            self.width,
        ));
        // `indicatif` only paints a bar whose ticker advanced: without this tick the anchored
        // 16-cell bar stays invisible while the spinner bars below it animate.
        self.summary.tick();
        let limit = render::stream_limit(self.width);
        for run in self.bars.values() {
            run.bar.set_message(render::running_row_with_tail(
                &run.kind,
                &run.name,
                run.started.elapsed(),
                &run.tail,
                limit,
                true,
            ));
            run.bar.tick();
        }
    }
}

fn style(template: &str) -> ProgressStyle {
    ProgressStyle::with_template(template)
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
        .tick_chars(BRAILLE)
}

/// Which live region the ticker refreshes.
pub(super) enum Screen {
    Bars(Arc<Mutex<Pane>>),
    Ascii(Arc<Mutex<AsciiLine>>),
}

/// Refreshes the visible pane every 120ms so the spinner and elapsed time stay live even when the
/// daemon is quiet: the colour bar (tails included), or the ASCII line of a `NO_COLOR` terminal.
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
                match &screen {
                    Screen::Bars(pane) => {
                        if let Ok(pane) = pane.lock() {
                            pane.refresh();
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
