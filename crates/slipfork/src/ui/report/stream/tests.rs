use super::{TailRing, visible};
use crate::ui::report::{Column, Table};
use crate::ui::report::{Entry, ReportRenderer, StreamKind, WallZone};
use crate::ui::{Terminal, UiMode};

const RED: &str = "\u{1b}[31mred\u{1b}[0m";

fn renderer(mode: UiMode) -> ReportRenderer {
    ReportRenderer::new(
        Terminal {
            cols: 100,
            rows: 30,
            mode,
            stdin_tty: false,
        },
        WallZone::fixed(2 * 3600).expect("paris"),
    )
}

#[test]
fn a_colour_terminal_keeps_the_child_bytes_and_every_other_viewer_strips_them() {
    assert_eq!(visible(RED, true, false), RED);
    assert_eq!(visible(RED, false, false), "red", "a pipe carries no ESC");
    assert_eq!(visible(RED, true, true), "red", "`--json` is escape-free");
    assert_eq!(visible(RED, false, true), "red");
}

#[test]
fn a_colour_line_measures_by_its_plain_width_so_the_gutter_never_shears() {
    let columns = [Column::left().at_least(6), Column::left()];
    let table = Table::new(&columns).gap(" ");
    let plain = table.widths(&[vec!["api".to_owned(), "red".to_owned()]]);
    let coloured = table.widths(&[vec!["api".to_owned(), RED.to_owned()]]);
    assert_eq!(plain, coloured);
    assert_eq!(
        crate::ui::strip(&table.line(&coloured, &["api".to_owned(), RED.to_owned()])),
        table.line(&plain, &["api".to_owned(), "red".to_owned()]),
        "the visible gutter never shears on a coloured line"
    );
}

#[test]
fn tail_ring_keeps_the_last_n_lines_and_splits_multi_line_chunks() {
    let mut ring = TailRing::new(3);
    ring.push("one\n");
    ring.push("header\n\n");
    assert_eq!(
        ring.drain(),
        vec!["one\n".to_owned(), "header\n".to_owned(), "\n".to_owned()],
        "a header chunk contributes its row and its blank line separately"
    );
    ring.push("a\nb\nc\n");
    ring.push("d\n");
    assert_eq!(
        ring.drain(),
        vec!["b\n", "c\n", "d\n"],
        "the ring evicts oldest first"
    );
}

#[test]
fn a_replay_entry_carries_the_fixed_paris_clock() {
    let renderer = renderer(UiMode::Plain);
    let mut out = Vec::new();
    let mut err = Vec::new();
    renderer
        .emit_entry_to(
            &Entry::Stream {
                indent: 2,
                stream: StreamKind::Stdout,
                line: "compiled".to_owned(),
                ts_ms: 1_782_907_200_000,
            },
            &mut out,
            &mut err,
        )
        .expect("render one replay row");
    assert_eq!(
        String::from_utf8(out).expect("utf8"),
        "  14:00:00.000 │ compiled\n"
    );
    assert!(err.is_empty());
}

#[test]
fn a_replay_entry_uses_the_named_paris_zone() {
    let renderer = ReportRenderer::new(
        Terminal {
            cols: 100,
            rows: 30,
            mode: UiMode::Plain,
            stdin_tty: false,
        },
        WallZone::named("Europe/Paris").expect("the host carries the IANA zone database"),
    );
    let mut out = Vec::new();
    let mut err = Vec::new();
    renderer
        .emit_entry_to(
            &Entry::Stream {
                indent: 2,
                stream: StreamKind::Stderr,
                line: "warn".to_owned(),
                ts_ms: 1_782_907_200_000,
            },
            &mut out,
            &mut err,
        )
        .expect("render one replay row");
    assert_eq!(
        String::from_utf8(out).expect("utf8"),
        "  14:00:00.000 ┃ warn\n",
        "2026-07-01T12:00:00Z is 14:00 Paris"
    );
    assert!(err.is_empty());
}
