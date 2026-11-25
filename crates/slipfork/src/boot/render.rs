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
    /// Widest observed lifecycle word (`running`, `ready`, `up to date`), so a row with a shorter
    /// state never shifts the command and timing columns.
    pub state: usize,
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

/// Renders one outer `[i/n]` row at column 0 in the plan's shared columns.
///
/// Schema: `icon | [i/n] | keyword | target | state | command | timing`. The lifecycle `state` is
/// always its own cell (styled by [`Palette::status`]); the `command` is plain and omitted when it
/// equals the target, so a state word is never pushed into the command slot.
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
    state: &str,
    command: Option<&str>,
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
        row.push_str(&palette.meta(&format!("[{index}/{total}]")));
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
    let state = state.trim();
    let state_width = widths.state.max(console::measure_text_width(state));
    let styled_state = if state == "failed" || state.starts_with("failed ") {
        palette.error(state)
    } else {
        palette.status(state)
    };
    row.push_str("  ");
    push_padded(
        &mut row,
        &styled_state,
        console::measure_text_width(state),
        state_width,
    );
    if let Some(command) = command
        .map(str::trim)
        .filter(|text| !text.is_empty() && *text != target.trim())
    {
        row.push_str("  ");
        row.push_str(command);
    }
    if let Some(millis) = millis {
        row.push_str("  ");
        row.push_str(&palette.meta(&elapsed_text(millis)));
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
    outer_row(
        &mark,
        (index > 0 && total > 0).then_some((index, total)),
        outer_verb(kind),
        name,
        "running",
        None,
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
    /// Raw state from the daemon; [`completed_row`] lowercases it into the state cell.
    pub state: &'a str,
    /// Plain command/detail for a prep row, shown only in the command column.
    pub command: Option<&'a str>,
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
        let state = event["state"].as_str().unwrap_or_default();
        // A join/prep frame carries only a `detail` phrase, which is its command-column detail; a
        // lifecycle frame keeps any explicit `command` and never borrows a human `detail`.
        let command = event["command"].as_str().or_else(|| {
            if state.is_empty() {
                event["detail"].as_str()
            } else {
                None
            }
        });
        Some(Self {
            kind: event["kind"].as_str()?,
            name: event["name"].as_str().or_else(|| event["label"].as_str())?,
            state,
            command,
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
            palette.meta(&"░".repeat(cells - filled)),
        )
    };
    let counter = palette.meta(&counter);
    let pct = palette.tint(&pct, "1");
    let label = palette.tint(label, "1");
    let seconds = palette.meta(&seconds);

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
        palette.meta("->"),
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
        palette.meta(&format!("({})", elapsed_text(millis))),
        palette.meta("->"),
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
        palette.meta(&format!("({})", elapsed_text(millis))),
    )
}

/// The `drop` verdict: `✓ Dropped jello (101ms)`, the fork name bold and the timing dim.
#[must_use]
pub fn dropped_row(fork: &str, millis: u64, styled: bool) -> String {
    let palette = Palette::new(styled);
    format!(
        "{} {} {} {}",
        palette.icon_ok(),
        palette.tint("Dropped", "32"),
        palette.tint(fork, "1"),
        palette.meta(&format!("({})", elapsed_text(millis))),
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
        palette.meta("•")
    };
    // A prep/join frame has no lifecycle state of its own: it settled cleanly.
    let state = if node.state.trim().is_empty() {
        "done"
    } else {
        node.state.trim()
    };
    outer_row(
        &mark,
        (index > 0 && total > 0).then_some((index, total)),
        outer_verb(node.kind),
        node.name,
        &state.to_lowercase(),
        node.command,
        Some(node.millis),
        widths,
        palette,
    )
}

#[cfg(test)]
#[path = "render/tests.rs"]
mod tests;
