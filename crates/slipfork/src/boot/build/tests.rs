use serde_json::json;

use super::super::layout::{pad, truncate_visible};
use super::{
    Action, Columns, LiveKey, LiveSteps, Narrative, Phase, gutter_line, image_done_row, wrap_gutter,
};
use crate::ui::{Palette, Verdict};

fn phase(columns: Columns) -> Phase {
    Phase::new(
        LiveKey::Boot {
            kind: "image".to_owned(),
            name: "api".to_owned(),
        },
        Some((2, 4)),
        "RUN".to_owned(),
        "api".to_owned(),
        "sh -c 'echo hi'".to_owned(),
        columns,
    )
}

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
fn the_build_end_row_counts_steps_and_sums_time() {
    let row = image_done_row("api", 4, 12300, Palette::new(false));
    assert_eq!(row, "✓ image api (4 steps, 12.3s)");
    assert_eq!(
        image_done_row("api", 1, 80, Palette::new(false)),
        "✓ image api (1 step, 80ms)"
    );
    assert!(!row.contains('\u{1b}'), "plain rows stay colourless");
}

#[test]
fn styled_rows_use_only_the_contract_sgr_codes() {
    let row = phase(Columns::default()).failed_row(Palette::new(true));
    assert!(row.contains("\u{1b}[31m✗"), "{row:?}");
    let live = phase(Columns::default()).live_row('⠹', 4800, 80, Palette::new(true));
    for code in ["\u{1b}[1m", "\u{1b}[2m"] {
        assert!(live.contains(code), "{live:?}");
    }
    assert!(!live.contains("\u{1b}[9"), "no bright codes: {live:?}");
}

#[test]
fn the_anchor_is_dim_and_names_the_instruction_and_target() {
    let row = phase(Columns::default()).anchor_row(Palette::new(false));
    assert_eq!(row, "→ [2/4] RUN api");
    let row = phase(Columns::default()).anchor_row(Palette::new(true));
    assert!(row.starts_with("\u{1b}[2m→ [2/4] RUN api"), "{row:?}");
}

#[test]
fn the_v3_role_table_pins_every_wire_byte() {
    let styled = Palette::new(true);
    let header = styled.header_line(
        "5be421",
        "RUN",
        "api",
        "sh -c 'echo hi'",
        Some("2026-11-24 10:59:40"),
    );
    assert_eq!(
        header,
        "\u{1b}[1m#5be421\u{1b}[0m  \u{1b}[2mRUN\u{1b}[0m  \u{1b}[1mapi\u{1b}[0m  sh -c 'echo hi'  \u{1b}[2m2026-11-24 10:59:40\u{1b}[0m"
    );
    let phase_row = phase(Columns::default()).started_row(styled);
    assert_eq!(
        phase_row,
        "→ \u{1b}[2m[2/4]\u{1b}[0m \u{1b}[2mRUN\u{1b}[0m \u{1b}[1mapi\u{1b}[0m  sh -c 'echo hi'"
    );
    let settled = phase(Columns::default()).settled_row(1200, styled);
    assert_eq!(
        settled,
        "\u{1b}[32m✓\u{1b}[0m \u{1b}[2m[2/4]\u{1b}[0m \u{1b}[2mRUN\u{1b}[0m \u{1b}[1mapi\u{1b}[0m  sh -c 'echo hi'  \u{1b}[2m1.2s\u{1b}[0m"
    );
    let build_end = image_done_row("api", 4, 12300, styled);
    assert_eq!(
        build_end,
        "\u{1b}[32m✓\u{1b}[0m \u{1b}[2mimage\u{1b}[0m \u{1b}[1mapi\u{1b}[0m \u{1b}[2m(4 steps, 12.3s)\u{1b}[0m"
    );
    let verdict = styled.verdict(&Verdict {
        verb: "RUN".to_owned(),
        target: "api".to_owned(),
        state: "failed".to_owned(),
        exit_code: Some(3),
        duration_ms: None,
    });
    assert_eq!(
        verdict,
        "\u{1b}[31m✗\u{1b}[0m \u{1b}[2mRUN\u{1b}[0m \u{1b}[1mapi\u{1b}[0m \u{1b}[31mfailed\u{1b}[0m \u{1b}[2m(exit 3)\u{1b}[0m"
    );
    let done_verdict = styled.verdict(&Verdict {
        verb: "RUN".to_owned(),
        target: "api".to_owned(),
        state: "done".to_owned(),
        exit_code: Some(0),
        duration_ms: Some(1200),
    });
    assert_eq!(
        done_verdict,
        "\u{1b}[32m✓\u{1b}[0m \u{1b}[2mRUN\u{1b}[0m \u{1b}[1mapi\u{1b}[0m done \u{1b}[2m(1.2s)\u{1b}[0m"
    );
    let next = format!(
        "  {} {}",
        styled.dim("next:"),
        styled.tint("slipfork logs -r 5be421", "36")
    );
    assert_eq!(
        next,
        "  \u{1b}[2mnext:\u{1b}[0m \u{1b}[36mslipfork logs -r 5be421\u{1b}[0m"
    );

    let plain = Palette::new(false);
    let rows = [
        plain.header_line(
            "5be421",
            "RUN",
            "api",
            "sh -c 'echo hi'",
            Some("2026-11-24 10:59:40"),
        ),
        phase(Columns::default()).started_row(plain),
        phase(Columns::default()).settled_row(1200, plain),
        plain.verdict(&Verdict {
            verb: "RUN".to_owned(),
            target: "api".to_owned(),
            state: "failed".to_owned(),
            exit_code: Some(3),
            duration_ms: None,
        }),
        plain.verdict(&Verdict {
            verb: "RUN".to_owned(),
            target: "api".to_owned(),
            state: "done".to_owned(),
            exit_code: Some(0),
            duration_ms: None,
        }),
        format!(
            "  {} {}",
            plain.dim("next:"),
            plain.tint("slipfork logs -r 5be421", "36")
        ),
    ];
    assert_eq!(
        rows,
        [
            "#5be421  RUN  api  sh -c 'echo hi'  2026-11-24 10:59:40",
            "→ [2/4] RUN api  sh -c 'echo hi'",
            "✓ [2/4] RUN api  sh -c 'echo hi'  1.2s",
            "✗ RUN api failed (exit 3)",
            "✓ RUN api done",
            "  next: slipfork logs -r 5be421",
        ]
    );
    assert!(rows.iter().all(|row| !row.contains("\u{1b}[")), "{rows:?}");
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
fn identical_target_and_args_never_repeat_on_phase_settled_header_or_verdict() {
    let mut columns = Columns::default();
    columns.observe("WORKDIR", "uv sync");
    columns.observe("setup", "longer-target");
    let setup = Phase::new(
        LiveKey::Boot {
            kind: "setup".to_owned(),
            name: "uv sync".to_owned(),
        },
        Some((2, 3)),
        "setup".to_owned(),
        "uv sync".to_owned(),
        "uv sync".to_owned(),
        columns,
    );
    let plain = Palette::new(false);
    assert_eq!(setup.started_row(plain), "→ [2/3] setup   uv sync");
    assert_eq!(setup.failed_row(plain), "✗ [2/3] setup   uv sync");
    assert_eq!(
        setup.live_row('⠹', 445, 80, plain),
        "⠹ [2/3] setup   uv sync        445ms"
    );
    assert_eq!(
        setup.settled_row(445, plain),
        "✓ [2/3] setup   uv sync        445ms"
    );
    assert_eq!(
        plain.header_line(
            "5be421",
            "setup",
            "uv sync",
            "uv sync",
            Some("2026-11-24 10:59:40"),
        ),
        "#5be421  setup  uv sync  2026-11-24 10:59:40"
    );
    assert_eq!(
        plain.verdict(&Verdict {
            verb: "uv sync".to_owned(),
            target: "uv sync".to_owned(),
            state: "failed".to_owned(),
            exit_code: Some(2),
            duration_ms: None,
        }),
        "✗ uv sync failed (exit 2)"
    );
    assert_eq!(
        plain.verdict(&Verdict {
            verb: "uv sync".to_owned(),
            target: "uv sync".to_owned(),
            state: "done".to_owned(),
            exit_code: Some(0),
            duration_ms: Some(445),
        }),
        "✓ uv sync done (445ms)"
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
