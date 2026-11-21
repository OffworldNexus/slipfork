//! YAML line/column lookup shared by migration diagnostics.

use crate::span::Position;

/// Converts a 1-based YAML line/column to a byte-offset position.
pub(crate) fn position(source: &str, line: u32, column: u32) -> Position {
    let mut offset = 0;
    for (index, text) in source.lines().enumerate() {
        if index as u32 + 1 == line {
            let column = column.max(1) as usize;
            let in_line = text
                .char_indices()
                .nth(column - 1)
                .map_or(text.len(), |(byte, _)| byte);
            return Position::new(line, column as u32, offset + in_line);
        }
        offset += text.len() + 1;
    }
    Position::new(1, 1, 0)
}

/// Finds the first mapping key (or scalar list item) matching `path`.
pub(crate) fn find(source: &str, path: &[&str]) -> Option<Position> {
    if path.is_empty() {
        return None;
    }
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut offset = 0;
    for (index, raw_line) in source.lines().enumerate() {
        let line = raw_line.trim_end();
        let content = line.trim_start();
        let content = content.strip_prefix("- ").unwrap_or(content);
        if content.is_empty() || content.starts_with('#') || content.starts_with('-') {
            offset += raw_line.len() + 1;
            continue;
        }
        let key = match content.split_once(':') {
            Some((key, _)) => key
                .trim()
                .trim_matches(|character| character == '"' || character == '\''),
            None => content.trim(),
        };
        if key.is_empty() {
            offset += raw_line.len() + 1;
            continue;
        }
        let indent = raw_line.len() - raw_line.trim_start_matches(' ').len();
        let indent = if raw_line.trim_start().starts_with("- ") {
            indent + 2
        } else {
            indent
        };
        while stack
            .last()
            .is_some_and(|(existing, _)| *existing >= indent)
        {
            stack.pop();
        }
        stack.push((indent, key.to_string()));
        if stack.len() >= path.len()
            && stack[stack.len() - path.len()..]
                .iter()
                .zip(path)
                .all(|((_, existing), expected)| existing == expected)
        {
            let column = raw_line.find(key).unwrap_or(0) + 1;
            return Some(Position::new(
                index as u32 + 1,
                column as u32,
                offset + column - 1,
            ));
        }
        offset += raw_line.len() + 1;
    }
    None
}

/// Returns `path`'s position, or line 1 when the path cannot be found.
pub(crate) fn locate(source: &str, path: &[&str]) -> Position {
    find(source, path).unwrap_or(Position::new(1, 1, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_mapping_keys_resolve_to_their_line_and_column() {
        let source = "modes:\n  dev:\n    components:\n      api:\n        disabled: true\n";
        let found =
            find(source, &["modes", "dev", "components", "api", "disabled"]).expect("nested key");
        assert_eq!(found.line, 5);
        assert_eq!(found.col, 9);
    }

    #[test]
    fn scalar_list_items_resolve() {
        let source = "domains:\n  - cache\n";
        let found = find(source, &["domains", "cache"]).expect("scalar list item");
        assert_eq!(found.line, 2);
        assert_eq!(found.col, 5);
    }
}
