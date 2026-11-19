//! Expression grammar: `<entity>.<prop>` refs, `var`/`secret`, `??` fallback, generators, `+`.

use crate::diag::Span;
use crate::lexer::Tok;
use crate::parser::Parser;
use crate::syntax::{Expr, GeneratorKind, Spanned};

impl Parser<'_> {
    /// Parse one value expression.
    pub(crate) fn parse_expr(&mut self) -> Expr {
        self.parse_concat()
    }

    fn parse_concat(&mut self) -> Expr {
        let mut parts = vec![self.parse_fallback()];
        while self.eat(&Tok::Plus) {
            parts.push(self.parse_fallback());
        }
        if parts.len() == 1 {
            return parts.remove(0);
        }
        let first = parts[0].span();
        let last = parts[parts.len() - 1].span();
        Expr::Concat {
            parts,
            span: first.merge(last),
        }
    }

    fn parse_fallback(&mut self) -> Expr {
        let lhs = self.parse_primary();
        if !self.eat(&Tok::Nullish) {
            return lhs;
        }
        let rhs = self.parse_fallback();
        let span = lhs.span().merge(rhs.span());
        Expr::Fallback {
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            span,
        }
    }

    fn parse_primary(&mut self) -> Expr {
        match self.peek().clone() {
            Tok::Str(value) | Tok::RawStr(value) => {
                let token = self.bump();
                Expr::Str(Spanned::new(value, self.token_span(&token)))
            }
            Tok::Int(value) => {
                let token = self.bump();
                Expr::Int(Spanned::new(value, self.token_span(&token)))
            }
            Tok::Ident(name) => self.parse_ident_expr(name),
            Tok::LParen => {
                self.bump();
                let inner = self.parse_expr();
                self.expect(&Tok::RParen, "`)`");
                inner
            }
            _ => {
                let span = self.span();
                self.push(span, "expected an expression");
                self.skip_line();
                Expr::Str(Spanned::new(String::new(), span))
            }
        }
    }

    fn parse_ident_expr(&mut self, name: String) -> Expr {
        let token = self.bump();
        let head = self.token_span(&token);
        if self.at(&Tok::LParen) {
            return self.parse_call(name, head);
        }
        if name == "true" || name == "false" {
            return Expr::Bool(Spanned::new(name == "true", head));
        }
        let mut segments = vec![name];
        let mut span = head;
        while self.at(&Tok::Dot) && matches!(self.peek_at(1), Tok::Ident(_)) {
            self.bump();
            if let Tok::Ident(segment) = self.peek().clone() {
                let token = self.bump();
                span = span.merge(self.token_span(&token));
                segments.push(segment);
            }
        }
        Expr::Ref(Spanned::new(segments, span))
    }

    fn parse_call(&mut self, name: String, head: Span) -> Expr {
        self.expect(&Tok::LParen, "`(`");
        match name.as_str() {
            "var" | "secret" => self.parse_port_call(name == "var", head),
            "random_hex" | "random_base64" | "random_string" => {
                self.parse_generator_call(name.as_str(), head)
            }
            "uuid4" => {
                let end = self
                    .expect(&Tok::RParen, "`)`")
                    .map_or(head, |token| self.token_span(&token));
                Expr::Generator {
                    kind: GeneratorKind::Uuid4,
                    arg: Spanned::new(0, head),
                    span: head.merge(end),
                }
            }
            other => {
                self.push(head, format!("unknown function `{other}`"));
                let mut depth = 1usize;
                while depth > 0 {
                    match self.bump().tok {
                        Tok::LParen => depth += 1,
                        Tok::RParen => depth -= 1,
                        Tok::Eof => break,
                        _ => {}
                    }
                }
                Expr::Str(Spanned::new(String::new(), head))
            }
        }
    }

    fn parse_port_call(&mut self, is_var: bool, head: Span) -> Expr {
        let key = match self.peek().clone() {
            Tok::Ident(value) | Tok::Str(value) => {
                let token = self.bump();
                Some(Spanned::new(value, self.token_span(&token)))
            }
            _ => {
                let span = self.span();
                self.push(span, "expected a variable name");
                None
            }
        };
        let end = self
            .expect(&Tok::RParen, "`)`")
            .map_or(head, |token| self.token_span(&token));
        let key = key.unwrap_or_else(|| Spanned::new(String::new(), head));
        let span = head.merge(end);
        if is_var {
            Expr::Var(Spanned::new(key.value, span))
        } else {
            Expr::Secret(Spanned::new(key.value, span))
        }
    }

    fn parse_generator_call(&mut self, name: &str, head: Span) -> Expr {
        let arg = match self.peek().clone() {
            Tok::Int(value) => {
                let token = self.bump();
                Some(Spanned::new(
                    value.clamp(0, i64::from(u32::MAX)) as u32,
                    self.token_span(&token),
                ))
            }
            _ => {
                let span = self.span();
                self.push(span, "expected a byte or character count");
                None
            }
        };
        let end = self
            .expect(&Tok::RParen, "`)`")
            .map_or(head, |token| self.token_span(&token));
        let kind = match name {
            "random_base64" => GeneratorKind::RandomBase64,
            "random_string" => GeneratorKind::RandomString,
            _ => GeneratorKind::RandomHex,
        };
        Expr::Generator {
            kind,
            arg: arg.unwrap_or_else(|| Spanned::new(32, head)),
            span: head.merge(end),
        }
    }
}
