//! `Forkfile:<line>:<col>` diagnostics with caret spans and `next:` remedies.

use crate::span::Span;
use std::fmt;

/// One parse or lexical error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The offending source region.
    pub span: Span,
    /// A single-line description of the problem.
    pub message: String,
    /// The concrete remedy shown after `next:`.
    pub next: String,
}

impl Diagnostic {
    /// Builds a diagnostic.
    pub fn new(span: Span, message: impl Into<String>, next: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
            next: next.into(),
        }
    }
}

/// A collection of diagnostics produced by one parse.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnostics {
    /// Diagnostics in source order.
    pub items: Vec<Diagnostic>,
}

impl Diagnostics {
    /// Builds an empty collection.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a diagnostic.
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.items.push(diagnostic);
    }

    /// Whether no diagnostics were collected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The number of collected diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// The first diagnostic, if any.
    #[must_use]
    pub fn first(&self) -> Option<&Diagnostic> {
        self.items.first()
    }

    /// Renders every diagnostic against the original source with a caret underline.
    #[must_use]
    pub fn render(&self, source: &str) -> String {
        let mut out = String::new();
        for diagnostic in &self.items {
            let line = diagnostic.span.line().max(1);
            let column = diagnostic.span.col().max(1);
            out.push_str(&format!("Forkfile:{line}:{column}\n"));
            let text = source.lines().nth(line as usize - 1).unwrap_or("");
            out.push_str(text);
            out.push('\n');
            for character in text.chars().take(column as usize - 1) {
                out.push(if character == '\t' { '\t' } else { ' ' });
            }
            out.push_str("^\n");
            out.push_str(&diagnostic.message);
            out.push('\n');
            out.push_str("next: ");
            out.push_str(&diagnostic.next);
            out.push_str("\n\n");
        }
        out.trim_end().to_string()
    }
}

impl fmt::Display for Diagnostics {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for diagnostic in &self.items {
            writeln!(
                formatter,
                "Forkfile:{}:{}: {}",
                diagnostic.span.line(),
                diagnostic.span.col(),
                diagnostic.message
            )?;
            writeln!(formatter, "  next: {}", diagnostic.next)?;
        }
        Ok(())
    }
}

impl std::error::Error for Diagnostics {}
