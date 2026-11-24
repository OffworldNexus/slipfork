//! Byte-exact role pins: the golden SGR/`NO_COLOR` lines the visual contract promises.

use super::super::{Columns, LiveKey, Phase, image_done_row};
use super::phase;
use crate::ui::{Palette, Verdict};

#[test]
fn the_build_end_row_counts_steps_and_sums_time() {
    let row = image_done_row("api", 4, 12300, Palette::new(false));
    assert_eq!(row, "✓ image api (4 steps, 12.3s)");
    assert_eq!(
        image_done_row("api", 1, 80, Palette::new(false)),
        "✓ image api (1 step, 80ms)"
    );
    assert!(!row.contains('\u{1b}'), "plain rows stay colourless");
    assert_eq!(
        image_done_row("api", 4, 12300, Palette::new(true)),
        "\u{1b}[32m✓\u{1b}[0m \u{1b}[35mimage\u{1b}[0m \u{1b}[1mapi\u{1b}[0m \u{1b}[2m(4 steps, 12.3s)\u{1b}[0m"
    );
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
    assert_eq!(
        row,
        "→ \u{1b}[2m[2/4]\u{1b}[0m \u{1b}[35mRUN\u{1b}[0m \u{1b}[1mapi\u{1b}[0m"
    );
}

#[test]
fn the_v3_role_table_pins_every_wire_byte() {
    let mut columns = Columns::default();
    columns.observe("FROM", "api");
    let running = Phase::new(
        LiveKey::Step {
            image: "api".to_owned(),
            index: 3,
        },
        Some((3, 5)),
        "RUN".to_owned(),
        "api".to_owned(),
        "sh /tmp/oni.sh && apt-get install -y blastit".to_owned(),
        columns,
    );
    let settled = Phase::new(
        LiveKey::Step {
            image: "api".to_owned(),
            index: 1,
        },
        Some((1, 5)),
        "FROM".to_owned(),
        "api".to_owned(),
        "python:3.14-trixie AS base".to_owned(),
        columns,
    );

    let styled = Palette::new(true);
    assert_eq!(
        running.live_row('⠋', 1200, 200, styled),
        "⠋ \u{1b}[2m[3/5]\u{1b}[0m \u{1b}[35mRUN \u{1b}[0m \u{1b}[1mapi\u{1b}[0m  sh /tmp/oni.sh && apt-get install -y blastit  \u{1b}[2m1.2s\u{1b}[0m"
    );
    assert_eq!(
        settled.settled_row(0, styled),
        "\u{1b}[32m✓\u{1b}[0m \u{1b}[2m[1/5]\u{1b}[0m \u{1b}[35mFROM\u{1b}[0m \u{1b}[1mapi\u{1b}[0m  python:3.14-trixie AS base  \u{1b}[2m<1ms\u{1b}[0m"
    );
    let verdict = styled.verdict(&Verdict {
        verb: "RUN".to_owned(),
        target: "api".to_owned(),
        state: "failed".to_owned(),
        exit_code: Some(1),
        duration_ms: None,
    });
    assert_eq!(
        verdict,
        "\u{1b}[31m✗\u{1b}[0m \u{1b}[35mRUN\u{1b}[0m \u{1b}[1mapi\u{1b}[0m \u{1b}[31mfailed\u{1b}[0m \u{1b}[2m(exit 1)\u{1b}[0m"
    );
    let header = styled.header_line(
        "b2ebd7",
        "RUN",
        "api",
        "sh /tmp/oni.sh …",
        Some("2026-11-24 13:00:00"),
    );
    assert_eq!(
        header,
        "\u{1b}[1m#b2ebd7\u{1b}[0m  \u{1b}[35mRUN\u{1b}[0m  \u{1b}[1mapi\u{1b}[0m  sh /tmp/oni.sh …  \u{1b}[2m2026-11-24 13:00:00\u{1b}[0m"
    );
    let next = format!(
        "  {} {}",
        styled.dim("next:"),
        styled.tint("slipfork logs -r b2ebd7", "36")
    );
    assert_eq!(
        next,
        "  \u{1b}[2mnext:\u{1b}[0m \u{1b}[36mslipfork logs -r b2ebd7\u{1b}[0m"
    );

    let plain = Palette::new(false);
    let rows = [
        running.live_row('⠋', 1200, 200, plain),
        settled.settled_row(0, plain),
        plain.verdict(&Verdict {
            verb: "RUN".to_owned(),
            target: "api".to_owned(),
            state: "failed".to_owned(),
            exit_code: Some(1),
            duration_ms: None,
        }),
        plain.header_line(
            "b2ebd7",
            "RUN",
            "api",
            "sh /tmp/oni.sh …",
            Some("2026-11-24 13:00:00"),
        ),
        format!(
            "  {} {}",
            plain.dim("next:"),
            plain.tint("slipfork logs -r b2ebd7", "36")
        ),
    ];
    assert_eq!(
        rows,
        [
            "⠋ [3/5] RUN  api  sh /tmp/oni.sh && apt-get install -y blastit  1.2s",
            "✓ [1/5] FROM api  python:3.14-trixie AS base  <1ms",
            "✗ RUN api failed (exit 1)",
            "#b2ebd7  RUN  api  sh /tmp/oni.sh …  2026-11-24 13:00:00",
            "  next: slipfork logs -r b2ebd7",
        ]
    );
    assert!(rows.iter().all(|row| !row.contains("\u{1b}[")), "{rows:?}");
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
