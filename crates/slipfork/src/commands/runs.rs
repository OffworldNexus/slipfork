//! `slipfork logs --runs`: the recorded-run catalog as one borderless [`Table`].
//!
//! The daemon owns the catalog read (`<store>/logs/<world>/runs/index.jsonl`); this module only
//! lays the rows out. Widths come from the complete set, so every `#run_id`, status and duration
//! edge is pixel-straight, and a colourless render carries zero `ESC[` bytes.

use serde_json::Value;

use crate::boot::elapsed_text;
use crate::ui::paint;
use crate::ui::table::{Column, Table};

use super::streams::format_ts;

/// One recorded run, projected from the daemon's `runs` frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunRow {
    pub run_id: String,
    pub kind: String,
    pub name: String,
    pub command: String,
    pub state: String,
    pub duration_ms: u64,
    pub ts_ms: u64,
}

impl RunRow {
    /// Projects one catalog entry; a row without an id is malformed and skipped.
    pub(crate) fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            run_id: value["run_id"].as_str()?.to_owned(),
            kind: value["kind"].as_str().unwrap_or_default().to_owned(),
            name: value["name"].as_str().unwrap_or_default().to_owned(),
            command: value["command"].as_str().unwrap_or_default().to_owned(),
            state: value["state"].as_str().unwrap_or_default().to_owned(),
            duration_ms: value["duration_ms"].as_u64().unwrap_or(0),
            ts_ms: value["ts_ms"].as_u64().unwrap_or(0),
        })
    }

    /// The catalog as one JSON object (the exact `--json` line, with no escapes added).
    pub(crate) fn to_value(&self) -> Value {
        serde_json::json!({
            "run_id": self.run_id,
            "kind": self.kind,
            "name": self.name,
            "command": self.command,
            "state": self.state,
            "duration_ms": self.duration_ms,
            "ts_ms": self.ts_ms,
        })
    }

    fn step(&self) -> String {
        match (self.name.is_empty(), self.command.is_empty()) {
            (true, true) => String::new(),
            (true, false) => self.command.clone(),
            (false, true) => self.name.clone(),
            (false, false) => format!("{} {}", self.name, self.command),
        }
    }
}

/// The six catalog columns: `#run_id`, kind, step/command, status, duration (right), recorded.
const COLUMNS: [Column; 6] = [
    Column::left().at_least(7),
    Column::left(),
    Column::left(),
    Column::left(),
    Column::right().at_least(6),
    Column::left(),
];

/// Renders the whole catalog: one dim header row and one row per recorded run.
pub(crate) fn render_runs(rows: &[RunRow], color: bool) -> Vec<String> {
    let table = Table::new(&COLUMNS);
    let header = [
        paint("#run_id", "2", color),
        paint("kind", "2", color),
        paint("step", "2", color),
        paint("status", "2", color),
        paint("duration", "2", color),
        paint("recorded", "2", color),
    ];
    let body: Vec<[String; 6]> = rows
        .iter()
        .map(|row| {
            let status = match row.state.as_str() {
                "failed" => paint(&row.state, "31", color),
                "built" | "pulled" | "ready" => paint(&row.state, "32", color),
                _ => row.state.clone(),
            };
            [
                paint(&format!("#{}", row.run_id), "2", color),
                row.kind.clone(),
                row.step(),
                status,
                elapsed_text(row.duration_ms),
                format_ts(row.ts_ms),
            ]
        })
        .collect();
    let mut cells: Vec<Vec<String>> = Vec::with_capacity(body.len() + 1);
    cells.push(header.to_vec());
    cells.extend(body.into_iter().map(Vec::from));
    let widths = table.widths(&cells);
    cells.iter().map(|row| table.line(&widths, row)).collect()
}

#[cfg(test)]
mod tests {
    use super::{RunRow, render_runs};
    use crate::ui::table::visible_width;

    fn row(run_id: &str, state: &str, duration_ms: u64) -> RunRow {
        RunRow {
            run_id: run_id.to_owned(),
            kind: "setup".to_owned(),
            name: "backend".to_owned(),
            command: "uv sync".to_owned(),
            state: state.to_owned(),
            duration_ms,
            ts_ms: 1_764_000_000_000,
        }
    }

    /// Every column starts at one byte index and every duration shares one right edge.
    #[test]
    fn the_catalog_table_keeps_pixel_straight_columns() {
        let rows = vec![
            row("b7f2a1", "ready", 41),
            row("c91e04", "failed", 1_234),
            row("0a1b2c", "ready", 7),
        ];
        let lines = render_runs(&rows, false);
        assert_eq!(lines.len(), 4, "{lines:?}");
        let body = &lines[1..];
        let id_column = body[0].find("#b7f2a1").expect("id column");
        let kind_column = body[0].find("setup").expect("kind column");
        let step_column = body[0].find("backend").expect("step column");
        let status_column = body[0].find("ready").expect("status column");
        let recorded_column = body[0].find("2025-").expect("recorded column");
        for line in body {
            assert_eq!(line.find("setup"), Some(kind_column), "{line:?}");
            assert_eq!(line.find("backend"), Some(step_column), "{line:?}");
            assert_eq!(line.find("2025-"), Some(recorded_column), "{line:?}");
            assert_eq!(
                line.find("ready").or_else(|| line.find("failed")),
                Some(status_column),
                "{line:?}"
            );
            let id_at = line.find('#').expect("every row carries #run_id");
            assert_eq!(id_at, id_column, "{line:?}");
        }
        let ends: Vec<usize> = body.iter().map(|line| line.trim_end().len()).collect();
        assert!(
            ends.windows(2).all(|pair| pair[0] == pair[1]),
            "one right edge: {ends:?}"
        );
        // The duration column is right-aligned, so every row ends on the recorded stamp.
        assert!(
            body.iter().all(|line| line.contains(" uv sync ")),
            "{body:?}"
        );
        for line in &lines {
            assert!(!line.contains('\u{1b}'), "NO_COLOR stays pure: {line:?}");
            for border in ['│', '─', '┌', '┐', '└', '┘', '├', '┤', '┼', '+', '|']
            {
                assert!(!line.contains(border), "borderless: {line:?}");
            }
        }
        assert!(visible_width(&lines[0]) >= visible_width("#run_id  kind"));
    }

    /// The `--json` object round-trips and carries zero terminal escapes.
    #[test]
    fn the_catalog_json_is_escape_free() {
        let line = serde_json::to_string(&row("b7f2a1", "ready", 41).to_value())
            .expect("serialize one run");
        assert!(line.starts_with("{\"command\":\"uv sync\""), "{line}");
        assert!(line.contains("\"run_id\":\"b7f2a1\""), "{line}");
        assert!(!line.contains('\u{1b}'), "{line}");
        assert!(!line.contains('\u{7}'), "{line}");
    }
}
