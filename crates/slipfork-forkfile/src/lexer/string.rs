//! Double-quoted interpolation and single-quoted literal lexing.

use crate::diag::Diagnostic;
use crate::span::{Position, Span};
use crate::token::{Token, TokenKind};

use super::Lexer;

impl<'a> Lexer<'a> {
    /// Lexes a `"..."` string, splitting out `${<expr>}` interpolation sites.
    pub(super) fn lex_string(&mut self, start: Position) -> Result<Token, Diagnostic> {
        self.bump_char();
        let mut pieces = Vec::new();
        let mut text = String::new();
        let mut interpolated = false;
        loop {
            match self.peek_char() {
                None | Some('\n') => return Err(Self::unterminated(start, self.position())),
                Some('"') => {
                    self.bump_char();
                    if !interpolated {
                        return Ok(Token {
                            kind: TokenKind::Str(text),
                            span: Span::new(start, self.position()),
                        });
                    }
                    if !text.is_empty() {
                        pieces.push(crate::token::TemplatePiece::Text(text));
                    }
                    return Ok(Token {
                        kind: TokenKind::Template(pieces),
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
                        '"' => text.push('"'),
                        '\\' => text.push('\\'),
                        'n' => text.push('\n'),
                        't' => text.push('\t'),
                        'r' => text.push('\r'),
                        '$' => {
                            // `\${` is the ONE escape for a literal `${` in a `"..."` string.
                            if self.peek_char() == Some('{') {
                                self.bump_char();
                                text.push_str("${");
                            } else {
                                text.push('$');
                            }
                        }
                        '\n' => return Err(Self::unterminated(start, self.position())),
                        other => {
                            return Err(Diagnostic::new(
                                Span::new(escape_start, self.position()),
                                format!("unknown escape `\\{other}`"),
                                "use `\\\"`, `\\\\`, `\\${`, `\\n`, `\\t`, or `\\r`",
                            ));
                        }
                    }
                }
                Some('$') if self.src[self.pos + 1..].starts_with('{') => {
                    interpolated = true;
                    if !text.is_empty() {
                        pieces.push(crate::token::TemplatePiece::Text(std::mem::take(&mut text)));
                    }
                    self.bump_char();
                    self.bump_char();
                    if self.peek_char() == Some('{') {
                        return Err(Diagnostic::new(
                            Span::new(start, self.position()),
                            "`${{ ... }}` is legacy Snow template syntax",
                            "use `\"${<expr>}\"` interpolation in a double-quoted string",
                        ));
                    }
                    let body_start = self.position();
                    let mut depth = 1_u32;
                    let mut body = String::new();
                    loop {
                        let Some(character) = self.peek_char() else {
                            return Err(Self::unterminated(start, self.position()));
                        };
                        if character == '\n' {
                            return Err(Self::unterminated(start, self.position()));
                        }
                        if character == '{' {
                            depth += 1;
                            self.bump_char();
                            body.push('{');
                            continue;
                        }
                        if character == '}' {
                            depth -= 1;
                            self.bump_char();
                            if depth == 0 {
                                break;
                            }
                            body.push('}');
                            continue;
                        }
                        self.bump_char();
                        body.push(character);
                    }
                    pieces.push(crate::token::TemplatePiece::Expr(
                        body.trim().to_string(),
                        Span::new(body_start, self.position()),
                    ));
                }
                Some(_) => {
                    if let Some(character) = self.bump_char() {
                        text.push(character);
                    }
                }
            }
        }
    }

    /// Lexes a `'...'` literal string; no escapes and no interpolation.
    pub(super) fn lex_single_string(&mut self, start: Position) -> Result<Token, Diagnostic> {
        self.bump_char();
        let mut value = String::new();
        loop {
            match self.peek_char() {
                None | Some('\n') => return Err(Self::unterminated(start, self.position())),
                Some('\'') => {
                    self.bump_char();
                    return Ok(Token {
                        kind: TokenKind::Literal(value),
                        span: Span::new(start, self.position()),
                    });
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
