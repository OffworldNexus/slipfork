//! Instruction-order release: one settled row per index, in index order, image end row last.

mod frames;
mod services;

use serde_json::json;

use super::super::{Action, Narrative};
use crate::ui::report::Palette;

use self::frames::{
    end_row, finished_frame, finished_frame_steps, frame, provisional_frame, settled_rows,
};

/// A started instruction only opens the live registry: it never prints a `→` row. A completed
/// index releases immediately, in index order, the moment its predecessors are printed.
#[test]
fn out_of_order_completions_release_once_per_index_then_the_end_row() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    let from = "python:3.14-trixie AS base";
    let copy = "--from=builder /app /app";
    let mut announced = Vec::new();
    for event in [
        frame("api", 1, 2, "FROM", from, "started", 0, 4),
        frame("api", 2, 2, "COPY", copy, "started", 0, 4),
    ] {
        announced.extend(narrative.step(&event, plain));
    }
    assert!(
        announced.is_empty(),
        "a started instruction prints no `→` row: {announced:?}"
    );

    let late = narrative.step(&frame("api", 2, 2, "COPY", copy, "done", 20, 4), plain);
    assert!(
        settled_rows(&late).is_empty(),
        "index 2 waits for index 1: {late:?}"
    );

    let early = narrative.step(&frame("api", 1, 2, "FROM", from, "done", 5, 4), plain);
    let rows = settled_rows(&early);
    assert_eq!(
        rows.len(),
        2,
        "one settled row per index, at once: {rows:?}"
    );
    assert!(rows[0].starts_with("  ✓ 1/2 from"), "{rows:?}");
    assert!(rows[1].starts_with("  ✓ 2/2 copy"), "{rows:?}");

    let tail = narrative.finish_node("image", "api", &finished_frame(), plain);
    let tail = tail.expect("the image settles");
    assert!(
        settled_rows(&tail).is_empty(),
        "already released rows never repeat at the image end: {tail:?}"
    );
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
}

#[test]
fn a_provisional_external_pull_row_is_superseded_by_the_real_instruction() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    let from = "python:3.14-trixie AS base";
    let truncated = "--from=ghcr.io/astral-sh/uv:latest";
    let real = "--from=ghcr.io/astral-sh/uv:latest /uv /uvx /bin/";
    let mut actions = Vec::new();

    // Exactly what the daemon sends: the pre-pull opens and settles a provisional `started` and
    // `done` before instruction 1 is ever announced.
    actions.extend(narrative.step(
        &provisional_frame("api", 2, 2, "COPY", truncated, "started", 0, 4),
        plain,
    ));
    actions.extend(narrative.step(
        &provisional_frame("api", 2, 2, "COPY", truncated, "done", 6, 4),
        plain,
    ));
    assert!(
        settled_rows(&actions).is_empty(),
        "a provisional pre-pull never releases, even as the next index: {actions:?}"
    );

    actions.extend(narrative.step(&frame("api", 1, 2, "FROM", from, "started", 0, 4), plain));
    actions.extend(narrative.step(&frame("api", 1, 2, "FROM", from, "done", 2, 4), plain));
    let between = settled_rows(&actions);
    assert_eq!(
        between.len(),
        1,
        "only index 1 released; the provisional row still waits: {between:?}"
    );
    assert!(between[0].starts_with("  ✓ 1/2 from"), "{between:?}");

    // The real instruction at the same index supersedes the buffered provisional row.
    actions.extend(narrative.step(&frame("api", 2, 2, "COPY", real, "started", 0, 4), plain));
    actions.extend(narrative.step(&frame("api", 2, 2, "COPY", real, "done", 43, 4), plain));
    let tail = narrative.finish_node("image", "api", &finished_frame(), plain);
    let tail = tail.expect("the image settles");
    let mut rows = settled_rows(&actions);
    rows.extend(settled_rows(&tail));

    assert_eq!(rows.len(), 2, "rows 1 and 2, once each: {rows:?}");
    assert!(rows[0].starts_with("  ✓ 1/2 from"), "{rows:?}");
    let copy_rows: Vec<&String> = rows.iter().filter(|row| row.contains("2/2")).collect();
    assert_eq!(copy_rows.len(), 1, "one row for index 2: {rows:?}");
    assert!(copy_rows[0].contains(real), "{copy_rows:?}");
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("ghcr.io") && !row.contains("uvx")),
        "the truncated provisional pre-pull row never prints: {rows:?}"
    );
    assert!(
        rows.iter().all(|row| !row.contains('\u{1b}')),
        "plain output has no escapes"
    );
    let end = end_row(&tail).expect("the image end row");
    assert_eq!(
        tail.iter()
            .filter(|action| matches!(action, Action::Print(_)))
            .count(),
        1,
        "the image end row prints exactly once: {tail:?}"
    );
    assert!(
        end.starts_with("✓ image  api  built 2 steps"),
        "the build-end row counts the settled instructions: {end}"
    );
}

/// A provisional row at the very next index still never releases on its own: only the real
/// instruction at that index may print.
#[test]
fn a_provisional_row_never_releases_even_when_its_index_is_next() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    let truncated = "--from=ghcr.io/astral-sh/uv:latest";
    let real = "--from=ghcr.io/astral-sh/uv:latest /uv /uvx /bin/";
    let mut actions = Vec::new();
    for state in ["started", "done"] {
        actions.extend(narrative.step(
            &provisional_frame(
                "api",
                1,
                1,
                "COPY",
                truncated,
                state,
                if state == "done" { 6 } else { 0 },
                4,
            ),
            plain,
        ));
    }
    assert!(
        settled_rows(&actions).is_empty(),
        "index 1 is next, yet the provisional row must not release: {actions:?}"
    );

    actions.extend(narrative.step(&frame("api", 1, 1, "COPY", real, "started", 0, 4), plain));
    actions.extend(narrative.step(&frame("api", 1, 1, "COPY", real, "done", 43, 4), plain));
    let rows = settled_rows(&actions);
    assert_eq!(rows.len(), 1, "exactly the real instruction: {rows:?}");
    assert!(rows[0].contains(real), "{rows:?}");
    assert!(!rows[0].contains("uv:latest  "), "{rows:?}");
}

/// A FROM failure after a provisional pre-pull at index 2 still names the last *instruction* that
/// started: the provisional announcement latches the opener but never the `at N/M` position.
#[test]
fn a_from_failure_after_a_provisional_pre_pull_reports_the_last_started_index() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    let truncated = "--from=ghcr.io/astral-sh/uv:latest";
    for state in ["started", "done"] {
        narrative.step(
            &provisional_frame(
                "api",
                2,
                2,
                "COPY",
                truncated,
                state,
                if state == "done" { 6 } else { 0 },
                4,
            ),
            plain,
        );
    }
    for state in ["started", "done"] {
        narrative.step(
            &frame(
                "api",
                1,
                2,
                "FROM",
                "python:3.14-trixie",
                state,
                if state == "done" { 5 } else { 0 },
                4,
            ),
            plain,
        );
    }
    let tail = narrative
        .finish_node(
            "image",
            "api",
            &json!({
                "event": "finished",
                "kind": "image",
                "name": "api",
                "state": "failed",
                "executed": 0,
                "duration_ms": 9,
                "elapsed_ms": 9,
            }),
            plain,
        )
        .expect("the image settles");
    let end = end_row(&tail).expect("the image end row");
    assert!(
        end.contains("failed at 1/2"),
        "the FROM failure is at 1/2: {end}"
    );
    assert!(
        !end.contains("at 2/2"),
        "the provisional index never leaks: {end}"
    );
}

#[test]
fn a_failed_image_drops_every_buffered_row_and_tally() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    let held = narrative.step(
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
    assert!(settled_rows(&held).is_empty(), "{held:?}");
    let verdict = narrative.failed(
        &json!({
            "event": "failed",
            "phase": "run",
            "target": "api",
            "exit_code": 1,
            "next": "slipfork logs -r 5be421",
        }),
        plain,
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
    let tail = narrative.finish_node("image", "api", &finished_frame_steps(3), plain);
    let tail = tail.expect("the image settles");
    let rows = settled_rows(&tail);
    assert_eq!(rows.len(), 2, "the hole never blocks later rows: {rows:?}");
    assert!(rows[0].starts_with("  ✓ 1/3 from"), "{rows:?}");
    assert!(rows[1].starts_with("  ✓ 3/3 run"), "{rows:?}");
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
        .finish_node("image", "api", &finished_frame_steps(3), plain)
        .expect("the image settles before the boot fallback");
    let rows = settled_rows(&actions);
    assert_eq!(rows.len(), 1, "the buffered row 1 is released: {rows:?}");
    assert!(rows[0].starts_with("  ✓ 1/3 from"), "{rows:?}");
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
