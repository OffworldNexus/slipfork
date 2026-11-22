//! The one named-role palette for every human CLI surface.
//!
//! A [`Palette`] is a plain value: the composition root resolves colour once, passes it down, and
//! every method degrades to the exact input text when colour is off. No caller formats an SGR code
//! by hand, so `NO_COLOR`, `TERM=dumb`, a pipe, and `--json` all emit zero `ESC[` bytes.

use super::paint;

/// The shipped visual roles: one method per thing a human reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    color: bool,
}

impl Palette {
    /// Resolves the one colour decision made by the composition root.
    #[must_use]
    pub const fn new(color: bool) -> Self {
        Self { color }
    }

    /// Whether this palette may emit SGR bytes.
    #[must_use]
    pub const fn color(self) -> bool {
        self.color
    }

    /// Tints `text` with a raw SGR parameter, or returns it untouched.
    #[must_use]
    pub fn tint(self, text: &str, code: &str) -> String {
        paint(text, code, self.color)
    }

    /// A plain bold label (`Btrfs`, `crun 1.14.1`).
    #[must_use]
    pub fn label(self, text: &str) -> String {
        self.tint(text, "1")
    }

    /// A filesystem path: bold cyan.
    #[must_use]
    pub fn path(self, text: &str) -> String {
        self.tint(text, "1;36")
    }

    /// A primary URL: bold cyan, underlined.
    #[must_use]
    pub fn url(self, text: &str) -> String {
        self.tint(text, "1;4;36")
    }

    /// A resolved value: bold white.
    #[must_use]
    pub fn value(self, text: &str) -> String {
        self.tint(text, "1;37")
    }

    /// Secondary prose: dim.
    #[must_use]
    pub fn dim(self, text: &str) -> String {
        self.tint(text, "2")
    }

    /// A component kind or identifier: cyan.
    #[must_use]
    pub fn kind(self, text: &str) -> String {
        self.tint(text, "36")
    }

    /// A variable or secret key: bold cyan.
    #[must_use]
    pub fn key(self, text: &str) -> String {
        self.tint(text, "1;36")
    }

    /// A redacted secret: dim yellow.
    #[must_use]
    pub fn redacted(self, text: &str) -> String {
        self.tint(text, "2;33")
    }

    /// A command a human can copy: bold green.
    #[must_use]
    pub fn command(self, text: &str) -> String {
        self.tint(text, "1;32")
    }

    /// A failure: bold red.
    #[must_use]
    pub fn error(self, text: &str) -> String {
        self.tint(text, "1;31")
    }

    /// A lifecycle state word, mapped to the shipped status colours.
    ///
    /// `running`/`ready` are green, `cached`/`up to date` cyan, `built`/`pulled`/`orphaned`
    /// yellow, and `failed`/`stopped` bold red. An unknown word stays plain.
    #[must_use]
    pub fn status(self, state: &str) -> String {
        let code = match state.to_ascii_lowercase().as_str() {
            "running" | "ready" => "32",
            "cached" | "up-to-date" | "up to date" => "36",
            "built" | "pulled" | "orphaned" => "33",
            "failed" | "stopped" => "1;31",
            _ => return state.to_owned(),
        };
        self.tint(state, code)
    }

    /// A success mark.
    #[must_use]
    pub fn icon_ok(self) -> String {
        self.tint("✓", "32")
    }

    /// A failure mark.
    #[must_use]
    pub fn icon_bad(self) -> String {
        self.tint("✗", "1;31")
    }

    /// A neutral information mark.
    #[must_use]
    pub fn icon_info(self) -> String {
        self.tint("•", "36")
    }

    /// The dim cyan gutter divider used by merged logs.
    #[must_use]
    pub fn divider(self) -> String {
        self.tint("│", "2;36")
    }
}
