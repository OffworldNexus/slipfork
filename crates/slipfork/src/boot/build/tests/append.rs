//! Append-only regressions: padded counters on both levels, one end row per image, and the
//! instruction-total accounting that separates `cached 9 · built 2` from the plan step count.

use serde_json::json;

use super::super::{Columns, LiveKey, Narrative, Phase};
use crate::boot::progress::BootProgress;
use crate::boot::render::{OuterWidths, completed_row, counter, digits, outer_row, outer_widths};
use crate::ui::report::Palette;
use crate::ui::report::{Counter as PlanCounter, Done, Outcome, Row};

#[test]
fn the_outer_counter_pads_to_the_widest_plan_position_on_both_sides_of_ten() {
    let plain = Palette::new(false);
    let widths = outer_widths(&[("image".to_owned(), "api".to_owned())]);
    let first = outer_row(
        "✓",
        Some((1, 11)),
        "image",
        "api",
        "running",
        None,
        None,
        &widths,
        plain,
    );
    let tenth = outer_row(
        "✓",
        Some((10, 11)),
        "image",
        "api",
        "running",
        None,
        None,
        &widths,
        plain,
    );
    assert!(first.contains("[ 1/11]"), "{first}");
    assert!(tenth.contains("[10/11]"), "{tenth}");
    assert_eq!(
        first.find("image"),
        tenth.find("image"),
        "the counter column never shifts the verb: {first:?} vs {tenth:?}"
    );
    assert_eq!(counter(1, 11, digits(11)), " 1/11");
    assert_eq!(counter(10, 11, digits(11)), "10/11");
}

#[test]
fn the_inner_counter_pads_to_the_widest_instruction_total() {
    let plain = Palette::new(false);
    let first = Phase::new(
        LiveKey::Step {
            image: "api".to_owned(),
            index: 1,
        },
        Some((1, 11)),
        "FROM".to_owned(),
        "api".to_owned(),
        "python:3.13-slim".to_owned(),
        Columns::default(),
    )
    .anchor_row(plain);
    let tenth = Phase::new(
        LiveKey::Step {
            image: "api".to_owned(),
            index: 10,
        },
        Some((10, 11)),
        "WORKDIR".to_owned(),
        "api".to_owned(),
        "/code".to_owned(),
        Columns::default(),
    )
    .anchor_row(plain);
    assert!(first.starts_with("  →  1/11"), "{first}");
    assert!(tenth.starts_with("  → 10/11"), "{tenth}");
    assert_eq!(
        first.find("from"),
        tenth.find("workdir"),
        "the instruction column stays put: {first:?} vs {tenth:?}"
    );
}

#[test]
fn an_image_emits_one_end_row_and_counts_its_outer_slot_once() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    narrative.started(
        &json!({
            "event": "started",
            "step": 1,
            "total": 3,
            "kind": "image",
            "name": "api",
        }),
        Some((1, 3)),
    );
    narrative.step(
        &json!({
            "event": "step",
            "image": "api",
            "index": 1,
            "total": 1,
            "instruction": "FROM",
            "args": "python:3.13-slim",
            "state": "done",
            "elapsed_ms": 5,
        }),
        plain,
    );
    let first = narrative
        .finish_node(
            "image",
            "api",
            &json!({
                "event": "finished",
                "kind": "image",
                "name": "api",
                "state": "built",
                "executed": 1,
                "duration_ms": 2500,
            }),
            plain,
        )
        .expect("the image settles once");
    let end_rows = first
        .iter()
        .filter(|action| matches!(action, crate::boot::build::Action::Print(row) if row.contains("image")))
        .count();
    assert_eq!(end_rows, 1, "exactly one end row: {first:?}");

    let duplicate = narrative
        .finish_node(
            "image",
            "api",
            &json!({
                "event": "up-to-date",
                "kind": "image",
                "name": "api",
                "state": "cached",
                "duration_ms": 8,
            }),
            plain,
        )
        .expect("a duplicate frame is absorbed");
    assert!(
        duplicate.is_empty(),
        "the reconcile `up-to-date` frame never prints a second end row: {duplicate:?}"
    );

    let mut progress = BootProgress::new("boot");
    progress.announce(
        &[
            ("image".to_owned(), "api".to_owned()),
            ("setup".to_owned(), "uv sync".to_owned()),
            ("service".to_owned(), "web".to_owned()),
        ],
        3,
    );
    progress.advance("image", "api");
    progress.advance("image", "api");
    let bar = progress.bar();
    assert_eq!(
        (bar.done, bar.total),
        (1, 3),
        "a duplicate finished/up-to-date pair must not show 2/3"
    );
}

#[test]
fn a_mixed_image_reports_cached_steps_and_built_steps_from_its_instruction_total() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    narrative.started(
        &json!({
            "event": "started",
            "step": 1,
            "total": 2,
            "kind": "image",
            "name": "api",
        }),
        Some((1, 2)),
    );
    for index in 1..=11 {
        narrative.step(
            &json!({
                "event": "step",
                "image": "api",
                "index": index,
                "total": 11,
                "instruction": "RUN",
                "args": format!("echo {index}"),
                "state": "done",
                "elapsed_ms": 1,
            }),
            plain,
        );
    }
    let actions = narrative
        .finish_node(
            "image",
            "api",
            &json!({
                "event": "finished",
                "kind": "image",
                "name": "api",
                "state": "built",
                "executed": 2,
                "duration_ms": 2500,
            }),
            plain,
        )
        .expect("the image settles");
    let end = actions
        .iter()
        .rev()
        .find_map(|action| match action {
            crate::boot::build::Action::Print(row) => Some(row.as_str()),
            _ => None,
        })
        .expect("the end row prints last");
    assert!(
        end.contains("cached 9 · built 2"),
        "a mixed 11-instruction image names both halves: {end}"
    );
    assert!(!end.contains("built 0"), "never `built 0 steps`: {end}");
    assert!(
        !end.contains("11 steps"),
        "the plan `total` (2) is never read as instructions: {end}"
    );

    let widths = OuterWidths {
        kind: 5,
        target: 7,
        state: 0,
    };
    let full = Row::settled(
        Some(PlanCounter { index: 1, total: 2 }),
        "image",
        "api",
        Outcome::Ok(Done::Built {
            steps: std::num::NonZeroUsize::new(11).expect("nonzero"),
        }),
        Some(2500),
    );
    let full = completed_row(&full, &widths, plain);
    assert!(full.contains("built 11 steps"), "{full}");
}
