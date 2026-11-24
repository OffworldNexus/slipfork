//! Layout behavior: column padding, ANSI truncation, block order, and failure expansion.

use serde_json::json;

use super::super::super::layout::{pad, truncate_visible};
use super::super::{
    Action, Columns, LiveKey, LiveSteps, Narrative, Phase, gutter_line, wrap_gutter,
};
use super::phase;
use crate::ui::Palette;

#[test]
fn instruction_and_target_columns_pad_to_the_image_wide_columns() {
    let mut columns = Columns::default();
    columns.observe("COPY", "api");
    let row = phase(columns).settled_row(1200, Palette::new(false));
    assert!(row.starts_with("✓ [2/4] RUN  api  sh -c"), "{row}");
    assert!(row.ends_with("  1.2s"), "{row}");
    assert_eq!(pad("RUN", 4), "RUN ");
    assert_eq!(pad("FROM", 4), "FROM");
}

#[test]
fn the_live_row_uses_settled_double_space_separators_and_truncates_args() {
    let row = phase(Columns::default()).live_row('⠹', 4800, 30, Palette::new(false));
    assert_eq!(row, "⠹ [2/4] RUN api  sh -c …  4.8s");
    assert!(row.chars().count() <= 30, "{row}");

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
fn gutter_lines_truncate_and_wrap_with_the_same_glyph() {
    let palette = Palette::new(false);
    let row = gutter_line("stdout", "0123456789", 10, palette);
    assert_eq!(row, "  │ 01234…");
    let row = gutter_line("stderr", "abc", 10, palette);
    assert_eq!(row, "  ┃ abc");
    let rows = wrap_gutter("stdout", "abcdefghijklmnopqrstuvwxyz", 14, palette);
    assert_eq!(rows.len(), 3, "{rows:?}");
    for row in &rows {
        assert!(row.starts_with("  │ "), "{row}");
        assert!(row.chars().count() <= 14, "{row}");
    }
}

#[test]
fn block_rows_stack_every_phase_and_show_only_the_most_recent_tail() {
    let first = phase(Columns::default());
    let first_key = first.key.clone();
    let second = Phase::new(
        LiveKey::Boot {
            kind: "service".to_owned(),
            name: "worker".to_owned(),
        },
        None,
        "start".to_owned(),
        "worker".to_owned(),
        "sleep 3600".to_owned(),
        Columns::default(),
    );
    let second_key = second.key.clone();
    let mut steps = LiveSteps::new();
    steps.begin(first);
    steps.append_output(&first_key, "stdout".to_owned(), "old-line".to_owned());
    steps.begin(second);
    steps.append_output(&second_key, "stdout".to_owned(), "fresh-1".to_owned());
    steps.append_output(&second_key, "stderr".to_owned(), "fresh-2".to_owned());

    let rows = steps.block_rows('⠹', 80, Palette::new(false));
    assert!(rows[0].starts_with("⠹ [2/4] RUN api  "), "{rows:?}");
    assert!(rows[1].starts_with("⠹ start worker  "), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("│ fresh-1")), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("┃ fresh-2")), "{rows:?}");
    assert!(
        !rows.iter().any(|row| row.contains("old-line")),
        "only the most recently active step shows a tail: {rows:?}"
    );
}

#[test]
fn a_failed_step_expands_both_streams_in_arrival_order() {
    let plain = Palette::new(false);
    let mut steps = LiveSteps::new();
    let phase = phase(Columns::default());
    let key = phase.key.clone();
    steps.begin(phase);
    for (stream, line) in [
        ("stdout", "installing blastit"),
        ("stderr", "E: Unable to locate package blastit"),
        ("stdout", "cleanup"),
    ] {
        assert!(
            steps
                .append_output(&key, stream.to_owned(), line.to_owned())
                .is_some(),
            "the live step records every line"
        );
    }
    let live = steps.take(&key).expect("the failed step is still live");
    let rows = live.expansion_rows(80, plain);
    assert!(rows[0].starts_with("✗ [2/4] RUN api"), "{rows:?}");
    let lines: Vec<&str> = rows[1..].iter().map(String::as_str).collect();
    assert_eq!(
        lines,
        [
            "  │ installing blastit",
            "  ┃ E: Unable to locate package blastit",
            "  │ cleanup",
        ]
    );
}

#[test]
fn a_live_row_is_ansi_truncated_at_the_width_with_an_ellipsis() {
    let phase = Phase::new(
        LiveKey::Step {
            image: "api".to_owned(),
            index: 3,
        },
        Some((3, 5)),
        "RUN".to_owned(),
        "api".to_owned(),
        "sh -c 'sleep 1; echo done'".to_owned(),
        Columns::default(),
    );
    let row = phase.live_row('⠋', 1200, 12, Palette::new(true));
    assert!(
        console::measure_text_width(&row) <= 12,
        "the live row never exceeds the terminal width: {row:?}"
    );
    assert!(
        row.contains('…'),
        "a clipped live row ends in an ellipsis: {row:?}"
    );
}

#[test]
fn twenty_line_failing_step_caps_live_region_at_eight_and_expands_all_twenty_to_scrollback() {
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
    for number in 1..=20 {
        let stream = if number % 2 == 0 { "stderr" } else { "stdout" };
        narrative.log(
            &json!({
                "event": "log",
                "image": "api",
                "index": 1,
                "stream": stream,
                "line": format!("line-{number:02}"),
            }),
            80,
            plain,
            true,
        );
    }

    // While running, the region paint caps the visible tail at 8 lines (1 phase row + 8 gutter).
    let live_paint = narrative
        .steps()
        .lock()
        .expect("live steps lock")
        .block_rows('⠹', 80, plain);
    assert_eq!(live_paint.len(), 9, "{live_paint:?}");
    assert!(!live_paint.iter().any(|row| row.contains("line-12")));
    assert!(live_paint.iter().any(|row| row.contains("line-13")));
    assert!(live_paint.iter().any(|row| row.contains("line-20")));

    // On failure, all 20 captured lines expand into scrollback after the `✗` phase row.
    let failed_actions = narrative.failed(
        &json!({
            "event": "failed",
            "phase": "run",
            "target": "api",
            "exit_code": 1,
            "run_id": "5be421",
            "next": "slipfork logs -r 5be421",
        }),
        80,
        plain,
        true,
    );
    let held: Vec<String> = failed_actions
        .iter()
        .filter_map(|action| match action {
            Action::Hold(row) => Some(row.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        held.len(),
        21,
        "1 failed phase row + 20 gutter lines: {held:?}"
    );
    assert_eq!(held[0], "✗ [1/2] RUN api  sh -c 'burst'");
    for (number, row) in held.iter().enumerate().skip(1) {
        let glyph = if number % 2 == 0 { '┃' } else { '│' };
        assert_eq!(row, &format!("  {glyph} line-{number:02}"));
    }

    // Append-only (`!anchored_pane`) already streamed every line live, so `failed` never repeats it.
    let mut append = Narrative::new(false);
    append.step(
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
    let mut streamed = 0;
    for number in 1..=20 {
        streamed += append
            .log(
                &json!({
                    "event": "log",
                    "image": "api",
                    "index": 1,
                    "stream": "stdout",
                    "line": format!("line-{number:02}"),
                }),
                80,
                plain,
                false,
            )
            .len();
    }
    assert_eq!(streamed, 20);
    let append_failed = append.failed(
        &json!({
            "event": "failed",
            "phase": "run",
            "target": "api",
            "exit_code": 1,
            "run_id": "5be421",
            "next": "slipfork logs -r 5be421",
        }),
        80,
        plain,
        false,
    );
    assert!(
        !append_failed
            .iter()
            .any(|action| matches!(action, Action::Hold(_))),
        "append-only failure must not repeat already-printed lines: {append_failed:?}"
    );
}

#[test]
fn engine_notes_stay_hidden_by_default_and_dim_under_verbose_without_a_gutter() {
    let frame = json!({
        "event": "note",
        "text": "sealed sha256:deadbeef: reusing cache",
    });
    assert!(
        Narrative::new(false)
            .note(&frame, Palette::new(true))
            .is_empty(),
        "default runs hide engine bookkeeping"
    );
    let verbose = Narrative::new(true).note(&frame, Palette::new(true));
    assert_eq!(verbose.len(), 1);
    let Action::Print(row) = &verbose[0] else {
        panic!("expected Action::Print, got {verbose:?}");
    };
    assert_eq!(
        row,
        "  \u{1b}[2msealed sha256:deadbeef: reusing cache\u{1b}[0m"
    );
    assert!(!row.contains('│') && !row.contains('┃'), "{row:?}");
}
