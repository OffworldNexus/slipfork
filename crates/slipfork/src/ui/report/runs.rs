//! `slipfork logs --runs`: the recorded-run catalog as one borderless [`Table`].
//!
//! The daemon owns the catalog read (`<store>/logs/<world>/runs/index.jsonl`); this module only
//! projects and lays out the rows. Widths come from the complete set, so every `#run_id`, status
//! and duration edge is pixel-straight, and a colourless render carries zero `ESC[` bytes.

use serde_json::Value;

use crate::ui::elapsed_text;
use crate::ui::table::{Column, Table};

use super::render::ReportRenderer;

/// One recorded run, projected from the daemon's `runs` frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    pub run_id: String,
    pub kind: String,
    pub target: String,
    pub command: String,
    pub state: String,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    pub duration_ms: u64,
    pub project: String,
    pub world: String,
}

impl RunRow {
    /// Projects one catalog entry; a row without an id is malformed and skipped.
    #[must_use]
    pub fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            run_id: value["run_id"].as_str()?.to_owned(),
            kind: value["kind"].as_str().unwrap_or_default().to_owned(),
            target: value["target"].as_str().unwrap_or_default().to_owned(),
            command: value["command"].as_str().unwrap_or_default().to_owned(),
            state: value["state"].as_str().unwrap_or_default().to_owned(),
            started_ms: value["started_ms"].as_u64().unwrap_or(0),
            finished_ms: value["finished_ms"].as_u64(),
            duration_ms: value["duration_ms"].as_u64().unwrap_or(0),
            project: value["project"].as_str().unwrap_or_default().to_owned(),
            world: value["world"].as_str().unwrap_or_default().to_owned(),
        })
    }

    fn step(&self) -> String {
        match (self.target.is_empty(), self.command.is_empty()) {
            (true, true) => String::new(),
            (true, false) => self.command.clone(),
            (false, true) => self.target.clone(),
            (false, false) if self.target == self.command => self.target.clone(),
            (false, false) => format!("{} {}", self.target, self.command),
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

impl ReportRenderer {
    /// Renders the whole catalog: one dim header row and one row per recorded run.
    #[must_use]
    pub fn runs_lines(&self, rows: &[RunRow]) -> Vec<String> {
        let palette = self.palette();
        let table = Table::new(&COLUMNS);
        let header = [
            palette.meta("#run_id"),
            palette.meta("kind"),
            palette.meta("step"),
            palette.meta("status"),
            palette.meta("duration"),
            palette.meta("recorded"),
        ];
        let body: Vec<[String; 6]> = rows
            .iter()
            .map(|row| {
                let status = match row.state.as_str() {
                    "failed" => palette.error(&row.state),
                    "built" | "pulled" | "ready" => palette.status(&row.state),
                    _ => row.state.clone(),
                };
                [
                    palette.meta(&format!("#{}", row.run_id)),
                    row.kind.clone(),
                    row.step(),
                    status,
                    elapsed_text(row.duration_ms),
                    self.wall().timestamp(row.started_ms),
                ]
            })
            .collect();
        let mut cells: Vec<Vec<String>> = Vec::with_capacity(body.len() + 1);
        cells.push(header.to_vec());
        cells.extend(body.into_iter().map(Vec::from));
        let widths = table.widths(&cells);
        cells.iter().map(|row| table.line(&widths, row)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::RunRow;
    use crate::ui::report::{ReportRenderer, WallZone};
    use crate::ui::table::visible_width;
    use crate::ui::{Terminal, UiMode, strip};

    fn row(run_id: &str, state: &str, duration_ms: u64) -> RunRow {
        RunRow {
            run_id: run_id.to_owned(),
            kind: "setup".to_owned(),
            target: "backend".to_owned(),
            command: "uv sync".to_owned(),
            state: state.to_owned(),
            started_ms: 1_764_000_000_000,
            finished_ms: Some(1_764_000_000_041),
            duration_ms,
            project: "/srv/demo".to_owned(),
            world: "demo".to_owned(),
        }
    }

    fn renderer(color: bool) -> ReportRenderer {
        let mode = if color {
            UiMode::Interactive { color: true }
        } else {
            UiMode::Plain
        };
        let terminal = Terminal {
            cols: 100,
            rows: 30,
            mode,
            stdin_tty: false,
        };
        ReportRenderer::new(terminal, WallZone::fixed(0).expect("utc"))
    }

    fn catalog() -> Vec<RunRow> {
        vec![
            row("b7f2a1", "ready", 41),
            row("c91e04", "failed", 1_234),
            row("0a1b2c", "ready", 7),
        ]
    }

    /// The exact legacy bytes: one dim header row and three pixel-straight catalog rows.
    #[test]
    fn the_catalog_table_keeps_pixel_straight_columns() {
        let lines = renderer(false).runs_lines(&catalog());
        assert_eq!(
            lines,
            vec![
                "#run_id  kind   step             status  duration  recorded".to_owned(),
                "#b7f2a1  setup  backend uv sync  ready       41ms  2025-11-24 16:00:00".to_owned(),
                "#c91e04  setup  backend uv sync  failed      1.2s  2025-11-24 16:00:00".to_owned(),
                "#0a1b2c  setup  backend uv sync  ready        7ms  2025-11-24 16:00:00".to_owned(),
            ],
            "{lines:?}"
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

    /// Colouring never moves a column: the visible bytes and every column start stay identical.
    #[test]
    fn the_colored_catalog_keeps_identical_visible_columns() {
        let plain = renderer(false).runs_lines(&catalog());
        let colored = renderer(true).runs_lines(&catalog());
        assert_eq!(plain.len(), colored.len(), "{colored:?}");
        assert!(
            colored.iter().any(|line| line.contains('\u{1b}')),
            "{colored:?}"
        );
        assert!(
            plain.iter().all(|line| !line.contains('\u{1b}')),
            "{plain:?}"
        );
        for (plain_line, colored_line) in plain.iter().zip(&colored) {
            assert_eq!(
                strip(colored_line).as_ref(),
                plain_line.as_str(),
                "{colored_line:?}"
            );
            assert_eq!(
                visible_width(colored_line),
                visible_width(plain_line),
                "{colored_line:?}"
            );
            assert_eq!(visible_width(colored_line), plain_line.len());
        }
    }

    /// The daemon's `RunRecord` serialization mirrors every catalog field with no escapes.
    #[test]
    fn the_catalog_json_mirrors_the_record_and_is_escape_free() {
        use slipfork_world::run_log::RunRecord;

        let record = RunRecord {
            run_id: "b7f2a1".to_owned(),
            kind: "setup".to_owned(),
            target: "backend".to_owned(),
            command: "uv sync".to_owned(),
            phase: String::new(),
            instruction: String::new(),
            source: String::new(),
            exit_code: None,
            reason: None,
            state: "ready".to_owned(),
            started_ms: 1_764_000_000_000,
            finished_ms: Some(1_764_000_000_041),
            duration_ms: 41,
            project: "/srv/demo".to_owned(),
            world: "demo".to_owned(),
        };
        let value = serde_json::to_value(&record).expect("serialize one run");
        assert_eq!(
            RunRow::from_value(&value),
            Some(row("b7f2a1", "ready", 41)),
            "the CLI projects the daemon's exact record"
        );
        let line = serde_json::to_string(&value).expect("serialize one run");
        assert!(line.starts_with("{\"command\":\"uv sync\""), "{line}");
        for field in [
            "\"run_id\":\"b7f2a1\"",
            "\"kind\":\"setup\"",
            "\"target\":\"backend\"",
            "\"state\":\"ready\"",
            "\"started_ms\":1764000000000",
            "\"finished_ms\":1764000000041",
            "\"duration_ms\":41",
            "\"project\":\"/srv/demo\"",
            "\"world\":\"demo\"",
        ] {
            assert!(line.contains(field), "missing {field}: {line}");
        }
        assert!(
            !line.contains("\"name\""),
            "the bare field is `target`: {line}"
        );
        assert!(!line.contains('\u{1b}'), "{line}");
        assert!(!line.contains('\u{7}'), "{line}");
    }

    /// `@<tool>` setup nodes carry `target == command`; the step renders that pair exactly once.
    #[test]
    fn the_step_never_repeats_an_identical_target_and_command() {
        let mut run = row("b7f2a1", "ready", 41);
        run.target = "uv sync".to_owned();
        run.command = "uv sync".to_owned();
        assert_eq!(run.step(), "uv sync");

        // A differing target and command still render both halves.
        run.target = "install".to_owned();
        assert_eq!(run.step(), "install uv sync");

        // One-sided and fully-empty rows keep their existing fallbacks.
        run.target = String::new();
        assert_eq!(run.step(), "uv sync");
        run.command = String::new();
        assert_eq!(run.step(), "");
        run.target = "backend".to_owned();
        assert_eq!(run.step(), "backend");

        // The rendered table itself carries the deduplicated step and zero escapes.
        run.target = "uv sync".to_owned();
        run.command = "uv sync".to_owned();
        let lines = renderer(false).runs_lines(&[run]);
        assert!(lines[1].contains("uv sync"), "{lines:?}");
        assert!(!lines[1].contains("uv sync uv sync"), "{lines:?}");
        assert!(!lines[1].contains('\u{1b}'), "{lines:?}");
    }
}
