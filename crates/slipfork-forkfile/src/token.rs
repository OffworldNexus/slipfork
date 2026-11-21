//! Tokens produced by the `Forkfile` lexer.

use crate::span::Span;

/// Every lexical token kind in the `Forkfile` grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// An identifier or bare feature name.
    Ident(String),
    /// A decimal integer literal.
    Int(u64),
    /// A double-quoted string with escapes already decoded.
    Str(String),
    /// An `@tool` auto-prequel tool name.
    Tool(String),
    /// The `$port` interpolation.
    Port,
    /// The `$env.KEY` interpolation.
    Env(String),
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `:`
    Colon,
    /// `=`
    Eq,
    /// `,`
    Comma,
    /// `.`
    Dot,
    /// `*`
    Star,
    /// `??`
    QuestionQuestion,
    /// `!`
    Bang,
    /// `&`
    Amp,
    /// `|`
    Pipe,
    /// A `#` line comment, retained verbatim for `fmt`.
    Comment(String),
    /// A line break.
    Newline,
    /// End of input.
    Eof,
}

impl TokenKind {
    /// A human-readable description used in diagnostics.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Ident(name) => format!("`{name}`"),
            Self::Int(value) => format!("`{value}`"),
            Self::Str(_) => "a string".to_string(),
            Self::Tool(name) => format!("`@{name}`"),
            Self::Port => "`$port`".to_string(),
            Self::Env(key) => format!("`$env.{key}`"),
            Self::LBrace => "`{`".to_string(),
            Self::RBrace => "`}`".to_string(),
            Self::LBracket => "`[`".to_string(),
            Self::RBracket => "`]`".to_string(),
            Self::LParen => "`(`".to_string(),
            Self::RParen => "`)`".to_string(),
            Self::Colon => "`:`".to_string(),
            Self::Eq => "`=`".to_string(),
            Self::Comma => "`,`".to_string(),
            Self::Dot => "`.`".to_string(),
            Self::Star => "`*`".to_string(),
            Self::QuestionQuestion => "`??`".to_string(),
            Self::Bang => "`!`".to_string(),
            Self::Amp => "`&`".to_string(),
            Self::Pipe => "`|`".to_string(),
            Self::Comment(_) => "a comment".to_string(),
            Self::Newline => "end of line".to_string(),
            Self::Eof => "end of file".to_string(),
        }
    }
}

/// A token paired with the source span it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// What the token is.
    pub kind: TokenKind,
    /// Where the token came from.
    pub span: Span,
}
