//! Source positions and spans shared by the lexer, parser, and diagnostics.

/// A 1-based line/column position paired with its UTF-8 byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    /// 1-based line number.
    pub line: u32,
    /// 1-based column number, counted in characters.
    pub col: u32,
    /// 0-based byte offset into the source.
    pub offset: usize,
}

impl Position {
    /// Builds a position.
    #[must_use]
    pub const fn new(line: u32, col: u32, offset: usize) -> Self {
        Self { line, col, offset }
    }
}

/// A half-open source region `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// First character of the region.
    pub start: Position,
    /// One past the last character of the region.
    pub end: Position,
}

impl Span {
    /// Builds a span from two positions.
    #[must_use]
    pub const fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }

    /// Builds an empty span anchored at a single position.
    #[must_use]
    pub const fn at(position: Position) -> Self {
        Self {
            start: position,
            end: position,
        }
    }

    /// Extends this span so it covers `other` as well.
    #[must_use]
    pub const fn merge(self, other: Self) -> Self {
        Self {
            start: self.start,
            end: other.end,
        }
    }

    /// Returns the 1-based line of the span start.
    #[must_use]
    pub const fn line(&self) -> u32 {
        self.start.line
    }

    /// Returns the 1-based column of the span start.
    #[must_use]
    pub const fn col(&self) -> u32 {
        self.start.col
    }
}
