//! The one boot progress model: outer plan completion plus the building image's inner steps.
//!
//! The bottom bar must never double count a build: before an image finishes it contributes only
//! its settled inner step fraction (`inner_done / inner_total`), and after its single `finished`
//! frame it contributes exactly one outer slot. The model owns the announced plan (so every outer
//! row keeps its `[i/n]` position), both levels, and the label, so [`super::pane`] only paints.

use std::collections::BTreeSet;

use serde_json::Value;

use super::render::BarProgress;

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

    /// The `[i/n]` a settled row prints: its announced plan position, so a per-frame total that
    /// disagrees with the merged plan can never fork the one visible denominator. A row the plan
    /// never announced (`restart`, `down`) keeps its own frame counter.
    pub(super) fn settled(&self, kind: &str, name: &str, frame: (usize, usize)) -> (usize, usize) {
        let (index, total) = self.position(kind, name);
        if index > 0 && total > 0 {
            (index, total)
        } else {
            frame
        }
    }

    /// Counts one settled outer slot (image, setup, service, hook).
    pub(super) fn advance(&mut self) {
        self.outer_done = self.outer_done.saturating_add(1);
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
        let image = event["image"].as_str().unwrap_or("image");
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
    }

    /// The bar's active label: `[1/3] image api · step 3/6 RUN`, then `[2/3] setup uv sync`.
    pub(super) fn label(&self) -> String {
        if let Some(building) = &self.building {
            let detail = format!(
                "image {} · step {}/{} {}",
                building.image, building.step, building.steps, building.verb
            );
            return if building.index > 0 && building.total > 0 {
                format!("[{}/{}] {detail}", building.index, building.total)
            } else {
                detail
            };
        }
        if let Some(active) = &self.active
            && active.index > 0
            && active.total > 0
        {
            return format!(
                "[{}/{}] {} {}",
                active.index, active.total, active.kind, active.target
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
mod tests {
    use std::time::Duration;

    use super::BootProgress;
    use crate::boot::render::{panel_line, percent_of, status_line};

    fn plan() -> Vec<(String, String)> {
        vec![
            ("resource".to_owned(), "pg".to_owned()),
            ("image".to_owned(), "api".to_owned()),
            ("service".to_owned(), "web".to_owned()),
        ]
    }

    /// M3.123: a fork announces its two prep rows plus every world row as one plan, so all five
    /// settled rows share the plan's denominator and the magenta kind role even when a daemon
    /// prep frame still echoes a narrower `1/3`.
    #[test]
    fn a_fork_plan_keeps_one_denominator_and_the_magenta_keyword() {
        use crate::boot::render::{Node, completed_row, outer_verb, outer_widths};

        let plan: Vec<(String, String)> = [
            ("fork", "snapshot"),
            ("git", "branch salut"),
            ("image", "api"),
            ("setup", "uv sync"),
            ("service", "web"),
        ]
        .into_iter()
        .map(|(kind, name)| (kind.to_owned(), name.to_owned()))
        .collect();
        let widths = outer_widths(&plan);
        let mut progress = BootProgress::new("Forking salut");
        progress.announce(&plan, plan.len());

        // The daemon's legacy prep frames disagree; the merged plan always wins.
        assert_eq!(progress.settled("fork", "snapshot", (1, 3)), (1, 5));
        assert_eq!(progress.settled("git", "branch salut", (2, 3)), (2, 5));
        assert_eq!(progress.settled("image", "api", (0, 0)), (3, 5));

        let rows = [
            (
                "fork",
                "snapshot",
                "",
                34,
                true,
                "✓ [1/5] fork   snapshot      34ms",
            ),
            (
                "git",
                "branch salut",
                "",
                3,
                true,
                "✓ [2/5] git    branch salut  3ms",
            ),
            (
                "image",
                "api",
                "cached",
                7,
                true,
                "✓ [3/5] image  api           cached  7ms",
            ),
            (
                "setup",
                "uv sync",
                "up to date",
                0,
                false,
                "• [4/5] setup  uv sync       up to date  <1ms",
            ),
            (
                "service",
                "web",
                "running",
                204,
                true,
                "✓ [5/5] start  web           running  204ms",
            ),
        ];
        for (position, (kind, name, state, millis, success, expected)) in
            rows.into_iter().enumerate()
        {
            assert_eq!(progress.settled(kind, name, (0, 0)), (position + 1, 5));
            let node = Node {
                kind,
                name,
                state,
                millis,
                run_id: None,
                success,
            };
            let plain = completed_row(&node, position + 1, 5, &widths, false);
            assert_eq!(plain, expected);
            assert!(!plain.contains('\u{1b}'));
            let styled = completed_row(&node, position + 1, 5, &widths, true);
            assert!(
                styled.contains(&format!("\u{1b}[35m{}\u{1b}[0m", outer_verb(kind))),
                "the kind keyword is magenta: {styled}"
            );
            assert!(
                styled.contains(&format!("\u{1b}[1m{name}\u{1b}[0m")),
                "the target is bold: {styled}"
            );
        }
    }

    /// Outer 1/3 settled plus inner 3/6 settled of the building image is exactly 50%, and settling
    /// the image never double counts.
    #[test]
    fn a_building_image_contributes_its_inner_fraction_once() {
        let mut progress = BootProgress::new("Booting demo");
        progress.announce(&plan(), 3);
        progress.advance();

        for step in 1..=3 {
            progress.begin_inner(2, 3, "api", step, 6, "RUN");
            progress.inner_done("api", step, 6);
        }
        progress.begin_inner(2, 3, "api", 4, 6, "RUN");
        assert_eq!(progress.fraction(), (9, 18), "1/3 + (3/6)/3 = 9/18");
        assert_eq!(percent_of(9, 18), 50);

        progress.begin_inner(2, 3, "api", 6, 6, "RUN");
        assert_eq!(
            progress.fraction(),
            (9, 18),
            "starting step 6/6 never marks the image 100% done while it still runs"
        );

        progress.finish_build();
        progress.advance();
        assert_eq!(
            progress.fraction(),
            (2, 3),
            "the finished image counts once as an outer slot"
        );
    }

    #[test]
    fn the_label_switches_between_the_build_step_and_the_outer_step() {
        let mut progress = BootProgress::new("Booting demo");
        progress.announce(
            &[
                ("image".to_owned(), "api".to_owned()),
                ("setup".to_owned(), "uv sync".to_owned()),
                ("service".to_owned(), "web".to_owned()),
            ],
            3,
        );
        progress.begin_inner(1, 3, "api", 3, 6, "RUN");
        assert_eq!(progress.label(), "[1/3] image api · step 3/6 RUN");
        progress.finish_build();
        progress.set_active(2, 3, "setup", "uv sync");
        assert_eq!(progress.label(), "[2/3] setup uv sync");
        progress.clear_active();
        assert_eq!(progress.label(), "Booting demo");
    }

    #[test]
    fn every_planned_row_keeps_its_one_based_plan_position() {
        let mut progress = BootProgress::new("Booting demo");
        progress.announce(&plan(), 3);
        assert_eq!(progress.position("resource", "pg"), (1, 3));
        assert_eq!(progress.position("image", "api"), (2, 3));
        assert_eq!(progress.position("service", "web"), (3, 3));
        assert_eq!(progress.position("hook", "post"), (0, 3));
    }

    /// Outer `0/2` + inner `2/4` settled renders `0/2  25%` (never the internal `2/8` fraction).
    #[test]
    fn bar_text_keeps_outer_counter_and_fractional_percent_without_mixed_units() {
        let mut progress = BootProgress::new("Booting demo");
        progress.announce(
            &[
                ("image".to_owned(), "api".to_owned()),
                ("service".to_owned(), "web".to_owned()),
            ],
            2,
        );
        for step in 1..=2 {
            progress.begin_inner(1, 2, "api", step, 4, "COPY");
            progress.inner_done("api", step, 4);
        }
        progress.begin_inner(1, 2, "api", 3, 4, "RUN");
        let bar = progress.bar();
        assert_eq!((bar.done, bar.total, bar.num, bar.den), (0, 2, 2, 8));

        let plain = status_line('⠸', bar, &progress.label(), Duration::from_millis(200), 110);
        assert_eq!(
            plain,
            "⠸ [######..................]  0/2  25% · [1/2] image api · step 3/4 RUN · 0.2s"
        );
        assert!(!plain.contains("2/8"), "{plain}");

        let styled = panel_line('⠸', bar, &progress.label(), Duration::from_millis(200), 110);
        assert!(styled.contains("0/2") && styled.contains("25%"), "{styled}");
        assert!(!styled.contains("2/8"), "{styled}");
    }
}
