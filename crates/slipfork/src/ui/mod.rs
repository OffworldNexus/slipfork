//! Output modes and the one colour layer for the shipped CLI.
//!
//! The composition root resolves [`UiMode`] once from the terminal and the environment; every
//! renderer receives it as a plain value and never asks a second time. [`report::Palette`] is the
//! single role API, its raw SGR primitive stays private to the report layer, and [`boot`] is the
//! only live pane the world verbs draw.

mod ansi;
mod elapsed;
pub mod report;
mod terminal;

pub use ansi::{colour_only, segments, strip, visible_width};
pub use elapsed::elapsed_text;
pub use terminal::{FALLBACK_COLS, Terminal, TerminalEnv, normalize_size};

/// How this invocation renders its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiMode {
    /// A real terminal: live panes and spinners. `color` is `false` under `NO_COLOR`/`TERM=dumb`,
    /// where the pane degrades to one `\r`-rewritten ASCII line instead of cursor movement.
    Interactive { color: bool },
    /// A pipe: plain text with zero `ESC[` bytes and zero `\r`.
    Plain,
    /// `--json`: machine-readable output with zero ANSI.
    Json,
}

impl UiMode {
    /// Whether ANSI styling is allowed.
    #[must_use]
    pub fn color(self) -> bool {
        matches!(self, Self::Interactive { color: true })
    }

    /// Whether stdout is a real terminal that can redraw a live status line.
    #[must_use]
    pub fn interactive(self) -> bool {
        matches!(self, Self::Interactive { .. })
    }

    /// Whether this invocation asked for the machine-readable object.
    #[must_use]
    pub fn json(self) -> bool {
        matches!(self, Self::Json)
    }
}
