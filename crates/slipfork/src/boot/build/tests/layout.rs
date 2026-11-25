//! Layout behavior: column padding, ANSI truncation, block order, and failure expansion.

use serde_json::json;

use super::super::super::layout::truncate_visible;
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
fn block_rows_stack_every_phase_and_show_only_the_most_recent_tail() {
    let first = phase(Columns::default());
    let first_key = first.key.clone();
    let second = Phase::new(
        LiveKey::Boot {
            kind: "service".to_owned(),
            name: "worker".to_owned(),
        },
        Some((3, 3)),
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
    assert!(rows[0].starts_with("⠹ [2/4] RUN  api  "), "{rows:?}");
    assert!(rows[1].starts_with("⠹ [3/3] start  worker  "), "{rows:?}");
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
    assert!(rows[0].starts_with("✗ [2/4] RUN  api"), "{rows:?}");
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
fn a_degenerate_width_keeps_the_elapsed_cell_and_drops_the_args() {
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
    assert!(row.contains("1.2s"), "the elapsed cell survives: {row:?}");
    assert!(
        !row.contains("sh -c") && !row.ends_with('…'),
        "the args are dropped, never the progress cell: {row:?}"
    );
}

#[test]
fn twenty_line_failing_step_caps_live_region_at_eight_and_expands_all_twenty_to_scrollback() {
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
    let start_actions = narrative.step(
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
    assert!(
        matches!(&start_actions[0], Action::Print(row) if row == "→ [1/2] image  api  running"),
        "the first step opens the outer parent row once: {start_actions:?}"
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

    // While running, the region paint caps the visible tail at 8 lines (1 inner phase + 8 gutter).
    let live_paint = narrative
        .steps()
        .lock()
        .expect("live steps lock")
        .block_rows('⠹', 80, plain);
    assert_eq!(live_paint.len(), 9, "{live_paint:?}");
    assert!(live_paint[0].starts_with("  ⠹ 1/2 run  sh -c 'burst'"));
    assert!(!live_paint[0].contains('['), "{live_paint:?}");
    assert!(!live_paint.iter().any(|row| row.contains("line-12")));
    assert!(live_paint.iter().any(|row| row.contains("    │ line-13")));
    assert!(live_paint.iter().any(|row| row.contains("    ┃ line-20")));

    // On failure, all 20 captured lines expand into scrollback at indent 4 after `  ✗ 1/2 run …`.
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
        "1 failed inner phase row + 20 gutter lines: {held:?}"
    );
    assert_eq!(held[0], "  ✗ 1/2 run  sh -c 'burst'");
    for (number, row) in held.iter().enumerate().skip(1) {
        let glyph = if number % 2 == 0 { '┃' } else { '│' };
        assert_eq!(row, &format!("    {glyph} line-{number:02}"));
    }
    let diagnosis: Vec<&str> = failed_actions
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
fn append_only_tail_prints_one_line_per_phase_and_none_when_settled_or_anchored() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    narrative.step(
        &json!({
            "event": "step",
            "image": "api",
            "index": 1,
            "total": 2,
            "instruction": "RUN",
            "args": "sh -c 'slow'",
            "state": "started",
            "elapsed_ms": 0,
        }),
        plain,
    );
    let tail = |phase: &str| {
        json!({
            "event": "step",
            "image": "api",
            "index": 1,
            "total": 2,
            "instruction": "RUN",
            "args": "sh -c 'slow'",
            "state": "tail",
            "phase": phase,
            "elapsed_ms": 3100,
        })
    };
    let printed = |actions: &[Action]| -> Vec<String> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Print(row) => Some(row.to_owned()),
                _ => None,
            })
            .collect()
    };

    for phase in ["sealing", "recording", "publishing"] {
        let rows = printed(&narrative.step(&tail(phase), plain));
        assert_eq!(rows.len(), 1, "one line per transition: {rows:?}");
        assert_eq!(
            rows[0],
            format!("    · 1/2 run  3.1s · {phase}"),
            "the append-only tail line names the phase"
        );
        assert!(
            printed(&narrative.step(&tail(phase), plain)).is_empty(),
            "a repeated label is not a new transition, so it never prints twice"
        );
    }
    assert!(
        printed(&narrative.step(&tail("executing"), plain)).is_empty(),
        "execution itself never gets a tail line"
    );
    assert!(
        printed(&narrative.step(&tail("recording"), Palette::new(true))).is_empty(),
        "an anchored pane repaints the row in place instead of printing"
    );
    // `--json` is gated in `BootTui::observe` before the narrative ever sees the frame, so it
    // cannot produce this line either.
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
        "  \u{1b}[38;5;244msealed sha256:deadbeef: reusing cache\u{1b}[0m"
    );
    assert!(!row.contains('│') && !row.contains('┃'), "{row:?}");
}
