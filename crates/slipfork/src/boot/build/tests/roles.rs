//! Byte-exact role pins: the golden SGR/`NO_COLOR` lines the two-level visual contract promises.

use std::num::NonZeroUsize;

use serde_json::json;

use super::super::{Columns, LiveKey, Narrative, Phase};
use super::phase;
use crate::boot::render::{OuterWidths, completed_row, outer_widths};
use crate::ui::report::{Counter, Done, Outcome, Row};
use crate::ui::report::{Palette, RunVerdict as Verdict};

/// One `built N steps` completion.
fn built(steps: usize) -> Outcome {
    Outcome::Ok(Done::Built {
        steps: NonZeroUsize::new(steps).expect("a built row always ran a step"),
    })
}

/// One image end row at `[index/total]` with no command.
fn image_row(index: usize, total: usize, outcome: Outcome, millis: u64) -> Row {
    Row::settled(
        Some(Counter { index, total }),
        "image",
        "api",
        outcome,
        Some(millis),
    )
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for ch in chars.by_ref() {
                if ('@'..='~').contains(&ch) {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[test]
fn the_build_end_row_carries_outer_slot_steps_and_time() {
    let widths = OuterWidths {
        kind: 5,
        target: 7,
        state: 0,
    };
    let row = image_row(1, 3, built(6), 12_300);
    assert_eq!(
        completed_row(&row, &widths, Palette::new(false)),
        "✓ [1/3] image  api      built 6 steps  12.3s"
    );
    assert_eq!(
        completed_row(&image_row(1, 3, built(1), 80), &widths, Palette::new(false)),
        "✓ [1/3] image  api      built 1 step  80ms"
    );
    let row = completed_row(&row, &widths, Palette::new(false));
    assert!(!row.contains('\u{1b}'), "plain rows stay colourless");
    assert_eq!(
        completed_row(
            &image_row(1, 3, built(6), 12_300),
            &widths,
            Palette::new(true)
        ),
        "\u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m[1/3]\u{1b}[0m \u{1b}[35mimage\u{1b}[0m  \u{1b}[1mapi\u{1b}[0m      \u{1b}[32mbuilt\u{1b}[0m 6 steps  \u{1b}[38;5;244m12.3s\u{1b}[0m"
    );
}

#[test]
fn a_failed_image_row_names_the_last_started_step_and_never_claims_built() {
    let widths = OuterWidths {
        kind: 5,
        target: 7,
        state: 0,
    };
    let failed = Outcome::Failed {
        at: Some(Counter { index: 2, total: 6 }),
    };
    let plain = completed_row(&image_row(1, 3, failed, 0), &widths, Palette::new(false));
    assert_eq!(plain, "✗ [1/3] image  api      failed at 2/6  <1ms");
    assert!(
        !plain.contains('✓') && !plain.contains("built"),
        "{plain:?}"
    );
    assert!(!plain.contains('\u{1b}'), "plain rows stay colourless");
    assert_eq!(
        completed_row(&image_row(1, 3, failed, 0), &widths, Palette::new(true)),
        "\u{1b}[31m✗\u{1b}[0m \u{1b}[38;5;244m[1/3]\u{1b}[0m \u{1b}[35mimage\u{1b}[0m  \u{1b}[1mapi\u{1b}[0m      \u{1b}[31mfailed at 2/6\u{1b}[0m  \u{1b}[38;5;244m<1ms\u{1b}[0m"
    );
    assert_eq!(
        completed_row(
            &image_row(1, 3, Outcome::Failed { at: None }, 0),
            &widths,
            Palette::new(false),
        ),
        "✗ [1/3] image  api      failed  <1ms"
    );
}

#[test]
fn a_built_row_counts_only_executed_steps_and_cached_never_says_built() {
    let widths = OuterWidths {
        kind: 5,
        target: 7,
        state: 0,
    };
    let pale = Palette::new(false);
    assert_eq!(
        completed_row(&image_row(1, 3, built(3), 1200), &widths, pale),
        "✓ [1/3] image  api      built 3 steps  1.2s"
    );
    assert_eq!(
        completed_row(&image_row(1, 3, Outcome::Cached, 8), &widths, pale),
        "✓ [1/3] image  api      cached  8ms"
    );
    let partial = Outcome::Ok(Done::Partial {
        cached: NonZeroUsize::new(9).expect("nonzero"),
        built: NonZeroUsize::new(2).expect("nonzero"),
    });
    assert_eq!(
        completed_row(&image_row(1, 3, partial, 2500), &widths, pale),
        "✓ [1/3] image  api      cached 9 · built 2  2.5s"
    );
    assert_eq!(
        completed_row(&image_row(1, 3, partial, 2500), &widths, Palette::new(true)),
        "\u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m[1/3]\u{1b}[0m \u{1b}[35mimage\u{1b}[0m  \u{1b}[1mapi\u{1b}[0m      cached 9 · built 2  \u{1b}[38;5;244m2.5s\u{1b}[0m"
    );
    let finished = completed_row(
        &image_row(1, 3, Outcome::Ok(Done::Finished), 0),
        &widths,
        pale,
    );
    assert_eq!(finished, "• [1/3] image  api      finished  <1ms");
    assert!(!finished.contains('✓'), "{finished:?}");
    assert_eq!(
        completed_row(
            &image_row(1, 3, Outcome::Ok(Done::Finished), 0),
            &widths,
            Palette::new(true),
        ),
        "\u{1b}[36m•\u{1b}[0m \u{1b}[38;5;244m[1/3]\u{1b}[0m \u{1b}[35mimage\u{1b}[0m  \u{1b}[1mapi\u{1b}[0m      finished  \u{1b}[38;5;244m<1ms\u{1b}[0m"
    );
}

#[test]
fn a_malformed_finished_frame_fails_closed_and_a_prep_frame_reads_done() {
    let plain = Palette::new(false);
    let mut narrative = Narrative::new(false);
    assert!(
        narrative
            .finish_node("setup", "docs", &json!({ "event": "finished" }), plain)
            .is_none(),
        "a frame without kind/name/duration must not render a row"
    );
    assert!(
        narrative
            .finish_node(
                "setup",
                "docs",
                &json!({ "event": "finished", "kind": "setup", "name": "docs" }),
                plain,
            )
            .is_none(),
        "a frame without a duration must not render a row"
    );
    let row = narrative.settled_row(
        &json!({
            "event": "finished", "kind": "setup", "name": "docs",
            "detail": "uv sync", "duration_ms": 0,
        }),
        "setup",
        "docs",
        0,
        (2, 5),
    );
    let widths = OuterWidths::default();
    assert_eq!(
        completed_row(&row, &widths, plain),
        "✓ [2/5] setup  docs  done  uv sync  <1ms"
    );
    assert!(
        completed_row(&row, &widths, Palette::new(true)).contains("\u{1b}[32mdone\u{1b}[0m"),
        "the prep state word stays green"
    );
}

#[test]
fn styled_rows_use_only_the_contract_sgr_codes_and_ban_cyan_on_steps() {
    let row = phase(Columns::default()).failed_row(Palette::new(true));
    assert!(row.contains("\u{1b}[31m✗"), "{row:?}");
    let live = phase(Columns::default()).live_row('⠹', 4800, 80, Palette::new(true));
    for code in ["\u{1b}[1m", "\u{1b}[38;5;244m", "\u{1b}[35m"] {
        assert!(live.contains(code), "{live:?}");
    }
    assert!(!live.contains("\u{1b}[9"), "no bright codes: {live:?}");
    assert!(
        !live.contains("\u{1b}[36m"),
        "no cyan on step rows: {live:?}"
    );
}

#[test]
fn re_anchor_rows_preserve_both_outer_and_inner_levels() {
    let outer = Phase::new(
        LiveKey::Boot {
            kind: "image".to_owned(),
            name: "api".to_owned(),
        },
        Some((1, 3)),
        "image".to_owned(),
        "api".to_owned(),
        String::new(),
        Columns { verb: 5, target: 7 },
    );
    assert_eq!(
        outer.anchor_row(Palette::new(false)),
        "→ [1/3] image  api      running"
    );
    assert_eq!(
        outer.anchor_row(Palette::new(true)),
        "→ \u{1b}[38;5;244m[1/3]\u{1b}[0m \u{1b}[35mimage\u{1b}[0m  \u{1b}[1mapi\u{1b}[0m      \u{1b}[32mrunning\u{1b}[0m"
    );

    let inner = Phase::new(
        LiveKey::Step {
            image: "api".to_owned(),
            index: 3,
        },
        Some((3, 6)),
        "RUN".to_owned(),
        "api".to_owned(),
        "sh -c \"…\"".to_owned(),
        Columns { verb: 4, target: 0 },
    );
    assert_eq!(
        inner.anchor_row(Palette::new(false)),
        "  → 3/6 run   sh -c \"…\""
    );
    assert_eq!(
        inner.anchor_row(Palette::new(true)),
        "  → \u{1b}[38;5;244m3/6\u{1b}[0m \u{1b}[38;5;132mrun\u{1b}[0m   sh -c \"…\""
    );
}

#[test]
fn golden_two_level_rows_pin_every_styled_and_no_color_byte() {
    let widths = outer_widths(&[
        ("image".to_owned(), "api".to_owned()),
        ("setup".to_owned(), "uv sync".to_owned()),
        ("service".to_owned(), "web".to_owned()),
    ]);
    assert_eq!(
        widths,
        OuterWidths {
            kind: 5,
            target: 7,
            state: 0
        }
    );

    let cached_row = image_row(1, 3, Outcome::Cached, 8);
    let outer_start = Phase::new(
        LiveKey::Boot {
            kind: "service".to_owned(),
            name: "web".to_owned(),
        },
        Some((3, 3)),
        "start".to_owned(),
        "web".to_owned(),
        "@uv run blastit dev --port 8000".to_owned(),
        Columns::from_outer(widths),
    );
    let inner_cols = Columns { verb: 4, target: 0 };
    let inner_running = Phase::new(
        LiveKey::Step {
            image: "api".to_owned(),
            index: 3,
        },
        Some((3, 6)),
        "RUN".to_owned(),
        "api".to_owned(),
        "sh -c \"…\"".to_owned(),
        inner_cols,
    );
    let inner_settled = Phase::new(
        LiveKey::Step {
            image: "api".to_owned(),
            index: 1,
        },
        Some((1, 6)),
        "FROM".to_owned(),
        "api".to_owned(),
        "python:3.13-slim".to_owned(),
        inner_cols,
    );
    let verdict_model = Verdict {
        verb: "RUN".to_owned(),
        target: "api".to_owned(),
        state: "failed".to_owned(),
        exit_code: Some(1),
        duration_ms: None,
    };

    let styled = Palette::new(true);
    let styled_rows = [
        completed_row(&cached_row, &widths, styled),
        outer_start.settled_row(195, styled),
        inner_running.live_row('⠸', 1200, 200, styled),
        inner_settled.settled_row(0, styled),
        completed_row(&image_row(1, 3, built(6), 12_300), &widths, styled),
        styled.verdict(&verdict_model),
        format!(
            "  {} {}",
            styled.meta("next:"),
            styled.next("slipfork logs -r b2ebd7")
        ),
        styled.header_line(
            "b2ebd7",
            "RUN",
            "api",
            "sh /tmp/oni.sh …",
            Some("2026-11-24 13:00:00"),
        ),
    ];
    assert_eq!(
        styled_rows,
        [
            "\u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m[1/3]\u{1b}[0m \u{1b}[35mimage\u{1b}[0m  \u{1b}[1mapi\u{1b}[0m      \u{1b}[32mcached\u{1b}[0m  \u{1b}[38;5;244m8ms\u{1b}[0m",
            "\u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m[3/3]\u{1b}[0m \u{1b}[35mstart\u{1b}[0m  \u{1b}[1mweb\u{1b}[0m      \u{1b}[32mrunning\u{1b}[0m  @uv run blastit dev --port 8000  \u{1b}[38;5;244m195ms\u{1b}[0m",
            "  ⠸ \u{1b}[38;5;244m3/6\u{1b}[0m \u{1b}[38;5;132mrun\u{1b}[0m   sh -c \"…\"  \u{1b}[38;5;244m1.2s\u{1b}[0m",
            "  \u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m1/6\u{1b}[0m \u{1b}[38;5;132mfrom\u{1b}[0m  python:3.13-slim  \u{1b}[38;5;244m<1ms\u{1b}[0m",
            "\u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m[1/3]\u{1b}[0m \u{1b}[35mimage\u{1b}[0m  \u{1b}[1mapi\u{1b}[0m      \u{1b}[32mbuilt\u{1b}[0m 6 steps  \u{1b}[38;5;244m12.3s\u{1b}[0m",
            "\u{1b}[31m✗\u{1b}[0m \u{1b}[35mRUN\u{1b}[0m \u{1b}[1mapi\u{1b}[0m \u{1b}[31mfailed\u{1b}[0m \u{1b}[38;5;244m(exit 1)\u{1b}[0m",
            "  \u{1b}[38;5;244mnext:\u{1b}[0m \u{1b}[36mslipfork logs -r b2ebd7\u{1b}[0m",
            "\u{1b}[1m#b2ebd7\u{1b}[0m  \u{1b}[35mRUN\u{1b}[0m  \u{1b}[1mapi\u{1b}[0m  sh /tmp/oni.sh …  \u{1b}[38;5;244m2026-11-24 13:00:00\u{1b}[0m",
        ]
    );

    let plain = Palette::new(false);
    let plain_rows = [
        completed_row(&cached_row, &widths, plain),
        outer_start.settled_row(195, plain),
        inner_running.live_row('⠸', 1200, 200, plain),
        inner_settled.settled_row(0, plain),
        completed_row(&image_row(1, 3, built(6), 12_300), &widths, plain),
        plain.verdict(&verdict_model),
        format!(
            "  {} {}",
            plain.meta("next:"),
            plain.next("slipfork logs -r b2ebd7")
        ),
        plain.header_line(
            "b2ebd7",
            "RUN",
            "api",
            "sh /tmp/oni.sh …",
            Some("2026-11-24 13:00:00"),
        ),
    ];
    assert_eq!(
        plain_rows,
        [
            "✓ [1/3] image  api      cached  8ms",
            "✓ [3/3] start  web      running  @uv run blastit dev --port 8000  195ms",
            "  ⠸ 3/6 run   sh -c \"…\"  1.2s",
            "  ✓ 1/6 from  python:3.13-slim  <1ms",
            "✓ [1/3] image  api      built 6 steps  12.3s",
            "✗ RUN api failed (exit 1)",
            "  next: slipfork logs -r b2ebd7",
            "#b2ebd7  RUN  api  sh /tmp/oni.sh …  2026-11-24 13:00:00",
        ]
    );
    assert!(
        plain_rows.iter().all(|row| !row.contains("\u{1b}[")),
        "{plain_rows:?}"
    );
    for (styled_row, plain_row) in styled_rows.iter().zip(plain_rows.iter()) {
        assert_eq!(strip_ansi(styled_row), *plain_row);
        assert!(
            styled_row.contains("\u{1b}[38;5;244m"),
            "every styled row keeps a neutral counter/timing cell: {styled_row:?}"
        );
        assert!(
            !styled_row.contains("\u{1b}[2m"),
            "metadata never uses SGR 2: {styled_row:?}"
        );
    }

    // Inner rows never carry `[` or the parent image target.
    for inner in [
        inner_running.anchor_row(plain),
        inner_running.live_row('⠸', 1200, 200, plain),
        inner_settled.settled_row(0, plain),
        inner_running.failed_row(plain),
        inner_running.anchor_row(plain),
        strip_ansi(&inner_running.live_row('⠸', 1200, 200, styled)),
        strip_ansi(&inner_settled.settled_row(0, styled)),
    ] {
        assert!(!inner.contains('['), "inner row carried `[`: {inner:?}");
        assert!(
            !inner.contains("api"),
            "inner row carried target: {inner:?}"
        );
    }
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
    assert_eq!(
        setup.anchor_row(plain),
        "→ [2/3] setup    uv sync        running"
    );
    assert_eq!(
        setup.failed_row(plain),
        "✗ [2/3] setup    uv sync        failed"
    );
    assert_eq!(
        setup.live_row('⠹', 445, 80, plain),
        "⠹ [2/3] setup    uv sync        running  445ms"
    );
    assert_eq!(
        setup.settled_row(445, plain),
        "✓ [2/3] setup    uv sync        running  445ms"
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
