//! The one named-role palette for every human CLI surface.
//!
//! A [`Palette`] is a plain value: the composition root resolves colour once, passes it down, and
//! every method degrades to the exact input text when colour is off. No caller formats an SGR code
//! by hand, so `NO_COLOR`, `TERM=dumb`, a pipe, and `--json` all emit zero `ESC[` bytes.

use crate::ui::elapsed_text;

use super::paint::paint;

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

/// The role one diff line plays, so the palette owns every diff hue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRole {
    /// A hunk marker (`@@`) or file header (`---`/`+++`): bold.
    Header,
    /// An added line: green.
    Added,
    /// A removed line: red.
    Removed,
    /// Unchanged surrounding context: untouched.
    Context,
}

impl DiffRole {
    /// Classifies one diff line by its leading marker, or `None` for context.
    #[must_use]
    pub fn of(line: &str) -> Option<Self> {
        if line.starts_with("---") || line.starts_with("+++") || line.starts_with("@@") {
            Some(Self::Header)
        } else if line.starts_with('+') {
            Some(Self::Added)
        } else if line.starts_with('-') {
            Some(Self::Removed)
        } else {
            None
        }
    }
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
    pub(super) fn tint(self, text: &str, code: &str) -> String {
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

    /// An instruction, verb, or step kind (`RUN`, `FROM`, `start`, `image`): magenta.
    ///
    /// The one hue bound to the keyword role on every surface — phase rows, the `logs -r` header,
    /// verdicts, and the build-end row.
    #[must_use]
    pub fn verb(self, text: &str) -> String {
        self.tint(text, "35")
    }

    /// An inner Dockerfile instruction (`from`, `copy`, `run`): a dustier magenta one level
    /// below the outer keyword role.
    #[must_use]
    pub fn substep(self, text: &str) -> String {
        self.tint(text, "38;5;132")
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

    /// A failure: red, in the one weight shared with the `error:` label.
    #[must_use]
    pub fn error(self, text: &str) -> String {
        self.tint(text, "31")
    }

    /// Success text in the plain weight (`✓ Stopped`, `Dropped`, an `active` feature).
    #[must_use]
    pub fn ok(self, text: &str) -> String {
        self.tint(text, "32")
    }

    /// A bold-green readiness row (`✓ Ready`).
    #[must_use]
    pub fn ready(self, text: &str) -> String {
        self.tint(text, "1;32")
    }

    /// Bold red failure text (`✗ 2 issue(s) found`).
    #[must_use]
    pub fn error_bold(self, text: &str) -> String {
        self.tint(text, "1;31")
    }

    /// A diagnostic report's leading success verdict (`✓ Ready`): bold green, in the settled
    /// `colour;1` order so the check result owns the first escape of the block.
    #[must_use]
    pub fn headline_ok(self, text: &str) -> String {
        self.tint(text, "32;1")
    }

    /// A diagnostic report's leading failure verdict (`✗ 2 issue(s) found`): bold red, same order.
    #[must_use]
    pub fn headline_bad(self, text: &str) -> String {
        self.tint(text, "31;1")
    }

    /// Receded, inactive content (`○`, a disabled feature's status).
    #[must_use]
    pub fn dim(self, text: &str) -> String {
        self.tint(text, "2")
    }

    /// A copyable `next:` command: cyan, never bold, because it is runnable not primary.
    #[must_use]
    pub fn next(self, text: &str) -> String {
        self.tint(text, "36")
    }

    /// A yellow caution mark (`•` for an orphaned world).
    #[must_use]
    pub fn warn(self, text: &str) -> String {
        self.tint(text, "33")
    }

    /// One diff line by [`DiffRole`]; context lines pass through untouched.
    #[must_use]
    pub fn diff(self, role: DiffRole, line: &str) -> String {
        match role {
            DiffRole::Header => self.tint(line, "1"),
            DiffRole::Added => self.ok(line),
            DiffRole::Removed => self.error(line),
            DiffRole::Context => line.to_owned(),
        }
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
            return format!("{} {rest}", self.ok(word));
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
        self.ok("✓")
    }

    /// A failure mark.
    #[must_use]
    pub fn icon_bad(self) -> String {
        self.error_bold("✗")
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
            self.value(target),
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
            "stderr" => self.warn("┃"),
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
            self.value(target)
        } else {
            format!("{} {}", self.verb(verb), self.value(target))
        };
        if state == "failed" {
            let mut line = format!("{} {subject} {}", self.error("✗"), self.error("failed"));
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{DiffRole, Palette};

    /// The raw-SGR call shapes: no caller outside this module may name a colour code.
    const RAW_SGR_CALLS: [&str; 2] = [concat!("tin", "t("), concat!("pai", "nt(")];

    #[test]
    fn the_named_roles_keep_their_settled_bytes() {
        let palette = Palette::new(true);
        assert_eq!(palette.ok("x"), "\u{1b}[32mx\u{1b}[0m");
        assert_eq!(palette.ready("x"), "\u{1b}[1;32mx\u{1b}[0m");
        assert_eq!(palette.error_bold("x"), "\u{1b}[1;31mx\u{1b}[0m");
        assert_eq!(palette.dim("x"), "\u{1b}[2mx\u{1b}[0m");
        assert_eq!(palette.next("x"), "\u{1b}[36mx\u{1b}[0m");
        assert_eq!(palette.warn("x"), "\u{1b}[33mx\u{1b}[0m");
        assert_eq!(palette.diff(DiffRole::Header, "x"), "\u{1b}[1mx\u{1b}[0m");
        assert_eq!(palette.diff(DiffRole::Added, "x"), "\u{1b}[32mx\u{1b}[0m");
        assert_eq!(palette.diff(DiffRole::Removed, "x"), "\u{1b}[31mx\u{1b}[0m");
        assert_eq!(palette.diff(DiffRole::Context, "x"), "x");
    }

    #[test]
    fn the_disabled_palette_emits_zero_escapes() {
        let plain = Palette::new(false);
        for line in [
            plain.ok("x"),
            plain.ready("x"),
            plain.error_bold("x"),
            plain.dim("x"),
            plain.next("x"),
            plain.diff(DiffRole::Added, "x"),
        ] {
            assert!(line.ends_with('x'), "{line:?}");
            assert!(!line.contains('\u{1b}'), "{line:?}");
        }
    }

    #[test]
    fn no_tint_or_paint_sgr_literal_outside_the_palette() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut violations = Vec::new();
        scan(&src, &src, &mut violations);
        assert!(
            violations.is_empty(),
            "raw SGR literal outside the palette:\n{}",
            violations.join("\n")
        );
    }

    fn scan(dir: &Path, src: &Path, violations: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                scan(&path, src, violations);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                if path == src.join("ui/report/palette.rs") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for needle in RAW_SGR_CALLS {
                    for (index, _) in text.match_indices(needle) {
                        let rest = &text[index..];
                        // The argument span runs to the call's closing `)`, so a string literal
                        // split across lines is still caught.
                        let arg_end = rest.find(')').unwrap_or(rest.len());
                        if rest[..arg_end].contains('"') {
                            let line = text[..index].matches('\n').count() + 1;
                            violations.push(format!(
                                "{}:{line}: raw colour call with an SGR string literal",
                                path.display()
                            ));
                        }
                    }
                }
            }
        }
    }
}
