//! Service settlement and the image-wide instruction column.

use serde_json::json;

use super::super::super::{Action, Narrative};
use super::frames::{end_row, finished_frame_steps, frame, printed_rows, settled_rows};
use crate::ui::report::Palette;

#[test]
fn cached_image_never_emits_inner_rows_and_service_failure_expands_at_outer_indent() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    assert!(
        narrative
            .started(
                &json!({
                    "event": "started",
                    "step": 1,
                    "total": 3,
                    "kind": "image",
                    "name": "api",
                }),
                Some((1, 3)),
            )
            .is_empty(),
        "cached image start opens nothing until a step frame arrives"
    );
    let cached = narrative
        .finish_node(
            "image",
            "api",
            &json!({
                "event": "finished",
                "kind": "image",
                "name": "api",
                "state": "cached",
                "duration_ms": 8,
            }),
            plain,
        )
        .expect("a cached image still emits its typed row");
    assert_eq!(
        end_row(&cached),
        Some("✓ [1/3] image  api  cached  8ms"),
        "a step-less cached image keeps its single typed cached row"
    );

    let begun = narrative.started(
        &json!({
            "event": "started",
            "step": 3,
            "total": 3,
            "kind": "service",
            "name": "web",
            "command": "sh -c 'exit 7'",
        }),
        Some((3, 3)),
    );
    assert!(
        begun.is_empty(),
        "an outer started frame prints no `→` row: {begun:?}"
    );
    let streamed = narrative.log(
        &json!({
            "event": "log",
            "kind": "service",
            "name": "web",
            "stream": "stderr",
            "line": "bind failed",
        }),
        80,
        plain,
    );
    let header = printed_rows(&streamed);
    assert_eq!(
        header,
        ["→ [3/3] start  web  running  sh -c 'exit 7'"],
        "the first streamed line prints the once-only outer header"
    );
    let failed = narrative.failed(
        &json!({
            "event": "failed",
            "kind": "service",
            "phase": "start",
            "target": "web",
            "exit_code": 7,
            "next": "slipfork logs -r 5be421",
        }),
        plain,
    );
    let printed: Vec<&str> = failed
        .iter()
        .filter_map(|action| match action {
            Action::Print(row) => Some(row.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        printed,
        ["✗ [3/3] start  web  failed"],
        "append-only failure prints the verdict row and never replays the stream"
    );
    let diagnosis: Vec<&str> = failed
        .iter()
        .filter_map(|action| match action {
            Action::Diagnose(row) => Some(row.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        diagnosis,
        [
            "✗ start web failed (exit 7)",
            "  next: slipfork logs -r 5be421",
        ]
    );
}

/// The `finished` frame's `detail` is narration, never a command: a settled service row keeps the
/// command its `started` frame registered, in the command column after the lifecycle `ready`.
#[test]
fn a_settled_service_keeps_its_started_command_and_states_ready() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    let begun = narrative.started(
        &json!({
            "event": "started",
            "step": 2,
            "total": 2,
            "kind": "service",
            "name": "web",
            "command": "python3 -m http.server 8195",
        }),
        Some((2, 2)),
    );
    assert!(
        begun.is_empty(),
        "a started service registers its live phase without printing: {begun:?}"
    );
    let settled = narrative.finished(
        &json!({
            "event": "finished",
            "step": 2,
            "total": 2,
            "kind": "service",
            "name": "web",
            "state": "ready",
            "detail": "started",
            "duration_ms": 1200,
        }),
        plain,
    );
    let Some(Action::Settle(row)) = settled else {
        panic!("a service finished frame settles one row, got {settled:?}");
    };
    assert_eq!(
        row,
        "✓ [2/2] start  web  ready  python3 -m http.server 8195  1.2s"
    );
    assert!(!row.contains('\u{1b}'));
}

/// The keyword column is the widest keyword in the image's whole walked plan, so FROM/COPY rows
/// keep the same args column even though WORKDIR appears later.
#[test]
fn every_inner_row_shares_one_instruction_column_from_the_first_step() {
    let steps = [
        ("FROM", "python:3.14-trixie AS base", 1u64),
        ("COPY", ". /app", 2),
        ("ADD", "x /y", 3),
        ("RUN", "echo hi", 4),
        ("WORKDIR", "/code", 46),
    ];
    let expected_plain = [
        "  ✓ 1/5 from     python:3.14-trixie AS base  1ms",
        "  ✓ 2/5 copy     . /app  2ms",
        "  ✓ 3/5 add      x /y  3ms",
        "  ✓ 4/5 run      echo hi  4ms",
        "  ✓ 5/5 workdir  /code  46ms",
    ];
    let expected_styled = [
        "  \u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m1/5\u{1b}[0m \u{1b}[38;5;132mfrom\u{1b}[0m     python:3.14-trixie AS base  \u{1b}[38;5;244m1ms\u{1b}[0m",
        "  \u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m2/5\u{1b}[0m \u{1b}[38;5;132mcopy\u{1b}[0m     . /app  \u{1b}[38;5;244m2ms\u{1b}[0m",
        "  \u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m3/5\u{1b}[0m \u{1b}[38;5;132madd\u{1b}[0m      x /y  \u{1b}[38;5;244m3ms\u{1b}[0m",
        "  \u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m4/5\u{1b}[0m \u{1b}[38;5;132mrun\u{1b}[0m      echo hi  \u{1b}[38;5;244m4ms\u{1b}[0m",
        "  \u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m5/5\u{1b}[0m \u{1b}[38;5;132mworkdir\u{1b}[0m  /code  \u{1b}[38;5;244m46ms\u{1b}[0m",
    ];

    for styled in [false, true] {
        let palette = Palette::new(styled);
        let mut narrative = Narrative::new(false);
        let mut actions = Vec::new();
        for (position, (instruction, args, millis)) in steps.iter().enumerate() {
            let index = u64::try_from(position + 1).expect("tiny index");
            actions.extend(narrative.step(
                &frame("api", index, 5, instruction, args, "started", 0, 7),
                palette,
            ));
            actions.extend(narrative.step(
                &frame("api", index, 5, instruction, args, "done", *millis, 7),
                palette,
            ));
        }
        let tail = narrative
            .finish_node("image", "api", &finished_frame_steps(5), palette)
            .expect("the image settles");
        actions.extend(tail);
        let rows = settled_rows(&actions);
        assert_eq!(rows.len(), steps.len(), "one row per instruction: {rows:?}");

        let mut previous: Option<usize> = None;
        for (position, (row, (instruction, args, _))) in rows.iter().zip(steps.iter()).enumerate() {
            let expected = if styled {
                expected_styled[position]
            } else {
                expected_plain[position]
            };
            assert_eq!(row, expected, "row is byte-exact: {row:?}");
            assert!(
                row.contains(&instruction.to_lowercase()),
                "the lowercase keyword survives: {row:?}"
            );

            let column = row.find(args).expect("args are present");
            if let Some(previous) = previous {
                assert_eq!(column, previous, "args start at one column: {row:?}");
            }
            previous = Some(column);
        }
    }
}
