//! The phase row and the single live-step registry shared by [`super::build`] and [`super::pane`].
//!
//! One Dockerfile instruction or one boot command step owns a [`LiveStep`] in [`LiveSteps`] from
//! the moment it is announced until it settles. Every child output line is retained in full so a
//! failed step always expands its complete output into scrollback; the [`LIVE_LINES`] cap applies
//! strictly inside [`LiveSteps::block_rows`] when painting the anchored region.
//!
//! Two levels share one phase: an outer boot row at column 0 (`[i/n] KIND target detail`) and an
//! indented inner Dockerfile row (`  n/N INSTRUCTION args`, no target, no brackets). The level is
//! carried by the [`LiveKey`], so a caller never re-scans strings to decide the shape.

use std::time::Instant;

use crate::ui::Palette;
use crate::ui::elapsed_text;

use super::layout::{Columns, gutter_line, truncate_ansi, wrap_gutter};
use super::render::{outer_row, push_padded};

/// The tail of streamed output painted inside the anchored region while a step runs.
pub(super) const LIVE_LINES: usize = 8;

/// The identity a streamed line inherits: one Dockerfile step, or one boot command step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LiveKey {
    Step { image: String, index: usize },
    Boot { kind: String, name: String },
}

/// One phase row: what runs, its counter, and the clock a spinner reads.
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

    /// Whether this is an inner Dockerfile step (indented, `n/N`, no target).
    pub(super) fn is_inner(&self) -> bool {
        matches!(self.key, LiveKey::Step { .. })
    }

    /// The gutter indent owned by this row's child output: `4` inside a build, `2` under a step.
    pub(super) fn gutter_indent(&self) -> usize {
        if self.is_inner() { 4 } else { 2 }
    }

    /// The plain `→ …` row an append-only stream receives when a step starts.
    pub(super) fn started_row(&self, palette: Palette) -> String {
        if self.is_inner() {
            self.inner_row("→", None, palette)
        } else {
            self.outer_row("→", None, None, palette)
        }
    }

    /// The settled `✓ …` row that collapses the step.
    pub(super) fn settled_row(&self, millis: u64, palette: Palette) -> String {
        let mark = palette.icon_ok();
        if self.is_inner() {
            self.inner_row(&mark, Some(millis), palette)
        } else {
            self.outer_row(&mark, None, Some(millis), palette)
        }
    }

    /// The settled outer row for a cache hit: a dim bullet plus the `up to date` detail.
    pub(super) fn settled_outer(
        &self,
        millis: u64,
        detail: Option<&str>,
        stale: bool,
        palette: Palette,
    ) -> String {
        let mark = if stale {
            palette.dim("•")
        } else {
            palette.icon_ok()
        };
        self.outer_row(&mark, detail, Some(millis), palette)
    }

    /// The expanded `✗ …` row printed with a failed step's output.
    pub(super) fn failed_row(&self, palette: Palette) -> String {
        let mark = palette.tint("✗", "31");
        if self.is_inner() {
            self.inner_row(&mark, None, palette)
        } else {
            self.outer_row(&mark, None, None, palette)
        }
    }

    /// The re-anchor an append-only stream prints before output from another step.
    pub(super) fn anchor_row(&self, palette: Palette) -> String {
        if self.is_inner() {
            self.inner_row("→", None, palette)
        } else {
            self.outer_row("→", Some(""), None, palette)
        }
    }

    /// Wall time since the step started, for the live spinner row.
    pub(super) fn elapsed_millis(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// The anchored live row, truncated to `width`.
    pub(super) fn live_row(
        &self,
        spinner: char,
        millis: u64,
        width: usize,
        palette: Palette,
    ) -> String {
        let mark = spinner.to_string();
        let row = if self.is_inner() {
            self.inner_row(&mark, Some(millis), palette)
        } else {
            self.outer_row(&mark, None, Some(millis), palette)
        };
        truncate_ansi(&row, width)
    }

    /// `  ✓ n/N INSTRUCTION  args  elapsed` — no target, no brackets, indent 2.
    fn inner_row(&self, mark: &str, millis: Option<u64>, palette: Palette) -> String {
        let mut row = String::from("  ");
        row.push_str(mark);
        row.push(' ');
        if let Some((index, total)) = self.count {
            row.push_str(&palette.dim(&format!("{index}/{total}")));
            row.push(' ');
        }
        push_padded(
            &mut row,
            &palette.verb(&self.verb),
            self.verb.chars().count(),
            self.columns.verb,
        );
        let args = self.args.trim();
        if !args.is_empty() {
            row.push_str("  ");
            row.push_str(args);
        }
        if let Some(millis) = millis {
            row.push_str("  ");
            row.push_str(&palette.dim(&elapsed_text(millis)));
        }
        while row.ends_with(' ') {
            row.pop();
        }
        row
    }

    /// `[i/n] KIND  target  detail  elapsed` at column 0.
    fn outer_row(
        &self,
        mark: &str,
        detail: Option<&str>,
        millis: Option<u64>,
        palette: Palette,
    ) -> String {
        let resolved = detail.unwrap_or_else(|| self.shown_args());
        outer_row(
            mark,
            self.count,
            &self.verb,
            &self.target,
            Some(resolved),
            millis,
            &self.columns.as_outer(),
            palette,
        )
    }

    fn shown_args(&self) -> &str {
        let trimmed = self.args.trim();
        if trimmed.is_empty() || trimmed == self.target.trim() {
            ""
        } else {
            &self.args
        }
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
        let indent = self.phase.gutter_indent();
        for (stream, text) in &self.output {
            rows.extend(wrap_gutter(stream, text, width, palette, indent));
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
            && let Some(step) = self
                .steps
                .iter()
                .rev()
                .find(|step| step.phase.target == target)
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
                    .rposition(|step| step.phase.target == target)
            })
            .or_else(|| {
                active.and_then(|key| self.steps.iter().rposition(|step| &step.phase.key == key))
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
            let indent = tail.phase.gutter_indent();
            rows.extend(
                tail.output[start..]
                    .iter()
                    .map(|(stream, text)| gutter_line(stream, text, width, palette, indent)),
            );
        }
        rows
    }
}
