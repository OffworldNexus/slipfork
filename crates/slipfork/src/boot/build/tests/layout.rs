//! Layout behavior: column padding, ANSI truncation, append-only streaming, and failure output.

use serde_json::json;

use super::super::super::layout::truncate_visible;
use super::super::{Action, Columns, LiveKey, Narrative, Phase, gutter_line, wrap_gutter};
use super::phase;
use crate::ui::report::Palette;

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
fn twenty_streamed_lines_survive_and_the_failure_never_repeats_them() {
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
    let mut streamed = 0;
    let mut headers = 0;
    for number in 1..=20 {
        for action in narrative.log(
            &json!({
                "event": "log",
                "image": "api",
                "index": 1,
                "stream": "stdout",
                "line": format!("line-{number:02}"),
            }),
            80,
            plain,
        ) {
            match action {
                Action::Output { .. } => streamed += 1,
                Action::Print(_) => headers += 1,
                _ => {}
            }
        }
    }
    assert_eq!(streamed, 20, "all twenty lines stream live");
    assert_eq!(headers, 1, "the step prints one header, never one per line");

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
        ["  ✗ 1/2 run  sh -c 'burst'"],
        "the append-only failure keeps one `✗` row"
    );
    assert!(
        !failed
            .iter()
            .any(|action| matches!(action, Action::Output { .. })),
        "the twenty already-streamed lines are not replayed"
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

#[test]
fn no_color_journals_emit_zero_escapes_and_zero_carriage_returns() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    let mut actions = Vec::new();
    actions.extend(narrative.started(
        &json!({
            "event": "started",
            "step": 2,
            "total": 2,
            "kind": "service",
            "name": "web",
            "command": "python3 -m http.server 8195",
        }),
        Some((2, 2)),
    ));
    actions.extend(narrative.step(
        &json!({
            "event": "step",
            "image": "api",
            "index": 1,
            "total": 1,
            "instruction": "RUN",
            "args": "echo hi",
            "state": "started",
            "elapsed_ms": 0,
        }),
        plain,
    ));
    actions.extend(narrative.step(
        &json!({
            "event": "step",
            "image": "api",
            "index": 1,
            "total": 1,
            "instruction": "RUN",
            "args": "echo hi",
            "state": "done",
            "elapsed_ms": 4,
        }),
        plain,
    ));
    actions.extend(narrative.log(
        &json!({
            "event": "log",
            "image": "api",
            "index": 1,
            "stream": "stdout",
            "line": "hi",
        }),
        80,
        plain,
    ));
    actions.extend(narrative.finished(
        &json!({
            "event": "finished",
            "step": 2,
            "total": 2,
            "kind": "service",
            "name": "web",
            "state": "ready",
            "duration_ms": 1200,
        }),
        plain,
    ));
    actions.extend(narrative.failed(
        &json!({
            "event": "failed",
            "phase": "run",
            "target": "web",
            "exit_code": 1,
            "next": "slipfork logs -r 5be421",
        }),
        plain,
    ));

    let mut texts = Vec::new();
    for action in &actions {
        match action {
            Action::Output {
                text,
                stream,
                indent,
            } => {
                texts.push(gutter_line(stream, text, 80, plain, *indent));
            }
            Action::Settle(row) | Action::Print(row) | Action::Diagnose(row) => {
                texts.push(row.clone());
            }
        }
    }
    for text in &texts {
        assert!(
            !text.contains('\u{1b}'),
            "NO_COLOR/pipe output is escape-free: {text:?}"
        );
        assert!(
            !text.contains('\r'),
            "NO_COLOR/pipe output is carriage-return-free: {text:?}"
        );
    }
}

#[test]
fn a_coloured_child_line_wraps_at_visible_width_without_splitting_the_sequence() {
    let plain = Palette::new(false);
    let line = "\u{1b}[31mabcdefghij\u{1b}[0m";
    // width 12 minus the 2-space indent and 2-cell gutter leaves the 8-cell minimum budget.
    let rows = wrap_gutter("stdout", line, 12, plain, 2);
    assert_eq!(rows.len(), 2, "{rows:?}");
    let visible: Vec<String> = rows
        .iter()
        .map(|row| crate::ui::strip(row).into_owned())
        .collect();
    assert_eq!(visible, vec!["  │ abcdefgh", "  │ ij"]);
    for row in &rows {
        assert!(
            crate::ui::visible_width(row) <= 12,
            "a coloured row must wrap at its visible width: {row:?}"
        );
        assert!(
            !crate::ui::strip(row).contains('\u{1b}'),
            "no sequence fragment survives the wrap: {row:?}"
        );
    }
    // The reset closes the row, so colour never bleeds onto the next one.
    assert!(rows[0].ends_with("\u{1b}[0m"), "{:?}", rows[0]);

    let clipped = gutter_line("stdout", line, 8, plain, 2);
    assert_eq!(crate::ui::strip(&clipped), "  │ abc…");
    assert!(crate::ui::visible_width(&clipped) <= 8, "{clipped:?}");
    assert!(!crate::ui::strip(&clipped).contains('\u{1b}'));
}
