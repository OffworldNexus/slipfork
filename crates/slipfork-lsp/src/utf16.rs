//! UTF-16 position conversion.
//!
//! The canonical `Forkfile` spans carry 1-based `char` columns, while LSP — and specifically Zed,
//! which ignores the negotiated `positionEncoding` — always speaks UTF-16 code units. A non-BMP
//! emoji therefore occupies two columns, one more than `chars().count()` reports.

use slipfork_forkfile::Span;

/// Convert an LSP `(line, character)` pair into a byte offset in `text`.
///
/// `line` is 0-based and `character` counts UTF-16 code units. A character inside a surrogate pair
/// clamps to the end of that scalar. A line past the end clamps to end of document.
pub fn position_to_offset(text: &str, line: u32, character: u32) -> Option<usize> {
    let mut offset = 0usize;
    for (index, current) in text.split('\n').enumerate() {
        if index as u32 == line {
            let mut units = 0u32;
            for ch in current.chars() {
                if units >= character {
                    break;
                }
                units += ch.len_utf16() as u32;
                offset += ch.len_utf8();
            }
            return Some(offset);
        }
        offset += current.len() + 1;
    }
    (line as usize >= text.split('\n').count()).then_some(text.len())
}

/// Convert a byte offset into an LSP `(line, character)` pair.
pub fn offset_to_position(text: &str, offset: usize) -> (u32, u32) {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let mut line = 0u32;
    let mut line_start = 0usize;
    for (index, byte) in text.bytes().enumerate().take(offset) {
        if byte == b'\n' {
            line += 1;
            line_start = index + 1;
        }
    }
    let prefix = text.get(line_start..offset).unwrap_or_default();
    (line, prefix.chars().map(|ch| ch.len_utf16() as u32).sum())
}

/// Convert a 1-based `char` column on a 1-based line into a 0-based UTF-16 column.
pub fn char_column_to_utf16(text: &str, line: u32, column: u32) -> u32 {
    let line_text = text
        .split('\n')
        .nth(line.saturating_sub(1) as usize)
        .unwrap_or_default();
    line_text
        .chars()
        .take(column.saturating_sub(1) as usize)
        .map(|ch| ch.len_utf16() as u32)
        .sum()
}

/// The LSP range for a canonical [`Span`]: a start from its `char` column, an end from its bytes.
pub fn span_range(text: &str, span: &Span) -> ((u32, u32), (u32, u32)) {
    let start = (
        span.line.saturating_sub(1),
        char_column_to_utf16(text, span.line, span.col),
    );
    let end = offset_to_position(text, span.offset.saturating_add(span.len));
    (start, end)
}

/// The range covering the whole document, used by a full-document format edit.
pub fn document_range(text: &str) -> ((u32, u32), (u32, u32)) {
    ((0, 0), offset_to_position(text, text.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_bmp_emoji_shifts_columns_by_one_utf16_unit() {
        let line = "    A = \"🔑\" + missing";
        assert_eq!(char_column_to_utf16(line, 1, 15), 15);
        assert_eq!(char_column_to_utf16(line, 1, 10), 9);

        let text = format!("env emoji {{\n{line}\n}}\n");
        let offset = text.find("missing").expect("identifier");
        assert_eq!(offset_to_position(&text, offset), (1, 15));
        assert_eq!(position_to_offset(&text, 1, 15), Some(offset));
        assert_eq!(position_to_offset(&text, 9, 0), Some(text.len()));
    }

    #[test]
    fn a_column_inside_a_surrogate_pair_clamps_to_the_scalar_end() {
        let text = "🔑x";
        assert_eq!(position_to_offset(text, 0, 0), Some(0));
        assert_eq!(position_to_offset(text, 0, 1), Some(4));
        assert_eq!(position_to_offset(text, 0, 2), Some(4));
        assert_eq!(position_to_offset(text, 0, 3), Some(5));
    }
}
