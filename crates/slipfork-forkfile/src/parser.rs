//! Recursive-descent parser for top-level `Forkfile` statements and resources.
//!
//! Parsing never stops at the first error: every failure records a [`Diagnostic`] and resynchronises
//! at the next line or closing brace, so the checker can report the whole manifest at once.

use crate::diag::{Diagnostic, LineIndex, Span};
use crate::lexer::{Tok, Token, lex};
use crate::syntax::{Condition, ForkFile, OnBlock, Spanned, Stmt};

/// Where [`Parser::capture_raw`] stops consuming tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureStop {
    /// Stop at a line break, statement separator, or block brace (raw command text).
    Line,
    /// Stop at the first opening brace (a `from <image>` reference).
    Brace,
}

/// A single-pass parser over the `Forkfile` token stream.
pub(crate) struct Parser<'a> {
    pub(crate) source: &'a str,
    pub(crate) index: LineIndex,
    pub(crate) tokens: Vec<Token>,
    pub(crate) pos: usize,
    pub(crate) diags: Vec<Diagnostic>,
}

/// Parse `source` into a checked [`ForkFile`].
///
/// Returns every diagnostic — syntax **and** static — in one pass; the manifest is returned only
/// when nothing failed.
pub fn parse_forkfile(source: &str) -> Result<ForkFile, Vec<Diagnostic>> {
    let mut parser = Parser::new(source);
    let statements = parser.parse_statements(false);
    let file = ForkFile { statements };
    let mut diags = parser.diags;
    if !file.statements.is_empty() {
        diags.extend(crate::check::check(&file));
    }
    diags.sort_by_key(|diagnostic| {
        (
            diagnostic.span.offset,
            diagnostic.span.line,
            diagnostic.span.col,
        )
    });
    diags.dedup_by(|left, right| left.span == right.span && left.message == right.message);
    if diags.is_empty() {
        Ok(file)
    } else {
        Err(diags)
    }
}

impl<'a> Parser<'a> {
    pub(crate) fn new(source: &'a str) -> Self {
        let (tokens, index, diags) = lex(source);
        Self {
            source,
            index,
            tokens,
            pos: 0,
            diags,
        }
    }

    pub(crate) fn peek(&self) -> &Tok {
        self.peek_at(0)
    }

    pub(crate) fn peek_at(&self, offset: usize) -> &Tok {
        self.tokens
            .get(self.pos + offset)
            .map_or(&Tok::Eof, |token| &token.tok)
    }

    pub(crate) fn bump(&mut self) -> Token {
        let token = self.tokens.get(self.pos).cloned().unwrap_or(Token {
            tok: Tok::Eof,
            offset: self.source.len(),
            len: 0,
        });
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        token
    }

    pub(crate) fn at(&self, kind: &Tok) -> bool {
        self.peek() == kind
    }

    pub(crate) fn at_ident(&self, name: &str) -> bool {
        matches!(self.peek(), Tok::Ident(value) if value == name)
    }

    pub(crate) fn eat(&mut self, kind: &Tok) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    pub(crate) fn token_span(&self, token: &Token) -> Span {
        self.index.span(self.source, token.offset, token.len)
    }

    pub(crate) fn span(&self) -> Span {
        self.tokens
            .get(self.pos)
            .map_or(Span::zero(), |token| self.token_span(token))
    }

    pub(crate) fn push(&mut self, span: Span, message: impl Into<String>) {
        self.diags.push(Diagnostic::new(span, message));
    }

    pub(crate) fn expect(&mut self, kind: &Tok, what: &str) -> Option<Token> {
        if self.at(kind) {
            Some(self.bump())
        } else {
            let span = self.span();
            self.push(span, format!("expected {what}"));
            None
        }
    }

    pub(crate) fn expect_ident(&mut self, what: &str) -> Option<Spanned<String>> {
        match self.peek().clone() {
            Tok::Ident(name) => {
                let token = self.bump();
                Some(Spanned::new(name, self.token_span(&token)))
            }
            _ => {
                let span = self.span();
                self.push(span, format!("expected {what}"));
                None
            }
        }
    }

    pub(crate) fn skip_separators(&mut self) {
        while matches!(self.peek(), Tok::Newline | Tok::Semi) {
            self.bump();
        }
    }

    pub(crate) fn skip_line(&mut self) {
        while !matches!(
            self.peek(),
            Tok::Newline | Tok::Semi | Tok::Eof | Tok::RBrace
        ) {
            self.bump();
        }
        self.skip_separators();
    }

    pub(crate) fn finish_line(&mut self) {
        if matches!(
            self.peek(),
            Tok::Newline | Tok::Semi | Tok::Eof | Tok::RBrace
        ) {
            return;
        }
        let span = self.span();
        self.push(span, "unexpected token after the statement");
        self.skip_line();
    }

    /// Slice the raw source text up to the next structural boundary.
    ///
    /// The returned range ends at the last consumed token, so trailing comments and whitespace are
    /// never folded into commands or paths.
    pub(crate) fn capture_raw(&mut self, stop: CaptureStop) -> (String, Span) {
        let Some(first) = self.tokens.get(self.pos) else {
            return (String::new(), Span::zero());
        };
        let start = first.offset;
        let mut end = start;
        while let Some(token) = self.tokens.get(self.pos) {
            let boundary = matches!(
                (&token.tok, stop),
                (Tok::Eof | Tok::Newline | Tok::Semi, _)
                    | (Tok::LBrace | Tok::RBrace, _)
                    | (Tok::RParen | Tok::RBracket, CaptureStop::Brace)
            );
            if boundary {
                break;
            }
            end = token.offset + token.len;
            self.pos += 1;
        }
        let raw = self.source.get(start..end).unwrap_or_default();
        let trimmed = raw.trim_end();
        let span = self.index.span(self.source, start, trimmed.len());
        (trimmed.to_owned(), span)
    }

    /// Capture a `{ ... }` body as one command line per non-empty line.
    pub(crate) fn capture_brace_lines(&mut self) -> Vec<crate::syntax::Argv> {
        let open = self.bump();
        let inner_start = open.offset + open.len;
        let mut depth = 1usize;
        let mut inner_end = inner_start;
        let mut terminated = false;
        while let Some(token) = self.tokens.get(self.pos) {
            match token.tok {
                Tok::LBrace => depth += 1,
                Tok::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        inner_end = token.offset;
                        self.pos += 1;
                        terminated = true;
                        break;
                    }
                }
                Tok::Eof => {
                    inner_end = token.offset;
                    break;
                }
                _ => {}
            }
            self.pos += 1;
        }
        if !terminated {
            self.push(
                self.index.span(self.source, inner_start, 0),
                "unterminated `{ ... }` block",
            );
        }
        let inner = self.source.get(inner_start..inner_end).unwrap_or_default();
        let mut commands = Vec::new();
        let mut line_start = 0usize;
        for line in inner.split('\n') {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                let leading = line.len() - line.trim_start().len();
                let base = inner_start + line_start + leading;
                let (argv, mut diags) =
                    crate::argv::parse_argv(trimmed, base, self.source, &self.index);
                self.diags.append(&mut diags);
                commands.push(argv);
            }
            line_start += line.len() + 1;
        }
        commands
    }

    /// Parse statements until end of input or the caller's closing brace (left unconsumed).
    pub(crate) fn parse_statements(&mut self, until_rbrace: bool) -> Vec<Stmt> {
        let mut statements = Vec::new();
        loop {
            self.skip_separators();
            match self.peek() {
                Tok::Eof => break,
                Tok::RBrace if until_rbrace => break,
                Tok::RBrace => {
                    let span = self.span();
                    self.push(span, "unexpected `}`");
                    self.bump();
                }
                _ => {
                    let start = self.pos;
                    if let Some(statement) = self.parse_statement() {
                        statements.push(statement);
                    }
                    if self.pos == start {
                        self.bump();
                    }
                }
            }
        }
        statements
    }

    fn parse_statement(&mut self) -> Option<Stmt> {
        let Tok::Ident(keyword) = self.peek().clone() else {
            let span = self.span();
            self.push(span, "expected a top-level statement");
            self.skip_line();
            return None;
        };
        match keyword.as_str() {
            "project" => self.parse_project().map(Stmt::Project),
            "features" => self
                .parse_features()
                .map(|(definitions, span)| Stmt::Features { definitions, span }),
            "postgres" => self.parse_postgres().map(Stmt::Postgres),
            "redis" => self.parse_redis().map(Stmt::Redis),
            "s3" => self.parse_s3().map(Stmt::S3),
            "domain" => self.parse_domain().map(Stmt::Domain),
            "env" => self.parse_env_group().map(Stmt::EnvGroup),
            "service" => self.parse_service().map(Stmt::Service),
            "task" => self.parse_task().map(Stmt::Task),
            "on" => self.parse_on().map(Stmt::On),
            other => {
                let span = self.span();
                self.push(span, format!("unknown statement `{other}`"));
                self.skip_line();
                None
            }
        }
    }

    fn parse_project(&mut self) -> Option<Spanned<String>> {
        let keyword = self.bump();
        let id = self.expect_ident("a project slug")?;
        let span = self.token_span(&keyword).merge(id.span);
        self.finish_line();
        Some(Spanned::new(id.value, span))
    }

    fn parse_on(&mut self) -> Option<OnBlock> {
        let keyword = self.bump();
        let cond = self.parse_condition();
        let mut span = self.token_span(&keyword).merge(cond.span());
        let body = if self.eat(&Tok::LBrace) {
            let body = self.parse_statements(true);
            if self.at(&Tok::RBrace) {
                let close = self.bump();
                span = span.merge(self.token_span(&close));
            }
            body
        } else if self.eat(&Tok::Colon) {
            self.parse_statement().into_iter().collect()
        } else {
            self.push(self.span(), "expected `{` or `:` after an `on` condition");
            self.skip_line();
            Vec::new()
        };
        Some(OnBlock { cond, body, span })
    }

    pub(crate) fn parse_condition(&mut self) -> Condition {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Condition {
        let mut lhs = self.parse_and();
        while self.eat(&Tok::Pipe) {
            let rhs = self.parse_and();
            let span = lhs.span().merge(rhs.span());
            lhs = Condition::Or {
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        lhs
    }

    fn parse_and(&mut self) -> Condition {
        let mut lhs = self.parse_not();
        while self.eat(&Tok::Amp) {
            let rhs = self.parse_not();
            let span = lhs.span().merge(rhs.span());
            lhs = Condition::And {
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        lhs
    }

    fn parse_not(&mut self) -> Condition {
        if self.at(&Tok::Bang) {
            let token = self.bump();
            let inner = self.parse_not();
            let span = self.token_span(&token).merge(inner.span());
            Condition::Not {
                inner: Box::new(inner),
                span,
            }
        } else {
            self.parse_condition_atom()
        }
    }

    fn parse_condition_atom(&mut self) -> Condition {
        if self.eat(&Tok::LParen) {
            let inner = self.parse_or();
            self.expect(&Tok::RParen, "`)`");
            return inner;
        }
        if let Tok::Ident(name) = self.peek().clone() {
            let token = self.bump();
            return Condition::Feature(Spanned::new(name, self.token_span(&token)));
        }
        let span = self.span();
        self.push(span, "expected a feature condition");
        Condition::Feature(Spanned::new(String::new(), span))
    }
}

/// Strip a matching pair of surrounding quotes from raw captured text.
pub(crate) fn unquote(raw: &str) -> String {
    let trimmed = raw.trim();
    let quoted = trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')));
    if quoted {
        trimmed[1..trimmed.len() - 1].to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_capture_ends_at_the_last_token_before_a_comment() {
        let source = "service api ./api {\n    run echo hi # note\n}\n";
        let mut parser = Parser::new(source);
        parser.pos = 0;
        while !matches!(parser.peek(), Tok::Ident(value) if value == "run") {
            parser.bump();
        }
        parser.bump();
        let (raw, span) = parser.capture_raw(CaptureStop::Line);
        assert_eq!(raw, "echo hi");
        assert_eq!(span.line, 2);
    }
}
