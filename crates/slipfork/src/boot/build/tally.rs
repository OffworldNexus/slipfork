//! Per-image build tally: ordered step buffer, outer plan slot, and widest instruction column.
//!
//! Finished inner rows are buffered by 1-based instruction index and released strictly from `next`
//! upward so rows print once, in declaration order. A row releases the moment its predecessors are
//! printed (on any frame, including `done`); an index that never announced a real `started` frame is
//! a provisional external-pull row, so it waits for the real instruction (which supersedes it) or
//! for the image's final settle, and can never print twice.

use std::collections::{BTreeMap, BTreeSet};

/// One image's running totals, so the build-end row can count steps and sum wall time.
#[derive(Debug, Clone)]
pub(super) struct ImageTally {
    pub(super) image: String,
    pub(super) total: usize,
    pub(super) millis: u64,
    pub(super) buffer: BTreeMap<usize, String>,
    /// Highest inner instruction index that ever started, so a failed row can say `at N/M`.
    pub(super) last_index: usize,
    next: usize,
    /// The image's outer `[i/n]` plan slot, latched from the boot plan.
    pub(super) outer: (usize, usize),
    /// Indices that announced a real instruction (`started`), not just a provisional pull.
    started: BTreeSet<usize>,
    /// Widest inner Dockerfile instruction seen so far for this image.
    pub(super) verb_width: usize,
}

impl ImageTally {
    pub(super) fn new(image: &str) -> Self {
        Self {
            image: image.to_owned(),
            total: 0,
            millis: 0,
            buffer: BTreeMap::new(),
            last_index: 0,
            next: 1,
            outer: (0, 0),
            started: BTreeSet::new(),
            verb_width: 0,
        }
    }

    /// Records an inner `started` instruction and returns the image-wide verb column width.
    pub(super) fn begin_step(
        &mut self,
        index: usize,
        total: usize,
        instruction_width: usize,
    ) -> usize {
        self.total = self.total.max(total);
        self.last_index = self.last_index.max(index);
        // A real instruction re-announcing a provisional external-pull index wins.
        self.buffer.remove(&index);
        if index > 0 {
            self.started.insert(index);
        }
        self.verb_width = self.verb_width.max(instruction_width);
        self.verb_width
    }

    /// The instruction count and the last started inner index, when both are known.
    pub(super) fn failure_at(&self) -> Option<(usize, usize)> {
        (self.last_index > 0 && self.total > 0).then_some((self.last_index, self.total))
    }

    /// Whether zero inner steps were ever recorded, so a cached image falls through to its single
    /// outer `completed_row` with no inner rows.
    pub(super) fn is_unused(&self) -> bool {
        self.total == 0 && self.buffer.is_empty() && self.started.is_empty()
    }

    /// Releases every buffered row contiguous from `next` upward; a hole (or a provisional row that
    /// has not been announced yet) stops the release, keeping plan index order.
    pub(super) fn release(&mut self) -> Vec<String> {
        let mut rows = Vec::new();
        while self.started.contains(&self.next) {
            let Some(row) = self.buffer.remove(&self.next) else {
                break;
            };
            rows.push(row);
            self.next += 1;
        }
        rows
    }

    /// Drains the contiguous rows, any residual rows, and the instruction count and wall time.
    pub(super) fn settle(&mut self) -> (Vec<String>, usize, u64) {
        let mut rows = self.release();
        rows.extend(std::mem::take(&mut self.buffer).into_values());
        let steps = if self.total > 0 {
            self.total
        } else {
            rows.len()
        };
        (rows, steps, self.millis)
    }
}
