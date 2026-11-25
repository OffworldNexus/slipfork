//! The failing-build journey: one truthful `✗` image row, the failing step, and one remedy.

use serde_json::json;

use super::super::{Action, Narrative};
use crate::ui::Palette;

#[test]
fn a_step_less_cached_image_uses_the_typed_cached_row_and_unknown_never_succeeds() {
    let plain = Palette::new(false);
    let mut cached = Narrative::new(false);
    cached.started(
        &json!({
            "event": "started",
            "step": 1,
            "total": 1,
            "kind": "image",
            "name": "api",
        }),
        Some((1, 1)),
    );
    let actions = cached
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
        .expect("a step-less image still settles");
    let rows: Vec<&String> = actions
        .iter()
        .filter_map(|action| match action {
            Action::Print(row) => Some(row),
            _ => None,
        })
        .collect();
    assert_eq!(
        rows.last().map(|row| row.as_str()),
        Some("✓ [1/1] image  api  cached  8ms")
    );

    let mut unknown = Narrative::new(false);
    unknown.started(
        &json!({
            "event": "started",
            "step": 1,
            "total": 1,
            "kind": "image",
            "name": "api",
        }),
        Some((1, 1)),
    );
    let actions = unknown
        .finish_node(
            "image",
            "api",
            &json!({
                "event": "finished",
                "kind": "image",
                "name": "api",
                "duration_ms": 0,
            }),
            plain,
        )
        .expect("an unknown image still settles");
    let rows: Vec<&String> = actions
        .iter()
        .filter_map(|action| match action {
            Action::Print(row) => Some(row),
            _ => None,
        })
        .collect();
    assert_eq!(
        rows.last().map(|row| row.as_str()),
        Some("• [1/1] image  api  finished  <1ms")
    );
    assert!(rows.iter().all(|row| !row.contains('✓')), "{rows:?}");
}

#[test]
fn a_failed_build_prints_the_failing_step_and_never_a_success_row() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    narrative.started(
        &json!({
            "event": "started",
            "step": 1,
            "total": 1,
            "kind": "image",
            "name": "api",
        }),
        Some((1, 1)),
    );
    for index in 1..=4 {
        narrative.step(
            &json!({
                "event": "step",
                "image": "api",
                "index": index,
                "total": 7,
                "instruction": "RUN",
                "args": "",
                "state": "done",
                "elapsed_ms": 0,
            }),
            plain,
        );
    }
    let started = narrative.step(
        &json!({
            "event": "step",
            "image": "api",
            "index": 5,
            "total": 7,
            "instruction": "RUN",
            "args": "apt-get update",
            "state": "started",
            "elapsed_ms": 0,
        }),
        plain,
    );
    assert!(matches!(&started[0], Action::Print(row) if row == "→ [1/1] image  api  running"));

    let finished = narrative
        .finish_node(
            "image",
            "api",
            &json!({
                "event": "finished",
                "kind": "image",
                "name": "api",
                "state": "failed",
                "executed": 0,
                "elapsed_ms": 0,
                "duration_ms": 0,
                "run_id": "5be421",
            }),
            plain,
        )
        .expect("the image settles");
    let rows: Vec<&String> = finished
        .iter()
        .filter_map(|action| match action {
            Action::Print(row) => Some(row),
            _ => None,
        })
        .collect();
    assert_eq!(
        rows.last().map(|row| row.as_str()),
        Some("✗ [1/1] image  api  failed at 5/7  <1ms")
    );
    assert!(
        rows.iter()
            .all(|row| !row.contains('✓') && !row.contains("built")),
        "{rows:?}"
    );

    let failed = narrative.failed(
        &json!({
            "event": "failed",
            "phase": "run",
            "target": "api",
            "exit_code": 1,
            "run_id": "5be421",
            "next": "slipfork logs -r 5be421",
        }),
        plain,
    );
    let held: Vec<&String> = failed
        .iter()
        .filter_map(|action| match action {
            Action::Print(row) => Some(row),
            _ => None,
        })
        .collect();
    assert_eq!(
        held.first().map(|row| row.as_str()),
        Some("  ✗ 5/7 run  apt-get update"),
        "the failing instruction keeps its own row: {held:?}"
    );
    assert_eq!(held.len(), 1, "the verdict prints one `✗` row: {held:?}");
    let diagnosis: Vec<&String> = failed
        .iter()
        .filter_map(|action| match action {
            Action::Diagnose(row) => Some(row),
            _ => None,
        })
        .collect();
    assert_eq!(
        diagnosis,
        [
            "✗ RUN api failed (exit 1)",
            "  next: slipfork logs -r 5be421",
        ]
    );
    assert!(
        !failed
            .iter()
            .any(|action| matches!(action, Action::Print(row) if row.is_empty())),
        "no blank rows are emitted before the remedy"
    );
}
