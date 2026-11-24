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
            Some("project + world data"),
            34,
            true,
            "✓ [1/5] fork   snapshot      done  project + world data  34ms",
        ),
        (
            "git",
            "branch salut",
            "",
            Some("checked out"),
            3,
            true,
            "✓ [2/5] git    branch salut  done  checked out  3ms",
        ),
        (
            "image",
            "api",
            "cached",
            None,
            7,
            true,
            "✓ [3/5] image  api           cached  7ms",
        ),
        (
            "setup",
            "uv sync",
            "up to date",
            None,
            0,
            false,
            "• [4/5] setup  uv sync       up to date  <1ms",
        ),
        (
            "service",
            "web",
            "running",
            None,
            204,
            true,
            "✓ [5/5] start  web           running  204ms",
        ),
    ];
    for (position, (kind, name, state, command, millis, success, expected)) in
        rows.into_iter().enumerate()
    {
        assert_eq!(progress.settled(kind, name, (0, 0)), (position + 1, 5));
        let node = Node {
            kind,
            name,
            state,
            command,
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

/// M3.133: a `drop` plan repeats `(stop, jello)` and `(delete, jello)`; each frame's own step
/// must resolve its row, so the settled stop prints `[2/4]` rather than the first match.
#[test]
fn a_repeated_plan_row_resolves_from_the_frame_step() {
    let rows: Vec<(String, String)> = ["stop", "stop", "delete", "delete"]
        .into_iter()
        .map(|kind| (kind.to_owned(), "jello".to_owned()))
        .collect();
    let mut progress = BootProgress::new("Dropping jello");
    progress.announce(&rows, 4);
    assert_eq!(progress.position_at("stop", "jello", 1), (1, 4));
    assert_eq!(progress.position_at("stop", "jello", 2), (2, 4));
    assert_eq!(progress.position_at("delete", "jello", 3), (3, 4));
    assert_eq!(progress.position_at("delete", "jello", 4), (4, 4));
    // A frame whose step points at another row falls back to the first planned match.
    assert_eq!(progress.position_at("stop", "jello", 3), (1, 4));
    assert_eq!(progress.settled("stop", "jello", (2, 4)), (2, 4));
    assert_eq!(progress.settled("delete", "jello", (4, 4)), (4, 4));
    assert_eq!(progress.settled("stop", "jello", (0, 0)), (1, 4));
}

/// M3.133: on a unique plan the frame-derived lookup can never move a row away from its plan
/// position, so `up`/`fork` counters keep their bytes for any frame step.
#[test]
fn a_unique_plan_ignores_the_frame_step() {
    let mut progress = BootProgress::new("Booting demo");
    progress.announce(&plan(), 3);
    for step in 0..6 {
        for (kind, name) in [("resource", "pg"), ("image", "api"), ("service", "web")] {
            assert_eq!(
                progress.position_at(kind, name, step),
                progress.position(kind, name),
                "step {step} {kind} {name}"
            );
        }
    }
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
