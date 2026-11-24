//! Pure boot-pane text: exact rows, bars, and panel lines with no terminal state.
//!
//! The live driver owns the scroll region and the ticker thread; this module owns every byte
//! those drivers print. Every colour role comes from the shipped [`Palette`], so a `NO_COLOR`,
//! `TERM=dumb`, piped, or `--json` invocation emits zero `ESC[` bytes. Failure copy lives in the
//! binary's `boot::failure`, and the live build narrative in `boot::build`.

use std::time::Duration;

use serde_json::Value;

use crate::ui::Palette;
pub use crate::ui::elapsed_text;
use crate::ui::table::{Column, ColumnWidths, Table, visible_width};

/// The six fixed boot-row columns: icon, kind, name, state, duration (6), run id (7).
const BOOT: [Column; 6] = [
    Column::left().at_least(1),
    Column::left(),
    Column::left(),
    Column::left(),
    Column::right().at_least(6),
    Column::left().at_least(7),
];

/// The one boot row grid: six borderless columns with two-space gaps.
#[must_use]
pub fn boot_table() -> Table<'static> {
    Table::new(&BOOT)
}

/// Precomputes the six boot columns from a complete known row or plan set.
#[must_use]
pub fn boot_widths(rows: &[Vec<String>]) -> ColumnWidths {
    boot_table().widths(rows)
}

/// The widest state any finished row can carry.
///
/// A plan reserves this many cells for the state column, so a later `up to date` never widens it
/// under a row that is already printed.
const WIDEST_STATE: &str = "up to date";

/// Measures the six boot columns from the complete announced plan: icon, kind, name, state,
/// duration (>=6, right), and the reserved run-id slot (>=7).
#[must_use]
pub fn plan_widths(rows: &[(String, String)]) -> ColumnWidths {
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|(kind, name)| {
            vec![
                String::new(),
                kind.clone(),
                name.clone(),
                WIDEST_STATE.to_owned(),
                String::new(),
                String::new(),
            ]
        })
        .collect();
    boot_widths(&cells)
}

/// One live task row: neutral Braille spinner in the icon cell, magenta kind, bold name, green
/// `running`, right-aligned live elapsed, and the reserved run-id slot.
#[must_use]
pub fn running_row(
    kind: &str,
    name: &str,
    spinner: char,
    millis: u64,
    widths: &ColumnWidths,
) -> String {
    let palette = Palette::new(true);
    boot_table().line(
        widths,
        &[
            spinner.to_string(),
            palette.verb(kind),
            palette.tint(name, "1"),
            palette.status("running"),
            palette.dim(&elapsed_text(millis)),
            String::new(),
        ],
    )
}

/// The panel's active summary when several tasks run at once: `4 running (pg, front, worker, api)`.
#[must_use]
pub fn running_summary(names: &[&str]) -> String {
    format!("{} running ({})", names.len(), names.join(", "))
}

/// Below this many columns the anchored bar is dropped for the counter/percent/elapsed panel.
pub const NARROW: usize = 80;
/// The panel bar is exactly this many cells wide (`specs/02` §3).
pub const BAR_CELLS: usize = 24;

/// One finished boot node, already parsed out of a typed daemon `event` frame.
///
/// A frame without a kind, a name/label, or a duration is malformed: parsing yields `None` rather
/// than inventing a `step`/`ready`/`0` row, so a broken event stream can never print fiction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node<'a> {
    /// Node kind (`image`, `service`, `postgres`, …), magenta and padded to 8 cells.
    pub kind: &'a str,
    /// Node name, printed bold.
    pub name: &'a str,
    /// Raw state from the daemon; [`completed_row`] lowercases it.
    pub state: &'a str,
    /// Wall time the node took, rendered by [`elapsed_text`].
    pub millis: u64,
    /// Minted run id for a step that executed work; `None` for a cache hit.
    pub run_id: Option<&'a str>,
    /// `false` for an `up-to-date` node, which earns a dim bullet instead of a green check.
    pub success: bool,
}

impl<'a> Node<'a> {
    /// Projects one `finished`/`up-to-date` event frame into a row description.
    #[must_use]
    pub fn from_event(event: &'a Value) -> Option<Self> {
        Some(Self {
            kind: event["kind"].as_str()?,
            name: event["name"].as_str().or_else(|| event["label"].as_str())?,
            state: event["state"].as_str().unwrap_or_default(),
            millis: event["duration_ms"]
                .as_u64()
                .or_else(|| event["elapsed_ms"].as_u64())?,
            run_id: event["run_id"].as_str(),
            success: event["event"].as_str() != Some("up-to-date"),
        })
    }
}

/// The anchored panel's cell count, or `None` when the terminal is too narrow for a bar.
#[must_use]
pub fn pane_geometry(width: usize) -> Option<usize> {
    (width >= NARROW).then_some(BAR_CELLS)
}

/// `round(done / total * 100)`, clamped to `100`.
#[must_use]
pub fn percent(done: usize, total: usize) -> usize {
    filled(done, total, 100)
}

/// The fully styled live panel for one colour TTY: spinner, 24-cell bar, counters, percent,
/// active summary, and elapsed time, clamped to `width` columns.
#[must_use]
pub fn panel_line(
    spinner: char,
    done: usize,
    total: usize,
    summary: &str,
    elapsed: Duration,
    width: usize,
) -> String {
    assemble(
        spinner,
        done,
        total,
        summary,
        elapsed,
        width,
        Palette::new(true),
    )
}

/// One `\r`-rewritten colourless status line; carries zero escapes and zero carriage returns.
#[must_use]
pub fn status_line(
    spinner: char,
    done: usize,
    total: usize,
    summary: &str,
    elapsed: Duration,
    width: usize,
) -> String {
    assemble(
        spinner,
        done,
        total,
        summary,
        elapsed,
        width,
        Palette::new(false),
    )
}

fn assemble(
    spinner: char,
    done: usize,
    total: usize,
    summary: &str,
    elapsed: Duration,
    width: usize,
    palette: Palette,
) -> String {
    let colour = palette.color();
    let counter = format!("{done}/{total}");
    let pct = format!("{}%", percent(done, total));
    let seconds = format!("{:.1}s", elapsed.as_secs_f64());
    // The spinner is content, never a keyword or a copyable command: neutral on every surface.
    let spin = spinner.to_string();
    let bar = |cells: usize| -> String {
        let filled = filled(done, total, cells);
        if !colour {
            return format!("[{}{}]", "#".repeat(filled), ".".repeat(cells - filled));
        }
        // The bar fill is content: neutral, never a hue. Only the empty cells recede to dim.
        format!(
            "[{}{}]",
            "█".repeat(filled),
            palette.dim(&"░".repeat(cells - filled)),
        )
    };
    let counter = palette.tint(&counter, "1");
    let pct = palette.tint(&pct, "1");
    let summary = palette.tint(summary, "1");
    let seconds = palette.dim(&seconds);

    if let Some(cells) = pane_geometry(width) {
        let full = format!(
            "{spin} {}  {counter}  {pct} · {summary} · {seconds}",
            bar(cells)
        );
        if visible_width(&full) <= width {
            return full;
        }
        let without_summary = format!("{spin} {}  {counter}  {pct} · {seconds}", bar(cells));
        if visible_width(&without_summary) <= width {
            return without_summary;
        }
    }
    let with_summary = format!("{spin} {counter}  {pct} · {summary} · {seconds}");
    if visible_width(&with_summary) <= width {
        return with_summary;
    }
    format!("{spin} {counter}  {pct} · {seconds}")
}

fn filled(done: usize, total: usize, cells: usize) -> usize {
    if total == 0 {
        return 0;
    }
    (done.saturating_mul(cells) + total / 2) / total
}

/// The pipe header for a node that just started: `→ [4/12] image base`.
#[must_use]
pub fn header_row(step: u64, total: u64, kind: &str, name: &str) -> String {
    format!("→ [{step}/{total}] {kind} {name}")
}

/// The world-ready verdict, with the primary URL always bold cyan and underlined.
#[must_use]
pub fn ready_row(url: &str, styled: bool) -> String {
    let palette = Palette::new(styled);
    format!(
        "{} {} {}",
        palette.tint("✓ Ready", "1;32"),
        palette.dim("->"),
        palette.url(url),
    )
}

/// The fork verdict, with the fork name bold and the primary URL bold cyan.
#[must_use]
pub fn forked_row(fork: &str, millis: u64, url: &str, styled: bool) -> String {
    let palette = Palette::new(styled);
    format!(
        "{} {} {} {} {}",
        palette.icon_ok(),
        palette.tint(fork, "1"),
        palette.dim(&format!("({})", elapsed_text(millis))),
        palette.dim("->"),
        palette.url(url),
    )
}

/// One stopped world: `✓ Stopped demo (1.2s)`.
#[must_use]
pub fn stopped_row(world: &str, millis: u64, styled: bool) -> String {
    let palette = Palette::new(styled);
    format!(
        "{} {} {}",
        palette.tint("✓ Stopped", "32"),
        palette.tint(world, "1"),
        palette.dim(&format!("({})", elapsed_text(millis))),
    )
}

/// `✓  image  base  cached   420ms` on a pipe, or the styled check, magenta kind, bold name,
/// lowercase state, right-aligned dim duration, and the reserved run-id slot on a colour terminal.
///
/// `widths` comes from [`boot_widths`] over the complete boot plan, so a row printed while the boot
/// is still streaming lands in the same grid a batch render would produce.
#[must_use]
pub fn completed_row(node: &Node<'_>, widths: &ColumnWidths, styled: bool) -> String {
    let palette = Palette::new(styled);
    let state = node.state.to_lowercase();
    let mark = if node.success {
        palette.icon_ok()
    } else {
        palette.dim("•")
    };
    let state = if state.is_empty() {
        String::new()
    } else {
        palette.status(&state)
    };
    // The run-id slot reserves seven cells in `BOOT` and shows a dim `#id` only for a step that
    // really executed; a `cached` row leaves it empty so earlier columns never move.
    let run_id = node
        .run_id
        .map(|id| palette.dim(&format!("#{id}")))
        .unwrap_or_default();
    boot_table().line(
        widths,
        &[
            mark,
            palette.verb(node.kind),
            palette.tint(node.name, "1"),
            state,
            palette.dim(&elapsed_text(node.millis)),
            run_id,
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::{Node, completed_row, plan_widths};

    /// Plan-derived widths keep every row in the same columns, even when a later row carries a
    /// much longer name and an `up to date` state.
    #[test]
    fn plan_widths_hold_every_row_in_the_same_columns() {
        let plan = vec![
            ("resource".to_owned(), "pg".to_owned()),
            ("service".to_owned(), "a-very-long-service-name".to_owned()),
        ];
        let widths = plan_widths(&plan);
        let short = Node {
            kind: "resource",
            name: "pg",
            state: "Ready",
            millis: 420,
            run_id: Some("b7f2a1"),
            success: true,
        };
        let long = Node {
            kind: "service",
            name: "a-very-long-service-name",
            state: "up to date",
            millis: 0,
            run_id: None,
            success: false,
        };
        let rows = [
            completed_row(&short, &widths, false),
            completed_row(&long, &widths, false),
        ];

        // Byte offsets (`✓` is three UTF-8 bytes): icon at 0, gap, kind at 5; kind 8 + gap =
        // name at 15; name 24 + gap = state at 41; state 10 + gap + duration 6 ends at 59.
        const KIND: usize = 5;
        const NAME: usize = 15;
        const STATE: usize = 41;
        const DURATION_END: usize = 59;
        assert_eq!(rows[0].find("resource"), Some(KIND), "{:?}", rows[0]);
        assert_eq!(rows[1].find("service"), Some(KIND), "{:?}", rows[1]);
        assert_eq!(rows[0].find("pg"), Some(NAME), "{:?}", rows[0]);
        assert_eq!(
            rows[1].find("a-very-long-service-name"),
            Some(NAME),
            "{:?}",
            rows[1]
        );
        assert_eq!(rows[0].find("ready"), Some(STATE), "{:?}", rows[0]);
        assert_eq!(rows[1].find("up to date"), Some(STATE), "{:?}", rows[1]);
        // The duration column is right-aligned, so its final byte lands on `DURATION_END` in every
        // row — with or without a run id after it.
        for (row, duration) in [(&rows[0], "420ms"), (&rows[1], "<1ms")] {
            let end = row.find(duration).expect("duration cell") + duration.len();
            assert_eq!(end, DURATION_END, "duration right edge: {row:?}");
        }
        // Filled slot: exactly `GAP + 7` cells after the duration edge.
        assert_eq!(
            rows[0].find("#b7f2a1"),
            Some(DURATION_END + 2),
            "run-id column start: {:?}",
            rows[0]
        );
        assert_eq!(rows[0].trim_end().len(), DURATION_END + 2 + 7);
        // Empty slot: the seven-cell column stays reserved in the shared grid; only this row's
        // trailing padding is trimmed, and no `#` is invented for a cache hit.
        assert!(!rows[1].contains('#'), "{:?}", rows[1]);
        assert_eq!(rows[1].trim_end().len(), DURATION_END);
        assert!(!rows[1].contains('\u{1b}'), "plain rows stay colourless");
    }
}
