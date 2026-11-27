//! The one boot progress model: outer plan completion plus the building image's inner steps.
//!
//! The bottom bar must never double count a build: before an image finishes it contributes only
//! its settled inner step fraction (`inner_done / inner_total`), and after its single `finished`
//! frame it contributes exactly one outer slot. The model owns the announced plan (so every outer
//! row keeps its `[i/n]` position), both levels, and the label, so [`super::status::StatusLine`]
//! only paints.

use std::collections::BTreeSet;

use serde_json::Value;

use super::render::{BarProgress, counter, digits};

/// The outer plan step currently running, when no image build owns the bar.
#[derive(Debug, Clone)]
struct Active {
    index: usize,
    total: usize,
    kind: String,
    target: String,
}

/// The image whose Dockerfile steps drive the bar's inner fraction and label.
#[derive(Debug, Clone)]
struct Building {
    index: usize,
    total: usize,
    image: String,
    /// Settled 1-based instruction indices (`state == "done"`), driving the bar fraction.
    settled: BTreeSet<usize>,
    /// Currently running or most recently announced instruction index (`3` in `step 3/6 RUN`).
    step: usize,
    steps: usize,
    verb: String,
}

/// Two-level boot progress: the announced plan, outer slots, and one image's inner steps.
#[derive(Debug, Clone)]
pub(super) struct BootProgress {
    title: String,
    outer_done: usize,
    outer_total: usize,
    announced: bool,
    plan: Vec<(String, String)>,
    active: Option<Active>,
    building: Option<Building>,
    /// Outer rows already settled, so a late `step` frame cannot recreate a finished build.
    finished: BTreeSet<(String, String)>,
}

impl BootProgress {
    pub(super) fn new(title: &str) -> Self {
        Self {
            title: title.to_owned(),
            outer_done: 0,
            outer_total: 0,
            announced: false,
            plan: Vec::new(),
            active: None,
            building: None,
            finished: BTreeSet::new(),
        }
    }

    /// Latches the one `plan` frame: every announced row keeps its 1-based position.
    pub(super) fn announce(&mut self, rows: &[(String, String)], total: usize) {
        self.announced = true;
        self.plan = rows.to_vec();
        self.outer_total = self.outer_total.max(total).max(rows.len());
    }

    /// Whether the daemon already announced the row grid.
    pub(super) fn announced(&self) -> bool {
        self.announced
    }

    /// The announced plan as `(kind, name)` pairs, shared by the row grids.
    pub(super) fn plan(&self) -> &[(String, String)] {
        &self.plan
    }

    /// Records a row seen without a `plan` frame, so a later rerender keeps its position.
    pub(super) fn remember(&mut self, kind: &str, name: &str) {
        if !self.planned(kind, name) {
            self.plan.push((kind.to_owned(), name.to_owned()));
        }
        self.outer_total = self.outer_total.max(self.plan.len());
    }

    /// Whether the plan announced this exact outer row.
    pub(super) fn planned(&self, kind: &str, name: &str) -> bool {
        self.plan
            .iter()
            .any(|(planned_kind, planned_name)| planned_kind == kind && planned_name == name)
    }

    /// The row's `[i/n]` position: `(index, total)` with a 1-based index, or `(0, total)` when the
    /// row was never planned (then [`super::render::completed_row`] omits the counter).
    pub(super) fn position(&self, kind: &str, name: &str) -> (usize, usize) {
        let total = self.outer_total.max(self.plan.len());
        let index = self
            .plan
            .iter()
            .position(|(planned_kind, planned_name)| planned_kind == kind && planned_name == name)
            .map_or(0, |position| position + 1);
        (index, total)
    }

    /// The row's `[i/n]` position, honouring a plan that repeats one `(kind, name)`.
    ///
    /// When the frame's own `step` names this exact row in the plan it wins (a second repeated row
    /// settles at its own position, not the first match's); otherwise the first planned match is
    /// used, so an unplanned or reassembled frame keeps [`Self::position`]'s behaviour.
    pub(super) fn position_at(&self, kind: &str, name: &str, frame_step: usize) -> (usize, usize) {
        let total = self.outer_total.max(self.plan.len());
        let index = frame_step
            .checked_sub(1)
            .and_then(|at| self.plan.get(at))
            .filter(|(planned_kind, planned_name)| planned_kind == kind && planned_name == name)
            .map_or_else(|| self.position(kind, name).0, |_| frame_step);
        (index, total)
    }

    /// The `[i/n]` a settled row prints: its announced plan position, so a per-frame total that
    /// disagrees with the merged plan can never fork the one visible denominator. A row the plan
    /// never announced (`restart`, `down`) keeps its own frame counter.
    pub(super) fn settled(&self, kind: &str, name: &str, frame: (usize, usize)) -> (usize, usize) {
        let (index, total) = self.position_at(kind, name, frame.0);
        if index > 0 && total > 0 {
            (index, total)
        } else {
            frame
        }
    }

    /// Counts one settled outer slot and remembers the row, clamped to a known plan total so a
    /// duplicate `finished` frame can never push `done` past `total`.
    pub(super) fn advance(&mut self, kind: &str, name: &str) {
        // A duplicate `finished`/`up-to-date` frame for the same `(kind, name)` (the reconcile
        // frame and the boot-scheduler frame) settles the row once: the first insert wins.
        if !self.finished.insert((kind.to_owned(), name.to_owned())) {
            return;
        }
        let next = self.outer_done.saturating_add(1);
        self.outer_done = if self.outer_total > 0 {
            next.min(self.outer_total)
        } else {
            next
        };
    }

    /// Names the outer step that currently owns the bar.
    pub(super) fn set_active(&mut self, index: usize, total: usize, kind: &str, target: &str) {
        self.outer_total = self.outer_total.max(total).max(index);
        self.active = Some(Active {
            index,
            total: self.outer_total,
            kind: kind.to_owned(),
            target: target.to_owned(),
        });
    }

    /// Drops the outer step once it settles.
    pub(super) fn clear_active(&mut self) {
        self.active = None;
    }

    /// Latches the image build's inner instruction as the bar's second level.
    pub(super) fn begin_inner(
        &mut self,
        index: usize,
        total: usize,
        image: &str,
        inner_index: usize,
        inner_total: usize,
        verb: &str,
    ) {
        self.outer_total = self.outer_total.max(total).max(index);
        match &mut self.building {
            Some(building) if building.image == image => {
                // A real instruction re-announcing a provisional pre-pull index supersedes it.
                building.settled.remove(&inner_index);
                building.index = index;
                building.total = self.outer_total;
                building.step = inner_index;
                building.steps = building.steps.max(inner_total);
                building.verb = verb.to_owned();
            }
            _ => {
                self.building = Some(Building {
                    index,
                    total: self.outer_total,
                    image: image.to_owned(),
                    settled: BTreeSet::new(),
                    step: inner_index,
                    steps: inner_total,
                    verb: verb.to_owned(),
                });
            }
        }
    }

    /// Records one inner instruction settling (`state == "done"`).
    pub(super) fn inner_done(&mut self, image: &str, inner_index: usize, inner_total: usize) {
        if let Some(building) = &mut self.building
            && building.image == image
            && inner_index > 0
        {
            building.steps = building.steps.max(inner_total);
            building.settled.insert(inner_index);
        }
    }

    /// Stops the inner fraction the moment the image's one `finished` frame counts its outer slot.
    pub(super) fn finish_build(&mut self) {
        self.building = None;
        self.active = None;
    }

    /// Applies one `step` frame to the inner level: `started` opens it, `done` advances it.
    pub(super) fn observe_step(&mut self, event: &Value) {
        // A pre-build external-pull row is not a planned instruction: it never enters the inner
        // fraction or the `step i/n` label, so a pull can never read as a built step.
        if event["provisional"].as_bool().unwrap_or(false) {
            return;
        }
        let image = event["image"].as_str().unwrap_or("image");
        // The image's outer slot already carries its work; a late or duplicate step frame must not
        // recreate a `Building` and add an inner fraction on top of a fully counted outer level.
        if self
            .finished
            .iter()
            .any(|(kind, name)| kind == "image" && name == image)
        {
            return;
        }
        let index = usize_of(event["index"].as_u64());
        let total = usize_of(event["total"].as_u64());
        let instruction = event["instruction"].as_str().unwrap_or("RUN");
        match event["state"].as_str().unwrap_or_default() {
            "started" => {
                let (outer_index, outer_total) = self.position("image", image);
                self.begin_inner(outer_index, outer_total, image, index, total, instruction);
            }
            "done" => self.inner_done(image, index, total),
            _ => {}
        }
    }

    /// The bar fraction: `(outer_done * inner_total + inner_done, outer_total * inner_total)`.
    ///
    /// The building image is not yet in `outer_done`, so its inner fraction is added once and its
    /// outer slot arrives only when it finishes — the two levels never overlap.
    pub(super) fn fraction(&self) -> (u64, u64) {
        let outer = self.outer_total as u64;
        if outer == 0 {
            return (0, 0);
        }
        match &self.building {
            Some(building) if building.steps > 0 => {
                let steps = building.steps as u64;
                let done = building.settled.len().min(building.steps) as u64;
                (self.outer_done as u64 * steps + done, outer * steps)
            }
            _ => (self.outer_done as u64, outer),
        }
    }

    /// Outer `done/total` counter paired with the two-level fractional fill, so internal
    /// `outer_total * inner_total` denominators never leak into the visible counter cell.
    pub(super) fn bar(&self) -> BarProgress {
        if self.outer_total == 0 {
            return BarProgress::outer(0, 0);
        }
        let (num, den) = self.fraction();
        BarProgress {
            done: self.outer_done,
            total: self.outer_total,
            num,
            den,
        }
        .clamped()
    }

    /// The bar's active label: `[1/3] image api · step  3/11 RUN`, then `[2/3] setup uv sync`.
    pub(super) fn label(&self) -> String {
        if let Some(building) = &self.building {
            let detail = format!(
                "image {} · step {} {}",
                building.image,
                counter(building.step, building.steps, digits(building.steps)),
                building.verb
            );
            return if building.index > 0 && building.total > 0 {
                format!(
                    "[{}] {detail}",
                    counter(building.index, building.total, digits(building.total))
                )
            } else {
                detail
            };
        }
        if let Some(active) = &self.active
            && active.index > 0
            && active.total > 0
        {
            return format!(
                "[{}] {} {}",
                counter(active.index, active.total, digits(active.total)),
                active.kind,
                active.target
            );
        }
        self.title.clone()
    }
}

fn usize_of(value: Option<u64>) -> usize {
    value
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
