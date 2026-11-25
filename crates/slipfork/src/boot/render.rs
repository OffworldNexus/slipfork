//! Pure boot text: exact rows, bars, and status lines with no terminal state.
//!
//! The append-only driver owns the one status line and the ticker thread; this module owns every
//! byte those drivers print. Every colour role comes from the shipped [`Palette`], so a `NO_COLOR`,
//! `TERM=dumb`, piped, or `--json` invocation emits zero `ESC[` bytes. Failure copy is projected
//! from `ui::report::FailureFacts`, and the live build narrative lives in `boot::build`.

use std::time::Duration;

pub use crate::ui::elapsed_text;
use crate::ui::report::Palette;
use crate::ui::report::Row;
use crate::ui::report::visible_width;

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

/// The decimal width of `total`, so `1/11` and `10/11` share one right-aligned counter column.
#[must_use]
pub const fn digits(total: usize) -> usize {
    let mut width = 1;
    let mut value = total;
    while value >= 10 {
        value /= 10;
        width += 1;
    }
    width
}

/// One right-aligned `i/N` counter; an unknown total (`0`) prints unpadded as before.
#[must_use]
pub fn counter(index: usize, total: usize, index_width: usize) -> String {
    if index == 0 || total == 0 || index_width == 0 {
        return format!("{index}/{total}");
    }
    format!("{index:>index_width$}/{total}")
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
        row.push_str(&palette.meta(&format!("[{}]", counter(index, total, digits(total)))));
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

    /// The one structural invariant: `done <= total` and `num <= den`.
    ///
    /// A daemon can settle an outer row after its inner steps were already counted (or echo a
    /// late `step` frame), which would otherwise push `num` past `den`. Clamping here keeps the
    /// counter cell consistent and makes every downstream `filled`/`percent` read total.
    #[must_use]
    pub const fn clamped(self) -> Self {
        let total = if self.total >= self.done {
            self.total
        } else {
            self.done
        };
        let (num, den) = if self.den == 0 {
            (0, 0)
        } else if self.num > self.den {
            (self.den, self.den)
        } else {
            (self.num, self.den)
        };
        Self {
            done: self.done,
            total,
            num,
            den,
        }
    }
}

/// The status-line bar's cell count, or `None` when the terminal is too narrow for a bar.
#[must_use]
pub fn pane_geometry(width: usize) -> Option<usize> {
    (width >= NARROW).then_some(BAR_CELLS)
}

/// `round(num / den * 100)`, clamped to `100`; zero when no plan total is known yet.
#[must_use]
pub fn percent_of(num: u64, den: u64) -> usize {
    usize::try_from(filled(num, den, 100))
        .unwrap_or(100)
        .min(100)
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
#[allow(dead_code)]
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
    let progress = progress.clamped();
    let colour = palette.color();
    let count = counter(progress.done, progress.total, digits(progress.total));
    let pct = format!("{}%", percent_of(progress.num, progress.den));
    let seconds = format!("{:.1}s", elapsed.as_secs_f64());
    // The spinner is content, never a keyword or a copyable command: neutral on every surface.
    let spin = spinner.to_string();
    let bar = |cells: usize| -> String {
        let filled = filled(progress.num, progress.den, cells as u64) as usize;
        let empty = cells.saturating_sub(filled);
        if !colour {
            return format!("[{}{}]", "#".repeat(filled), ".".repeat(empty));
        }
        // The bar fill is content: neutral, never a hue. Only the empty cells recede to dim.
        format!(
            "[{}{}]",
            "█".repeat(filled),
            palette.meta(&"░".repeat(empty)),
        )
    };
    let counter = palette.meta(&count);
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

/// Rounded `num / den` of `cells`, never wider than the bar itself.
fn filled(num: u64, den: u64, cells: u64) -> u64 {
    if den == 0 || cells == 0 {
        return 0;
    }
    (num.saturating_mul(cells).saturating_add(den / 2) / den).min(cells)
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

/// One settled outer boot row, projected from a typed [`Row`] into the plan's shared columns:
/// `✓ [1/3] image  api      cached  8ms` or `• [2/3] setup  uv sync  up to date  <1ms`.
///
/// `widths` comes from [`outer_widths`] over the complete boot plan, so a row printed while the
/// boot is still streaming lands in the same kind/target columns a batch render would produce.
#[must_use]
pub fn completed_row(row: &Row, widths: &OuterWidths, palette: Palette) -> String {
    let counter = row
        .counter
        .filter(|counter| counter.index > 0 && counter.total > 0)
        .map(|counter| (counter.index, counter.total));
    outer_row(
        &row.state.icon(palette),
        counter,
        outer_verb(row.keyword.as_deref().unwrap_or("step")),
        row.target.as_deref().unwrap_or_default(),
        &row.state.plain(),
        row.command.as_deref(),
        row.timing_ms,
        widths,
        palette,
    )
}

#[cfg(test)]
#[path = "render/tests.rs"]
mod tests;
