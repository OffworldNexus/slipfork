//! UTF-16-aware text buffers and LSP position mapping.

use serde_json::{Value, json};
use slipfork_forkfile::Span;

/// A zero-based line/character position counted in UTF-16 code units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    /// Zero-based line index.
    pub line: u32,
    /// Zero-based UTF-16 code-unit offset within the line.
    pub character: u32,
}

impl Position {
    /// Builds a position.
    #[must_use]
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }

    /// Encodes the position as the LSP JSON shape.
    #[must_use]
    pub fn to_value(self) -> Value {
        json!({ "line": self.line, "character": self.character })
    }

    /// Decodes an LSP JSON position.
    #[must_use]
    pub fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            line: u32::try_from(value.get("line")?.as_u64()?).ok()?,
            character: u32::try_from(value.get("character")?.as_u64()?).ok()?,
        })
    }
}

/// A half-open LSP range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    /// Inclusive start.
    pub start: Position,
    /// Exclusive end.
    pub end: Position,
}

impl Range {
    /// Builds a range.
    #[must_use]
    pub const fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }

    /// Encodes the range as the LSP JSON shape.
    #[must_use]
    pub fn to_value(self) -> Value {
        json!({ "start": self.start.to_value(), "end": self.end.to_value() })
    }

    /// Decodes an LSP JSON range.
    #[must_use]
    pub fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            start: Position::from_value(value.get("start")?)?,
            end: Position::from_value(value.get("end")?)?,
        })
    }

    /// Whether `position` falls inside the range (or on an empty range).
    #[must_use]
    pub fn contains(self, position: Position) -> bool {
        self.start <= position
            && (position < self.end || (self.start == self.end && position == self.start))
    }
}

/// A UTF-8 source buffer with a line index for fast offset/position mapping.
#[derive(Debug, Clone)]
pub struct TextDocument {
    text: String,
    line_starts: Vec<usize>,
}

impl TextDocument {
    /// Builds a document and indexes its line starts.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let line_starts = line_starts(&text);
        Self { text, line_starts }
    }

    /// The document text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Maps a UTF-8 byte offset to a UTF-16 LSP position.
    #[must_use]
    pub fn position(&self, offset: usize) -> Position {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = self
            .line_starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1);
        let line_start = self.line_starts[line];
        let character: usize = self.text[line_start..offset]
            .chars()
            .map(char::len_utf16)
            .sum();
        Position {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            character: u32::try_from(character).unwrap_or(u32::MAX),
        }
    }

    /// Maps a UTF-16 LSP position to a UTF-8 byte offset, clamped to the line.
    #[must_use]
    pub fn offset(&self, position: Position) -> usize {
        let line = (position.line as usize).min(self.line_starts.len().saturating_sub(1));
        let start = self.line_starts[line];
        let end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.text.len());
        let mut units = 0_usize;
        for (index, character) in self.text[start..end].char_indices() {
            if units >= position.character as usize {
                return start + index;
            }
            units += character.len_utf16();
        }
        end
    }

    /// Byte offset of the first character on a zero-based line.
    #[must_use]
    pub fn line_start(&self, line: u32) -> usize {
        self.line_starts
            .get(line as usize)
            .copied()
            .unwrap_or(self.text.len())
    }

    /// The text of a zero-based line without its terminator.
    #[must_use]
    pub fn line_text(&self, line: u32) -> &str {
        let start = self.line_start(line);
        let end = self
            .line_starts
            .get(line as usize + 1)
            .copied()
            .unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\r', '\n'])
    }

    /// The position one past the final byte.
    #[must_use]
    pub fn end_position(&self) -> Position {
        self.position(self.text.len())
    }

    /// Replaces `range` (or the whole document) with `new_text`.
    pub fn apply(&mut self, range: Option<Range>, new_text: &str) {
        let (start, end) = match range {
            Some(range) => (self.offset(range.start), self.offset(range.end)),
            None => (0, self.text.len()),
        };
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        self.text.replace_range(start..end, new_text);
        self.line_starts = line_starts(&self.text);
    }

    /// Converts a parser span into an LSP range.
    #[must_use]
    pub fn range(&self, span: Span) -> Range {
        Range::new(
            self.position(span.start.offset),
            self.position(span.end.offset),
        )
    }
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

/// Finds `word` in `haystack` on identifier boundaries.
pub(crate) fn find_word(haystack: &str, word: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(index) = haystack[from..].find(word) {
        let start = from + index;
        let end = start + word.len();
        let before = haystack[..start].chars().next_back();
        let after = haystack[end..].chars().next();
        if !before.is_some_and(is_word) && !after.is_some_and(is_word) {
            return Some(start);
        }
        from = end;
    }
    None
}

fn is_word(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_' || character == '-'
}

#[cfg(test)]
mod tests {
    use super::{Position, Range, TextDocument};

    #[test]
    fn astral_and_accented_columns_are_utf16() {
        let document = TextDocument::new("# forkfile: 0.1\n# 🚀 café\n");
        let emoji = document.text().find('🚀').expect("emoji");
        assert_eq!(document.position(emoji), Position::new(1, 2));
        assert_eq!(
            document.position(emoji + '🚀'.len_utf8()),
            Position::new(1, 4)
        );
        let accented = document.text().find('é').expect("accent");
        assert_eq!(document.position(accented), Position::new(1, 8));
        assert_eq!(document.offset(Position::new(1, 8)), accented);
        assert_eq!(document.offset(Position::new(1, 4)), emoji + 4);
    }

    #[test]
    fn incremental_edits_round_trip_utf16() {
        let mut document = TextDocument::new("a 🚀 b\n");
        document.apply(
            Some(Range::new(Position::new(0, 2), Position::new(0, 4))),
            "🌍🌍",
        );
        assert_eq!(document.text(), "a 🌍🌍 b\n");
        assert_eq!(
            document.position(document.text().find('b').unwrap()),
            Position::new(0, 7)
        );
    }
}
