//! The one terminal resolve site: output mode, geometry, and stdin tty status.

use std::ffi::{OsStr, OsString};
use std::io::IsTerminal;

use super::UiMode;

/// Narrowest usable terminal: a zero-column probe (a `script`/PTY with no inherited winsize)
/// reads as 80 columns, so no gutter ever wraps at eight characters.
pub const FALLBACK_COLS: usize = 80;

/// The terminal contract resolved once for this invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terminal {
    /// Visible columns for live panes; [`FALLBACK_COLS`] for every non-interactive invocation.
    pub cols: usize,
    /// Visible rows; `0` when there is no anchored region.
    pub rows: usize,
    /// How this invocation renders its result.
    pub mode: UiMode,
    /// Whether stdin is a real terminal, resolved once by the composition root.
    pub stdin_tty: bool,
}

impl Terminal {
    /// Probes the terminal and the environment exactly once for the composition root.
    #[must_use]
    pub fn detect(args: &[OsString]) -> Self {
        let mode = detect_ui(args);
        let (cols, rows) = if mode.interactive() {
            crossterm::terminal::size().map_or((FALLBACK_COLS, 0), |(cols, rows)| {
                normalize_size(usize::from(cols), usize::from(rows))
            })
        } else {
            (FALLBACK_COLS, 0)
        };
        Self {
            cols,
            rows,
            mode,
            stdin_tty: std::io::stdin().is_terminal(),
        }
    }

    /// The live-pane width in visible columns.
    #[must_use]
    pub const fn width(&self) -> usize {
        self.cols
    }
}

/// Resolves the one output mode for this invocation.
///
/// `--json` wins over everything; a real terminal becomes [`UiMode::Interactive`] with `color`
/// gated by `NO_COLOR` and `TERM=dumb`; anything else is [`UiMode::Plain`], so a pipe can never
/// carry an escape or a carriage return.
fn detect_ui(args: &[OsString]) -> UiMode {
    let tty = std::io::stdout().is_terminal();
    let color = tty
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var_os("TERM").is_none_or(|term| term != "dumb");
    let json = args
        .iter()
        .take_while(|argument| argument.as_os_str() != OsStr::new("--"))
        .any(|argument| argument == "--json");
    if json {
        UiMode::Json
    } else if tty {
        UiMode::Interactive { color }
    } else {
        UiMode::Plain
    }
}

/// Normalizes one raw `(cols, rows)` probe.
///
/// Zero columns becomes [`FALLBACK_COLS`]; zero rows stays zero, which every caller reads as
/// "no anchored region" and falls back to the append-only path.
#[must_use]
pub const fn normalize_size(cols: usize, rows: usize) -> (usize, usize) {
    (if cols == 0 { FALLBACK_COLS } else { cols }, rows)
}

#[cfg(test)]
mod tests {
    use super::normalize_size;

    #[test]
    fn a_zero_size_probe_reads_as_eighty_columns_and_no_region() {
        assert_eq!(normalize_size(0, 0), (80, 0));
        assert_eq!(normalize_size(0, 32), (80, 32));
        assert_eq!(normalize_size(120, 32), (120, 32));
        assert_eq!(normalize_size(1, 0), (1, 0));
    }
}
