//! `service` declarations, ports, ingress routes, and lifecycle hooks.

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::expr::Command;
use crate::parser::Parser;
use crate::span::Span;
use crate::token::TokenKind;

impl<'a> Parser<'a> {
    pub(crate) fn parse_service(&mut self, span: Span) -> Result<Service, Diagnostic> {
        let (id, id_span) =
            self.expect_ident_spanned("a service id", "write `service <id> { ... }`")?;
        let body = self.parse_block(|parser| parser.parse_service_item())?;
        Ok(Service {
            id,
            id_span,
            body,
            span,
        })
    }

    fn parse_service_item(&mut self) -> Result<ServiceItem, Diagnostic> {
        let token = self.bump()?;
        let span = token.span;
        let name = match token.kind {
            TokenKind::Ident(name) => name,
            other => {
                return Err(Diagnostic::new(
                    token.span,
                    format!("expected a service property, found {}", other.describe()),
                    "use `port`, `internal_ports`, `route`, `run`, `before-start`, or `on`",
                ));
            }
        };
        match name.as_str() {
            "port" => self.expect_port().map(ServiceItem::Port),
            "internal_ports" => self.parse_internal_ports().map(ServiceItem::InternalPorts),
            "route" => self.parse_route(span).map(ServiceItem::Route),
            "before-start" => self.parse_hook(true, span).map(ServiceItem::BeforeStart),
            "pre-deploy" => self.parse_hook(false, span).map(ServiceItem::PreDeploy),
            "post-deploy" => self.parse_hook(false, span).map(ServiceItem::PostDeploy),
            "failed-deploy" => self.parse_hook(false, span).map(ServiceItem::FailedDeploy),
            "run" => self.parse_hook(false, span).map(ServiceItem::Run),
            "on" => self
                .parse_on_after_keyword(span, |parser| parser.parse_service_item())
                .map(ServiceItem::On),
            other => Err(Diagnostic::new(
                span,
                format!("unknown service property `{other}`"),
                "use `port`, `internal_ports`, `route`, `run`, `before-start`, `pre-deploy`, `post-deploy`, `failed-deploy`, or `on`",
            )),
        }
    }

    fn parse_internal_ports(&mut self) -> Result<Vec<u16>, Diagnostic> {
        self.expect_punct(
            TokenKind::LBracket,
            "`[`",
            "write `internal_ports [8081, 8082]`",
        )?;
        let mut ports = Vec::new();
        loop {
            if self.eat(&TokenKind::RBracket)? {
                break;
            }
            ports.push(self.expect_port()?);
            if self.eat(&TokenKind::Comma)? {
                continue;
            }
            self.expect_punct(TokenKind::RBracket, "`]`", "close the port list with `]`")?;
            break;
        }
        Ok(ports)
    }

    fn parse_route(&mut self, span: Span) -> Result<Route, Diagnostic> {
        let mut path = None;
        let mut kind = None;
        let mut domain = None;
        loop {
            if self.lexer.at_line_end()
                || matches!(self.lexer.peek_significant_char(), Some('}' | '#'))
            {
                break;
            }
            let (word, word_span) = self.raw_word()?;
            match word.as_str() {
                "strip-prefix" => {
                    if kind.is_some() {
                        return Err(duplicate_route_modifier(word_span));
                    }
                    kind = Some(RouteKind::StripPrefix);
                }
                "rewrite" => {
                    if kind.is_some() {
                        return Err(duplicate_route_modifier(word_span));
                    }
                    let (target, _) = self.raw_word()?;
                    kind = Some(RouteKind::Rewrite(target));
                }
                "exact" => {
                    if kind.is_some() {
                        return Err(duplicate_route_modifier(word_span));
                    }
                    let (target, _) = self.raw_word()?;
                    kind = Some(RouteKind::Exact(target));
                }
                "domain" => {
                    let (filter, filter_span) = self.raw_word()?;
                    domain = Some(parse_domain_filter(&filter, filter_span)?);
                }
                _ if word.starts_with('/') => {
                    if path.is_some() {
                        return Err(Diagnostic::new(
                            word_span,
                            "duplicate route path",
                            "declare the path once before the route modifiers",
                        ));
                    }
                    path = Some(word);
                }
                other => {
                    return Err(Diagnostic::new(
                        word_span,
                        format!("unknown route modifier `{other}`"),
                        "use `<path>`, `strip-prefix`, `rewrite <path>`, `exact <path>`, or `domain <id>`",
                    ));
                }
            }
        }
        Ok(Route {
            path,
            kind: kind.unwrap_or(RouteKind::Preserve),
            domain,
            span,
        })
    }

    pub(crate) fn parse_hook(&mut self, allow_when: bool, span: Span) -> Result<Hook, Diagnostic> {
        let when_changed = if allow_when && self.peek_ident_is("when")? {
            self.bump()?;
            let keyword = self.bump()?;
            match keyword.kind {
                TokenKind::Ident(name) if name == "changed" => {}
                other => {
                    return Err(Diagnostic::new(
                        keyword.span,
                        format!(
                            "expected `changed` after `when`, found {}",
                            other.describe()
                        ),
                        "write `when changed(<paths>)`",
                    ));
                }
            }
            self.expect_punct(TokenKind::LParen, "`(`", "write `when changed(<paths>)`")?;
            let mut paths = Vec::new();
            loop {
                if self.next_is(')') {
                    self.bump()?;
                    break;
                }
                let (path, _) = self.raw_word()?;
                paths.push(path);
                if self.next_is(',') {
                    self.bump()?;
                    continue;
                }
                self.expect_punct(TokenKind::RParen, "`)`", "close the change list with `)`")?;
                break;
            }
            Some(paths)
        } else {
            None
        };
        let token = self.bump()?;
        let body = match token.kind {
            TokenKind::Colon => {
                let (raw, command_span) = self.raw_to_line_end()?;
                HookBody::Inline(Command::parse(&raw, command_span)?)
            }
            TokenKind::LBrace => HookBody::Block(self.parse_command_block()?),
            other => {
                return Err(Diagnostic::new(
                    token.span,
                    format!(
                        "expected `:` or `{{` after the hook name, found {}",
                        other.describe()
                    ),
                    "write `run: <cmd>` or `run { ... }`",
                ));
            }
        };
        Ok(Hook {
            when_changed,
            body,
            span,
        })
    }

    fn parse_command_block(&mut self) -> Result<Block<Command>, Diagnostic> {
        let mut block = Block::default();
        let mut pending = Vec::new();
        loop {
            self.lexer.skip_inline_space();
            match self.lexer.peek_char() {
                Some('\n') => {
                    self.lexer.bump_char();
                }
                None => {
                    return Err(Diagnostic::new(
                        self.current_span()?,
                        "unclosed hook block",
                        "add a closing `}`",
                    ));
                }
                Some('}') => {
                    self.lexer.bump_char();
                    Self::flush_comments(&mut block, &mut pending);
                    return Ok(block);
                }
                Some('#') => {
                    let (text, _) = self.raw_to_line_end()?;
                    pending.push(text);
                }
                Some(_) => {
                    let (raw, command_span) = self.raw_to_line_end()?;
                    if raw.is_empty() {
                        continue;
                    }
                    let command = Command::parse(&raw, command_span)?;
                    Self::flush_comments(&mut block, &mut pending);
                    block.items.push(command);
                }
            }
        }
    }
}

fn duplicate_route_modifier(span: Span) -> Diagnostic {
    Diagnostic::new(
        span,
        "duplicate route modifier",
        "keep only one of `strip-prefix`, `rewrite`, or `exact`",
    )
}

fn parse_domain_filter(word: &str, span: Span) -> Result<DomainFilter, Diagnostic> {
    if let Some(star) = word.find('*') {
        let mut start = span.start;
        start.col += word[..star].chars().count() as u32;
        start.offset += star;
        let mut end = start;
        end.col += 1;
        end.offset += 1;
        let span = Span::new(start, end);
        return Err(match word.strip_suffix(".*") {
            Some(id) => Diagnostic::new(
                span,
                "route domain wildcards are gone",
                format!("declare 'domain {id}' at top level and write 'route / domain {id}'"),
            ),
            None => Diagnostic::new(
                span,
                "domain ids are lowercase [a-z0-9-]",
                "drop the `*` and declare a `domain <id>` of only lowercase [a-z0-9-]",
            ),
        });
    }
    Ok(DomainFilter::Domain {
        name: word.to_string(),
        span,
    })
}
