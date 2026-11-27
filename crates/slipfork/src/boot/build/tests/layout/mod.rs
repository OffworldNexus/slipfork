//! Layout behavior: column padding and the image-wide instruction column.

mod streaming;

use super::super::super::layout::truncate_visible;
use super::super::{Action, Columns, Narrative, gutter_line, wrap_gutter};
use super::phase;
use crate::ui::report::Palette;

use serde_json::json;

#[test]
fn instruction_and_target_columns_pad_to_the_image_wide_columns() {
    let mut columns = Columns::default();
    columns.observe("COPY", "api");
    let row = phase(columns).settled_row(1200, Palette::new(false));
    assert!(
        row.starts_with("✓ [2/4] RUN   api  running  sh -c"),
        "{row}"
    );
    assert!(row.ends_with("  1.2s"), "{row}");
}

#[test]
fn the_live_row_uses_settled_double_space_separators_and_truncates_args() {
    let row = phase(Columns::default()).live_row('⠹', 4800, 30, Palette::new(false));
    assert!(
        row.ends_with("4.8s"),
        "the elapsed cell is never sheared: {row}"
    );
    assert!(!row.contains("sh -c"), "the args are what gets cut: {row}");
    assert!(console::measure_text_width(&row) <= 30, "{row}");

    let wide = phase(Columns::default()).live_row('⠹', 1200, 80, Palette::new(false));
    let settled = phase(Columns::default()).settled_row(1200, Palette::new(false));
    assert_eq!(
        wide.trim_start_matches('⠹'),
        settled.trim_start_matches('✓')
    );
    assert_eq!(truncate_visible("abcdef", 4), "abc…");
    assert_eq!(truncate_visible("abc", 4), "abc");
}

#[test]
fn gutter_lines_truncate_and_wrap_at_both_outer_and_inner_indents() {
    let palette = Palette::new(false);
    assert_eq!(
        gutter_line("stdout", "0123456789", 10, palette, 2),
        "  │ 01234…"
    );
    assert_eq!(gutter_line("stderr", "abc", 10, palette, 2), "  ┃ abc");
    assert_eq!(
        gutter_line("stdout", "fetching installer", 40, palette, 4),
        "    │ fetching installer"
    );
    let rows = wrap_gutter("stdout", "abcdefghijklmnopqrstuvwxyz", 14, palette, 2);
    assert_eq!(rows.len(), 3, "{rows:?}");
    for row in &rows {
        assert!(row.starts_with("  │ "), "{row}");
        assert!(row.chars().count() <= 14, "{row}");
    }
    let inner_rows = wrap_gutter("stderr", "0123456789abcdef", 14, palette, 4);
    assert_eq!(inner_rows, ["    ┃ 01234567", "    ┃ 89abcdef"]);
}

#[test]
fn every_log_frame_streams_as_an_output_action_in_arrival_order() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    narrative.step(
        &json!({
            "event": "step",
            "image": "api",
            "index": 1,
            "total": 2,
            "instruction": "RUN",
            "args": "sh -c 'burst'",
            "state": "started",
            "elapsed_ms": 0,
        }),
        plain,
    );
    let mut outputs = Vec::new();
    for (stream, line) in [
        ("stdout", "installing blastit"),
        ("stderr", "E: Unable to locate package blastit"),
        ("stdout", "cleanup"),
    ] {
        let actions = narrative.log(
            &json!({
                "event": "log",
                "image": "api",
                "index": 1,
                "stream": stream,
                "line": line,
            }),
            80,
            plain,
        );
        outputs.extend(actions.into_iter().filter_map(|action| match action {
            Action::Output {
                stream,
                text,
                indent,
            } => Some((stream, text, indent)),
            _ => None,
        }));
    }
    assert_eq!(
        outputs,
        [
            ("stdout".to_owned(), "installing blastit".to_owned(), 4),
            (
                "stderr".to_owned(),
                "E: Unable to locate package blastit".to_owned(),
                4
            ),
            ("stdout".to_owned(), "cleanup".to_owned(), 4),
        ],
        "every line is a live Output with the inner gutter indent"
    );
}

#[test]
fn interleaved_streams_print_one_header_per_live_step_ever() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    narrative.step(
        &json!({
            "event": "step",
            "image": "api",
            "index": 1,
            "total": 2,
            "instruction": "RUN",
            "args": "echo hi",
            "state": "started",
            "elapsed_ms": 0,
            "instruction_width": 4,
        }),
        plain,
    );
    narrative.started(
        &json!({
            "event": "started",
            "step": 2,
            "total": 2,
            "kind": "service",
            "name": "web",
            "command": "echo web",
        }),
        Some((2, 2)),
    );

    let mut printed = Vec::new();
    let mut outputs = 0_usize;
    for frame in [
        json!({ "event": "log", "image": "api", "index": 1, "stream": "stdout", "line": "one" }),
        json!({ "event": "log", "kind": "service", "name": "web", "stream": "stdout", "line": "two" }),
        json!({ "event": "log", "image": "api", "index": 1, "stream": "stdout", "line": "three" }),
        json!({ "event": "log", "kind": "service", "name": "web", "stream": "stderr", "line": "four" }),
    ] {
        for action in narrative.log(&frame, 80, plain) {
            match action {
                Action::Print(row) => printed.push(row),
                Action::Output { .. } => outputs += 1,
                _ => {}
            }
        }
    }

    assert_eq!(
        printed,
        [
            "  → 1/2 run   echo hi".to_owned(),
            "→ [2/2] start  web  running  echo web".to_owned(),
        ],
        "each interleaved key earns exactly one header"
    );
    assert_eq!(outputs, 4, "every line still streams as a gutter");
}

#[test]
fn a_failed_step_prints_one_verdict_row_and_never_repeats_streamed_lines() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    narrative.step(
        &json!({
            "event": "step",
            "image": "api",
            "index": 1,
            "total": 2,
            "instruction": "RUN",
            "args": "apt-get update",
            "state": "started",
            "elapsed_ms": 0,
        }),
        plain,
    );
    for (stream, line) in [
        ("stdout", "installing blastit"),
        ("stderr", "E: Unable to locate package blastit"),
        ("stdout", "cleanup"),
    ] {
        narrative.log(
            &json!({
                "event": "log",
                "image": "api",
                "index": 1,
                "stream": stream,
                "line": line,
            }),
            80,
            plain,
        );
    }
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
    let printed: Vec<&str> = failed
        .iter()
        .filter_map(|action| match action {
            Action::Print(row) => Some(row.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        printed,
        ["  ✗ 1/2 run  apt-get update"],
        "the verdict is one `✗` row, not a re-print of the stream"
    );
    assert!(
        !failed
            .iter()
            .any(|action| matches!(action, Action::Output { .. })),
        "already streamed output is never replayed: {failed:?}"
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
            "✗ RUN api failed (exit 1)",
            "  next: slipfork logs -r 5be421"
        ]
    );
}
