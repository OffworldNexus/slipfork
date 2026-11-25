//! Pure phase-row layout text: identity columns, gutter lines, and the build-end row.
//!
//! Nothing here touches terminal state or an event frame, so [`super::build`] can own the state
//! machine while every byte it prints stays testable in isolation. A `NO_COLOR` palette returns
//! these same strings with zero escapes.

use crate::ui::Palette;

use super::render::{OuterWidths, outer_row};

/// The widest instruction/verb and target column painted for a build or outer boot step.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Columns {
    pub(super) verb: usize,
    pub(super) target: usize,
}

impl Columns {
    pub(super) const fn from_outer(widths: OuterWidths) -> Self {
        Self {
            verb: widths.kind,
            target: widths.target,
        }
    }

    pub(super) const fn as_outer(self) -> OuterWidths {
        OuterWidths {
            kind: self.verb,
            target: self.target,
            state: 0,
        }
    }

    #[cfg(test)]
    pub(super) fn observe(&mut self, verb: &str, target: &str) {
        self.verb = self.verb.max(verb.chars().count());
        self.target = self.target.max(target.chars().count());
    }
}

/// One streamed child line in the gutter shape: `{indent}│ text` / `{indent}┃ text`.
///
/// The indent follows the level of the row that owns the output: `2` under an outer boot step,
/// `4` under an indented inner Dockerfile step.
pub(super) fn gutter_line(
    stream: &str,
    text: &str,
    width: usize,
    palette: Palette,
    indent: usize,
) -> String {
    let text = truncate_visible(text, width.saturating_sub(indent + 2));
    format!("{}{} {text}", " ".repeat(indent), palette.gutter(stream))
}

/// A scrollback line in full: wrapped at `width`, the gutter glyph repeated on every row.
pub(super) fn wrap_gutter(
    stream: &str,
    text: &str,
    width: usize,
    palette: Palette,
    indent: usize,
) -> Vec<String> {
    let budget = width.saturating_sub(indent + 2).max(8);
    let glyph = palette.gutter(stream);
    let lead = " ".repeat(indent);
    if text.is_empty() {
        return vec![format!("{lead}{glyph} ")];
    }
    text.chars()
        .collect::<Vec<_>>()
        .chunks(budget)
        .map(|chunk| format!("{lead}{glyph} {}", chunk.iter().collect::<String>()))
        .collect()
}

/// One image outcome parsed once from a `finished` frame. Exhaustive, so a new state is explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ImageState {
    Failed,
    Cached,
    Built,
    Pulled,
    Unknown,
}

/// One settled image end row, owned by the typed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ImageEnd {
    pub(super) state: ImageState,
    /// Steps that really ran; `None` means an older frame carried no count (print plain `built`).
    pub(super) executed: Option<usize>,
    pub(super) millis: u64,
    pub(super) at: Option<(usize, usize)>,
}

impl ImageEnd {
    pub(super) const fn new(
        state: ImageState,
        executed: Option<usize>,
        millis: u64,
        at: Option<(usize, usize)>,
    ) -> Self {
        Self {
            state,
            executed,
            millis,
            at,
        }
    }
}

/// The one image end row: `✓ … built N steps`, `✓ … cached`, `✓ … pulled`, `✗ … failed at N/M`,
/// or a neutral `• … finished` when the frame state was missing/unknown.
pub(super) fn image_end_row(
    index: usize,
    image_count: usize,
    image: &str,
    end: ImageEnd,
    widths: &OuterWidths,
    palette: Palette,
) -> String {
    let count = (index > 0 && image_count > 0).then_some((index, image_count));
    match end.state {
        ImageState::Cached => outer_row(
            &palette.icon_ok(),
            count,
            "image",
            image,
            "cached",
            None,
            Some(end.millis),
            widths,
            palette,
        ),
        ImageState::Built => {
            let detail = end.executed.map_or_else(
                || "built".to_owned(),
                |executed| {
                    let plural = if executed == 1 { "step" } else { "steps" };
                    format!("built {executed} {plural}")
                },
            );
            outer_row(
                &palette.icon_ok(),
                count,
                "image",
                image,
                &detail,
                None,
                Some(end.millis),
                widths,
                palette,
            )
        }
        ImageState::Pulled => outer_row(
            &palette.icon_ok(),
            count,
            "image",
            image,
            "pulled",
            None,
            Some(end.millis),
            widths,
            palette,
        ),
        ImageState::Failed => {
            let detail = match end.at {
                Some((step, total)) if step > 0 && total > 0 => format!("failed at {step}/{total}"),
                _ => "failed".to_owned(),
            };
            outer_row(
                &palette.tint("✗", "31"),
                count,
                "image",
                image,
                &detail,
                None,
                Some(end.millis),
                widths,
                palette,
            )
        }
        ImageState::Unknown => outer_row(
            &palette.icon_info(),
            count,
            "image",
            image,
            "finished",
            None,
            Some(end.millis),
            widths,
            palette,
        ),
    }
}

/// The failed build-end outer row: `✗ [1/3] image  api      failed at 2/6`.
#[cfg(test)]
pub(super) fn image_failed_row(
    index: usize,
    image_count: usize,
    image: &str,
    at: Option<(usize, usize)>,
    widths: &OuterWidths,
    palette: Palette,
) -> String {
    image_end_row(
        index,
        image_count,
        image,
        ImageEnd::new(ImageState::Failed, None, 0, at),
        widths,
        palette,
    )
}

/// The successful build-end outer row: `✓ [1/3] image  api      built 6 steps  12.3s`.
#[cfg(test)]
pub(super) fn image_done_row(
    index: usize,
    total: usize,
    image: &str,
    steps: usize,
    millis: u64,
    widths: &OuterWidths,
    palette: Palette,
) -> String {
    image_end_row(
        index,
        total,
        image,
        ImageEnd::new(ImageState::Built, Some(steps), millis, None),
        widths,
        palette,
    )
}

/// Truncates an already styled row to `width` visible cells, ending in `…` when it was cut.
///
/// SGR sequences are copied whole and never counted, and a clipped row carries one final reset so
/// a colour can never bleed into the next line of the live region.
pub(super) fn truncate_ansi(row: &str, width: usize) -> String {
    if console::measure_text_width(row) <= width {
        return row.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut visible = 0usize;
    let mut styled = false;
    let mut chars = row.chars();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            styled = true;
            out.push(ch);
            if let Some('[') = chars.next() {
                out.push('[');
                for ch in chars.by_ref() {
                    out.push(ch);
                    if ('@'..='~').contains(&ch) {
                        break;
                    }
                }
            }
            continue;
        }
        if visible == width.saturating_sub(1) {
            break;
        }
        out.push(ch);
        visible += 1;
    }
    out.push('…');
    if styled {
        out.push_str("\u{1b}[0m");
    }
    out
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
    let head: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}
