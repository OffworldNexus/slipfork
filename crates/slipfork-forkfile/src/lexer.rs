//! Hand-written, span-tracking lexer for the `Forkfile` language.

use crate::diag::Diagnostic;
use crate::expr::CommandPart;
use crate::span::{Position, Span};
use crate::token::{Token, TokenKind};

/// A streaming lexer over one `Forkfile` source string.
pub(crate) struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    line: u32,
    col: u32,
}

impl<'a> Lexer<'a> {
    /// Starts lexing at line 1, column 1.
    pub(crate) const fn new(src: &'a str) -> Self {
        Self {
            src,
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn position(&self) -> Position {
        Position::new(self.line, self.col, self.pos)
    }

    /// The next character without consuming it.
    pub(crate) fn peek_char(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    /// Consumes and returns the next character.
    pub(crate) fn bump_char(&mut self) -> Option<char> {
        let character = self.peek_char()?;
        self.pos += character.len_utf8();
        if character == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(character)
    }

    /// Skips spaces, tabs, and carriage returns (never newlines).
    pub(crate) fn skip_inline_space(&mut self) {
        while matches!(self.peek_char(), Some(' ' | '\t' | '\r')) {
            self.bump_char();
        }
    }

    /// Whether only inline space remains before the end of the line.
    pub(crate) fn at_line_end(&mut self) -> bool {
        self.skip_inline_space();
        matches!(self.peek_char(), None | Some('\n'))
    }

    /// The next non-space character, without consuming it.
    pub(crate) fn peek_significant_char(&mut self) -> Option<char> {
        self.skip_inline_space();
        self.peek_char()
    }

    /// Whether the next non-space character is `expected`.
    pub(crate) fn next_is(&mut self, expected: char) -> bool {
        self.peek_significant_char() == Some(expected)
    }

    /// Reads one bare word (path, image reference, route path, change pattern).
    pub(crate) fn raw_word(&mut self) -> Result<(String, Span), Diagnostic> {
        self.skip_inline_space();
        let start = self.position();
        while let Some(character) = self.peek_char() {
            if character.is_whitespace() || matches!(character, '{' | '}' | '(' | ')' | ',') {
                break;
            }
            self.bump_char();
        }
        if self.pos == start.offset {
            return Err(Diagnostic::new(
                Span::at(start),
                "expected a value",
                "add a path, image reference, or route path",
            ));
        }
        Ok((
            self.src[start.offset..self.pos].to_string(),
            Span::new(start, self.position()),
        ))
    }

    /// Reads the rest of the current line verbatim (trailing space trimmed).
    pub(crate) fn raw_to_line_end(&mut self) -> Result<(String, Span), Diagnostic> {
        self.skip_inline_space();
        let start = self.position();
        while let Some(character) = self.peek_char() {
            if character == '\n' {
                break;
            }
            self.bump_char();
        }
        let text = self.src[start.offset..self.pos].trim_end().to_string();
        Ok((text, Span::new(start, self.position())))
    }

    /// Lexes the next token, including comments and newlines.
    pub(crate) fn next_token(&mut self) -> Result<Token, Diagnostic> {
        self.skip_inline_space();
        let start = self.position();
        let Some(current) = self.peek_char() else {
            return Ok(Token {
                kind: TokenKind::Eof,
                span: Span::at(start),
            });
        };
        let kind = match current {
            '\n' => {
                self.bump_char();
                TokenKind::Newline
            }
            '#' => {
                while let Some(character) = self.peek_char() {
                    if character == '\n' {
                        break;
                    }
                    self.bump_char();
                }
                TokenKind::Comment(self.src[start.offset..self.pos].trim_end().to_string())
            }
            '{' => self.single(TokenKind::LBrace),
            '}' => self.single(TokenKind::RBrace),
            '[' => self.single(TokenKind::LBracket),
            ']' => self.single(TokenKind::RBracket),
            '(' => self.single(TokenKind::LParen),
            ')' => self.single(TokenKind::RParen),
            ':' => self.single(TokenKind::Colon),
            '=' => self.single(TokenKind::Eq),
            ',' => self.single(TokenKind::Comma),
            '+' => self.single(TokenKind::Plus),
            '.' => self.single(TokenKind::Dot),
            '*' => self.single(TokenKind::Star),
            '!' => self.single(TokenKind::Bang),
            '&' => self.single(TokenKind::Amp),
            '|' => self.single(TokenKind::Pipe),
            '?' => {
                self.bump_char();
                if self.peek_char() == Some('?') {
                    self.bump_char();
                    TokenKind::QuestionQuestion
                } else {
                    return Err(Diagnostic::new(
                        Span::new(start, self.position()),
                        "unexpected `?`",
                        "use `??` for a fallback value",
                    ));
                }
            }
            '"' => return self.lex_string(start),
            '@' => return self.lex_tool(start),
            '$' => return self.lex_variable(start),
            _ if current.is_ascii_alphabetic() || current == '_' => {
                while matches!(
                    self.peek_char(),
                    Some(character) if character.is_ascii_alphanumeric() || character == '_' || character == '-'
                ) {
                    self.bump_char();
                }
                TokenKind::Ident(self.src[start.offset..self.pos].to_string())
            }
            _ if current.is_ascii_digit() => return self.lex_int(start),
            other => {
                self.bump_char();
                return Err(Diagnostic::new(
                    Span::new(start, self.position()),
                    format!("unexpected character `{other}`"),
                    "remove the character or quote it inside a string",
                ));
            }
        };
        Ok(Token {
            kind,
            span: Span::new(start, self.position()),
        })
    }

    fn single(&mut self, kind: TokenKind) -> TokenKind {
        self.bump_char();
        kind
    }

    fn lex_ident_name(&mut self) -> Option<String> {
        if !matches!(self.peek_char(), Some(character) if character.is_ascii_alphabetic() || character == '_')
        {
            return None;
        }
        let start = self.pos;
        while matches!(
            self.peek_char(),
            Some(character) if character.is_ascii_alphanumeric() || character == '_' || character == '-'
        ) {
            self.bump_char();
        }
        Some(self.src[start..self.pos].to_string())
    }

    fn lex_int(&mut self, start: Position) -> Result<Token, Diagnostic> {
        while matches!(self.peek_char(), Some(character) if character.is_ascii_digit() || character == '_')
        {
            self.bump_char();
        }
        if matches!(
            self.peek_char(),
            Some(character) if character.is_ascii_alphanumeric() || character == '_'
        ) {
            while matches!(
                self.peek_char(),
                Some(character) if character.is_ascii_alphanumeric() || character == '_'
            ) {
                self.bump_char();
            }
            let text = &self.src[start.offset..self.pos];
            return Err(Diagnostic::new(
                Span::new(start, self.position()),
                format!("invalid number `{text}`"),
                "write an integer such as `8000`",
            ));
        }
        let text = &self.src[start.offset..self.pos];
        let value = text.replace('_', "").parse::<u64>().map_err(|_| {
            Diagnostic::new(
                Span::new(start, self.position()),
                "number is too large",
                "use a smaller integer",
            )
        })?;
        Ok(Token {
            kind: TokenKind::Int(value),
            span: Span::new(start, self.position()),
        })
    }

    fn lex_tool(&mut self, start: Position) -> Result<Token, Diagnostic> {
        self.bump_char();
        match self.lex_ident_name() {
            Some(name) => Ok(Token {
                kind: TokenKind::Tool(name),
                span: Span::new(start, self.position()),
            }),
            None => Err(Diagnostic::new(
                Span::new(start, self.position()),
                "expected a tool name after `@`",
                "write `@uv run ...`",
            )),
        }
    }

    fn lex_variable(&mut self, start: Position) -> Result<Token, Diagnostic> {
        self.bump_char();
        let Some(name) = self.lex_ident_name() else {
            return Err(Diagnostic::new(
                Span::new(start, self.position()),
                "expected a variable name after `$`",
                "use `$port` or `$env.KEY`",
            ));
        };
        match name.as_str() {
            "port" => Ok(Token {
                kind: TokenKind::Port,
                span: Span::new(start, self.position()),
            }),
            "env" => {
                if self.peek_char() != Some('.') {
                    return Err(Diagnostic::new(
                        Span::new(start, self.position()),
                        "`$env` needs a key",
                        "write `$env.KEY`",
                    ));
                }
                self.bump_char();
                let Some(key) = self.lex_ident_name() else {
                    return Err(Diagnostic::new(
                        Span::new(start, self.position()),
                        "expected a key after `$env.`",
                        "write `$env.KEY`",
                    ));
                };
                Ok(Token {
                    kind: TokenKind::Env(key),
                    span: Span::new(start, self.position()),
                })
            }
            other => Err(Diagnostic::new(
                Span::new(start, self.position()),
                format!("unknown variable `${other}`"),
                "use `$port` or `$env.KEY`",
            )),
        }
    }

    fn lex_string(&mut self, start: Position) -> Result<Token, Diagnostic> {
        self.bump_char();
        let mut value = String::new();
        loop {
            match self.peek_char() {
                None | Some('\n') => return Err(Self::unterminated(start, self.position())),
                Some('"') => {
                    self.bump_char();
                    return Ok(Token {
                        kind: TokenKind::Str(value),
                        span: Span::new(start, self.position()),
                    });
                }
                Some('\\') => {
                    let escape_start = self.position();
                    self.bump_char();
                    let Some(escaped) = self.bump_char() else {
                        return Err(Self::unterminated(start, self.position()));
                    };
                    match escaped {
                        '"' => value.push('"'),
                        '\\' => value.push('\\'),
                        'n' => value.push('\n'),
                        't' => value.push('\t'),
                        'r' => value.push('\r'),
                        '\n' => return Err(Self::unterminated(start, self.position())),
                        other => {
                            return Err(Diagnostic::new(
                                Span::new(escape_start, self.position()),
                                format!("unknown escape `\\{other}`"),
                                "use `\\\"`, `\\\\`, `\\n`, `\\t`, or `\\r`",
                            ));
                        }
                    }
                }
                Some(_) => {
                    if let Some(character) = self.bump_char() {
                        value.push(character);
                    }
                }
            }
        }
    }

    fn unterminated(start: Position, end: Position) -> Diagnostic {
        Diagnostic::new(
            Span::new(start, end),
            "unterminated string literal",
            "close the string with `\"` before the end of the line",
        )
    }
}

/// Splits a raw shell command into verbatim text and `$port` / `$env.KEY` sites.
///
/// This is the single owner of `Forkfile` shell interpolation scanning.
pub(crate) fn command_parts(body: &str, start: Position) -> Vec<CommandPart> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut chars = body.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        if character != '$' {
            text.push(character);
            continue;
        }
        let rest = &body[index + 1..];
        if let Some(after) = rest.strip_prefix("port") {
            let boundary = !matches!(after.chars().next(), Some(ch) if ch.is_ascii_alphanumeric() || ch == '_');
            if boundary {
                if !text.is_empty() {
                    parts.push(CommandPart::Text(std::mem::take(&mut text)));
                }
                parts.push(CommandPart::Port);
                for _ in 0..4 {
                    chars.next();
                }
                continue;
            }
        }
        if let Some(after) = rest.strip_prefix("env.") {
            let end = after
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                .unwrap_or(after.len());
            let key = &after[..end];
            if !key.is_empty() {
                if !text.is_empty() {
                    parts.push(CommandPart::Text(std::mem::take(&mut text)));
                }
                parts.push(CommandPart::Env {
                    key: key.to_string(),
                    span: Span::new(
                        command_position(start, &body[..index]),
                        command_position(start, &body[..index + 4 + key.len()]),
                    ),
                });
                for _ in 0..4 + key.len() {
                    chars.next();
                }
                continue;
            }
        }
        text.push('$');
    }
    if !text.is_empty() {
        parts.push(CommandPart::Text(text));
    }
    parts
}

fn command_position(start: Position, prefix: &str) -> Position {
    Position::new(
        start.line,
        start
            .col
            .saturating_add(u32::try_from(prefix.chars().count()).unwrap_or(u32::MAX)),
        start.offset + prefix.len(),
    )
}
