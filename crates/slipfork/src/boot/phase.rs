//!
//! One Dockerfile instruction or one boot command step owns a [`LiveStep`] in [`LiveSteps`] from
//! the moment it is announced until it settles. The status line reads the most recently active
//! step's [`Phase`] so it can keep the spinner, counter, and elapsed clock live.
//!
//! Two levels share one phase: an outer boot row at column 0 (`[i/n] KIND target detail`) and an
//! indented inner Dockerfile row (`  n/N INSTRUCTION args`, no target, no brackets). The level is
//! carried by the [`LiveKey`], so a caller never re-scans strings to decide the shape.

use std::time::Instant;

use slipfork_world::snapshot::TailPhase;

use crate::ui::Palette;
use crate::ui::elapsed_text;

use super::layout::{Columns, compose_live};
use super::render::{counter, digits, outer_row, push_padded};

/// The typed [`TailPhase`] vocabulary is shared with the wire layer; this render-side
/// mirror exists because `lib.rs` also compiles `boot` while `world/` is binary-only.
mod steps;
pub(super) use steps::LiveSteps;

/// The wire label one tail frame writes (`sealing`, `recording`, `publishing`, …).
pub(super) const fn tail_label(phase: TailPhase) -> &'static str {
    match phase {
        TailPhase::Cloning => "cloning",
        TailPhase::Executing => "executing",
        TailPhase::Sealing => "sealing",
        TailPhase::Recording => "recording",
        TailPhase::Publishing => "publishing",
    }
}

/// Parses one `step` frame's `phase` label back into the typed tail phase.
pub(super) fn tail_phase(value: &str) -> Option<TailPhase> {
    match value {
        "cloning" => Some(TailPhase::Cloning),
        "executing" => Some(TailPhase::Executing),
        "sealing" => Some(TailPhase::Sealing),
        "recording" => Some(TailPhase::Recording),
        "publishing" => Some(TailPhase::Publishing),
        _ => None,
    }
}

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
    /// The post-execution phase this row is in, if any; never resets `started`.
    pub(super) tail: Option<TailPhase>,
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
            tail: None,
        }
    }

    /// Applies one tail phase in place; the row's `started` clock keeps ticking.
    pub(super) fn set_tail(&mut self, phase: TailPhase) {
        self.tail = Some(phase);
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
            self.outer_row("→", "running", Some(self.shown_args()), None, palette)
        }
    }

    /// The settled `✓ …` row that collapses the step.
    ///
    /// An inner Dockerfile step has no lifecycle cell; an outer row keeps a real state word
    /// (`running` for the boot command that just reached it) instead of the non-lifecycle `done`.
    /// A settled row never keeps the live tail suffix: the phase label belongs to the running row.
    pub(super) fn settled_row(&self, millis: u64, palette: Palette) -> String {
        let mark = palette.icon_ok();
        if self.is_inner() {
            self.inner_row(&mark, Some(millis), palette)
        } else {
            self.outer_row(
                &mark,
                "running",
                Some(self.shown_args()),
                Some(millis),
                palette,
            )
        }
    }

    /// The settled outer row: its lifecycle `state` follows the frame, the command stays separate.
    pub(super) fn settled_outer(
        &self,
        millis: u64,
        state: &str,
        command: Option<&str>,
        stale: bool,
        palette: Palette,
    ) -> String {
        let mark = if stale {
            palette.meta("•")
        } else {
            palette.icon_ok()
        };
        // A cache hit ran no command; a live service keeps its run command in the command column,
        // while a prep frame supplies its own `detail` phrase as the override.
        let command = if stale {
            None
        } else {
            command.or(Some(self.shown_args()))
        };
        self.outer_row(&mark, state, command, Some(millis), palette)
    }

    /// The expanded `✗ …` row printed with a failed step's output.
    pub(super) fn failed_row(&self, palette: Palette) -> String {
        let mark = palette.tint("✗", "31");
        if self.is_inner() {
            self.inner_row(&mark, None, palette)
        } else {
            self.outer_row(&mark, "failed", None, None, palette)
        }
    }

    /// The re-anchor an append-only stream prints before output from another step.
    pub(super) fn anchor_row(&self, palette: Palette) -> String {
        if self.is_inner() {
            self.inner_row("→", None, palette)
        } else {
            self.outer_row("→", "running", Some(self.shown_args()), None, palette)
        }
    }

    /// Wall time since the step started, for the live spinner row.
    pub(super) fn elapsed_millis(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// The one in-place status line for this phase, laid out so `width` is spent on the args.
    ///
    /// This is the only shape that appends the `· sealing`/`· recording`/… tail suffix: the
    /// suffix describes work happening right now, so it never survives into a settled row. The
    /// row is composed as `prefix + args + suffix` and a long instruction only truncates its
    /// `args` (with an ellipsis); the elapsed time and tail phase are always kept whole, because
    /// shearing them is exactly what makes a slow step look hung.
    pub(super) fn live_row(
        &self,
        spinner: char,
        millis: u64,
        width: usize,
        palette: Palette,
    ) -> String {
        let mark = spinner.to_string();
        let elapsed = palette.meta(&elapsed_text(millis));
        if self.is_inner() {
            compose_live(
                self.inner_prefix(&mark, palette),
                self.args.trim(),
                self.inner_suffix(&elapsed, palette),
                width,
            )
        } else {
            let prefix = outer_row(
                &mark,
                self.count,
                &self.verb,
                &self.target,
                "running",
                None,
                None,
                &self.columns.as_outer(),
                palette,
            );
            compose_live(
                prefix,
                self.shown_args().trim(),
                format!("  {elapsed}"),
                width,
            )
        }
    }

    /// `  {mark} n/N instruction ` — the one inner identity prefix both row shapes share.
    fn inner_prefix(&self, mark: &str, palette: Palette) -> String {
        let mut prefix = String::from("  ");
        prefix.push_str(mark);
        prefix.push(' ');
        if let Some((index, total)) = self.count
            && index > 0
            && total > 0
        {
            prefix.push_str(&palette.meta(&counter(index, total, digits(total))));
            prefix.push(' ');
        }
        let keyword = self.verb.to_lowercase();
        push_padded(
            &mut prefix,
            &palette.substep(&keyword),
            keyword.chars().count(),
            self.columns.verb,
        );
        prefix
    }

    /// The one live suffix (`  elapsed · label`); only [`Phase::live_row`] paints it.
    fn inner_suffix(&self, elapsed: &str, palette: Palette) -> String {
        let mut suffix = format!("  {elapsed}");
        if let Some(tail) = self.tail
            && tail != TailPhase::Executing
        {
            suffix.push_str(&palette.meta(&format!(" · {}", tail_label(tail))));
        }
        suffix
    }

    /// `  ✓ n/N instruction  args  elapsed` — no target, no brackets, indent 2.
    fn inner_row(&self, mark: &str, millis: Option<u64>, palette: Palette) -> String {
        let mut row = self.inner_prefix(mark, palette);
        let args = self.args.trim();
        if !args.is_empty() {
            row.push_str("  ");
            row.push_str(args);
        }
        if let Some(millis) = millis {
            row.push_str("  ");
            row.push_str(&palette.meta(&elapsed_text(millis)));
        }
        while row.ends_with(' ') {
            row.pop();
        }
        row
    }

    /// `[i/n] KIND  target  state  command  elapsed` at column 0.
    fn outer_row(
        &self,
        mark: &str,
        state: &str,
        command: Option<&str>,
        millis: Option<u64>,
        palette: Palette,
    ) -> String {
        outer_row(
            mark,
            self.count,
            &self.verb,
            &self.target,
            state,
            command,
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

#[cfg(test)]
mod tests {
    use super::{LiveKey, LiveSteps, Phase, tail_label};
    use crate::boot::layout::Columns;
    use crate::ui::Palette;
    use slipfork_world::snapshot::TailPhase;

    fn inner() -> Phase {
        Phase::new(
            LiveKey::Step {
                image: "api".to_owned(),
                index: 6,
            },
            Some((6, 7)),
            "RUN".to_owned(),
            "api".to_owned(),
            "sh /tmp/oni.sh && apt-get install -y blastit".to_owned(),
            Columns { verb: 5, target: 0 },
        )
    }

    #[test]
    fn tail_phase_renders_after_the_elapsed_time_and_is_pure_without_colour() {
        let mut phase = inner();
        phase.set_tail(TailPhase::Sealing);
        let row = phase.live_row('⠋', 7300, 200, Palette::new(false));
        assert!(row.contains("7.3s · sealing"), "{row:?}");
        assert!(!row.contains('\u{1b}'), "NO_COLOR rows are pure: {row:?}");

        let coloured = phase.live_row('⠋', 7300, 200, Palette::new(true));
        assert!(coloured.contains('\u{1b}'));
        assert!(coloured.contains("sealing"), "{coloured:?}");
    }

    #[test]
    fn executing_never_paints_a_suffix_on_the_live_row() {
        let mut phase = inner();
        phase.set_tail(TailPhase::Executing);
        let row = phase.live_row('⠋', 7300, 200, Palette::new(false));
        assert!(row.ends_with("7.3s"), "{row:?}");
        assert!(!row.contains("executing"), "{row:?}");
    }

    #[test]
    fn settled_rows_drop_the_tail_label_while_the_live_row_keeps_it() {
        let mut phase = inner();
        phase.set_tail(TailPhase::Recording);
        let plain = Palette::new(false);

        let live = phase.live_row('⠼', 3200, 200, plain);
        assert!(live.contains("3.2s · recording"), "{live:?}");

        let settled = phase.settled_row(3200, plain);
        assert!(settled.contains("3.2s"), "{settled:?}");
        assert!(
            !settled.contains('·') && !settled.contains("recording"),
            "a settled row never carries a live tail label: {settled:?}"
        );
        assert!(!settled.contains('\u{1b}'), "{settled:?}");
        assert!(!phase.failed_row(plain).contains('·'));
        assert!(!phase.anchor_row(plain).contains('·'));
        assert!(!phase.started_row(plain).contains('·'));
    }

    #[test]
    fn long_args_shear_at_the_ellipsis_and_keep_the_elapsed_and_tail_phase() {
        let mut phase = inner();
        phase.args = concat!(
            "sh /tmp/oni.sh && head -c 500M /dev/zero > /code/fat.bin && mkdir -p /code/own && ",
            "for i in $(seq 1 9); do cp /code/fat.bin /code/own/$i; done"
        )
        .to_owned();
        phase.set_tail(TailPhase::Sealing);

        let row = phase.live_row('⠼', 3200, 110, Palette::new(false));
        assert!(row.ends_with("3.2s · sealing"), "{row:?}");
        assert!(row.contains('…'), "long args are ellipsised: {row:?}");
        assert!(
            !row.contains("$(seq"),
            "args are the cells that go: {row:?}"
        );
        assert!(console::measure_text_width(&row) <= 110, "{row:?}");
        assert!(
            !row.contains('\u{1b}'),
            "NO_COLOR live rows are pure: {row:?}"
        );
    }

    #[test]
    fn a_degenerate_width_clamps_to_the_tail_suffix_with_an_ellipsis() {
        let mut phase = inner();
        phase.set_tail(TailPhase::Sealing);
        let row = phase.live_row('⠼', 3200, 12, Palette::new(false));
        assert!(
            console::measure_text_width(&row) <= 12,
            "the row is clamped to the terminal width: {row:?}"
        );
        assert!(
            row.ends_with('…'),
            "the clamped row ends in an ellipsis instead of overflowing: {row:?}"
        );
        assert!(!row.contains("blastit"), "{row:?}");
    }

    #[test]
    fn a_short_live_row_is_byte_identical_when_it_fits() {
        let mut phase = inner();
        phase.args = "apk add curl".to_owned();
        phase.set_tail(TailPhase::Sealing);
        let row = phase.live_row('⠼', 7300, 200, Palette::new(false));
        assert_eq!(row, "  ⠼ 6/7 run    apk add curl  7.3s · sealing");
    }

    #[test]
    fn empty_args_still_compose_the_prefix_and_progress_suffix() {
        let mut phase = inner();
        phase.args.clear();
        phase.set_tail(TailPhase::Sealing);
        let row = phase.live_row('⠼', 3200, 200, Palette::new(false));
        assert_eq!(row, "  ⠼ 6/7 run    3.2s · sealing");
        assert!(!row.contains('\u{1b}'), "{row:?}");
    }

    #[test]
    fn successive_tail_frames_relabel_one_row_and_never_restart_its_clock() {
        let key = LiveKey::Step {
            image: "api".to_owned(),
            index: 6,
        };
        let mut steps = LiveSteps::new();
        steps.begin(inner());
        let started = steps.steps[0].phase.started;

        for phase in [
            TailPhase::Sealing,
            TailPhase::Recording,
            TailPhase::Publishing,
        ] {
            assert!(
                steps.set_tail(&key, phase),
                "{phase:?} applies to the open row"
            );
            assert_eq!(
                steps.steps.len(),
                1,
                "a tail frame never opens a second row"
            );
            assert_eq!(steps.steps[0].phase.tail, Some(phase));
            assert_eq!(
                steps.steps[0].phase.started, started,
                "a later tail frame never restarts the clock"
            );
            let row = steps.steps[0]
                .phase
                .live_row('⠼', 3200, 200, Palette::new(false));
            assert!(row.contains(tail_label(phase)), "{phase:?} row: {row:?}");
        }
    }
}
