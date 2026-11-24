//! Pure boot-pane text: exact rows, bars, and panel lines with no terminal state.
//!
//! The live driver owns the scroll region and the ticker thread; this module owns every byte
//! those drivers print. Every colour role comes from the shipped [`Palette`], so a `NO_COLOR`,
//! `TERM=dumb`, piped, or `--json` invocation emits zero `ESC[` bytes. Failure copy lives in the
//! binary's `boot::failure`, and the live build narrative in `boot::build`.

use std::time::Duration;

use serde_json::Value;

use crate::ui::Palette;
pub use crate::ui::elapsed_text;
use crate::ui::table::visible_width;

/// Appends `styled` followed by enough plain spaces to occupy `width` visible cells.
///
/// Keeping pad spaces outside the SGR sequence guarantees `trim_end()` behaves identically on
/// styled and `NO_COLOR` rows, so `strip_ansi(&styled) == plain` holds on every row.
pub(crate) fn push_padded(out: &mut String, styled: &str, plain_len: usize, width: usize) {
    out.push_str(styled);
    for _ in 0..width.saturating_sub(plain_len) {
        out.push(' ');
    }
}

/// The outer boot verb for one planned `kind` (`"service"` renders as `"start"`).
#[must_use]
pub fn outer_verb(kind: &str) -> &str {
    match kind {
        "service" => "start",
        other => other,
    }
}

/// The plan-derived kind and target widths of the outer `[i/n]` rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OuterWidths {
    /// Widest outer verb (`image`, `setup`, `start`, `hook`, `resource`).
    pub kind: usize,
    /// Widest outer target (`api`, `uv sync`, `web`).
    pub target: usize,
}

/// Measures the outer kind and target columns from the complete announced plan.
#[must_use]
pub fn outer_widths(rows: &[(String, String)]) -> OuterWidths {
    let mut widths = OuterWidths::default();
    for (kind, name) in rows {
        widths.kind = widths.kind.max(outer_verb(kind).chars().count());
        widths.target = widths.target.max(name.chars().count());
    }
    widths
}

/// Renders one outer `[i/n]` row at column 0 in the plan's shared `kind` and `target` columns.
#[expect(
    clippy::too_many_arguments,
    reason = "cohesive pure formatter over the outer row's atomic cells"
)]
#[must_use]
pub fn outer_row(
    mark: &str,
    count: Option<(usize, usize)>,
    verb: &str,
    target: &str,
    detail: Option<&str>,
    millis: Option<u64>,
    widths: &OuterWidths,
    palette: Palette,
) -> String {
    let mut row = String::new();
    row.push_str(mark);
    row.push(' ');
    if let Some((index, total)) = count
        && index > 0
        && total > 0
    {
        row.push_str(&palette.dim(&format!("[{index}/{total}]")));
        row.push(' ');
    }
    push_padded(
        &mut row,
        &palette.verb(verb),
        verb.chars().count(),
        widths.kind,
    );
    row.push_str("  ");
    push_padded(
        &mut row,
        &palette.tint(target, "1"),
        target.chars().count(),
        widths.target,
    );
    if let Some(detail) = detail.filter(|text| !text.is_empty()) {
        row.push_str("  ");
        row.push_str(detail);
    }
    if let Some(millis) = millis {
        row.push_str("  ");
        row.push_str(&palette.dim(&elapsed_text(millis)));
    }
    while row.ends_with(' ') {
        row.pop();
    }
    row
}

/// One live outer task row: neutral Braille spinner, dim `[i/n]`, magenta kind, bold target,
/// green `running`, and dim live elapsed in the same columns [`completed_row`] settles into.
#[must_use]
pub fn running_row(
    kind: &str,
    name: &str,
    index: usize,
    total: usize,
    spinner: char,
    millis: u64,
    widths: &OuterWidths,
) -> String {
    let palette = Palette::new(true);
    let mark = spinner.to_string();
    let state = palette.status("running");
    outer_row(
        &mark,
        (index > 0 && total > 0).then_some((index, total)),
        outer_verb(kind),
        name,
        Some(&state),
        Some(millis),
        widths,
        palette,
    )
}

/// Below this many columns the anchored bar is dropped for the counter/percent/elapsed panel.
pub const NARROW: usize = 80;
/// The panel bar is exactly this many cells wide (`specs/02` §3).
pub const BAR_CELLS: usize = 24;

/// Outer plan counter (`done/total`) paired with the exact fractional fill (`num/den`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarProgress {
    /// Settled outer plan slots shown in the `done/total` counter cell.
    pub done: usize,
    /// Total outer plan slots shown in the `done/total` counter cell.
    pub total: usize,
    /// Fractional numerator driving the 24-cell bar fill and percentage.
    pub num: u64,
    /// Fractional denominator driving the 24-cell bar fill and percentage.
    pub den: u64,
}

impl BarProgress {
    /// Uniform outer-only progress (`done/total` drives both the counter cell and the fill).
    #[must_use]
    pub const fn outer(done: usize, total: usize) -> Self {
        Self {
            done,
            total,
            num: done as u64,
            den: total as u64,
        }
    }
}

/// One finished boot node, already parsed out of a typed daemon `event` frame.
///
/// A frame without a kind, a name/label, or a duration is malformed: parsing yields `None` rather
/// than inventing a `step`/`ready`/`0` row, so a broken event stream can never print fiction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node<'a> {
    /// Node kind (`image`, `service`, `postgres`, …), rendered via [`outer_verb`] in magenta.
    pub kind: &'a str,
    /// Node name, printed bold.
    pub name: &'a str,
    /// Raw state from the daemon; [`completed_row`] lowercases it into the plain detail cell.
    pub state: &'a str,
    /// Wall time the node took, rendered by [`elapsed_text`].
    pub millis: u64,
    /// Minted run id for a step that executed work; `None` for a cache hit.
    pub run_id: Option<&'a str>,
    /// `false` for an `up-to-date` node, which earns a dim bullet instead of a green check.
    pub success: bool,
}

impl<'a> Node<'a> {
    /// Projects one `finished`/`up-to-date` event frame into a row description.
    #[must_use]
    pub fn from_event(event: &'a Value) -> Option<Self> {
        Some(Self {
            kind: event["kind"].as_str()?,
            name: event["name"].as_str().or_else(|| event["label"].as_str())?,
            state: event["state"].as_str().unwrap_or_default(),
            millis: event["duration_ms"]
                .as_u64()
                .or_else(|| event["elapsed_ms"].as_u64())?,
            run_id: event["run_id"].as_str(),
            success: event["event"].as_str() != Some("up-to-date"),
        })
    }
}

/// The anchored panel's cell count, or `None` when the terminal is too narrow for a bar.
#[must_use]
pub fn pane_geometry(width: usize) -> Option<usize> {
    (width >= NARROW).then_some(BAR_CELLS)
}

/// `round(num / den * 100)`, clamped to `100`; zero when no plan total is known yet.
#[must_use]
pub fn percent_of(num: u64, den: u64) -> usize {
    usize::try_from(filled(num, den, 100)).unwrap_or(100)
}

/// The fully styled live panel for one colour TTY: spinner, 24-cell bar, outer `done/total`
/// counter, fractional percent, active label, and elapsed time, clamped to `width` columns.
#[must_use]
pub fn panel_line(
    spinner: char,
    progress: BarProgress,
    label: &str,
    elapsed: Duration,
    width: usize,
) -> String {
    assemble(spinner, progress, label, elapsed, width, Palette::new(true))
}

/// One `\r`-rewritten colourless status line; carries zero escapes and zero carriage returns.
#[must_use]
pub fn status_line(
    spinner: char,
    progress: BarProgress,
    label: &str,
    elapsed: Duration,
    width: usize,
) -> String {
    assemble(
        spinner,
        progress,
        label,
        elapsed,
        width,
        Palette::new(false),
    )
}

fn assemble(
    spinner: char,
    progress: BarProgress,
    label: &str,
    elapsed: Duration,
    width: usize,
    palette: Palette,
) -> String {
    let colour = palette.color();
    let counter = format!("{}/{}", progress.done, progress.total);
    let pct = format!("{}%", percent_of(progress.num, progress.den));
    let seconds = format!("{:.1}s", elapsed.as_secs_f64());
    // The spinner is content, never a keyword or a copyable command: neutral on every surface.
    let spin = spinner.to_string();
    let bar = |cells: usize| -> String {
        let filled = filled(progress.num, progress.den, cells as u64) as usize;
        if !colour {
            return format!("[{}{}]", "#".repeat(filled), ".".repeat(cells - filled));
        }
        // The bar fill is content: neutral, never a hue. Only the empty cells recede to dim.
        format!(
            "[{}{}]",
            "█".repeat(filled),
            palette.dim(&"░".repeat(cells - filled)),
        )
    };
    let counter = palette.dim(&counter);
    let pct = palette.tint(&pct, "1");
    let label = palette.tint(label, "1");
    let seconds = palette.dim(&seconds);

    if let Some(cells) = pane_geometry(width) {
        let full = format!(
            "{spin} {}  {counter}  {pct} · {label} · {seconds}",
            bar(cells)
        );
        if visible_width(&full) <= width {
            return full;
        }
        let without_label = format!("{spin} {}  {counter}  {pct} · {seconds}", bar(cells));
        if visible_width(&without_label) <= width {
            return without_label;
        }
    }
    let with_label = format!("{spin} {counter}  {pct} · {label} · {seconds}");
    if visible_width(&with_label) <= width {
        return with_label;
    }
    format!("{spin} {counter}  {pct} · {seconds}")
}

fn filled(num: u64, den: u64, cells: u64) -> u64 {
    if den == 0 {
        return 0;
    }
    (num.saturating_mul(cells) + den / 2) / den
}

/// The pipe header for a node that just started: `→ [4/12] resource pg`.
#[must_use]
pub fn header_row(step: u64, total: u64, kind: &str, name: &str) -> String {
    format!("→ [{step}/{total}] {} {name}", outer_verb(kind))
}

/// The world-ready verdict, with the primary URL always bold cyan and underlined.
#[must_use]
pub fn ready_row(url: &str, styled: bool) -> String {
    let palette = Palette::new(styled);
    format!(
        "{} {} {}",
        palette.tint("✓ Ready", "1;32"),
        palette.dim("->"),
        palette.url(url),
    )
}

/// The fork verdict, with the fork name bold and the primary URL bold cyan.
#[must_use]
pub fn forked_row(fork: &str, millis: u64, url: &str, styled: bool) -> String {
    let palette = Palette::new(styled);
    format!(
        "{} {} {} {} {}",
        palette.icon_ok(),
        palette.tint(fork, "1"),
        palette.dim(&format!("({})", elapsed_text(millis))),
        palette.dim("->"),
        palette.url(url),
    )
}

/// One stopped world: `✓ Stopped demo (1.2s)`.
#[must_use]
pub fn stopped_row(world: &str, millis: u64, styled: bool) -> String {
    let palette = Palette::new(styled);
    format!(
        "{} {} {}",
        palette.tint("✓ Stopped", "32"),
        palette.tint(world, "1"),
        palette.dim(&format!("({})", elapsed_text(millis))),
    )
}

/// One outer boot row, always carrying its `[i/n]` plan counter:
/// `✓ [1/3] image  api      cached  8ms` or `• [2/3] setup  uv sync  up to date  <1ms`.
///
/// `widths` comes from [`outer_widths`] over the complete boot plan, so a row printed while the
/// boot is still streaming lands in the same kind/target columns a batch render would produce.
#[must_use]
pub fn completed_row(
    node: &Node<'_>,
    index: usize,
    total: usize,
    widths: &OuterWidths,
    styled: bool,
) -> String {
    let palette = Palette::new(styled);
    let mark = if node.success {
        palette.icon_ok()
    } else {
        palette.dim("•")
    };
    let detail = node.state.to_lowercase();
    outer_row(
        &mark,
        (index > 0 && total > 0).then_some((index, total)),
        outer_verb(node.kind),
        node.name,
        Some(&detail),
        Some(node.millis),
        widths,
        palette,
    )
}

#[cfg(test)]
mod tests {
    use super::{Node, OuterWidths, completed_row, outer_widths};

    #[test]
    fn plan_widths_hold_every_row_in_the_same_columns() {
        let plan = vec![
            ("image".to_owned(), "api".to_owned()),
            ("setup".to_owned(), "uv sync".to_owned()),
            ("service".to_owned(), "web".to_owned()),
        ];
        let widths = outer_widths(&plan);
        assert_eq!(widths, OuterWidths { kind: 5, target: 7 });
        let cached = Node {
            kind: "image",
            name: "api",
            state: "cached",
            millis: 8,
            run_id: None,
            success: true,
        };
        let settled = Node {
            kind: "setup",
            name: "uv sync",
            state: "up to date",
            millis: 0,
            run_id: None,
            success: false,
        };
        assert_eq!(
            completed_row(&cached, 1, 3, &widths, false),
            "✓ [1/3] image  api      cached  8ms"
        );
        assert_eq!(
            completed_row(&cached, 1, 3, &widths, true),
            "\u{1b}[32m✓\u{1b}[0m \u{1b}[2m[1/3]\u{1b}[0m \u{1b}[35mimage\u{1b}[0m  \u{1b}[1mapi\u{1b}[0m      cached  \u{1b}[2m8ms\u{1b}[0m"
        );
        assert_eq!(
            completed_row(&settled, 2, 3, &widths, false),
            "• [2/3] setup  uv sync  up to date  <1ms"
        );
        assert!(!completed_row(&cached, 1, 3, &widths, false).contains('\u{1b}'));
    }
}
