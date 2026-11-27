use super::ReportRenderer;
use crate::ui::report::{
    Counter, Done, Entry, Failure, Outcome, Report, Row, RowLevel, Section, StreamKind, Verdict,
    VerdictKind, WallZone,
};
use crate::ui::{Terminal, UiMode};

fn renderer(mode: UiMode, verbose: bool) -> ReportRenderer {
    let terminal = Terminal {
        cols: 100,
        rows: 30,
        mode,
        stdin_tty: false,
    };
    ReportRenderer::new(terminal, WallZone::fixed(0).expect("utc")).with_verbose(verbose)
}

fn bytes(report: &Report, mode: UiMode, verbose: bool) -> (String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    renderer(mode, verbose)
        .emit_to(report, &mut out, &mut err)
        .expect("render into memory");
    (
        String::from_utf8(out).expect("utf8 stdout"),
        String::from_utf8(err).expect("utf8 stderr"),
    )
}

fn row(state: Outcome) -> Row {
    Row {
        level: RowLevel::Outer,
        counter: Some(Counter { index: 1, total: 2 }),
        keyword: Some("setup".to_owned()),
        target: Some("api".to_owned()),
        state,
        command: Some("uv sync".to_owned()),
        timing_ms: Some(41),
        id: None,
    }
}

#[test]
fn a_failed_verdict_prints_only_stderr_with_no_blank_padding() {
    let report = Report::failure(Failure::new("cwd", "cannot read", "cd somewhere"));
    let (out, err) = bytes(&report, UiMode::Plain, false);
    assert!(out.is_empty(), "{out:?}");
    assert_eq!(err, "error: cannot read\nnext: cd somewhere\n");
}

#[test]
fn one_blank_line_separates_non_empty_sections_and_notes_need_verbose() {
    let report = Report {
        sections: vec![
            Section {
                header: Some("Build".to_owned()),
                entries: vec![
                    Entry::Row(row(Outcome::Ok(Done::Ready))),
                    Entry::Note("internal".to_owned()),
                ],
            },
            Section {
                header: None,
                entries: vec![Entry::Stream {
                    indent: 2,
                    stream: StreamKind::Stderr,
                    line: "warn".to_owned(),
                    ts_ms: 0,
                }],
            },
        ],
        verdict: None,
        remedy: None,
    };
    let (out, err) = bytes(&report, UiMode::Plain, false);
    assert_eq!(
        out,
        "Build\n[1/2] SETUP api ✓ ready uv sync (41ms)\n\n  ┃ warn\n"
    );
    assert!(err.is_empty(), "{err:?}");
    let (verbose, _) = bytes(&report, UiMode::Plain, true);
    assert!(verbose.contains("internal"), "{verbose:?}");
    assert!(!out.contains('\u{1b}'));
}

#[test]
fn a_verdict_ok_line_is_green_only_when_color_is_on() {
    let report = Report {
        sections: Vec::new(),
        verdict: Some(Verdict::Ok(crate::ui::report::VerdictKind::Ready)),
        remedy: None,
    };
    let (plain, _) = bytes(&report, UiMode::Plain, false);
    assert_eq!(plain, "✓ ready\n");
    let (colored, _) = bytes(&report, UiMode::Interactive { color: true }, false);
    assert_eq!(colored, "\u{1b}[32m✓\u{1b}[0m \u{1b}[32mready\u{1b}[0m\n");
    let (json, _) = bytes(&report, UiMode::Json, false);
    assert!(!json.contains('\u{1b}'), "{json:?}");
    assert!(json.contains("\"Ready\""), "{json:?}");
}

#[test]
fn a_json_failure_is_the_exact_error_projection() {
    let report = Report::failure(Failure::new("usage", "error: bad flag", "run --help"));
    let (out, err) = bytes(&report, UiMode::Json, false);
    assert!(err.is_empty(), "{err:?}");
    assert_eq!(
        out,
        "{\"error\":{\"code\":\"usage\",\"message\":\"bad flag\",\"next\":\"run --help\"}}\n"
    );
    assert!(!out.contains('\u{1b}'));
}

#[test]
fn a_remedy_renders_after_the_ok_verdict_and_standalone() {
    let report = Report {
        sections: Vec::new(),
        verdict: Some(Verdict::Ok(VerdictKind::Ready)),
        remedy: Some("run `slipfork ps`".to_owned()),
    };
    let (out, err) = bytes(&report, UiMode::Plain, false);
    assert_eq!(out, "✓ ready\nnext: run `slipfork ps`\n");
    assert!(err.is_empty(), "{err:?}");
    let standalone = Report {
        sections: Vec::new(),
        verdict: None,
        remedy: Some("run `slipfork ps`".to_owned()),
    };
    let (out, _) = bytes(&standalone, UiMode::Plain, false);
    assert_eq!(out, "next: run `slipfork ps`\n");
    assert!(!out.contains('\u{1b}'));
}

#[test]
fn a_recorded_stream_row_gets_its_replay_clock() {
    let recorded = Report {
        sections: vec![Section {
            header: None,
            entries: vec![Entry::Stream {
                indent: 2,
                stream: StreamKind::Stdout,
                line: "compiled".to_owned(),
                ts_ms: 1_782_907_200_000,
            }],
        }],
        verdict: None,
        remedy: None,
    };
    let (out, _) = bytes(&recorded, UiMode::Plain, false);
    assert_eq!(out, "  12:00:00.000 │ compiled\n");
    let unrecorded = Report {
        sections: vec![Section {
            header: None,
            entries: vec![Entry::Stream {
                indent: 0,
                stream: StreamKind::Stderr,
                line: "warn".to_owned(),
                ts_ms: 0,
            }],
        }],
        verdict: None,
        remedy: None,
    };
    let (out, _) = bytes(&unrecorded, UiMode::Plain, false);
    assert_eq!(out, "┃ warn\n", "epoch 0 means not recorded, not 1970");
}

#[test]
fn sections_survive_a_failed_verdict_in_both_modes() {
    let report = Report {
        sections: vec![Section {
            header: None,
            entries: vec![Entry::Row(row(Outcome::Ok(Done::Ready)))],
        }],
        verdict: Some(Verdict::Failed(Failure::new(
            "build",
            "cannot read",
            "retry",
        ))),
        remedy: None,
    };
    let (out, err) = bytes(&report, UiMode::Plain, false);
    assert_eq!(out, "[1/2] SETUP api ✓ ready uv sync (41ms)\n");
    assert_eq!(err, "error: cannot read\nnext: retry\n");

    let (json, err) = bytes(&report, UiMode::Json, false);
    assert!(err.is_empty(), "{err:?}");
    let value: serde_json::Value = serde_json::from_str(json.trim()).expect("one object");
    assert_eq!(value["sections"][0]["entries"][0]["Row"]["target"], "api");
    assert_eq!(value["verdict"]["Failed"]["code"], "build");
}

/// The raw byte contract `logs --runs` needs: one notice per line, one `--json` line.
#[test]
fn notice_sections_and_json_lines_keep_the_legacy_raw_bytes() {
    let report = Report {
        sections: vec![Section {
            header: None,
            entries: vec![
                Entry::Notice {
                    stream: StreamKind::Stdout,
                    text: "one".to_owned(),
                },
                Entry::Notice {
                    stream: StreamKind::Stderr,
                    text: "two".to_owned(),
                },
            ],
        }],
        verdict: None,
        remedy: None,
    };
    let (out, err) = bytes(&report, UiMode::Plain, false);
    assert_eq!(out, "one\n", "{out:?}");
    assert_eq!(err, "two\n", "{err:?}");
    assert!(!out.contains('\u{1b}') && !err.contains('\u{1b}'));

    // `json_exit` writes `serde_json::to_string(value) + "\n"`; `emit_to` is the same path.
    let (json, err) = bytes(&Report::notice("x"), UiMode::Json, false);
    assert!(err.is_empty(), "{err:?}");
    assert_eq!(
        json,
        "{\"sections\":[{\"header\":null,\"entries\":[{\"Notice\":{\"stream\":\"Stdout\",\"text\":\"x\"}}]}],\"verdict\":null,\"remedy\":null}\n"
    );
}
