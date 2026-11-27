//! Recursive-descent parser core: documents, blocks, helpers, and diagnostics.

use crate::ast::*;
use crate::diag::{Diagnostic, Diagnostics};
use crate::lexer::Lexer;
use crate::span::{Position, Span};
use crate::token::{Token, TokenKind};

/// Parses a whole `Forkfile`, collecting every recoverable diagnostic.
pub(crate) fn parse(source: &str) -> Result<Forkfile, Diagnostics> {
    let mut parser = Parser::new(source);
    match parser.parse_document() {
        Ok(document) if parser.diags.is_empty() => Ok(document),
        Ok(_) => Err(parser.diags),
        Err(diagnostic) => {
            parser.diags.push(diagnostic);
            Err(parser.diags)
        }
    }
}

pub(crate) mod trivia;

/// The parser state: a streaming lexer, one lookahead token, and diagnostics.
pub(crate) struct Parser<'a> {
    pub(crate) lexer: Lexer<'a>,
    pub(crate) peeked: Option<Token>,
    pub(crate) pushback: Vec<Token>,
    pub(crate) diags: Diagnostics,
    after_newline: bool,
    blank_pending: bool,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(source: &'a str) -> Self {
        Self {
            lexer: Lexer::new(source),
            peeked: None,
            pushback: Vec::new(),
            diags: Diagnostics::new(),
            after_newline: false,
            blank_pending: false,
        }
    }

    pub(crate) fn fill(&mut self) -> Result<(), Diagnostic> {
        if self.peeked.is_none() {
            self.peeked = Some(match self.pushback.pop() {
                Some(token) => token,
                None => self.lexer.next_token()?,
            });
        }
        Ok(())
    }

    /// Tests for `else` across the line breaks that end an inline `on` item,
    /// restoring every skipped token when the keyword is absent.
    fn peek_else_across_newlines(&mut self) -> Result<bool, Diagnostic> {
        self.fill()?;
        let mut skipped = Vec::new();
        loop {
            let token = self.peeked.take().expect("token filled");
            if matches!(token.kind, TokenKind::Newline) {
                skipped.push(token);
                self.fill()?;
                continue;
            }
            if matches!(&token.kind, TokenKind::Ident(name) if name == "else") {
                self.peeked = Some(token);
                return Ok(true);
            }
            self.pushback.push(token);
            for newline in skipped.into_iter().rev() {
                self.pushback.push(newline);
            }
            return Ok(false);
        }
    }

    pub(crate) fn current_span(&mut self) -> Result<Span, Diagnostic> {
        self.fill()?;
        Ok(self.peeked.as_ref().expect("token filled").span)
    }

    pub(crate) fn bump(&mut self) -> Result<Token, Diagnostic> {
        self.fill()?;
        let token = self.peeked.take().expect("token filled");
        self.after_newline = matches!(&token.kind, TokenKind::Newline);
        Ok(token)
    }

    /// Raw reads are only legal with no buffered token, so flush first.
    pub(crate) fn assert_raw(&self) {
        debug_assert!(
            self.peeked.is_none(),
            "raw reads require a flushed lookahead"
        );
    }

    pub(crate) fn raw_word(&mut self) -> Result<(String, Span), Diagnostic> {
        self.assert_raw();
        self.lexer.raw_word()
    }

    pub(crate) fn raw_to_line_end(&mut self) -> Result<(String, Span), Diagnostic> {
        self.assert_raw();
        self.lexer.raw_to_line_end()
    }

    pub(crate) fn next_is(&mut self, expected: char) -> bool {
        self.assert_raw();
        self.lexer.next_is(expected)
    }

    pub(crate) fn eat(&mut self, kind: &TokenKind) -> Result<bool, Diagnostic> {
        self.fill()?;
        let token = self.peeked.take().expect("token filled");
        if std::mem::discriminant(&token.kind) == std::mem::discriminant(kind) {
            Ok(true)
        } else {
            self.peeked = Some(token);
            Ok(false)
        }
    }

    pub(crate) fn expect_punct(
        &mut self,
        kind: TokenKind,
        what: &str,
        remedy: &str,
    ) -> Result<Token, Diagnostic> {
        let token = self.bump()?;
        if std::mem::discriminant(&token.kind) == std::mem::discriminant(&kind) {
            Ok(token)
        } else {
            Err(Diagnostic::new(
                token.span,
                format!("expected {what}, found {}", token.kind.describe()),
                remedy,
            ))
        }
    }

    pub(crate) fn expect_ident(&mut self, what: &str, remedy: &str) -> Result<String, Diagnostic> {
        self.expect_ident_spanned(what, remedy)
            .map(|(name, _)| name)
    }

    pub(crate) fn expect_ident_spanned(
        &mut self,
        what: &str,
        remedy: &str,
    ) -> Result<(String, Span), Diagnostic> {
        let token = self.bump()?;
        match token.kind {
            TokenKind::Ident(name) => Ok((name, token.span)),
            other => Err(Diagnostic::new(
                token.span,
                format!("expected {what}, found {}", other.describe()),
                remedy,
            )),
        }
    }

    pub(crate) fn peek_ident_is(&mut self, expected: &str) -> Result<bool, Diagnostic> {
        self.fill()?;
        let token = self.peeked.as_ref().expect("token filled");
        Ok(matches!(&token.kind, TokenKind::Ident(name) if name == expected))
    }

    pub(crate) fn expect_port(&mut self) -> Result<u16, Diagnostic> {
        let token = self.bump()?;
        match token.kind {
            TokenKind::Int(value) => u16::try_from(value).map_err(|_| {
                Diagnostic::new(
                    token.span,
                    format!("port {value} is out of range"),
                    "use a port between 0 and 65535",
                )
            }),
            other => Err(Diagnostic::new(
                token.span,
                format!("expected a port number, found {}", other.describe()),
                "write `port 8000`",
            )),
        }
    }

    /// Discards a failed declaration, never crossing the line break that ended it.
    pub(crate) fn recover(&mut self) {
        if self.after_newline && self.peeked.is_none() {
            return;
        }
        loop {
            if let Some(token) = self.peeked.take() {
                if matches!(&token.kind, TokenKind::Newline) {
                    self.after_newline = true;
                    return;
                }
                if matches!(&token.kind, TokenKind::RBrace | TokenKind::Eof) {
                    self.peeked = Some(token);
                    return;
                }
                continue;
            }
            match self.lexer.next_token() {
                Ok(token) => {
                    if matches!(&token.kind, TokenKind::Newline) {
                        self.after_newline = true;
                        return;
                    }
                    if matches!(&token.kind, TokenKind::RBrace | TokenKind::Eof) {
                        self.peeked = Some(token);
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    }

    fn parse_document(&mut self) -> Result<Forkfile, Diagnostic> {
        let header = self.lexer.next_token()?;
        let version = Self::header_version(&header)?;
        let mut items = Block::default();
        let mut pending = Vec::new();
        loop {
            if let Err(diagnostic) = self.skip_trivia(&mut pending) {
                self.diags.push(diagnostic);
                continue;
            }
            self.fill()?;
            if matches!(
                &self.peeked.as_ref().expect("token filled").kind,
                TokenKind::Eof
            ) {
                Self::flush_comments(&mut items, &mut pending);
                break;
            }
            let item_blank = std::mem::take(&mut self.blank_pending);
            match self.parse_top() {
                Ok(item) => {
                    Self::flush_comments(&mut items, &mut pending);
                    items.blank_lines.push(item_blank);
                    items.items.push(item);
                    if let Err(diagnostic) = self.finish_item(&mut items) {
                        self.diags.push(diagnostic);
                        self.recover();
                    }
                }
                Err(diagnostic) => {
                    self.diags.push(diagnostic);
                    self.recover();
                }
            }
        }
        Ok(Forkfile {
            version,
            span: header.span,
            items,
        })
    }

    fn header_version(header: &Token) -> Result<String, Diagnostic> {
        const REMEDY: &str = "add `# forkfile: 0.1` as line 1";
        let TokenKind::Comment(text) = &header.kind else {
            return Err(Diagnostic::new(
                Span::at(Position::new(1, 1, 0)),
                "missing `# forkfile: 0.1` header",
                REMEDY,
            ));
        };
        if header.span.line() != 1 {
            return Err(Diagnostic::new(
                Span::at(Position::new(1, 1, 0)),
                "missing `# forkfile: 0.1` header",
                REMEDY,
            ));
        }
        let body = text.trim_start_matches('#').trim();
        match body.strip_prefix("forkfile:").map(str::trim) {
            Some("0.1") => Ok("0.1".to_string()),
            Some(other) if !other.is_empty() => Err(Diagnostic::new(
                header.span,
                format!("unsupported `Forkfile` format `{other}`"),
                "line 1 must read `# forkfile: 0.1`",
            )),
            _ => Err(Diagnostic::new(
                header.span,
                "missing `# forkfile: 0.1` header",
                REMEDY,
            )),
        }
    }

    fn parse_top(&mut self) -> Result<TopItem, Diagnostic> {
        let token = self.bump()?;
        let span = token.span;
        let name = match token.kind {
            TokenKind::Ident(name) => name,
            other => {
                return Err(Diagnostic::new(
                    token.span,
                    format!("expected a declaration, found {}", other.describe()),
                    "start a top-level block with `features`, `image`, `on`, `domain`, `postgres`, `redis`, `s3`, or `env`",
                ));
            }
        };
        match name.as_str() {
            "features" => self.parse_features(span).map(TopItem::Features),
            "image" => self.parse_image(span).map(TopItem::Image),
            "on" => self
                .parse_on_after_keyword(span, &mut |parser: &mut Self| parser.parse_top())
                .map(TopItem::On),
            "domain" => self
                .expect_ident_spanned("a domain name", "write `domain <id>`")
                .map(|(id, id_span)| TopItem::Domain(Domain { id, id_span, span })),
            "postgres" => self.parse_postgres(span).map(TopItem::Postgres),
            "redis" => self.parse_redis(span).map(TopItem::Redis),
            "s3" => self.parse_s3(span).map(TopItem::S3),
            "env" => self.parse_env(span).map(TopItem::Env),
            other => Err(Diagnostic::new(
                token.span,
                format!("unknown declaration `{other}`"),
                "use `features`, `image`, `on`, `domain`, `postgres`, `redis`, `s3`, or `env`",
            )),
        }
    }

    pub(crate) fn parse_on_after_keyword<I>(
        &mut self,
        span: Span,
        item: &mut dyn FnMut(&mut Self) -> Result<I, Diagnostic>,
    ) -> Result<On<I>, Diagnostic> {
        let cond = self.parse_expr()?;
        let body = self.parse_on_body(item)?;
        let else_body = self.parse_on_else(item)?;
        Ok(On {
            cond,
            body,
            else_body,
            span,
        })
    }

    fn parse_on_body<I>(
        &mut self,
        item: &mut dyn FnMut(&mut Self) -> Result<I, Diagnostic>,
    ) -> Result<OnBody<I>, Diagnostic> {
        let token = self.bump()?;
        match token.kind {
            TokenKind::Colon => Ok(OnBody::Inline(Box::new(item(self)?))),
            TokenKind::LBrace => Ok(OnBody::Block(self.parse_block_body(item)?)),
            other => Err(Diagnostic::new(
                token.span,
                format!(
                    "expected `:` or `{{` after the `on` condition, found {}",
                    other.describe()
                ),
                "write `on <expr>: <declaration>` or `on <expr> { ... }`",
            )),
        }
    }

    /// Parses `else { ... }`, `else: <item>`, or `else on <expr> ...` after an `on` body.
    ///
    /// `else` may start on the next line after an inline `on <expr>: <item>`, so the
    /// separating line breaks are skipped before the keyword is tested.
    fn parse_on_else<I>(
        &mut self,
        item: &mut dyn FnMut(&mut Self) -> Result<I, Diagnostic>,
    ) -> Result<Option<OnElse<I>>, Diagnostic> {
        if !self.peek_else_across_newlines()? {
            return Ok(None);
        }
        let span = self.bump()?.span;
        if self.peek_ident_is("on")? {
            self.bump()?;
            let chained = self.parse_on_after_keyword(span, item)?;
            return Ok(Some(OnElse::Chain(Box::new(chained))));
        }
        let token = self.bump()?;
        match token.kind {
            TokenKind::Colon => Ok(Some(OnElse::Body(OnBody::Inline(Box::new(item(self)?))))),
            TokenKind::LBrace => Ok(Some(OnElse::Body(OnBody::Block(
                self.parse_block_body(item)?,
            )))),
            other => Err(Diagnostic::new(
                token.span,
                format!(
                    "expected `:`, `{{`, or `on` after `else`, found {}",
                    other.describe()
                ),
                "write `else { ... }`, `else: <declaration>`, or `else on <expr> ...`",
            )),
        }
    }

    pub(crate) fn parse_block<T>(
        &mut self,
        item: impl FnMut(&mut Self) -> Result<T, Diagnostic>,
    ) -> Result<Block<T>, Diagnostic> {
        self.expect_punct(TokenKind::LBrace, "`{`", "open the block with `{`")?;
        self.parse_block_body(item)
    }

    pub(crate) fn parse_optional_block<T>(
        &mut self,
        item: impl FnMut(&mut Self) -> Result<T, Diagnostic>,
    ) -> Result<Block<T>, Diagnostic> {
        self.fill()?;
        if matches!(
            &self.peeked.as_ref().expect("token filled").kind,
            TokenKind::LBrace
        ) {
            self.peeked = None;
            self.parse_block_body(item)
        } else {
            Ok(Block::default())
        }
    }

    pub(crate) fn parse_block_body<T>(
        &mut self,
        mut item: impl FnMut(&mut Self) -> Result<T, Diagnostic>,
    ) -> Result<Block<T>, Diagnostic> {
        let mut block = Block::default();
        let mut pending = Vec::new();
        loop {
            if let Err(diagnostic) = self.skip_trivia(&mut pending) {
                self.diags.push(diagnostic);
                continue;
            }
            let token = self.peeked.take().expect("token filled");
            if matches!(&token.kind, TokenKind::RBrace) {
                Self::flush_comments(&mut block, &mut pending);
                return Ok(block);
            }
            if matches!(&token.kind, TokenKind::Eof) {
                return Err(Diagnostic::new(
                    token.span,
                    "unclosed block",
                    "add a closing `}`",
                ));
            }
            let item_blank = std::mem::take(&mut self.blank_pending);
            self.peeked = Some(token);
            match item(self) {
                Ok(node) => {
                    Self::flush_comments(&mut block, &mut pending);
                    block.blank_lines.push(item_blank);
                    block.items.push(node);
                    if let Err(diagnostic) = self.finish_item(&mut block) {
                        self.diags.push(diagnostic);
                        self.recover();
                    }
                }
                Err(diagnostic) => {
                    self.diags.push(diagnostic);
                    self.recover();
                }
            }
        }
    }
}
