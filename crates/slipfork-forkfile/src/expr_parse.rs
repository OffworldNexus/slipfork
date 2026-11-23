//! Expression parsing: boolean `on` conditions, values, fallbacks, and generators.

use crate::diag::Diagnostic;
use crate::expr::{Expr, ExprKind, GeneratorSpec, InterpPart, RandomKind};
use crate::parser::Parser;
use crate::span::Span;
use crate::token::{TemplatePiece, TokenKind};

impl<'a> Parser<'a> {
    pub(crate) fn parse_expr(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_fallback()
    }

    fn parse_fallback(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.parse_or()?;
        loop {
            self.fill()?;
            if !matches!(
                &self.peeked.as_ref().expect("token filled").kind,
                TokenKind::QuestionQuestion
            ) {
                break;
            }
            self.peeked = None;
            let right = self.parse_or()?;
            let span = left.span.merge(right.span);
            left = Expr {
                kind: ExprKind::Fallback(Box::new(left), Box::new(right)),
                span,
            };
        }
        Ok(left)
    }

    fn parse_or(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.parse_and()?;
        loop {
            self.fill()?;
            if !matches!(
                &self.peeked.as_ref().expect("token filled").kind,
                TokenKind::Pipe
            ) {
                break;
            }
            self.peeked = None;
            let right = self.parse_and()?;
            let span = left.span.merge(right.span);
            left = Expr {
                kind: ExprKind::Or(Box::new(left), Box::new(right)),
                span,
            };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.parse_not()?;
        loop {
            self.fill()?;
            if !matches!(
                &self.peeked.as_ref().expect("token filled").kind,
                TokenKind::Amp
            ) {
                break;
            }
            self.peeked = None;
            let right = self.parse_not()?;
            let span = left.span.merge(right.span);
            left = Expr {
                kind: ExprKind::And(Box::new(left), Box::new(right)),
                span,
            };
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr, Diagnostic> {
        self.fill()?;
        if matches!(
            &self.peeked.as_ref().expect("token filled").kind,
            TokenKind::Bang
        ) {
            let bang = self.peeked.take().expect("token filled").span;
            let inner = self.parse_not()?;
            return Ok(Expr {
                span: bang.merge(inner.span),
                kind: ExprKind::Not(Box::new(inner)),
            });
        }
        self.parse_concat()
    }

    fn parse_concat(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.parse_atom()?;
        loop {
            self.fill()?;
            if !matches!(
                &self.peeked.as_ref().expect("token filled").kind,
                TokenKind::Plus
            ) {
                break;
            }
            self.peeked = None;
            let right = self.parse_atom()?;
            let span = left.span.merge(right.span);
            left = Expr {
                kind: ExprKind::Concat(Box::new(left), Box::new(right)),
                span,
            };
        }
        Ok(left)
    }

    fn parse_atom(&mut self) -> Result<Expr, Diagnostic> {
        let token = self.bump()?;
        let span = token.span;
        match token.kind {
            TokenKind::Int(value) => Ok(Expr {
                kind: ExprKind::Int(value),
                span,
            }),
            TokenKind::Str(value) | TokenKind::Literal(value) => Ok(Expr {
                kind: ExprKind::String(value),
                span,
            }),
            TokenKind::Template(pieces) => self.parse_template(pieces, span),
            TokenKind::Port => Ok(Expr {
                kind: ExprKind::Port,
                span,
            }),
            TokenKind::Env(key) => Ok(Expr {
                kind: ExprKind::EnvVar(key),
                span,
            }),
            TokenKind::Ident(name) => self.parse_ident_atom(name, span),
            TokenKind::LParen => {
                let inner = self.parse_expr()?;
                self.expect_punct(TokenKind::RParen, "`)`", "close the expression with `)`")?;
                Ok(inner)
            }
            other => Err(Diagnostic::new(
                span,
                format!("expected a value, found {}", other.describe()),
                "write a value such as `dev`, `true`, `\"text\"`, `var(KEY)`, or `secret(KEY)`",
            )),
        }
    }

    /// Parses each `${<expr>}` site of a double-quoted string through the canonical expression
    /// parser, keeping verbatim text literal.
    fn parse_template(
        &mut self,
        pieces: Vec<TemplatePiece>,
        span: Span,
    ) -> Result<Expr, Diagnostic> {
        let mut parts = Vec::new();
        for piece in pieces {
            match piece {
                TemplatePiece::Text(text) => parts.push(InterpPart::Text(text)),
                TemplatePiece::Expr(source, expr_span) => {
                    parts.push(InterpPart::Expr(Box::new(parse_embedded(
                        &source, expr_span,
                    )?)));
                }
            }
        }
        Ok(Expr {
            kind: ExprKind::Interpolated(parts),
            span,
        })
    }

    fn parse_ident_atom(&mut self, name: String, span: Span) -> Result<Expr, Diagnostic> {
        self.fill()?;
        let next = self.peeked.take().expect("token filled");
        match next.kind {
            TokenKind::LParen => match name.as_str() {
                "var" | "secret" => {
                    let key = self.parse_call_name()?;
                    let generator = if self.eat(&TokenKind::Comma)? {
                        Some(self.parse_generator(span)?)
                    } else {
                        None
                    };
                    self.expect_punct(TokenKind::RParen, "`)`", "close the call with `)`")?;
                    let kind = match (name.as_str(), generator) {
                        ("var", Some(generator)) => ExprKind::GeneratedVar { key, generator },
                        ("var", None) => ExprKind::Var(key),
                        (_, Some(generator)) => ExprKind::GeneratedSecret { key, generator },
                        (_, None) => ExprKind::Secret(key),
                    };
                    Ok(Expr { kind, span })
                }
                "random_string" | "random_hex" | "random_base64" | "random_uuid" => {
                    let generator = self.finish_generator(name, span, true)?;
                    Ok(Expr {
                        kind: ExprKind::Random(generator),
                        span,
                    })
                }
                other => Err(Diagnostic::new(
                    span,
                    format!("unknown function `{other}`"),
                    "use `var(KEY)`, `secret(KEY)`, or pass a generator as `secret(KEY, random_string(50))`",
                )),
            },
            TokenKind::Dot => {
                let prop = self.parse_call_name()?;
                Ok(Expr {
                    kind: ExprKind::Property { base: name, prop },
                    span,
                })
            }
            _ => {
                self.peeked = Some(next);
                match name.as_str() {
                    "true" => Ok(Expr {
                        kind: ExprKind::Bool(true),
                        span,
                    }),
                    "false" => Ok(Expr {
                        kind: ExprKind::Bool(false),
                        span,
                    }),
                    _ => Ok(Expr {
                        kind: ExprKind::Feature(name),
                        span,
                    }),
                }
            }
        }
    }

    /// Parses one `random_*` generator call. Only the picker in `parse_ident_atom` reaches it;
    /// standalone `random_*` values are kept as [`ExprKind::Random`] so the checker can explain
    /// that a generator has no meaning outside `secret`/`var`.
    fn parse_generator(&mut self, _span: Span) -> Result<GeneratorSpec, Diagnostic> {
        let token = self.bump()?;
        let name = match token.kind {
            TokenKind::Ident(name) => name,
            other => {
                return Err(Diagnostic::new(
                    token.span,
                    format!("expected a generator, found {}", other.describe()),
                    "use `random_string(50)`, `random_hex(32)`, `random_base64(32)`, or `random_uuid()`",
                ));
            }
        };
        self.finish_generator(name, token.span, false)
    }

    /// Finishes a generator whose name (and opening `(`) were already consumed.
    fn finish_generator(
        &mut self,
        name: String,
        span: Span,
        open_consumed: bool,
    ) -> Result<GeneratorSpec, Diagnostic> {
        let kind = match name.as_str() {
            "random_string" => RandomKind::String,
            "random_hex" => RandomKind::Hex,
            "random_base64" => RandomKind::Base64,
            "random_uuid" => {
                if !open_consumed {
                    self.expect_punct(TokenKind::LParen, "`(`", "write `random_uuid()`")?;
                }
                self.expect_punct(TokenKind::RParen, "`)`", "write `random_uuid()`")?;
                return Ok(GeneratorSpec {
                    kind: RandomKind::Uuid,
                    n: None,
                });
            }
            other => {
                return Err(Diagnostic::new(
                    span,
                    format!("unknown generator `{other}`"),
                    "use `random_string(n)`, `random_hex(n)`, `random_base64(n)`, or `random_uuid()`",
                ));
            }
        };
        if !open_consumed {
            self.expect_punct(
                TokenKind::LParen,
                "`(`",
                format!("write `{}(<n>)`", kind.call_name()).as_str(),
            )?;
        }
        let length = self.bump()?;
        let n = match length.kind {
            TokenKind::Int(value) => value,
            other => {
                return Err(Diagnostic::new(
                    length.span,
                    format!("expected a length, found {}", other.describe()),
                    format!(
                        "write a length in 1..=256 such as `{}(50)`",
                        kind.call_name()
                    ),
                ));
            }
        };
        self.expect_punct(TokenKind::RParen, "`)`", "close the generator with `)`")?;
        GeneratorSpec::sized(kind, Some(n), span)
    }

    fn parse_call_name(&mut self) -> Result<String, Diagnostic> {
        let token = self.bump()?;
        match token.kind {
            TokenKind::Ident(name) | TokenKind::Str(name) | TokenKind::Literal(name) => Ok(name),
            other => Err(Diagnostic::new(
                token.span,
                format!("expected a name, found {}", other.describe()),
                "write an identifier such as `SECRET_KEY`",
            )),
        }
    }
}

/// Parses one `${...}` body through the same expression grammar, rooted at the site span.
fn parse_embedded(source: &str, span: Span) -> Result<Expr, Diagnostic> {
    if source.is_empty() {
        return Err(Diagnostic::new(
            span,
            "empty `${...}` interpolation",
            "write an expression such as `${self.url}` or `${dom_b.name}`",
        ));
    }
    let mut parser = Parser::new(source);
    let mut expr = parser.parse_expr()?;
    parser.fill()?;
    if !matches!(
        &parser.peeked.as_ref().expect("token filled").kind,
        TokenKind::Eof
    ) {
        return Err(Diagnostic::new(
            span,
            "unexpected token inside `${...}` interpolation",
            "keep one expression inside `${...}`",
        ));
    }
    expr.span = span;
    Ok(expr)
}
