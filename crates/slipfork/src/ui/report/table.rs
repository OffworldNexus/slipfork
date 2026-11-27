//! One borderless, ANSI-aware column engine for every human table.
//!
//! A [`Table`] is a plain value: a fixed list of [`Column`] alignments, one gap string, and an
//! optional indent. It draws **no borders of any kind** — no ASCII or Unicode boxes — so straight
//! column starts are the whole visual contract. Widths are measured with
//! [`console::measure_text_width`], which strips `ESC[` sequences first, so a pre-styled cell lines
//! up exactly like a plain one and a `NO_COLOR`/piped/`--json` render still emits zero escapes.
//!
//! [`ColumnWidths`] is precomputed from a complete known row set. A batch caller measures once and
//! renders every row, while a streaming caller measures from its plan and renders each row as it
//! arrives — either way the grid is identical.

use crate::ui::visible_width;

/// The default gap between two adjacent columns.
pub const GAP: &str = "  ";

/// Horizontal alignment of one column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// Pad on the right.
    Left,
    /// Pad on the left.
    Right,
}

/// One column: an alignment plus a reserved minimum width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Column {
    align: Align,
    min: usize,
}

impl Column {
    /// A left-aligned column.
    #[must_use]
    pub const fn left() -> Self {
        Self {
            align: Align::Left,
            min: 0,
        }
    }

    /// A right-aligned column; numeric and duration columns live here.
    #[must_use]
    pub const fn right() -> Self {
        Self {
            align: Align::Right,
            min: 0,
        }
    }

    /// Reserves at least `min` visible cells even when every row is empty today (a wired slot).
    #[must_use]
    pub const fn at_least(self, min: usize) -> Self {
        Self { min, ..self }
    }
}

/// The resolved visible width of every column in one table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnWidths {
    widths: Vec<usize>,
}

impl ColumnWidths {
    /// Measures a complete row set, honouring every column minimum.
    #[must_use]
    pub fn measure(columns: &[Column], rows: &[Vec<String>]) -> Self {
        let mut widths: Vec<usize> = columns.iter().map(|column| column.min).collect();
        for row in rows {
            for (index, cell) in row.iter().enumerate() {
                if let Some(width) = widths.get_mut(index) {
                    *width = (*width).max(visible_width(cell));
                }
            }
        }
        Self { widths }
    }
}

/// A borderless table: the column grid plus its gap and indent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Table<'a> {
    columns: &'a [Column],
    gap: &'a str,
    indent: &'a str,
}

impl<'a> Table<'a> {
    /// Builds a table over `columns` with [`GAP`] and no indent.
    #[must_use]
    pub const fn new(columns: &'a [Column]) -> Self {
        Self {
            columns,
            gap: GAP,
            indent: "",
        }
    }

    /// Sets the gap drawn between two adjacent columns.
    #[must_use]
    pub const fn gap(mut self, gap: &'a str) -> Self {
        self.gap = gap;
        self
    }

    /// Prefixes every row with `indent`.
    #[must_use]
    pub const fn indent(mut self, indent: &'a str) -> Self {
        self.indent = indent;
        self
    }

    /// The precomputed grid for a complete row set.
    #[must_use]
    pub fn widths(&self, rows: &[Vec<String>]) -> ColumnWidths {
        ColumnWidths::measure(self.columns, rows)
    }

    /// Renders one row of already-styled cells against a precomputed grid.
    ///
    /// A zero-width column (absent from every row and with no minimum) is skipped together with
    /// its gap, and the row's trailing padding is trimmed, so an optional slot never leaves a
    /// ragged tail.
    #[must_use]
    pub fn line<S: AsRef<str>>(&self, widths: &ColumnWidths, cells: &[S]) -> String {
        let mut out = String::from(self.indent);
        let mut wrote = false;
        for (index, column) in self.columns.iter().enumerate() {
            let width = column
                .min
                .max(widths.widths.get(index).copied().unwrap_or(0));
            if width == 0 {
                continue;
            }
            if wrote {
                out.push_str(self.gap);
            }
            let cell = cells.get(index).map_or("", AsRef::as_ref);
            push_padded(&mut out, cell, width, column.align);
            wrote = true;
        }
        while out.ends_with(' ') {
            out.pop();
        }
        out
    }
}

fn push_padded(out: &mut String, cell: &str, width: usize, align: Align) {
    let fill = width.saturating_sub(visible_width(cell));
    match align {
        Align::Left => {
            out.push_str(cell);
            for _ in 0..fill {
                out.push(' ');
            }
        }
        Align::Right => {
            for _ in 0..fill {
                out.push(' ');
            }
            out.push_str(cell);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Column, ColumnWidths, Table};
    use crate::ui::report::visible_width;

    const PREFIX: [Column; 2] = [Column::left(), Column::right().at_least(6)];

    #[test]
    fn right_alignment_puts_every_duration_on_one_edge() {
        let table = Table::new(&PREFIX);
        let rows = vec![
            vec!["build".to_owned(), "3ms".to_owned()],
            vec!["cached".to_owned(), "812ms".to_owned()],
            vec!["pull".to_owned(), "7.3s".to_owned()],
        ];
        let widths = table.widths(&rows);
        let lines: Vec<String> = rows.iter().map(|row| table.line(&widths, row)).collect();
        assert_eq!(
            lines,
            vec![
                format!("{:<6}  {:>6}", "build", "3ms"),
                format!("{:<6}  {:>6}", "cached", "812ms"),
                format!("{:<6}  {:>6}", "pull", "7.3s"),
            ]
        );
        let end = lines[0].trim_end().len();
        for line in &lines {
            assert_eq!(line.trim_end().len(), end, "one right edge: {lines:?}");
        }
        assert!(lines[0].ends_with("3ms"));
        assert!(lines[1].ends_with("812ms"));
        assert!(lines[2].ends_with("7.3s"));
    }

    #[test]
    fn styled_cells_measure_like_their_plain_text() {
        let columns = [Column::left(), Column::right()];
        let table = Table::new(&columns);
        let styled = crate::ui::report::Palette::new(true).ready("api");
        let rows = vec![vec![styled.clone(), "3ms".to_owned()]];
        let widths = table.widths(&rows);
        assert_eq!(visible_width(&styled), 3);

        let coloured = table.line(&widths, &rows[0]);
        assert!(coloured.contains('\u{1b}'), "{coloured:?}");
        let plain = table.line(&widths, &["api".to_owned(), "3ms".to_owned()]);
        assert_eq!(visible_width(&coloured), visible_width(&plain));
        assert!(!plain.contains('\u{1b}'), "{plain:?}");
        for border in ['│', '─', '┌', '┐', '└', '┘', '├', '┤', '┼', '+', '|'] {
            assert!(!plain.contains(border), "borderless: {plain:?}");
        }
    }

    #[test]
    fn an_absent_optional_column_never_leaves_a_ragged_tail() {
        let columns = [Column::left(), Column::left(), Column::left().at_least(7)];
        let table = Table::new(&columns);
        let widths =
            ColumnWidths::measure(&columns, &[vec!["image".to_owned(), "base".to_owned()]]);
        assert_eq!(table.line(&widths, &["image", "base"]), "image  base");
        assert_eq!(
            table.line(&widths, &["image", "base", ""]),
            "image  base",
            "the reserved slot stays empty and unwritten"
        );
    }
}
