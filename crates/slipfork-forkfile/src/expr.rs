//! `Forkfile` expressions, secret generators, and shell interpolation parts.

use crate::diag::Diagnostic;
use crate::span::Span;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandPart {
    Text(String),
    Port,
    Env(String),
}

/// A shell command with `$port` / `$env.KEY` interpolation already parsed.
#[derive(Debug, Clone)]
pub struct Command {
    /// The `@tool` prequel, when declared.
    pub tool: Option<String>,
    /// Verbatim text interleaved with interpolation sites.
    pub parts: Vec<CommandPart>,
    /// The command source span.
    pub span: Span,
}

ast_eq!(Command { tool, parts });

impl Command {
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
        let (tool, body) = match text.strip_prefix('@') {
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
                let body = rest[end..].trim_start();
                if body.is_empty() {
                    return Err(Diagnostic::new(
                        span,
                        format!("expected a command after `@{name}`"),
                        format!("write `@{name} <command>`"),
                    ));
                }
                (Some(name.to_string()), body)
            }
            None => (None, text),
        };
        Ok(Self {
            tool,
            parts: crate::lexer::command_parts(body),
            span,
        })
    }
}
