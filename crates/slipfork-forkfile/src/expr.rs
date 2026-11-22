//! `Forkfile` expressions, secret generators, and shell interpolation parts.

use crate::diag::Diagnostic;
use crate::span::{Position, Span};

/// A `Forkfile` expression annotated with its source span.
#[derive(Debug, Clone)]
pub struct Expr {
    /// The expression shape.
    pub kind: ExprKind,
    /// Where the expression starts and ends.
    pub span: Span,
}

ast_eq!(Expr { kind });

/// The shape of an expression.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// `true` / `false`
    Bool(bool),
    /// An integer literal.
    Int(u64),
    /// A string literal.
    String(String),
    /// A bare feature name.
    Feature(String),
    /// `!<expr>`
    Not(Box<Expr>),
    /// `<expr> & <expr>`
    And(Box<Expr>, Box<Expr>),
    /// `<expr> | <expr>`
    Or(Box<Expr>, Box<Expr>),
    /// `<left> + <right>` string concatenation.
    Concat(Box<Expr>, Box<Expr>),
    /// `<expr> ?? <expr>`
    Fallback(Box<Expr>, Box<Expr>),
    /// `var(KEY)`
    Var(String),
    /// `secret(KEY)`
    Secret(String),
    /// `random_hex(n)`, `random_base64(n)`, `random_alnum(n)`, `random_uuid()`
    Random {
        /// Generator family.
        kind: RandomKind,
        /// Requested length, if any.
        n: Option<u64>,
    },
    /// `<base>.<prop>` typed resource property.
    Property {
        /// Resource symbol.
        base: String,
        /// Typed property.
        prop: String,
    },
    /// `$port`
    Port,
    /// `$env.KEY`
    EnvVar(String),
}

/// The secret generator families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RandomKind {
    Hex,
    Base64,
    Alnum,
    Uuid,
}

/// One segment of an interpolated shell command.
#[derive(Debug, Clone)]
pub enum CommandPart {
    Text(String),
    Port,
    Env {
        /// The referenced key.
        key: String,
        /// The exact `$env.KEY` span.
        span: Span,
    },
}

impl PartialEq for CommandPart {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Text(left), Self::Text(right)) => left == right,
            (Self::Port, Self::Port) => true,
            (Self::Env { key: left, .. }, Self::Env { key: right, .. }) => left == right,
            _ => false,
        }
    }
}

impl Eq for CommandPart {}

/// An `@<tool>` prequel with its exact source span.
#[derive(Debug, Clone)]
pub struct Tool {
    /// The tool name without `@`.
    pub name: String,
    /// The exact `@tool` span.
    pub span: Span,
}

impl PartialEq for Tool {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Tool {}

/// A shell command with `$port` / `$env.KEY` interpolation already parsed.
#[derive(Debug, Clone)]
pub struct Command {
    /// The `@tool` prequel, when declared.
    pub tool: Option<Tool>,
    /// Verbatim text interleaved with interpolation sites.
    pub parts: Vec<CommandPart>,
    /// The command source span.
    pub span: Span,
}

ast_eq!(Command { tool, parts });

impl Command {
    /// Whether any part interpolates a `$env.<key>` site.
    #[must_use]
    pub fn uses_env(&self) -> bool {
        self.parts
            .iter()
            .any(|part| matches!(part, CommandPart::Env { .. }))
    }

    /// Renders the command back to `Forkfile` source text.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::new();
        if let Some(tool) = &self.tool {
            out.push('@');
            out.push_str(&tool.name);
            if !self.parts.is_empty() {
                out.push(' ');
            }
        }
        for part in &self.parts {
            match part {
                CommandPart::Text(text) => out.push_str(text),
                CommandPart::Port => out.push_str("$port"),
                CommandPart::Env { key, .. } => {
                    out.push_str("$env.");
                    out.push_str(key);
                }
            }
        }
        out
    }

    /// Parses a raw command line, splitting the `@tool` prequel and interpolations.
    pub fn parse(raw: &str, span: Span) -> Result<Self, Diagnostic> {
        let text = raw.trim();
        if text.is_empty() {
            return Err(Diagnostic::new(
                span,
                "expected a command",
                "add a shell command after `:`",
            ));
        }
        let leading_bytes = raw.len() - raw.trim_start().len();
        let leading = &raw[..leading_bytes];
        let (tool, body, body_start) = match text.strip_prefix('@') {
            Some(rest) => {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                let name = &rest[..end];
                if name.is_empty() {
                    return Err(Diagnostic::new(
                        span,
                        "expected a tool name after `@`",
                        "write `@uv run ...`",
                    ));
                }
                if rest[end..].trim().is_empty() {
                    return Err(Diagnostic::new(
                        span,
                        format!("expected a command after `@{name}`"),
                        format!("write `@{name} <command>`"),
                    ));
                }
                let tool_start = advance(span.start, &format!("{leading}@"));
                let skipped = rest[end..].len() - rest[end..].trim_start().len();
                let body_offset = leading_bytes + 1 + end + skipped;
                (
                    Some(Tool {
                        name: name.to_string(),
                        span: Span::new(tool_start, advance(tool_start, name)),
                    }),
                    &raw[body_offset..],
                    advance(span.start, &raw[..body_offset]),
                )
            }
            None => (None, text, advance(span.start, leading)),
        };
        Ok(Self {
            tool,
            parts: crate::lexer::command_parts(body, body_start),
            span,
        })
    }
}

/// Splits an `@<tool>` prequel off a raw command, returning the tool name and the **verbatim**
/// remainder: only the `@tool` prefix and the whitespace run after it are removed.
///
/// This is the single owner of `@<tool>` recognition; callers never re-scan argv for `@`.
#[must_use]
pub fn tool_prefix(command: &str) -> Option<(&str, &str)> {
    let text = command.trim_start();
    let rest = text.strip_prefix('@')?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let name = &rest[..end];
    if name.is_empty() {
        return None;
    }
    Some((name, rest[end..].trim_start()))
}

/// Inserts `--no-sync` after the `run` subcommand of an `@uv` command.
///
/// The remainder is spliced back byte-for-byte, so `@uv  run\tapp` keeps every separator; an
/// already-synced command and any non-`uv` tool are returned unchanged.
#[must_use]
pub fn no_sync_uv(command: &str) -> String {
    let Some(("uv", rest)) = tool_prefix(command) else {
        return command.to_owned();
    };
    let tokens: Vec<&str> = rest.split_ascii_whitespace().collect();
    if tokens.first() != Some(&"run") || tokens.contains(&"--no-sync") {
        return command.to_owned();
    }
    let leading = rest.len() - rest.trim_start().len();
    let insert = command.len() - rest.len() + leading + "run".len();
    let mut normalized = command[..insert].to_owned();
    normalized.push_str(" --no-sync");
    normalized.push_str(&command[insert..]);
    normalized
}

fn advance(start: Position, prefix: &str) -> Position {
    Position::new(
        start.line,
        start
            .col
            .saturating_add(u32::try_from(prefix.chars().count()).unwrap_or(u32::MAX)),
        start.offset + prefix.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::{no_sync_uv, tool_prefix};

    #[test]
    fn tool_prefix_returns_the_tool_and_verbatim_remainder() {
        assert_eq!(tool_prefix("@uv run app"), Some(("uv", "run app")));
        assert_eq!(tool_prefix("  @uv\trun  app"), Some(("uv", "run  app")));
        assert_eq!(tool_prefix("@node app.js"), Some(("node", "app.js")));
        assert_eq!(tool_prefix("uv run app"), None);
        assert_eq!(tool_prefix("@"), None);
    }

    #[test]
    fn no_sync_uv_preserves_every_separator() {
        assert_eq!(no_sync_uv("@uv run app"), "@uv run --no-sync app");
        assert_eq!(no_sync_uv("@uv  run  app"), "@uv  run --no-sync  app");
        assert_eq!(no_sync_uv("@uv\trun\tapp"), "@uv\trun --no-sync\tapp");
        assert_eq!(no_sync_uv("  @uv run  app"), "  @uv run --no-sync  app");
        assert_eq!(no_sync_uv("@uv run --no-sync app"), "@uv run --no-sync app");
        assert_eq!(no_sync_uv("@uv sync"), "@uv sync");
        assert_eq!(no_sync_uv("uv run app"), "uv run app");
    }
}
