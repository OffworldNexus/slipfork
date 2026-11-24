//! The task grid behind [`super::Pane`]: one fixed slot per live task, scrolled line by line.

use std::time::Instant;

/// One live task row: its fixed slot, identity, and the clock its spinner reads.
#[derive(Debug, Clone)]
pub(super) struct Task {
    pub(super) slot: usize,
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) started: Instant,
    pub(super) done: bool,
}

/// Where one allocation landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Slot {
    /// A free content row.
    Row(usize),
    /// A free content row that required one physical region scroll first.
    Scrolled(usize),
    /// No region (`rows < 3`): counters only.
    Unavailable,
}

impl Slot {
    pub(super) fn row(self) -> Option<usize> {
        match self {
            Self::Row(row) | Self::Scrolled(row) => Some(row),
            Self::Unavailable => None,
        }
    }

    pub(super) fn scrolled(self) -> bool {
        matches!(self, Self::Scrolled(_))
    }
}

/// The 2D content grid: one fixed slot per task, scrolled one line at a time when it fills up.
#[derive(Debug)]
pub(super) struct TaskRows {
    tasks: Vec<Task>,
    /// The next free content row; `bottom + 1` means the grid is full.
    next: usize,
    bottom: usize,
}

impl TaskRows {
    pub(super) fn new(next: usize, bottom: usize) -> Self {
        Self {
            tasks: Vec::new(),
            next,
            bottom,
        }
    }

    /// Allocates a fixed row for one started task.
    pub(super) fn begin(&mut self, kind: &str, name: &str) -> Slot {
        let slot = self.take();
        if let Some(row) = slot.row() {
            self.tasks.push(Task {
                slot: row,
                kind: kind.to_owned(),
                name: name.to_owned(),
                started: Instant::now(),
                done: false,
            });
        }
        slot
    }

    /// Allocates a free row for one free-form line.
    pub(super) fn place(&mut self) -> Slot {
        self.take()
    }

    /// Marks the matching task finished and returns the fixed slot its row must be rewritten in.
    pub(super) fn complete(&mut self, kind: &str, name: &str) -> Option<usize> {
        let task = self
            .tasks
            .iter_mut()
            .find(|task| !task.done && task.kind == kind && task.name == name)?;
        task.done = true;
        Some(task.slot)
    }

    /// Tasks that still own a spinning row, top to bottom.
    pub(super) fn live(&self) -> impl Iterator<Item = &Task> {
        self.tasks.iter().filter(|task| !task.done)
    }

    /// The names of every running task, in slot order.
    #[cfg(test)]
    pub(super) fn running(&self) -> Vec<&str> {
        self.live().map(|task| task.name.as_str()).collect()
    }

    /// The blank row under the last task, when one is still free.
    pub(super) fn free_row(&self) -> Option<usize> {
        (self.next <= self.bottom).then_some(self.next)
    }

    /// The next allocation row, clamped to the content region.
    pub(super) fn next_row(&self) -> usize {
        self.next.min(self.bottom.max(1))
    }

    /// Lowers the grid's bottom when `block` is empty, without scrolling settled rows.
    pub(super) fn set_bottom(&mut self, bottom: usize) {
        self.bottom = bottom;
    }

    /// Reserves rows below `bottom` for a non-empty narrative block, returning the physical region
    /// scrolls needed so settled rows at `bottom + 1 .. self.next` move up instead of being
    /// overwritten by `block`.
    pub(super) fn reserve_below(&mut self, bottom: usize) -> usize {
        self.bottom = bottom;
        let occupied = self.next.saturating_sub(1);
        if occupied <= bottom {
            return 0;
        }
        let shift = occupied - bottom;
        for task in &mut self.tasks {
            task.slot = task.slot.saturating_sub(shift);
        }
        self.tasks.retain(|task| task.slot >= 1);
        self.next = bottom + 1;
        shift
    }

    /// Re-seeds the grid after a resize, dropping rows the terminal no longer owns.
    pub(super) fn fit(&mut self, next: usize, bottom: usize) {
        self.bottom = bottom;
        if bottom == 0 {
            self.tasks.clear();
            self.next = 1;
            return;
        }
        self.next = next.clamp(1, bottom);
        self.tasks.retain(|task| task.slot <= bottom);
        for task in &mut self.tasks {
            task.slot = task.slot.clamp(1, bottom);
        }
    }

    fn take(&mut self) -> Slot {
        if self.bottom == 0 {
            return Slot::Unavailable;
        }
        let scrolled = self.next > self.bottom;
        if scrolled {
            // One line leaves the top of the region: every surviving slot moves up one, and a
            // task that lived on row 1 is gone from the grid for good.
            for task in &mut self.tasks {
                task.slot = task.slot.saturating_sub(1);
            }
            self.tasks.retain(|task| task.slot >= 1);
            self.next = self.bottom;
        }
        let slot = self.next;
        self.next += 1;
        if scrolled {
            Slot::Scrolled(slot)
        } else {
            Slot::Row(slot)
        }
    }
}
