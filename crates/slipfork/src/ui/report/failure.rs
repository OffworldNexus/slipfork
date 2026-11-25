//! One `Failure` value: the human `error:`/`next:` lines and the `--json` projection.
//!
//! Both shapes derive from the same fields, so a command cannot print one message and serialize
//! another. Empty lines inside a message are dropped so the block starts immediately after the
//! last row, and exactly one `next:` line closes it.

use serde::Serialize;

use crate::ui::{Palette, Verdict, elapsed_text};

use super::row::FailureFacts;

/// One CLI failure: the stable code, the human message, the one remedy, and optional typed facts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Failure {
    /// The stable machine code (`ingress-bind`, `usage`, …).
    pub code: String,
    /// The human message, possibly multi-line and possibly carrying a leading `✗`.
    pub message: String,
    /// The one `next:` remedy.
    pub next: String,
    /// Typed settlement facts, when a step produced them.
    pub facts: Option<FailureFacts>,
}

impl Failure {
    /// Builds one failure from its three required strings.
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        next: impl Into<String>,
    ) -> Self {
        let next = next.into().trim().to_owned();
        debug_assert!(
            !next.is_empty(),
            "a Failure always carries one non-empty `next:` remedy"
        );
        Self {
            code: code.into().trim().to_owned(),
            message: message.into().trim().to_owned(),
            next,
            facts: None,
        }
    }

    /// Builds a lifecycle failure whose message may already carry its own `next:` tail.
    ///
    /// The embedded remedy wins so a daemon message and its projection never disagree; `next` is
    /// only the fallback when the message carries none.
    #[must_use]
    pub fn remedied(
        code: impl Into<String>,
        message: impl Into<String>,
        next: impl Into<String>,
    ) -> Self {
        let (message, embedded) = crate::ui::split_remedy(&message.into());
        Self::new(code, message, embedded.unwrap_or_else(|| next.into()))
    }

    /// Builds the one failure a settled step reports, keeping its typed facts.
    #[must_use]
    pub fn from_facts(facts: FailureFacts) -> Self {
        let mut subject = facts.kind.clone();
        if let Some(target) = facts.target.as_deref().filter(|target| !target.is_empty()) {
            subject.push(' ');
            subject.push_str(target);
        }
        let mut message = format!("✗ {subject} failed");
        if let Some(exit_code) = facts.exit_code {
            message.push_str(&format!(" (exit {exit_code})"));
        }
        if !facts.reason.is_empty() {
            message.push('\n');
            message.push_str(&facts.reason);
        }
        Self {
            code: facts.code.clone(),
            message,
            next: facts.next.clone(),
            facts: Some(facts),
        }
    }

    /// The exact `{"error":{"code","message","next"}}` object `--json` emits.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        let mut lines = self.message.lines().filter(|line| !line.trim().is_empty());
        let first = lines.next().unwrap_or("the command failed");
        let first = first
            .strip_prefix("error: ")
            .or(first.strip_prefix("✗ "))
            .unwrap_or(first);
        let message = std::iter::once(first)
            .chain(lines)
            .collect::<Vec<_>>()
            .join("\n");
        serde_json::json!({
            "error": {
                "code": self.code,
                "message": message,
                "next": self.next,
            }
        })
    }

    /// The exact stderr lines a human reads: `error:` body plus exactly one `next:` remedy.
    #[must_use]
    pub fn human(&self, palette: Palette, verbose: bool) -> Vec<String> {
        let mut lines = self.message.lines().filter(|line| !line.trim().is_empty());
        let first = lines.next().unwrap_or("the command failed");
        let mut rows = Vec::new();
        if let Some(rest) = first.strip_prefix("✗ ") {
            rows.push(format!("{} {}", palette.icon_bad(), palette.value(rest)));
            for line in lines {
                rows.push(palette.value(line));
            }
        } else {
            let first = first.strip_prefix("error: ").unwrap_or(first);
            rows.push(format!(
                "{} {}",
                palette.error("error:"),
                palette.error(first)
            ));
            for line in lines {
                rows.push(palette.error(line));
            }
        }
        if verbose && let Some(runtime) = self.facts.as_ref().and_then(|facts| facts.runtime_ms) {
            rows.push(format!(
                "  runtime: {}",
                palette.meta(&elapsed_text(runtime))
            ));
        }
        let next = self.next.lines().next().unwrap_or_default().trim();
        if !next.is_empty() {
            rows.push(format!(
                "{} {}",
                palette.meta("next:"),
                palette.tint(next, "36")
            ));
        }
        rows
    }
}

impl FailureFacts {
    /// The boot pane's only projection of typed facts: one red `✗` verdict, the `-v`-only
    /// plumbing row, and exactly one indented `next:` remedy.
    ///
    /// `verb` is the token the phase row printed (`RUN`, `start`, `hook`). A failure with no
    /// phase row (`step_less`) carries an exit-less reason on the verdict line itself; every
    /// other failure leaves the reason to the streamed gutter line.
    #[must_use]
    pub fn boot_lines(
        &self,
        verb: &str,
        plumbing: Option<&str>,
        step_less: bool,
        verbose: bool,
        palette: Palette,
    ) -> Vec<String> {
        let verdict = Verdict {
            verb: verb.to_owned(),
            target: self
                .target
                .as_deref()
                .filter(|target| !target.is_empty())
                .unwrap_or("step")
                .to_owned(),
            state: "failed".to_owned(),
            exit_code: self.exit_code,
            duration_ms: None,
        };
        let mut rows = vec![match self.reason.as_str() {
            reason if step_less && self.exit_code.is_none() && !reason.is_empty() => {
                palette.verdict_detail(&verdict, reason)
            }
            _ => palette.verdict(&verdict),
        }];
        if verbose && let Some(plumbing) = plumbing.filter(|text| !text.is_empty()) {
            rows.push(format!(
                "  {}",
                palette.meta(&format!("runtime: {plumbing}"))
            ));
        }
        if !self.next.is_empty() {
            rows.push(format!(
                "  {} {}",
                palette.meta("next:"),
                palette.tint(&self.next, "36")
            ));
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::Failure;
    use crate::ui::Palette;
    use crate::ui::report::{Counter, FailureFacts};

    #[test]
    fn no_color_prints_one_error_line_and_one_next_without_escapes() {
        let rows = Failure::new(
            "build",
            "build `api` failed at `RUN apt-get update`: expired metadata",
            "slipfork logs -r 3f9c21",
        )
        .human(Palette::new(false), false);
        assert_eq!(
            rows,
            vec![
                "error: build `api` failed at `RUN apt-get update`: expired metadata".to_owned(),
                "next: slipfork logs -r 3f9c21".to_owned(),
            ]
        );
        assert!(rows.iter().all(|row| !row.contains('\u{1b}')));
    }

    #[test]
    fn colored_roles_are_red_error_meta_next_and_cyan_command() {
        let rows = Failure::new(
            "ingress-bind",
            "cannot bind 127.0.0.1:80",
            "use another port",
        )
        .human(Palette::new(true), false);
        assert_eq!(
            rows,
            vec![
                "\u{1b}[31merror:\u{1b}[0m \u{1b}[31mcannot bind 127.0.0.1:80\u{1b}[0m".to_owned(),
                "\u{1b}[38;5;244mnext:\u{1b}[0m \u{1b}[36muse another port\u{1b}[0m".to_owned(),
            ]
        );
    }

    #[test]
    fn a_typed_verdict_keeps_its_glyph_and_still_gets_one_next() {
        assert_eq!(
            Failure::new(
                "run",
                "✗ RUN api failed (exit 3)",
                "slipfork logs -r abc123"
            )
            .human(Palette::new(false), false),
            vec![
                "✗ RUN api failed (exit 3)".to_owned(),
                "next: slipfork logs -r abc123".to_owned(),
            ]
        );
    }

    #[test]
    fn an_embedded_next_wins_over_the_fallback_remedy() {
        let failure = Failure::remedied(
            "supervisor-exited",
            "the supervisor exited\nnext: inspect `.slipfork/supervisor.log`",
            "retry `slipfork up`",
        );
        assert_eq!(failure.next, "inspect `.slipfork/supervisor.log`");
        assert_eq!(
            failure.human(Palette::new(false), false),
            vec![
                "error: the supervisor exited".to_owned(),
                "next: inspect `.slipfork/supervisor.log`".to_owned(),
            ]
        );
    }

    #[test]
    fn the_json_projection_keeps_every_message_line() {
        let value = Failure::new(
            "boot-failed",
            "the supervisor exited\n\ninspect `.slipfork/supervisor.log`",
            "run `slipfork up`",
        )
        .json();
        assert_eq!(
            value["error"]["message"],
            "the supervisor exited\ninspect `.slipfork/supervisor.log`"
        );
    }

    #[test]
    fn the_json_projection_carries_the_same_three_strings() {
        let value = Failure::new(
            "usage",
            "error: unexpected argument",
            "run `slipfork --help`",
        )
        .json();
        assert_eq!(
            value,
            serde_json::json!({
                "error": {
                    "code": "usage",
                    "message": "unexpected argument",
                    "next": "run `slipfork --help`",
                }
            })
        );
    }

    fn facts(runtime_ms: Option<u64>) -> FailureFacts {
        FailureFacts {
            code: "build".to_owned(),
            kind: "RUN".to_owned(),
            phase: Some("build".to_owned()),
            target: Some("api".to_owned()),
            at: Some(Counter { index: 1, total: 2 }),
            exit_code: Some(3),
            run_id: Some("abc123".to_owned()),
            reason: "no such file".to_owned(),
            runtime_ms,
            next: "slipfork logs -r abc123".to_owned(),
        }
    }

    #[test]
    fn from_facts_renders_the_glyph_exit_and_reason() {
        let failure = Failure::from_facts(facts(Some(1_200)));
        assert_eq!(
            failure.human(Palette::new(false), false),
            vec![
                "✗ RUN api failed (exit 3)".to_owned(),
                "no such file".to_owned(),
                "next: slipfork logs -r abc123".to_owned(),
            ]
        );
        assert_eq!(
            failure.json()["error"]["message"],
            "RUN api failed (exit 3)\nno such file",
            "the json projection drops the typed glyph and keeps the body"
        );
        assert_eq!(failure.json()["error"]["code"], "build");
        assert_eq!(failure.json()["error"]["next"], "slipfork logs -r abc123");
    }

    #[test]
    fn verbose_adds_exactly_one_runtime_line_before_the_remedy() {
        let failure = Failure::from_facts(facts(Some(1_200)));
        let plain = failure.human(Palette::new(false), false);
        let verbose = failure.human(Palette::new(false), true);
        assert_eq!(verbose.len(), plain.len() + 1, "{verbose:?}");
        let runtime = verbose
            .iter()
            .position(|line| line.starts_with("  runtime: "))
            .expect("the verbose detail line");
        assert_eq!(runtime, verbose.len() - 2, "runtime sits before next:");
        assert_eq!(verbose.last(), plain.last(), "same one remedy line");
    }

    #[test]
    fn boot_lines_project_one_verdict_and_one_indented_next() {
        let rows = facts(None).boot_lines("RUN", None, false, false, Palette::new(false));
        assert_eq!(
            rows,
            vec![
                "✗ RUN api failed (exit 3)".to_owned(),
                "  next: slipfork logs -r abc123".to_owned(),
            ]
        );
        let block = rows.join("\n");
        assert_eq!(block.matches("next:").count(), 1, "{block}");
        assert!(
            !block.contains("no such file"),
            "the gutter owns the reason"
        );
        assert!(!block.contains('\u{1b}'), "{block}");
    }

    #[test]
    fn boot_lines_add_the_dim_runtime_row_before_the_remedy_only_under_verbose() {
        let rows = facts(None).boot_lines(
            "RUN",
            Some("apt-get install -y blastit"),
            false,
            true,
            Palette::new(false),
        );
        assert_eq!(
            rows,
            vec![
                "✗ RUN api failed (exit 3)".to_owned(),
                "  runtime: apt-get install -y blastit".to_owned(),
                "  next: slipfork logs -r abc123".to_owned(),
            ]
        );
        let plain = facts(None).boot_lines(
            "RUN",
            Some("apt-get install -y blastit"),
            false,
            false,
            Palette::new(false),
        );
        assert!(
            plain.iter().all(|row| !row.contains("apt-get")),
            "{plain:?}"
        );
    }

    #[test]
    fn boot_lines_color_the_verdict_red_and_the_remedy_cyan() {
        let rows = facts(None).boot_lines("RUN", None, false, false, Palette::new(true));
        assert!(rows[0].contains("\u{1b}[31m✗"), "{rows:?}");
        assert!(rows[0].contains("\u{1b}[31mfailed"), "{rows:?}");
        assert!(
            rows[1].contains("\u{1b}[36mslipfork logs -r abc123"),
            "{rows:?}"
        );
        assert!(
            rows.iter().all(|row| !row.contains("\u{1b}[9")),
            "no bright codes: {rows:?}"
        );
    }

    #[test]
    fn boot_lines_a_step_less_failure_carries_its_reason_on_the_verdict() {
        let mut facts = facts(None);
        facts.exit_code = None;
        facts.reason = "Dockerfile:2: instruction appears before any `FROM`".to_owned();
        let rows = facts.boot_lines("image", None, true, false, Palette::new(false));
        assert_eq!(
            rows,
            vec![
                "✗ image api failed: Dockerfile:2: instruction appears before any `FROM`"
                    .to_owned(),
                "  next: slipfork logs -r abc123".to_owned(),
            ]
        );
    }

    #[test]
    fn boot_lines_a_missing_exit_code_and_remedy_never_print_empty_fields() {
        let facts = FailureFacts {
            code: "boot-failed".to_owned(),
            kind: "boot".to_owned(),
            phase: None,
            target: Some("web".to_owned()),
            at: None,
            exit_code: None,
            run_id: None,
            reason: String::new(),
            runtime_ms: None,
            next: String::new(),
        };
        assert_eq!(
            facts.boot_lines("start", None, false, false, Palette::new(false)),
            vec!["✗ start web failed".to_owned()]
        );
    }
}
