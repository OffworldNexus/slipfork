//! The phase row and the single live-step registry shared by [`super::build`] and [`super::pane`].
//!
//! One Dockerfile instruction or one boot command step owns a [`LiveStep`] in [`LiveSteps`] from
//! the moment it is announced until it settles. Every child output line is retained in full so a
//! failed step always expands its complete output into scrollback; the [`LIVE_LINES`] cap applies
//! strictly inside [`LiveSteps::block_rows`] when painting the anchored region.

use std::time::Instant;

use crate::ui::Palette;
use crate::ui::elapsed_text;

use super::layout::{Columns, gutter_line, pad, truncate_ansi, truncate_visible, wrap_gutter};

/// The tail of streamed output painted inside the anchored region while a step runs.
pub(super) const LIVE_LINES: usize = 8;

/// The identity a streamed line inherits: one Dockerfile step, or one boot command step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LiveKey {
    Step { image: String, index: usize },
    Boot { kind: String, name: String },
}

/// One phase row: what runs, its `[i/n]`, and the clock a spinner reads.
#[derive(Debug, Clone)]
pub(super) struct Phase {
    pub(super) key: LiveKey,
    pub(super) count: Option<(usize, usize)>,
    pub(super) verb: String,
    pub(super) target: String,
    pub(super) args: String,
    pub(super) columns: Columns,
    pub(super) started: Instant,
}

impl Phase {
    pub(super) fn new(
        key: LiveKey,
        count: Option<(usize, usize)>,
        verb: String,
        target: String,
        args: String,
        columns: Columns,
    ) -> Self {
        Self {
            key,
            count,
            verb,
            target,
            args,
            columns,
            started: Instant::now(),
        }
    }

    /// The plain `→ [i/n] VERB target  args` row an append-only stream receives when a step starts.
    pub(super) fn started_row(&self, palette: Palette) -> String {
        self.assemble("→".to_owned(), None, palette)
    }

    /// The settled `✓ [i/n] VERB target  args  elapsed` row that collapses the step.
    pub(super) fn settled_row(&self, millis: u64, palette: Palette) -> String {
        self.assemble(palette.icon_ok(), Some(millis), palette)
    }

    /// The expanded `✗ [i/n] VERB target  args` row printed with a failed step's output.
    pub(super) fn failed_row(&self, palette: Palette) -> String {
        self.assemble(palette.tint("✗", "31"), None, palette)
    }

    /// The re-anchor an append-only stream prints before output from another step.
    ///
    /// Content stays neutral (`→`), the counter is dim, the verb magenta, and the target bold.
    pub(super) fn anchor_row(&self, palette: Palette) -> String {
        let mut row = String::from("→ ");
        if let Some((index, total)) = self.count {
            row.push_str(&palette.dim(&format!("[{index}/{total}]")));
            row.push(' ');
        }
        row.push_str(&palette.verb(&self.verb));
        row.push(' ');
        row.push_str(&palette.tint(&self.target, "1"));
        row
    }

    /// Wall time since the step started, for the live spinner row.
    pub(super) fn elapsed_millis(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// The anchored live row: neutral spinner, ticking dim elapsed, args truncated to `width`.
    ///
    /// Uses [`Self::head`] and `"  "` separators identical to [`Self::assemble`], so collapsing a
    /// running row to its settled form never shifts a column.
    pub(super) fn live_row(
        &self,
        spinner: char,
        millis: u64,
        width: usize,
        palette: Palette,
    ) -> String {
        let mut row = self.head(&spinner.to_string(), true, palette);
        let elapsed = elapsed_text(millis);
        let head = console::measure_text_width(&row);
        let budget = width.saturating_sub(head + elapsed.chars().count() + 4);
        let args = truncate_visible(self.shown_args(), budget);
        if !args.is_empty() {
            row.push_str("  ");
            row.push_str(&args);
        }
        row.push_str("  ");
        row.push_str(&palette.dim(&elapsed));
        truncate_ansi(&row, width)
    }

    fn assemble(&self, mark: String, elapsed: Option<u64>, palette: Palette) -> String {
        let args = self.shown_args();
        let has_tail = !args.is_empty() || elapsed.is_some();
        let mut row = self.head(&mark, has_tail, palette);
        if !args.is_empty() {
            row.push_str("  ");
            row.push_str(args);
        }
        if let Some(millis) = elapsed {
            row.push_str("  ");
            row.push_str(&palette.dim(&elapsed_text(millis)));
        }
        row
    }

    fn shown_args(&self) -> &str {
        let trimmed = self.args.trim();
        if trimmed.is_empty() || trimmed == self.target.trim() {
            ""
        } else {
            &self.args
        }
    }

    fn head(&self, mark: &str, pad_target: bool, palette: Palette) -> String {
        let mut row = format!("{mark} ");
        if let Some((index, total)) = self.count {
            row.push_str(&palette.dim(&format!("[{index}/{total}]")));
            row.push(' ');
        }
        row.push_str(&palette.verb(&pad(&self.verb, self.columns.verb)));
        row.push(' ');
        let target = if pad_target {
            pad(&self.target, self.columns.target)
        } else {
            self.target.clone()
        };
        row.push_str(&palette.tint(&target, "1"));
        row
    }
}

/// One running step in [`LiveSteps`]: its phase row, full child output, and activity stamp.
#[derive(Debug)]
pub(super) struct LiveStep {
    pub(super) phase: Phase,
    /// Every captured line in arrival order, across both streams.
    pub(super) output: Vec<(String, String)>,
    pub(super) touched: u64,
}

impl LiveStep {
    /// The failed step's full scrollback block: its phase row, then every line in arrival order.
    ///
    /// The one ordered `output` vector is the contract: an interleaved `out`/`err`/`out` step
    /// expands in exactly that order, matching `slipfork logs -r` and the run capture.
    pub(super) fn expansion_rows(&self, width: usize, palette: Palette) -> Vec<String> {
        let mut rows = vec![self.phase.failed_row(palette)];
        for (stream, text) in &self.output {
            rows.extend(wrap_gutter(stream, text, width, palette));
        }
        rows
    }
}

/// The single live-step registry: owns every running phase and every child line it produced.
#[derive(Debug, Default)]
pub(super) struct LiveSteps {
    steps: Vec<LiveStep>,
    tick: u64,
}

impl LiveSteps {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Registers `phase` as running, replacing any stale entry with the same key.
    pub(super) fn begin(&mut self, phase: Phase) {
        self.steps.retain(|step| step.phase.key != phase.key);
        self.tick = self.tick.wrapping_add(1);
        self.steps.push(LiveStep {
            phase,
            output: Vec::new(),
            touched: self.tick,
        });
    }

    /// Appends one child line in full to `key` and marks that step most recently active.
    pub(super) fn append_output(
        &mut self,
        key: &LiveKey,
        stream: String,
        text: String,
    ) -> Option<Phase> {
        let step = self.steps.iter_mut().find(|step| &step.phase.key == key)?;
        self.tick = self.tick.wrapping_add(1);
        step.output.push((stream, text));
        step.touched = self.tick;
        Some(step.phase.clone())
    }

    /// Resolves the live step key from explicit frame keys, or `fallback` when none were pinned.
    pub(super) fn resolve_key(
        &self,
        candidates: &[LiveKey],
        target: Option<&str>,
        fallback: Option<&LiveKey>,
    ) -> Option<LiveKey> {
        for key in candidates {
            if self.steps.iter().any(|step| &step.phase.key == key) {
                return Some(key.clone());
            }
        }
        if let Some(target) = target
            && let Some(step) = self.steps.iter().find(|step| step.phase.target == target)
        {
            return Some(step.phase.key.clone());
        }
        if candidates.is_empty()
            && target.is_none()
            && let Some(key) = fallback
            && self.steps.iter().any(|step| &step.phase.key == key)
        {
            return Some(key.clone());
        }
        None
    }

    /// Removes and returns one settled step by exact key.
    pub(super) fn take(&mut self, key: &LiveKey) -> Option<LiveStep> {
        let position = self.steps.iter().position(|step| &step.phase.key == key)?;
        Some(self.steps.remove(position))
    }

    /// Removes and returns a boot command step matching `(kind, name)` or `target == name`.
    pub(super) fn take_boot(&mut self, kind: &str, name: &str) -> Option<LiveStep> {
        let position = self
            .steps
            .iter()
            .position(|step| {
                matches!(
                    &step.phase.key,
                    LiveKey::Boot { kind: planned, name: planned_name }
                        if planned == kind && planned_name == name
                )
            })
            .or_else(|| self.steps.iter().position(|step| step.phase.target == name))?;
        Some(self.steps.remove(position))
    }

    /// Removes the failing step, clears any sibling phases, and returns the failing step in full.
    pub(super) fn take_failure(
        &mut self,
        target: Option<&str>,
        active: Option<&LiveKey>,
    ) -> Option<LiveStep> {
        let position = target
            .and_then(|target| {
                self.steps
                    .iter()
                    .position(|step| step.phase.target == target)
            })
            .or_else(|| {
                active.and_then(|key| self.steps.iter().position(|step| &step.phase.key == key))
            })
            .or_else(|| self.steps.len().checked_sub(1));
        let removed = position.map(|index| self.steps.remove(index));
        self.steps.clear();
        removed
    }

    /// Clears every running step when the region restores.
    pub(super) fn clear(&mut self) {
        self.steps.clear();
    }

    /// Most recently started surviving step key.
    pub(super) fn last_key(&self) -> Option<LiveKey> {
        self.steps.last().map(|step| step.phase.key.clone())
    }

    /// Paints stacked phase rows plus at most [`LIVE_LINES`] tail lines of the newest active step.
    pub(super) fn block_rows(&self, spinner: char, width: usize, palette: Palette) -> Vec<String> {
        let mut rows: Vec<String> = self
            .steps
            .iter()
            .map(|step| {
                step.phase
                    .live_row(spinner, step.phase.elapsed_millis(), width, palette)
            })
            .collect();
        if let Some(tail) = self.steps.iter().max_by_key(|step| step.touched) {
            let start = tail.output.len().saturating_sub(LIVE_LINES);
            rows.extend(
                tail.output[start..]
                    .iter()
                    .map(|(stream, text)| gutter_line(stream, text, width, palette)),
            );
        }
        rows
    }
}
