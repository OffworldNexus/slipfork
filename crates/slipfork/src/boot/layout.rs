//! Pure phase-row layout text: identity columns, gutter lines, and the build-end row.
//!
//! Nothing here touches terminal state or an event frame, so [`super::build`] can own the state
//! machine while every byte it prints stays testable in isolation. A `NO_COLOR` palette returns
//! these same strings with zero escapes.

use crate::ui::Palette;
use crate::ui::elapsed_text;

/// The widest instruction and target column painted so far for the running image.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Columns {
    pub(super) verb: usize,
    pub(super) target: usize,
}

impl Columns {
    pub(super) fn observe(&mut self, verb: &str, target: &str) {
        self.verb = self.verb.max(verb.chars().count());
        self.target = self.target.max(target.chars().count());
    }
}

/// One streamed child line in the gutter shape: `  │ text` (stdout) / `  ┃ text` (stderr).
pub(super) fn gutter_line(stream: &str, text: &str, width: usize, palette: Palette) -> String {
    let text = truncate_visible(text, width.saturating_sub(4));
    format!("  {} {text}", palette.gutter(stream))
}

/// A scrollback line in full: wrapped at `width`, the gutter glyph repeated on every row.
pub(super) fn wrap_gutter(stream: &str, text: &str, width: usize, palette: Palette) -> Vec<String> {
    let budget = width.saturating_sub(4).max(8);
    let glyph = palette.gutter(stream);
    if text.is_empty() {
        return vec![format!("  {glyph} ")];
    }
    text.chars()
        .collect::<Vec<_>>()
        .chunks(budget)
        .map(|chunk| format!("  {glyph} {}", chunk.iter().collect::<String>()))
        .collect()
}

/// The build-end row: `✓ image web (4 steps, 1.2s)`.
pub(super) fn image_done_row(image: &str, steps: usize, millis: u64, palette: Palette) -> String {
    let plural = if steps == 1 { "step" } else { "steps" };
    format!(
        "{} {} {} {}",
        palette.icon_ok(),
        palette.dim("image"),
        palette.tint(image, "1"),
        palette.dim(&format!("({steps} {plural}, {})", elapsed_text(millis))),
    )
}

/// Pads `text` on the right to `width` plain characters.
pub(super) fn pad(text: &str, width: usize) -> String {
    let length = text.chars().count();
    if length >= width {
        return text.to_owned();
    }
    format!("{text}{}", " ".repeat(width - length))
}

/// Truncates `text` to at most `max` visible characters, ending in `…` when it was cut.
pub(super) fn truncate_visible(text: &str, max: usize) -> String {
    let length = text.chars().count();
    if length <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let head: String = text.chars().take(max - 1).collect();
    format!("{head}…")
}
