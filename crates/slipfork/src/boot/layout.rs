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
///
/// Escape sequences are zero-width atoms: a coloured line never loses a cell count and a sequence
/// is never split across rows. A wrapped row that carried SGR closes it before the next row, so
/// colour never bleeds past its own line.
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
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    for (slice, escape) in crate::ui::segments(text) {
        if escape {
            current.push_str(slice);
            continue;
        }
        for ch in slice.chars() {
            let mut buffer = [0_u8; 4];
            let cell = console::measure_text_width(ch.encode_utf8(&mut buffer));
            if used > 0 && used + cell > budget {
                close_sgr(&mut current);
                rows.push(format!("{lead}{glyph} {current}"));
                current.clear();
                used = 0;
            }
            current.push(ch);
            used += cell;
        }
    }
    if !current.is_empty() {
        rows.push(format!("{lead}{glyph} {current}"));
    }
    rows
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
    /// The image's whole instruction total, so a mixed run can say `cached 9 · built 2`.
    pub(super) total: Option<usize>,
    pub(super) millis: u64,
    pub(super) at: Option<(usize, usize)>,
}

impl ImageEnd {
    pub(super) const fn new(
        state: ImageState,
        executed: Option<usize>,
        total: Option<usize>,
        millis: u64,
        at: Option<(usize, usize)>,
    ) -> Self {
        Self {
            state,
            executed,
            total,
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
            let detail = match (end.executed, end.total) {
                (Some(executed), Some(total)) if total > 0 => {
                    if executed == 0 {
                        "cached".to_owned()
                    } else if executed >= total {
                        built_steps(executed)
                    } else {
                        format!("cached {} · built {executed}", total - executed)
                    }
                }
                (Some(executed), _) => built_steps(executed),
                (None, _) => "built".to_owned(),
            };
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

/// `built N step(s)` for a fully executed image.
fn built_steps(executed: usize) -> String {
    let plural = if executed == 1 { "step" } else { "steps" };
    format!("built {executed} {plural}")
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
        ImageEnd::new(ImageState::Failed, None, None, 0, at),
        widths,
        palette,
    )
}

/// The successful build-end outer row: `✓ [1/3] image  api      built 6 steps  12.3s`.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn image_done_row(
    index: usize,
    total: usize,
    image: &str,
    steps: usize,
    instruction_total: usize,
    millis: u64,
    widths: &OuterWidths,
    palette: Palette,
) -> String {
    image_end_row(
        index,
        total,
        image,
        ImageEnd::new(
            ImageState::Built,
            Some(steps),
            Some(instruction_total),
            millis,
            None,
        ),
        widths,
        palette,
    )
}

/// Joins a styled `prefix`, plain `args`, and styled `suffix` into `width` visible cells.
///
/// Only `args` is ever shortened. When even `prefix + suffix` overflows — a degenerate width, or
/// a suffix wider than the row — the whole progress suffix wins over the identity columns, so the
/// cell that proves liveness is never the cell that disappears.
pub(super) fn compose_live(prefix: String, args: &str, suffix: String, width: usize) -> String {
    clamp_visible(&compose_live_unclamped(prefix, args, suffix, width), width)
}

/// Truncates any styled row to `width` visible cells without splitting an SGR sequence.
pub(super) fn clamp_visible(styled: &str, width: usize) -> String {
    if visible_width_escaped(styled) <= width {
        return styled.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0usize;
    'outer: for (slice, escape) in crate::ui::segments(styled) {
        if escape {
            out.push_str(slice);
            continue;
        }
        for ch in slice.chars() {
            let mut buffer = [0_u8; 4];
            let cell = console::measure_text_width(ch.encode_utf8(&mut buffer));
            if used + cell > width.saturating_sub(1) {
                break 'outer;
            }
            out.push(ch);
            used += cell;
        }
    }
    close_sgr(&mut out);
    out.push('…');
    out
}

fn visible_width_escaped(styled: &str) -> usize {
    crate::ui::visible_width(styled)
}

/// Closes an open SGR run (`ESC[0m`) before a row ends, so colour never bleeds past it.
fn close_sgr(out: &mut String) {
    if out.contains('\u{1b}') {
        out.push_str("\u{1b}[0m");
    }
}

fn compose_live_unclamped(prefix: String, args: &str, suffix: String, width: usize) -> String {
    let args = args.trim();
    let prefix_width = console::measure_text_width(&prefix);
    let suffix_width = console::measure_text_width(&suffix);
    if args.is_empty() {
        if prefix_width + suffix_width <= width {
            return format!("{prefix}{suffix}");
        }
        return keep_suffix(suffix);
    }
    if prefix_width + 2 + suffix_width > width {
        if prefix_width + suffix_width <= width {
            return format!("{prefix}{suffix}");
        }
        return keep_suffix(suffix);
    }
    let available = width - prefix_width - 2 - suffix_width;
    let fitted = fit_args(args, available);
    if fitted.is_empty() {
        format!("{prefix}{suffix}")
    } else {
        format!("{prefix}  {fitted}{suffix}")
    }
}

/// The suffix alone, without its leading gap: the last resort when nothing else fits.
fn keep_suffix(suffix: String) -> String {
    suffix.trim_start_matches(' ').to_owned()
}

/// Shortens `args` to `available` visible cells, ending in `…` when it had to be cut.
fn fit_args(args: &str, available: usize) -> String {
    if console::measure_text_width(args) <= available {
        return args.to_owned();
    }
    if available == 0 {
        return String::new();
    }
    let mut head = String::new();
    let mut used = 0usize;
    for ch in args.chars() {
        let width = console::measure_text_width(&ch.to_string());
        if used + width > available - 1 {
            break;
        }
        head.push(ch);
        used += width;
    }
    head.push('…');
    head
}

/// Truncates `text` to at most `max` visible characters, ending in `…` when it was cut.
pub(super) fn truncate_visible(text: &str, max: usize) -> String {
    if visible_width_escaped(text) <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut head = String::new();
    let mut used = 0usize;
    'outer: for (slice, escape) in crate::ui::segments(text) {
        if escape {
            head.push_str(slice);
            continue;
        }
        for ch in slice.chars() {
            let mut buffer = [0_u8; 4];
            let cell = console::measure_text_width(ch.encode_utf8(&mut buffer));
            if used + cell > max.saturating_sub(1) {
                break 'outer;
            }
            head.push(ch);
            used += cell;
        }
    }
    close_sgr(&mut head);
    format!("{head}…")
}
