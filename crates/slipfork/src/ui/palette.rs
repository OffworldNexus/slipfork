//! The one named-role palette for every human CLI surface.
//!
//! A [`Palette`] is a plain value: the composition root resolves colour once, passes it down, and
//! every method degrades to the exact input text when colour is off. No caller formats an SGR code
//! by hand, so `NO_COLOR`, `TERM=dumb`, a pipe, and `--json` all emit zero `ESC[` bytes.

use super::elapsed_text;

use super::paint;

/// One settled run's verdict inputs, shared by every surface that prints the result line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// Instruction or step verb (`RUN`, `start`).
    pub verb: String,
    /// The thing that ran (`api`).
    pub target: String,
    /// Final state (`failed`, `ready`, …).
    pub state: String,
    /// Child exit code when the run carried one.
    pub exit_code: Option<i32>,
    /// Wall time of the run in milliseconds, when the run settled to completion.
    pub duration_ms: Option<u64>,
}

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

    /// A filesystem path: neutral metadata, because a location is not a command.
    #[must_use]
    pub fn path(self, text: &str) -> String {
        self.meta(text)
    }

    /// A primary URL: bold cyan, underlined.
    #[must_use]
    pub fn url(self, text: &str) -> String {
        self.tint(text, "1;4;36")
    }

    /// A resolved value or name: bold, so it reads on light and dark themes alike.
    #[must_use]
    pub fn value(self, text: &str) -> String {
        self.tint(text, "1")
    }

    /// Secondary metadata (counters, timings, timestamps, gutters): neutral 256-colour 244.
    ///
    /// Never SGR `2`: many terminals render dim as the default foreground, erasing the role.
    #[must_use]
    pub fn meta(self, text: &str) -> String {
        self.tint(text, "38;5;244")
    }

    /// Thin alias kept for callers outside this node's paths; delegates to [`Self::meta`].
    #[must_use]
    pub fn dim(self, text: &str) -> String {
        self.meta(text)
    }

    /// An instruction, verb, or step kind (`RUN`, `FROM`, `start`, `image`): magenta.
    ///
    /// The one hue bound to the keyword role on every surface — phase rows, the `logs -r` header,
    /// verdicts, and the build-end row.
    #[must_use]
    pub fn verb(self, text: &str) -> String {
        self.tint(text, "35")
    }

    /// A component kind or identifier: cyan.
    #[must_use]
    pub fn kind(self, text: &str) -> String {
        self.tint(text, "36")
    }

    /// A variable or secret key: bold; cyan stays reserved for commands and URLs.
    #[must_use]
    pub fn key(self, text: &str) -> String {
        self.tint(text, "1")
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
    /// Every settled/reached word (`cached`, `up to date`, `built`, `pulled`, `ready`, `running`,
    /// `done`) is green; `starting`/`booting`/`stopped`/`orphaned` are yellow; `failed` is red. Cyan
    /// stays reserved for a copyable `next:` command and primary URLs. `built 6 steps` tints only
    /// the lifecycle word, so the plain step count never becomes a second semantic. An unknown
    /// word stays plain.
    #[must_use]
    pub fn status(self, state: &str) -> String {
        if let Some((word, rest)) = state.split_once(' ')
            && word.eq_ignore_ascii_case("built")
        {
            return format!("{} {rest}", self.tint(word, "32"));
        }
        let code = match state.to_ascii_lowercase().as_str() {
            "cached" | "up-to-date" | "up to date" | "built" | "pulled" | "ready" | "running"
            | "done" => "32",
            "starting" | "booting" | "stopped" | "orphaned" => "33",
            "failed" => "31",
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

    /// The `logs -r` header row.
    ///
    /// `#id` bold, verb magenta, target bold, command neutral (omitted when it equals the
    /// target), started dim. The caller formats the started clock so `ui` stays below `commands`.
    #[must_use]
    pub fn header_line(
        self,
        run_id: &str,
        verb: &str,
        target: &str,
        command: &str,
        started: Option<&str>,
    ) -> String {
        let mut fields = vec![
            self.tint(&format!("#{run_id}"), "1"),
            self.verb(verb),
            self.tint(target, "1"),
        ];
        if !command.trim().is_empty() && command.trim() != target.trim() {
            fields.push(command.to_owned());
        }
        if let Some(started) = started {
            fields.push(self.meta(started));
        }
        fields.join("  ")
    }

    /// The capture gutter glyph for one wire stream: stdout `│` dim, stderr `┃` yellow.
    ///
    /// The glyphs differ only by weight, so `NO_COLOR` and a pipe keep the streams unambiguous
    /// with zero escapes and no words.
    #[must_use]
    pub fn gutter(self, stream: &str) -> String {
        match stream {
            "stderr" => self.tint("┃", "33"),
            _ => self.meta("│"),
        }
    }

    /// The one verdict line for a settled run.
    ///
    /// `✗ RUN api failed (exit 3)` or `✓ RUN api done (0.8s)`; `next:` is never part of it, and
    /// `verb` is omitted if empty or equal to `target`.
    #[must_use]
    pub fn verdict(self, verdict: &Verdict) -> String {
        let Verdict {
            verb,
            target,
            state,
            exit_code,
            duration_ms,
        } = verdict;
        let subject = if verb.trim().is_empty() || verb.trim() == target.trim() {
            self.tint(target, "1")
        } else {
            format!("{} {}", self.verb(verb), self.tint(target, "1"))
        };
        if state == "failed" {
            let mut line = format!(
                "{} {subject} {}",
                self.tint("✗", "31"),
                self.tint("failed", "31")
            );
            if let Some(code) = exit_code {
                line.push(' ');
                line.push_str(&self.meta(&format!("(exit {code})")));
            }
            return line;
        }
        let mut line = format!("{} {subject} done", self.icon_ok());
        if let Some(millis) = duration_ms {
            line.push(' ');
            line.push_str(&self.meta(&format!("({})", elapsed_text(*millis))));
        }
        line
    }

    /// The verdict with the reason it failed, used only when no gutter line above carries it.
    ///
    /// `✗ image api failed: no build stage named `staging`` — the reason stays neutral, and the
    /// verdict keeps its own styles.
    #[must_use]
    pub fn verdict_detail(self, verdict: &Verdict, reason: &str) -> String {
        format!("{}: {reason}", self.verdict(verdict))
    }
}
