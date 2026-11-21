//! The single output mode resolved once by the composition root.
//!
//! Every renderer takes this value instead of probing stdio: the CLI never asks twice whether
//! stdout is a terminal, and `--json` can never leak an escape sequence.

/// How this invocation renders its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiMode {
    /// An interactive terminal with colour allowed: ANSI, spinners, and live progress.
    InteractiveTty,
    /// A pipe, `NO_COLOR`, or `TERM=dumb`: plain text with zero `ESC[` bytes.
    Plain,
    /// `--json`: one machine-readable object with zero ANSI.
    Json,
}

impl UiMode {
    /// Whether ANSI styling is allowed.
    #[must_use]
    pub fn color(self) -> bool {
        matches!(self, Self::InteractiveTty)
    }
}
