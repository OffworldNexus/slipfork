//! The one child-output filter: drop escapes a viewer cannot take, keep only colour where it can.
//!
//! Child programs colourise because the pod env asks them to (`CLICOLOR_FORCE`), and the daemon
//! stores their bytes raw so a later real terminal can show them. Every viewer resolves one of two
//! views here: [`strip`] removes every escape for a pipe/`NO_COLOR`/`--json`, while [`colour_only`]
//! keeps SGR colour but drops cursor moves, alt-screen switches, and OSC hyperlinks so an untrusted
//! child can never corrupt our pane. [`visible_width`] measures the printed cells of either view.

use std::borrow::Cow;

/// Removes SGR/CSI, OSC/DCS, and other `ESC`-introduced sequences from `line`.
///
/// A line with no `ESC` is returned borrowed; colour bytes stay untouched unless the caller asks.
#[must_use]
pub fn strip(line: &str) -> Cow<'_, str> {
    if !line.contains('\u{1b}') {
        return Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len());
    for (slice, escape) in segments(line) {
        if !escape {
            out.push_str(slice);
        }
    }
    Cow::Owned(out)
}

/// Keeps only SGR colour sequences and printable text; drops every other escape and control byte.
///
/// A colour terminal may show an application's `\x1b[31m…`, but never its `\x1b[2J`, OSC 8 link,
/// bare `\r`, or other terminal controls: our gutter owns the pane.
#[must_use]
pub fn colour_only(line: &str) -> Cow<'_, str> {
    if !line.contains('\u{1b}') && !line.chars().any(is_control) {
        return Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len());
    for (slice, escape) in segments(line) {
        if escape {
            if is_sgr(slice) {
                out.push_str(slice);
            }
        } else {
            out.extend(slice.chars().filter(|ch| !is_control(*ch)));
        }
    }
    Cow::Owned(out)
}

/// The visible cell width of `text`, ignoring every escape sequence.
#[must_use]
pub fn visible_width(text: &str) -> usize {
    console::measure_text_width(&strip(text))
}

/// Splits `text` into zero-width escape sequences and plain text runs, in order.
///
/// Wrapping a line with escapes must never split a sequence; this iterator is the one tokenizer.
#[must_use]
pub fn segments(text: &str) -> Segments<'_> {
    Segments { rest: text }
}

/// One zero-width escape sequence or one plain text run of a line.
pub struct Segments<'a> {
    rest: &'a str,
}

impl<'a> Iterator for Segments<'a> {
    /// `(slice, is_escape)`; the slice is a borrow of the source line.
    type Item = (&'a str, bool);

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let Some(position) = self.rest.find('\u{1b}') else {
            let all = self.rest;
            self.rest = "";
            return Some((all, false));
        };
        if position > 0 {
            let (plain, tail) = self.rest.split_at(position);
            self.rest = tail;
            return Some((plain, false));
        }
        let bytes = self.rest.as_bytes();
        let end = match bytes.get(1).copied() {
            Some(b'[') => {
                let mut end = 2;
                while end < bytes.len() {
                    let byte = bytes[end];
                    end += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
                end
            }
            Some(b']' | b'P' | b'_' | b'^') => {
                let mut end = 2;
                while end < bytes.len() {
                    let byte = bytes[end];
                    end += 1;
                    if byte == 0x07 {
                        break;
                    }
                    if byte == 0x1b {
                        if bytes.get(end) == Some(&b'\\') {
                            end += 1;
                        }
                        break;
                    }
                }
                end
            }
            Some(byte) => {
                if (0x20..=0x2f).contains(&byte) && bytes.len() > 2 {
                    3
                } else {
                    2
                }
            }
            None => bytes.len(),
        };
        let (escape, tail) = self.rest.split_at(end);
        self.rest = tail;
        Some((escape, true))
    }
}

/// A C0 control byte other than a tab, or `DEL`: never printable in our gutter.
fn is_control(ch: char) -> bool {
    (ch < '\u{20}' && ch != '\t') || ch == '\u{7f}'
}

/// `true` when `escape` is a complete SGR (`ESC [ … m`) sequence.
fn is_sgr(escape: &str) -> bool {
    escape.len() >= 3 && escape.starts_with("\u{1b}[") && escape.ends_with('m')
}

#[cfg(test)]
mod tests {
    use super::{colour_only, segments, strip, visible_width};

    #[test]
    fn sgr_and_cursor_sequences_disappear_while_text_stays() {
        assert_eq!(strip("\u{1b}[31mred\u{1b}[0m"), "red");
        assert_eq!(strip("\u{1b}[1;38;5;196mhot\u{1b}[m!"), "hot!");
        assert_eq!(strip("a\u{1b}[2Kb"), "ab");
        assert_eq!(strip("plain"), "plain");
        assert!(strip("\u{1b}[0m").is_empty());
    }

    #[test]
    fn hyperlinks_and_two_byte_escapes_are_removed_too() {
        assert_eq!(
            strip("\u{1b}]8;;https://example.test\u{07}link\u{1b}]8;;\u{07}"),
            "link"
        );
        assert_eq!(strip("\u{1b}(Btext"), "text");
        assert!(!strip("x\u{1b}]8;;u\u{1b}\\y").contains('\u{1b}'));
    }

    #[test]
    fn colour_only_keeps_sgr_but_drops_terminal_controls() {
        assert_eq!(
            colour_only("\u{1b}[31mred\u{1b}[0m"),
            "\u{1b}[31mred\u{1b}[0m"
        );
        assert_eq!(colour_only("\u{1b}[2Jgone\u{1b}[?1049h"), "gone");
        assert_eq!(
            colour_only("\u{1b}]8;;https://x\u{07}link\u{1b}]8;;\u{07}"),
            "link"
        );
        assert_eq!(colour_only("one\rtwo"), "onetwo");
        assert_eq!(colour_only("plain"), "plain");
    }

    #[test]
    fn segments_never_split_a_sequence_and_width_ignores_it() {
        let line = "a\u{1b}[38;2;1;2;3mb\u{1b}[0m";
        let parts: Vec<(&str, bool)> = segments(line).collect();
        assert_eq!(
            parts,
            vec![
                ("a", false),
                ("\u{1b}[38;2;1;2;3m", true),
                ("b", false),
                ("\u{1b}[0m", true),
            ]
        );
        assert_eq!(visible_width(line), 2);
        assert_eq!(visible_width("red"), 3);
    }
}
