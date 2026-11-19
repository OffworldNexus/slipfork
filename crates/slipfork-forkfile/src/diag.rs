//! Source spans, multi-error diagnostics, and the rustc-style caret renderer.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A byte range in the original `Forkfile`, with its `1`-based line and column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// Byte offset of the first character.
    pub offset: usize,
    /// Byte length of the ranged text.
    pub len: usize,
    /// `1`-based source line.
    pub line: u32,
    /// `1`-based source column.
    pub col: u32,
}

impl Span {
    /// Build a span from an explicit offset, length, line, and column.
    pub const fn new(offset: usize, len: usize, line: u32, col: u32) -> Self {
        Self {
            offset,
            len,
            line,
            col,
        }
    }

    /// A span for diagnostics that have no better anchor (the start of the manifest).
    pub const fn zero() -> Self {
        Self {
            offset: 0,
            len: 0,
            line: 1,
            col: 1,
        }
    }

    /// The smallest span covering both inputs.
    pub fn merge(self, other: Self) -> Self {
        let offset = self.offset.min(other.offset);
        let end = (self.offset + self.len).max(other.offset + other.len);
        let (line, col) = if self.offset <= other.offset {
            (self.line, self.col)
        } else {
            (other.line, other.col)
        };
        Self {
            offset,
            len: end.saturating_sub(offset),
            line,
            col,
        }
    }
}

/// Byte offsets of every source line start, used to turn lexer offsets into [`Span`]s.
#[derive(Debug, Clone)]
pub struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    /// Index every line start of `source`.
    pub fn new(source: &str) -> Self {
        let mut starts = vec![0];
        for (offset, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                starts.push(offset + 1);
            }
        }
        Self { starts }
    }

    /// Compute the `1`-based [`Span`] of `len` bytes at `offset`.
    pub fn span(&self, source: &str, offset: usize, len: usize) -> Span {
        let offset = offset.min(source.len());
        let line_index = match self.starts.binary_search(&offset) {
            Ok(index) => index,
            Err(index) => index.saturating_sub(1),
        };
        let line_start = self.starts[line_index];
        let col = source
            .get(line_start..offset)
            .map_or(1, |prefix| prefix.chars().count() + 1);
        Span {
            offset,
            len,
            line: (line_index + 1) as u32,
            col: col as u32,
        }
    }
}

/// One static diagnostic: a message, the offending span, and an optional remedial hint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// The human-readable error message (one line).
    pub message: String,
    /// The span the message points at.
    pub span: Span,
    /// An optional one-line remedy appended after the caret.
    pub hint: Option<String>,
}

impl Diagnostic {
    /// Build a diagnostic anchored at `span`.
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            span,
            hint: None,
        }
    }

    /// Attach a one-line remedy.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Render the rustc-style caret snippet for this diagnostic against `source`.
    pub fn render(&self, source: &str, file: &str) -> String {
        let line_number = self.span.line.max(1) as usize;
        let col = self.span.col.max(1) as usize;
        let line_text = source.lines().nth(line_number - 1).unwrap_or_default();
        let gutter = line_number.to_string().len().max(1);
        let arrow_pad = " ".repeat(gutter);
        let bar_pad = " ".repeat(gutter + 1);
        let visible = line_text.chars().count();
        let available = visible.saturating_sub(col - 1).max(1);
        let carets = self.span.len.max(1).min(available);
        let mut rendered = format!(
            "error: {}\n{arrow_pad}--> {file}:{line_number}:{col}\n{bar_pad}|\n{line_number} | {line_text}\n{bar_pad}| {}{}",
            self.message,
            " ".repeat(col - 1),
            "^".repeat(carets)
        );
        if let Some(hint) = &self.hint {
            rendered.push(' ');
            rendered.push_str(hint);
        }
        rendered
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Forkfile:{}:{}: error: {}",
            self.span.line, self.span.col, self.message
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str =
        "features {\n    default = [dev]\n}\n\nenv common {\n    DATABASE_URL = pg.url\n}\n";

    #[test]
    fn caret_snippet_aligns_under_the_offending_span() {
        let index = LineIndex::new(SOURCE);
        let offset = SOURCE.find("pg.url").expect("reference");
        let span = index.span(SOURCE, offset, "pg.url".len());
        assert_eq!((span.line, span.col), (6, 20));

        let diagnostic = Diagnostic::new(span, "`pg` is a postgres cluster, not a database")
            .with_hint("did you mean database `app_db.url`?");
        assert_eq!(
            diagnostic.render(SOURCE, "Forkfile"),
            concat!(
                "error: `pg` is a postgres cluster, not a database\n",
                " --> Forkfile:6:20\n",
                "  |\n",
                "6 |     DATABASE_URL = pg.url\n",
                "  |                    ^^^^^^ did you mean database `app_db.url`?"
            )
        );
    }

    #[test]
    fn merging_spans_keeps_the_earliest_position() {
        let index = LineIndex::new(SOURCE);
        let first = index.span(SOURCE, 0, 8);
        let second = index.span(SOURCE, 10, 4);
        let merged = first.merge(second);
        assert_eq!((merged.line, merged.col), (1, 1));
        assert_eq!(merged.offset, 0);
        assert_eq!(merged.len, 14);
    }
}
