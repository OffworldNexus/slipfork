//! Per-image build tally: ordered step buffer, outer plan slot, and widest instruction column.
//!
//! Finished inner rows are buffered by 1-based instruction index and released strictly from `next`
//! upward so rows print once, in declaration order. A row releases the moment its predecessors are
//! printed (on any frame, including `done`); an index that only announced a provisional external
//! pull (`provisional`) is not a real instruction, so it never enters `started` and can only be
//! superseded by the real instruction at its index (which replaces its buffered row), never
//! released on its own.

use std::collections::{BTreeMap, BTreeSet};

/// One image's running totals, so the build-end row can count steps and sum wall time.
#[derive(Debug, Clone)]
pub(super) struct ImageTally {
    pub(super) image: String,
    pub(super) total: usize,
    pub(super) millis: u64,
    pub(super) buffer: BTreeMap<usize, String>,
    /// Highest inner index ever announced, provisional pulls included: only latches the image
    /// opener, never the `at N/M` failure position.
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
    ///
    /// A provisional external-pull row is never counted as started: it may only ever be replaced
    /// by the real instruction at the same index, so it can never release on its own.
    pub(super) fn begin_step(
        &mut self,
        index: usize,
        total: usize,
        instruction_width: usize,
        provisional: bool,
    ) -> usize {
        self.total = self.total.max(total);
        self.last_index = self.last_index.max(index);
        // A real (or provisional) announcement supersedes any buffered provisional row.
        self.buffer.remove(&index);
        if index > 0 && !provisional {
            self.started.insert(index);
        }
        self.verb_width = self.verb_width.max(instruction_width);
        self.verb_width
    }

    /// The instruction count and the highest *real* started inner index, when both are known.
    ///
    /// `last_index` is only a header latch: a provisional external-pull announcement raises it
    /// without being an instruction, so the failure position comes from `started` alone.
    pub(super) fn failure_at(&self) -> Option<(usize, usize)> {
        let last = self.started.iter().next_back().copied()?;
        (self.total > 0).then_some((last, self.total))
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
