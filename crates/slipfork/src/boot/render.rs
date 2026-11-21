//! Pure boot-pane text: exact rows, bars, and status lines with no terminal state.
//!
//! The live driver owns the ticker thread and the `indicatif` panes; this module owns every byte
//! those panes print. Colour arrives as a plain `bool` and geometry as a plain column count, so
//! the contract tests assert the shipped strings byte for byte.

use std::collections::VecDeque;
use std::time::Duration;

use serde_json::Value;

use crate::ui::paint;

/// Below this many columns the anchored bar is dropped for the counter/title/elapsed line.
pub const NARROW: usize = 48;
/// The anchored bar is exactly this many cells wide.
pub const BAR_CELLS: usize = 16;
/// Streamed lines kept live under a running node, and later printed once.
pub const TAIL_LINES: usize = 3;
/// The gutter `  │ ` plus a column of slack, subtracted before clamping a streamed line.
pub const GUTTER: usize = 5;

/// One finished boot node, already parsed out of a typed daemon `event` frame.
///
/// A frame without a kind, a name/label, or a duration is malformed: parsing yields `None` rather
/// than inventing a `step`/`ready`/`0` row, so a broken event stream can never print fiction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node<'a> {
    /// Node kind (`image`, `service`, `postgres`, …), dimmed and padded to 8 cells.
    pub kind: &'a str,
    /// Node name, printed bold.
    pub name: &'a str,
    /// Raw state from the daemon; [`completed_row`] lowercases it.
    pub state: &'a str,
    /// Wall time the node took, rendered by [`elapsed_text`].
    pub millis: u64,
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
            success: event["event"].as_str() != Some("up-to-date"),
        })
    }
}

/// One captured log line, kept raw until a driver draws it.
///
/// The step/total pair travels with the line so [`stream_row`] can prefix it at draw time without
/// the scheduler ever formatting a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailLine {
    /// The unclamped log line exactly as the daemon streamed it.
    pub text: String,
    /// Boot step the line belongs to (`0` when the frame carries no step).
    pub step: u64,
    /// Total steps of the boot (`0` when the frame carries no total).
    pub total: u64,
}

impl TailLine {
    /// Projects one `log` event frame, or `None` when it carries no line.
    #[must_use]
    pub fn from_event(event: &Value) -> Option<Self> {
        Some(Self {
            text: event["line"].as_str()?.to_owned(),
            step: event["step"].as_u64().unwrap_or(0),
            total: event["total"].as_u64().unwrap_or(0),
        })
    }
}

/// The anchored bar's cell count, or `None` when the terminal is too narrow for a bar.
#[must_use]
pub fn pane_geometry(width: usize) -> Option<usize> {
    (width >= NARROW).then_some(BAR_CELLS)
}

/// The visible columns a streamed line may occupy once the `  │ ` gutter is drawn.
#[must_use]
pub fn stream_limit(width: usize) -> usize {
    width.saturating_sub(GUTTER).max(8)
}

/// One live running row: `image     base  1.2s` with the kind dimmed to 8 cells and the elapsed
/// time dimmed, or the same fields plain.
#[must_use]
pub fn running_row(kind: &str, name: &str, elapsed: Duration, styled: bool) -> String {
    let seconds = format!("{:.1}s", elapsed.as_secs_f64());
    if !styled {
        return format!("{kind:<8}  {name}  {seconds}");
    }
    format!(
        "{}  {}  {}",
        paint(&format!("{kind:<8}"), "2", true),
        paint(name, "1", true),
        paint(&seconds, "2", true),
    )
}

/// A running row followed by its live rolling tail, so a quiet daemon still shows the last
/// [`TAIL_LINES`] lines under the active spinner.
#[must_use]
pub fn running_row_with_tail(
    kind: &str,
    name: &str,
    elapsed: Duration,
    tail: &VecDeque<TailLine>,
    limit: usize,
    styled: bool,
) -> String {
    let mut row = running_row(kind, name, elapsed, styled);
    for line in tail {
        row.push('\n');
        row.push_str(&stream_row(
            &line.text, line.step, line.total, limit, styled,
        ));
    }
    row
}

/// One streamed `RUN`/hook line under the boot pane: the `[{step}/{total}] ` prefix, clamped to
/// `limit` visible columns, behind a dim `  │ ` gutter.
#[must_use]
pub fn stream_row(line: &str, step: u64, total: u64, limit: usize, styled: bool) -> String {
    let text = if total > 0 {
        format!("[{step}/{total}] {line}")
    } else {
        line.to_owned()
    };
    let clamped = console::truncate_str(&text, limit, "…");
    if styled {
        format!("{} {}", paint("  │", "2", true), paint(&clamped, "2", true))
    } else {
        format!("  │ {clamped}")
    }
}

/// The rows a finished node leaves behind: each buffered line clamped to [`stream_limit`], so a
/// verdict can print the tail before itself in every non-`indicatif` driver.
#[must_use]
pub fn tail_rows(tail: Option<VecDeque<TailLine>>, width: usize, styled: bool) -> Vec<String> {
    let limit = stream_limit(width);
    tail.map(|tail| {
        tail.iter()
            .map(|line| stream_row(&line.text, line.step, line.total, limit, styled))
            .collect()
    })
    .unwrap_or_default()
}

/// The pipe header for a node that just started: `→ [4/12] image base`.
#[must_use]
pub fn header_row(step: u64, total: u64, kind: &str, name: &str) -> String {
    format!("→ [{step}/{total}] {kind} {name}")
}

/// One failure verdict: `✗ <kind> <name>` with the mark red.
#[must_use]
pub fn failed_row(kind: &str, name: &str, styled: bool) -> String {
    format!("{} {kind} {name}", paint("✗", "31", styled))
}

/// The failure block under the verdict: an optional `  Forkfile:{line}`, then `error:`, then
/// `next:`. One owner, so every driver prints the same three lines in the same order.
#[must_use]
pub fn failure_lines(line: u64, message: &str, remedy: Option<&str>) -> Vec<String> {
    let next = match remedy {
        Some(remedy) => remedy.to_owned(),
        None if line > 0 => format!("fix Forkfile:{line}, then run `slipfork up`"),
        None => "read `.slipfork/supervisor.log`, then run `slipfork up`".to_owned(),
    };
    let mut lines = Vec::with_capacity(3);
    if line > 0 {
        lines.push(format!("  Forkfile:{line}"));
    }
    lines.push(format!("error: {message}"));
    lines.push(format!("next: {next}"));
    lines
}

/// The world-ready verdict, with the primary URL always bold cyan.
#[must_use]
pub fn ready_row(url: &str, styled: bool) -> String {
    format!(
        "{} {}",
        paint("✓ Ready ->", "32", styled),
        paint(url, "1;36", styled)
    )
}

/// The fork verdict, with the fork name bold and the primary URL bold cyan.
#[must_use]
pub fn forked_row(fork: &str, millis: u64, url: &str, styled: bool) -> String {
    format!(
        "{} {} {} -> {}",
        paint("✓", "32", styled),
        paint(fork, "1", styled),
        paint(&format!("({})", elapsed_text(millis)), "2", styled),
        paint(url, "1;36", styled),
    )
}

/// One stopped world: `✓ Stopped demo (1.2s)`.
#[must_use]
pub fn stopped_row(world: &str, millis: u64, styled: bool) -> String {
    format!(
        "{} {} {}",
        paint("✓ Stopped", "32", styled),
        paint(world, "1", styled),
        paint(&format!("({})", elapsed_text(millis)), "2", styled),
    )
}

/// The dim hint under a world-ready verdict.
#[must_use]
pub fn hint_line(styled: bool) -> String {
    paint(
        "world keeps running; use `slipfork down` when finished",
        "2",
        styled,
    )
}

/// `✓ image    base cached    420ms` on a pipe, or the styled check, dim 8-cell kind, bold name,
/// lowercase state, and right-aligned dim duration on a colour terminal.
#[must_use]
pub fn completed_row(node: &Node<'_>, styled: bool) -> String {
    let state = node.state.to_lowercase();
    let duration = elapsed_text(node.millis);
    let mark = if node.success { "✓" } else { "•" };
    if !styled {
        let mut row = format!("{mark} {} {}", node.kind, node.name);
        if !state.is_empty() {
            row.push(' ');
            row.push_str(&state);
        }
        row.push_str(&format!("  {duration}"));
        return row;
    }
    let mark = if node.success {
        paint(mark, "32", true)
    } else {
        paint(mark, "2", true)
    };
    let mut row = format!(
        "{mark} {}  {}",
        paint(&format!("{:<8}", node.kind), "2", true),
        paint(node.name, "1", true),
    );
    if !state.is_empty() {
        row.push(' ');
        row.push_str(&state);
    }
    row.push_str(&format!(
        "  {}",
        paint(&format!("{duration:>7}"), "2", true)
    ));
    row
}

/// The anchored summary: `[██████████░░░░░░] 6/10  Booting demo  1.2s`, or the narrow
/// `6/10  Booting demo  1.2s` with no bar.
#[must_use]
pub fn summary_line(
    total: usize,
    done: usize,
    title: &str,
    elapsed: Duration,
    width: usize,
) -> String {
    let counter = format!("{done}/{total}");
    let seconds = format!("{:.1}s", elapsed.as_secs_f64());
    match pane_geometry(width) {
        Some(cells) => format!(
            "[{}] {counter}  {title}  {seconds}",
            bar(cells, done, total, '█', '░')
        ),
        None => format!("{counter}  {title}  {seconds}"),
    }
}

/// One `\r`-rewritten colourless status line: `| [##########......] 6/10  Booting demo  api  1.2s`.
///
/// The caller prepends the single `\r`; this string itself carries zero escapes and zero
/// carriage returns, and narrow terminals degrade to the same counter/title/elapsed shape.
#[must_use]
pub fn status_line(
    spinner: char,
    done: usize,
    total: usize,
    title: &str,
    current: &str,
    elapsed: Duration,
    width: usize,
) -> String {
    let counter = format!("{done}/{total}");
    let seconds = format!("{:.1}s", elapsed.as_secs_f64());
    let Some(cells) = pane_geometry(width) else {
        return format!("{counter}  {title}  {seconds}");
    };
    let mut line = format!(
        "{spinner} [{}] {counter}  {title}",
        bar(cells, done, total, '#', '.')
    );
    if !current.is_empty() {
        line.push_str("  ");
        line.push_str(current);
    }
    line.push_str("  ");
    line.push_str(&seconds);
    line
}

/// `18ms`, `1.2s`, or `<1ms`, as the whole CLI spells durations.
#[must_use]
pub fn elapsed_text(millis: u64) -> String {
    if millis == 0 {
        "<1ms".to_owned()
    } else if millis < 1000 {
        format!("{millis}ms")
    } else {
        format!("{:.1}s", millis as f64 / 1000.0)
    }
}

/// An exactly `cells`-wide bar whose fill rounds to the nearest cell.
fn bar(cells: usize, done: usize, total: usize, full: char, empty: char) -> String {
    let filled = (done * cells + total / 2)
        .checked_div(total)
        .unwrap_or(0)
        .min(cells);
    std::iter::repeat_n(full, filled)
        .chain(std::iter::repeat_n(empty, cells - filled))
        .collect()
}
