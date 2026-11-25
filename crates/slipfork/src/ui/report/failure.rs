//! One `Failure` value: the human `error:`/`next:` lines and the `--json` projection.
//!
//! Both shapes derive from the same fields, so a command cannot print one message and serialize
//! another. Empty lines inside a message are dropped so the block starts immediately after the
//! last row, and exactly one `next:` line closes it.

use serde::Serialize;

use crate::ui::{Palette, elapsed_text};

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
        let message = self
            .message
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("the command failed");
        let message = message
            .strip_prefix("error: ")
            .or(message.strip_prefix("✗ "))
            .unwrap_or(message);
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
            "RUN api failed (exit 3)",
            "the json projection drops the typed glyph"
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
}
