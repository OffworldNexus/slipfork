//! Expression parsing: boolean `on` conditions, values, and fallbacks.

use crate::diag::Diagnostic;
use crate::expr::{Expr, ExprKind, RandomKind};
use crate::parser::Parser;
use crate::span::Span;
use crate::token::TokenKind;

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
            TokenKind::Str(value) => Ok(Expr {
                kind: ExprKind::String(value),
                span,
            }),
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

    fn parse_ident_atom(&mut self, name: String, span: Span) -> Result<Expr, Diagnostic> {
        self.fill()?;
        let next = self.peeked.take().expect("token filled");
        match next.kind {
            TokenKind::LParen => match name.as_str() {
                "var" | "secret" => {
                    let key = self.parse_call_name()?;
                    self.expect_punct(TokenKind::RParen, "`)`", "close the call with `)`")?;
                    let kind = if name == "var" {
                        ExprKind::Var(key)
                    } else {
                        ExprKind::Secret(key)
                    };
                    Ok(Expr { kind, span })
                }
                "random_hex" | "random_base64" | "random_alnum" => {
                    let n = self.parse_optional_int()?;
                    self.expect_punct(TokenKind::RParen, "`)`", "close the call with `)`")?;
                    let kind = match name.as_str() {
                        "random_hex" => RandomKind::Hex,
                        "random_base64" => RandomKind::Base64,
                        _ => RandomKind::Alnum,
                    };
                    Ok(Expr {
                        kind: ExprKind::Random { kind, n },
                        span,
                    })
                }
                "random_uuid" => {
                    self.expect_punct(TokenKind::RParen, "`)`", "write `random_uuid()`")?;
                    Ok(Expr {
                        kind: ExprKind::Random {
                            kind: RandomKind::Uuid,
                            n: None,
                        },
                        span,
                    })
                }
                other => Err(Diagnostic::new(
                    span,
                    format!("unknown function `{other}`"),
                    "use `var`, `secret`, `random_hex`, `random_base64`, `random_alnum`, or `random_uuid`",
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

    fn parse_call_name(&mut self) -> Result<String, Diagnostic> {
        let token = self.bump()?;
        match token.kind {
            TokenKind::Ident(name) | TokenKind::Str(name) => Ok(name),
            other => Err(Diagnostic::new(
                token.span,
                format!("expected a name, found {}", other.describe()),
                "write an identifier such as `SECRET_KEY`",
            )),
        }
    }

    fn parse_optional_int(&mut self) -> Result<Option<u64>, Diagnostic> {
        self.fill()?;
        if matches!(
            &self.peeked.as_ref().expect("token filled").kind,
            TokenKind::RParen
        ) {
            return Ok(None);
        }
        let token = self.bump()?;
        match token.kind {
            TokenKind::Int(value) => Ok(Some(value)),
            other => Err(Diagnostic::new(
                token.span,
                format!("expected a length, found {}", other.describe()),
                "write a digit count such as `random_hex(32)`",
            )),
        }
    }
}
