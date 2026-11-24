//! Per-image build tally: ordered step buffer, outer plan slot, and widest instruction column.
//!
//! Finished inner rows are buffered by 1-based instruction index and released strictly from `next`
//! upward so rows print once, in declaration order; a provisional external-pull row is superseded
//! when the real instruction at that index starts.

use std::collections::BTreeMap;

/// One image's running totals, so the build-end row can count steps and sum wall time.
#[derive(Debug, Clone)]
pub(super) struct ImageTally {
    pub(super) image: String,
    pub(super) total: usize,
    pub(super) millis: u64,
    pub(super) buffer: BTreeMap<usize, String>,
    next: usize,
    /// The image's outer `[i/n]` plan slot, latched from the boot plan.
    pub(super) outer: (usize, usize),
    /// Whether the outer `→ [i/n] image` parent row has already opened in scrollback.
    opened: bool,
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
            next: 1,
            outer: (0, 0),
            opened: false,
            verb_width: 0,
        }
    }

    /// Records an inner `started` instruction and returns `Some((index, total))` on the very first
    /// step when the image owns an outer plan slot, so `→ [i/n] image <id>` opens once.
    pub(super) fn begin_step(
        &mut self,
        index: usize,
        total: usize,
        instruction: &str,
    ) -> (Option<(usize, usize)>, usize) {
        self.total = self.total.max(total);
        // A real instruction re-announcing a provisional external-pull index wins.
        self.buffer.remove(&index);
        self.verb_width = self.verb_width.max(instruction.chars().count());
        let open = if !self.opened && self.outer.0 > 0 {
            self.opened = true;
            Some(self.outer)
        } else {
            None
        };
        (open, self.verb_width)
    }

    /// Whether zero inner steps were ever recorded, so a cached image falls through to its single
    /// outer `completed_row` with no inner rows.
    pub(super) fn is_unused(&self) -> bool {
        self.total == 0 && self.buffer.is_empty() && !self.opened
    }

    /// Releases every buffered row contiguous from `next` upward; a hole stops the release.
    pub(super) fn release(&mut self) -> Vec<String> {
        let mut rows = Vec::new();
        while let Some(row) = self.buffer.remove(&self.next) {
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
