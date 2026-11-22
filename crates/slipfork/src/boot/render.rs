//! Pure boot-pane text: exact rows, bars, and panel lines with no terminal state.
//!
//! The live driver owns the scroll region and the ticker thread; this module owns every byte
//! those drivers print. Every colour role comes from the shipped [`Palette`], so a `NO_COLOR`,
//! `TERM=dumb`, piped, or `--json` invocation emits zero `ESC[` bytes.

use std::time::Duration;

use serde_json::Value;

use crate::status::widths::visible_width;
use crate::ui::Palette;

/// Below this many columns the anchored bar is dropped for the counter/percent/elapsed panel.
pub const NARROW: usize = 80;
/// The panel bar is exactly this many cells wide (`specs/02` §3).
pub const BAR_CELLS: usize = 24;

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

/// The anchored panel's cell count, or `None` when the terminal is too narrow for a bar.
#[must_use]
pub fn pane_geometry(width: usize) -> Option<usize> {
    (width >= NARROW).then_some(BAR_CELLS)
}

/// `round(done / total * 100)`, clamped to `100`.
#[must_use]
pub fn percent(done: usize, total: usize) -> usize {
    filled(done, total, 100)
}

/// The fully styled live panel for one colour TTY: spinner, 24-cell bar, counters, percent,
/// active summary, and elapsed time, clamped to `width` columns.
#[must_use]
pub fn panel_line(
    spinner: char,
    done: usize,
    total: usize,
    summary: &str,
    elapsed: Duration,
    width: usize,
) -> String {
    assemble(
        spinner,
        done,
        total,
        summary,
        elapsed,
        width,
        Palette::new(true),
    )
}

/// One `\r`-rewritten colourless status line; carries zero escapes and zero carriage returns.
#[must_use]
pub fn status_line(
    spinner: char,
    done: usize,
    total: usize,
    summary: &str,
    elapsed: Duration,
    width: usize,
) -> String {
    assemble(
        spinner,
        done,
        total,
        summary,
        elapsed,
        width,
        Palette::new(false),
    )
}

fn assemble(
    spinner: char,
    done: usize,
    total: usize,
    summary: &str,
    elapsed: Duration,
    width: usize,
    palette: Palette,
) -> String {
    let colour = palette.color();
    let counter = format!("{done}/{total}");
    let pct = format!("{}%", percent(done, total));
    let seconds = format!("{:.1}s", elapsed.as_secs_f64());
    let spin = if colour {
        palette.kind(&spinner.to_string())
    } else {
        spinner.to_string()
    };
    let bar = |cells: usize| -> String {
        let filled = filled(done, total, cells);
        if !colour {
            return format!("[{}{}]", "#".repeat(filled), ".".repeat(cells - filled));
        }
        format!(
            "[{}{}]",
            palette.tint(&"█".repeat(filled), "1;36"),
            palette.dim(&"░".repeat(cells - filled)),
        )
    };
    let counter = palette.value(&counter);
    let pct = palette.value(&pct);
    let summary = palette.label(summary);
    let seconds = palette.dim(&seconds);

    if let Some(cells) = pane_geometry(width) {
        let full = format!(
            "{spin} {}  {counter}  {pct} · {summary} · {seconds}",
            bar(cells)
        );
        if visible_width(&full) <= width {
            return full;
        }
        let without_summary = format!("{spin} {}  {counter}  {pct} · {seconds}", bar(cells));
        if visible_width(&without_summary) <= width {
            return without_summary;
        }
    }
    let with_summary = format!("{spin} {counter}  {pct} · {summary} · {seconds}");
    if visible_width(&with_summary) <= width {
        return with_summary;
    }
    format!("{spin} {counter}  {pct} · {seconds}")
}

fn filled(done: usize, total: usize, cells: usize) -> usize {
    if total == 0 {
        return 0;
    }
    (done.saturating_mul(cells) + total / 2) / total
}

/// The pipe header for a node that just started: `→ [4/12] image base`.
#[must_use]
pub fn header_row(step: u64, total: u64, kind: &str, name: &str) -> String {
    format!("→ [{step}/{total}] {kind} {name}")
}

/// One failure verdict: `✗ image  backend  failed` with every role from the palette.
#[must_use]
pub fn failed_row(kind: &str, name: &str, styled: bool) -> String {
    let palette = Palette::new(styled);
    format!(
        "{} {}  {}  {}",
        palette.icon_bad(),
        palette.dim(&format!("{kind:<8}")),
        palette.label(name),
        palette.status("failed"),
    )
}

/// The indented diagnostic rows printed directly under a [`failed_row`]: the event's `message`
/// (prefixed with its `Forkfile:<line>` context when known) wrapped to `width` visible cells, then
/// the `next:` remedy when one was reported.
///
/// Rows carry no glyph of their own — the failed row and the final [`failure_sentence`] each keep
/// their single leading `✗` — and uncoloured rows contain zero escape bytes.
#[must_use]
pub fn failure_details(
    message: &str,
    remedy: Option<&str>,
    line: Option<u64>,
    width: usize,
    styled: bool,
) -> Vec<String> {
    let palette = Palette::new(styled);
    let mut rows = Vec::new();
    // A `slipfork-world::Error` Display already ends in `next: …`; that line is the remedy, not
    // more evidence, so it is lifted out of the message and rendered once in the remedy role.
    let mut embedded = None;
    let mut evidence = Vec::new();
    for text in message
        .lines()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        match text.strip_prefix("next:") {
            Some(rest) if embedded.is_none() => embedded = Some(rest.trim()),
            _ => evidence.push(text),
        }
    }
    let mut first = true;
    for text in evidence {
        let text = if std::mem::take(&mut first) {
            match line {
                Some(line) if line > 0 => format!("Forkfile:{line}: {text}"),
                _ => text.to_owned(),
            }
        } else {
            text.to_owned()
        };
        rows.extend(
            wrap_row("  ", &text, width)
                .into_iter()
                .map(|row| palette.dim(&row)),
        );
    }
    let remedy = remedy
        .map(str::trim)
        .filter(|remedy| !remedy.is_empty())
        .or(embedded);
    if let Some(remedy) = remedy {
        let text = if remedy.starts_with("next:") {
            remedy.to_owned()
        } else {
            format!("next: {remedy}")
        };
        for (index, row) in wrap_row("  ", &text, width).into_iter().enumerate() {
            let body = row.trim_start();
            let body = match body.strip_prefix("next:") {
                Some(rest) if index == 0 => rest.trim_start(),
                _ => body,
            };
            let prefix = if index == 0 {
                format!("{} ", palette.kind("next:"))
            } else {
                String::new()
            };
            rows.push(format!("  {prefix}{}", palette.command(body)));
        }
    }
    rows
}

/// Greedy word wrap on visible width, repeating `indent` on every continuation row.
fn wrap_row(indent: &str, text: &str, width: usize) -> Vec<String> {
    let width = width.max(24);
    let mut rows = Vec::new();
    let mut current = String::from(indent);
    for word in text.split_whitespace() {
        let candidate = if current.trim().is_empty() {
            format!("{indent}{word}")
        } else {
            format!("{current} {word}")
        };
        if visible_width(&candidate) > width && !current.trim().is_empty() {
            rows.push(current);
            current = format!("{indent}{word}");
        } else {
            current = candidate;
        }
    }
    if !current.trim().is_empty() {
        rows.push(current);
    }
    rows
}

/// The one crisp remedy sentence printed after the failed row.
#[must_use]
pub fn failure_sentence(kind: &str, name: &str) -> String {
    let verb = match kind {
        "image" => "build image",
        "hook" => "run hook",
        "service" => "start service",
        "setup" => "run setup",
        "resource" => "start resource",
        _ => "finish step",
    };
    format!("✗ Failed to {verb} {name} — check logs with: slipfork logs {name}")
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
        palette.label(fork),
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
        palette.label(world),
        palette.dim(&format!("({})", elapsed_text(millis))),
    )
}

/// `✓ image    base cached    420ms` on a pipe, or the styled check, dim 8-cell kind, bold name,
/// lowercase state, and right-aligned dim duration on a colour terminal.
#[must_use]
pub fn completed_row(node: &Node<'_>, styled: bool) -> String {
    let palette = Palette::new(styled);
    let state = node.state.to_lowercase();
    let duration = elapsed_text(node.millis);
    let mark = if node.success {
        palette.icon_ok()
    } else {
        palette.dim("•")
    };
    let mut row = format!(
        "{mark} {}  {}",
        palette.dim(&format!("{:<8}", node.kind)),
        palette.label(node.name),
    );
    if !state.is_empty() {
        row.push(' ');
        row.push_str(&palette.status(&state));
    }
    row.push_str(&format!("  {}", palette.dim(&format!("{duration:>7}"))));
    row
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

#[cfg(test)]
mod tests {
    use super::{failed_row, failure_details, failure_sentence};

    #[test]
    fn a_failure_block_prints_its_message_and_remedy_once_with_one_glyph_per_row() {
        let message = "`crun` exited with code 1";
        let remedy = "fix the `RUN` command above, then rerun `slipfork up`";
        let details = failure_details(message, Some(remedy), Some(12), 100, false);
        assert_eq!(
            details,
            vec![
                "  Forkfile:12: `crun` exited with code 1".to_owned(),
                "  next: fix the `RUN` command above, then rerun `slipfork up`".to_owned(),
            ]
        );

        let mut block = vec![failed_row("image", "frontend", false)];
        block.extend(details.iter().cloned());
        block.push(failure_sentence("image", "frontend"));
        for row in &block {
            assert!(
                row.matches('✗').count() <= 1,
                "one glyph per row, got `{row}`"
            );
            assert!(!row.contains('\u{1b}'), "NO_COLOR rows stay colourless");
        }
        let block = block.join("\n");
        assert_eq!(block.matches(message).count(), 1, "{block}");
        assert_eq!(block.matches(remedy).count(), 1, "{block}");
        assert!(
            block.contains("check logs with: slipfork logs frontend"),
            "{block}"
        );
    }

    #[test]
    fn an_embedded_next_line_becomes_the_single_styled_remedy() {
        let message =
            "`crun` exited with code 1\nnext: fix the `RUN` command and rerun `slipfork up`";
        let rows = failure_details(message, None, Some(12), 100, false);
        assert_eq!(
            rows,
            vec![
                "  Forkfile:12: `crun` exited with code 1".to_owned(),
                "  next: fix the `RUN` command and rerun `slipfork up`".to_owned(),
            ]
        );
        let styled = failure_details(message, None, Some(12), 100, true);
        assert!(styled[1].contains("\u{1b}[36mnext:"), "{styled:?}");
        assert_eq!(styled.len(), 2, "the embedded next line prints once");
    }

    #[test]
    fn a_long_failure_message_wraps_inside_the_visible_width() {
        let rows = failure_details(
            "the build step wrote a message long enough that it must wrap onto more than one row",
            None,
            None,
            48,
            false,
        );
        assert!(rows.len() > 1, "{rows:?}");
        for row in &rows {
            assert!(
                console::measure_text_width(row) <= 48,
                "`{row}` is too wide"
            );
            assert!(row.starts_with("  "), "`{row}` is not indented");
        }
    }
}
