//! Instruction-order release: one settled row per index, in index order, image end row last.

use serde_json::{Value, json};

use super::super::{Action, Narrative};
use crate::ui::Palette;

#[allow(clippy::too_many_arguments)]
fn frame(
    image: &str,
    index: u64,
    total: u64,
    instruction: &str,
    args: &str,
    state: &str,
    elapsed_ms: u64,
    instruction_width: u64,
) -> Value {
    json!({
        "event": "step",
        "image": image,
        "index": index,
        "total": total,
        "instruction": instruction,
        "args": args,
        "state": state,
        "elapsed_ms": elapsed_ms,
        "instruction_width": instruction_width,
    })
}

fn finished_frame() -> Value {
    json!({
        "event": "finished",
        "kind": "image",
        "name": "api",
        "duration_ms": 2500,
    })
}

fn settled_rows(actions: &[Action]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Settle(row) => Some(row.clone()),
            _ => None,
        })
        .collect()
}

fn begins(actions: &[Action]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Begin(phase) => Some(phase.started_row(Palette::new(false))),
            _ => None,
        })
        .collect()
}

fn end_row(actions: &[Action]) -> Option<&str> {
    actions.iter().find_map(|action| match action {
        Action::Print(row) => Some(row.as_str()),
        _ => None,
    })
}

#[test]
fn out_of_order_completions_release_once_per_index_then_the_end_row() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    let mut actions = Vec::new();
    let mut start_rows = Vec::new();
    for event in [
        frame(
            "api",
            1,
            2,
            "FROM",
            "python:3.14-trixie AS base",
            "started",
            0,
            4,
        ),
        frame(
            "api",
            2,
            2,
            "COPY",
            "--from=builder /app /app",
            "started",
            0,
            4,
        ),
    ] {
        let step = narrative.step(&event, plain);
        start_rows.extend(begins(&step));
        actions.extend(step);
    }
    for event in [
        frame(
            "api",
            2,
            2,
            "COPY",
            "--from=builder /app /app",
            "done",
            20,
            4,
        ),
        frame(
            "api",
            1,
            2,
            "FROM",
            "python:3.14-trixie AS base",
            "done",
            5,
            4,
        ),
    ] {
        actions.extend(narrative.step(&event, plain));
    }
    assert!(
        settled_rows(&actions).is_empty(),
        "a done frame must never settle a row directly: {actions:?}"
    );

    let tail = narrative.finish_node("image", "api", &finished_frame(), plain);
    let tail = tail.expect("the image settles");
    let rows = settled_rows(&tail);
    assert_eq!(rows.len(), 2, "one settled row per index: {rows:?}");
    assert!(rows[0].starts_with("  ✓ 1/2 FROM"), "{rows:?}");
    assert!(rows[1].starts_with("  ✓ 2/2 COPY"), "{rows:?}");
    let end = end_row(&tail).expect("the image end row");
    assert_eq!(
        Some(end),
        tail.last().and_then(|action| match action {
            Action::Print(row) => Some(row.as_str()),
            _ => None,
        }),
        "the image end row prints last"
    );
    assert!(
        end.starts_with("✓ image  api  built 2 steps"),
        "the build-end row replaces `✓ image … (N steps`: {end}"
    );
    for row in &rows {
        assert!(
            !row.contains('\u{1b}'),
            "plain output has no escapes: {row:?}"
        );
        assert!(
            !row.contains('['),
            "an inner row never carries brackets: {row:?}"
        );
    }
    assert!(
        !end.contains('\u{1b}'),
        "plain end row has no escapes: {end:?}"
    );
    // `Begin` only repaints inside the anchored driver; the append-only driver prints it once.
    assert_eq!(start_rows.len(), 2, "each start is announced exactly once");
    assert_eq!(
        start_rows.iter().filter(|row| row.contains("1/2")).count(),
        1
    );
    assert_eq!(
        start_rows.iter().filter(|row| row.contains("2/2")).count(),
        1
    );
}

#[test]
fn a_provisional_external_pull_row_is_superseded_by_the_real_instruction() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    let pre_pull = "--from=ghcr.io/astral-sh/uv:latest /uv /bin/";
    let real = "--from=ghcr.io/astral-sh/uv:latest /uv /uvx /bin/";
    let mut actions = Vec::new();

    // The external image pre-pull settles before instruction 1 is even announced.
    actions.extend(narrative.step(&frame("api", 2, 2, "COPY", pre_pull, "done", 1, 4), plain));
    actions.extend(narrative.step(
        &frame(
            "api",
            1,
            2,
            "FROM",
            "python:3.14-trixie AS base",
            "started",
            0,
            4,
        ),
        plain,
    ));
    actions.extend(narrative.step(
        &frame(
            "api",
            1,
            2,
            "FROM",
            "python:3.14-trixie AS base",
            "done",
            2,
            4,
        ),
        plain,
    ));
    // The real COPY re-announces index 2: the provisional row must be dropped, not printed.
    actions.extend(narrative.step(&frame("api", 2, 2, "COPY", real, "started", 0, 4), plain));
    actions.extend(narrative.step(&frame("api", 2, 2, "COPY", real, "done", 26, 4), plain));
    let tail = narrative.finish_node("image", "api", &finished_frame(), plain);
    let tail = tail.expect("the image settles");
    let mut rows = settled_rows(&actions);
    rows.extend(settled_rows(&tail));

    assert_eq!(rows.len(), 2, "rows 1 and 2, once each: {rows:?}");
    assert!(rows[0].starts_with("  ✓ 1/2 FROM"), "{rows:?}");
    let uri_rows: Vec<&String> = rows.iter().filter(|row| row.contains("2/2")).collect();
    assert_eq!(uri_rows.len(), 1, "one row for index 2: {rows:?}");
    assert!(uri_rows[0].contains(real), "{uri_rows:?}");
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("ghcr.io") && !row.contains("uvx")),
        "the provisional pre-pull row never prints: {rows:?}"
    );
    assert!(
        rows.iter().all(|row| !row.contains('\u{1b}')),
        "plain output has no escapes"
    );
    let end = end_row(&tail).expect("the image end row");
    assert!(
        end.starts_with("✓ image  api  built 2 steps"),
        "the build-end row counts the settled instructions: {end}"
    );
}

#[test]
fn a_failed_image_drops_every_buffered_row_and_tally() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    narrative.step(
        &frame(
            "api",
            1,
            2,
            "FROM",
            "python:3.14-trixie AS base",
            "done",
            5,
            4,
        ),
        plain,
    );
    let verdict = narrative.failed(
        &json!({
            "event": "failed",
            "phase": "run",
            "target": "api",
            "exit_code": 1,
            "next": "slipfork logs -r 5be421",
        }),
        80,
        plain,
        true,
    );
    assert!(narrative.images.is_empty(), "no tally survives a verdict");
    let tail = narrative.finish_node("image", "api", &finished_frame(), plain);
    assert!(
        tail.as_ref()
            .is_none_or(|actions| settled_rows(actions).is_empty()),
        "no settled row prints after a verdict: {verdict:?} {tail:?}"
    );
}

#[test]
fn a_buffered_hole_still_releases_ascending_with_the_instruction_total() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    narrative.step(
        &frame("api", 1, 3, "FROM", "python:3.14-trixie", "done", 5, 4),
        plain,
    );
    narrative.step(
        &frame("api", 3, 3, "RUN", "echo last", "done", 20, 3),
        plain,
    );
    let tail = narrative.finish_node("image", "api", &finished_frame(), plain);
    let tail = tail.expect("the image settles");
    let rows = settled_rows(&tail);
    assert_eq!(rows.len(), 2, "the hole never blocks later rows: {rows:?}");
    assert!(rows[0].starts_with("  ✓ 1/3 FROM"), "{rows:?}");
    assert!(rows[1].starts_with("  ✓ 3/3 RUN"), "{rows:?}");
    let end = end_row(&tail).expect("the image end row");
    assert!(
        end.starts_with("✓ image  api  built 3 steps"),
        "the build-end row sums the instruction total: {end}"
    );
}

#[test]
fn image_first_finish_releases_buffered_rows_with_an_in_flight_step() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    narrative.step(
        &frame(
            "api",
            2,
            3,
            "COPY",
            "--from=builder /app /app",
            "started",
            0,
            4,
        ),
        plain,
    );
    narrative.step(
        &frame("api", 1, 3, "FROM", "python:3.14-trixie", "done", 5, 4),
        plain,
    );
    let actions = narrative
        .finish_node("image", "api", &finished_frame(), plain)
        .expect("the image settles before the boot fallback");
    let rows = settled_rows(&actions);
    assert_eq!(rows.len(), 1, "the buffered row 1 is released: {rows:?}");
    assert!(rows[0].starts_with("  ✓ 1/3 FROM"), "{rows:?}");
    assert!(
        actions
            .last()
            .is_some_and(|action| matches!(action, Action::Print(_))),
        "the end row is last: {actions:?}"
    );
    let end = end_row(&actions).expect("the image end row");
    assert!(
        end.starts_with("✓ image  api  built 3 steps"),
        "the build-end row follows the released rows: {end}"
    );
}

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
    assert!(
        narrative
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
            .is_none(),
        "cached image falls through to the single outer completed_row with zero inner rows"
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
    assert_eq!(
        begins(&begun),
        ["→ [3/3] start  web  running  sh -c 'exit 7'"]
    );
    narrative.log(
        &json!({
            "event": "log",
            "kind": "service",
            "name": "web",
            "stream": "stderr",
            "line": "bind failed",
        }),
        80,
        plain,
        true,
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
        80,
        plain,
        true,
    );
    let held: Vec<&str> = failed
        .iter()
        .filter_map(|action| match action {
            Action::Hold(row) => Some(row.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(held, ["✗ [3/3] start  web  failed", "  ┃ bind failed"]);
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
    let begins: Vec<&str> = begun
        .iter()
        .filter_map(|action| match action {
            Action::Begin(phase) => Some(phase.target.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(begins, ["web"], "the service registers one live phase");
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
        "  ✓ 1/5 FROM     python:3.14-trixie AS base  1ms",
        "  ✓ 2/5 COPY     . /app  2ms",
        "  ✓ 3/5 ADD      x /y  3ms",
        "  ✓ 4/5 RUN      echo hi  4ms",
        "  ✓ 5/5 WORKDIR  /code  46ms",
    ];
    let expected_styled = [
        "  \u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m1/5\u{1b}[0m \u{1b}[35mFROM\u{1b}[0m     python:3.14-trixie AS base  \u{1b}[38;5;244m1ms\u{1b}[0m",
        "  \u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m2/5\u{1b}[0m \u{1b}[35mCOPY\u{1b}[0m     . /app  \u{1b}[38;5;244m2ms\u{1b}[0m",
        "  \u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m3/5\u{1b}[0m \u{1b}[35mADD\u{1b}[0m      x /y  \u{1b}[38;5;244m3ms\u{1b}[0m",
        "  \u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m4/5\u{1b}[0m \u{1b}[35mRUN\u{1b}[0m      echo hi  \u{1b}[38;5;244m4ms\u{1b}[0m",
        "  \u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m5/5\u{1b}[0m \u{1b}[35mWORKDIR\u{1b}[0m  /code  \u{1b}[38;5;244m46ms\u{1b}[0m",
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
            .finish_node("image", "api", &finished_frame(), palette)
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
            assert!(row.contains(instruction), "the keyword survives: {row:?}");

            let column = row.find(args).expect("args are present");
            if let Some(previous) = previous {
                assert_eq!(column, previous, "args start at one column: {row:?}");
            }
            previous = Some(column);
        }
    }
}
