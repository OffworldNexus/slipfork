//! `service` declarations, their directive list, and lifecycle-hook capture.

use crate::diag::Span;
use crate::lexer::Tok;
use crate::parser::{CaptureStop, Parser, unquote};
use crate::syntax::{
    Argv, EnvAssignment, HookPhase, ImageSource, ServiceDef, ServiceDirective, Spanned,
};

impl Parser<'_> {
    /// `service <id> <./dir | from <image_ref>> { ... }`
    pub(crate) fn parse_service(&mut self) -> Option<ServiceDef> {
        let keyword = self.bump();
        let id = self.expect_ident("a service id")?;
        let mut span = self.token_span(&keyword).merge(id.span);
        let source = match self.peek().clone() {
            Tok::Path(context_dir) => {
                let token = self.bump();
                let context_span = self.token_span(&token);
                span = span.merge(context_span);
                ImageSource::Dockerfile {
                    context_dir,
                    span: context_span,
                }
            }
            Tok::Ident(word) if word == "from" => {
                self.bump();
                let (raw, raw_span) = self.capture_raw(CaptureStop::Brace);
                if raw.is_empty() {
                    self.push(raw_span, "expected an image reference after `from`");
                    self.skip_line();
                    return None;
                }
                span = span.merge(raw_span);
                ImageSource::OciImage {
                    image_ref: Spanned::new(unquote(&raw), raw_span),
                }
            }
            _ => {
                self.push(
                    self.span(),
                    "expected `./dir` or `from <image>` after the service id",
                );
                self.skip_line();
                return None;
            }
        };
        let directives = if self.eat(&Tok::LBrace) {
            let directives = self.parse_service_directives();
            if self.at(&Tok::RBrace) {
                let close = self.bump();
                span = span.merge(self.token_span(&close));
            }
            directives
        } else {
            self.finish_line();
            Vec::new()
        };
        Some(ServiceDef {
            id,
            source,
            directives,
            span,
        })
    }

    /// Parse directives until the closing brace, which stays unconsumed for the caller.
    pub(crate) fn parse_service_directives(&mut self) -> Vec<ServiceDirective> {
        let mut directives = Vec::new();
        loop {
            self.skip_separators();
            match self.peek().clone() {
                Tok::RBrace => break,
                Tok::Eof => {
                    self.push(self.span(), "unterminated `service` block");
                    break;
                }
                Tok::Ident(keyword) => {
                    let start = self.pos;
                    if let Some(directive) = self.parse_service_directive(&keyword) {
                        directives.push(directive);
                    }
                    if self.pos == start {
                        self.bump();
                    }
                }
                _ => {
                    let span = self.span();
                    self.push(span, "expected a service directive");
                    self.skip_line();
                }
            }
        }
        directives
    }

    fn parse_service_directive(&mut self, keyword: &str) -> Option<ServiceDirective> {
        match keyword {
            "stage" => {
                self.bump();
                let stage = self.parse_scalar("a build stage name")?;
                self.finish_line();
                Some(ServiceDirective::Stage(stage))
            }
            "dockerfile" => {
                self.bump();
                let (raw, span) = self.capture_raw(CaptureStop::Line);
                if raw.is_empty() {
                    self.push(span, "expected a Dockerfile path");
                    return None;
                }
                self.finish_line();
                Some(ServiceDirective::Dockerfile(Spanned::new(
                    unquote(&raw),
                    span,
                )))
            }
            "size" => {
                self.bump();
                let size = self.parse_scalar("a size hint")?;
                self.finish_line();
                Some(ServiceDirective::Size(size))
            }
            "port" => {
                self.bump();
                let port = self.parse_port("a port number")?;
                self.finish_line();
                Some(ServiceDirective::Port(port))
            }
            "internal_ports" => {
                self.bump();
                let directive = self.parse_internal_ports()?;
                self.finish_line();
                Some(directive)
            }
            "route" => {
                self.bump();
                let (raw, span) = self.capture_raw(CaptureStop::Line);
                if raw.is_empty() {
                    self.push(span, "expected a route");
                    return None;
                }
                self.finish_line();
                Some(ServiceDirective::Route(Spanned::new(unquote(&raw), span)))
            }
            "volume" => {
                self.bump();
                let (raw, span) = self.capture_raw(CaptureStop::Line);
                if raw.is_empty() {
                    self.push(span, "expected a volume path");
                    return None;
                }
                self.finish_line();
                Some(ServiceDirective::Volume(Spanned::new(unquote(&raw), span)))
            }
            "use" => {
                self.bump();
                if !self.at_ident("env") {
                    self.push(self.span(), "expected `use env <group>`");
                    self.skip_line();
                    return None;
                }
                self.bump();
                let group = self.expect_ident("an env group name")?;
                self.finish_line();
                Some(ServiceDirective::UseEnv(group))
            }
            "env" => {
                self.bump();
                if self.eat(&Tok::Dot) {
                    let key = self.expect_ident("an env variable name")?;
                    if self.expect(&Tok::Equals, "`=`").is_none() {
                        self.skip_line();
                        return None;
                    }
                    let expr = self.parse_expr();
                    let span = key.span.merge(expr.span());
                    self.finish_line();
                    Some(ServiceDirective::EnvAssignment(EnvAssignment {
                        key,
                        expr,
                        span,
                    }))
                } else if self.at(&Tok::LBrace) {
                    let (vars, span) = self.parse_env_block();
                    Some(ServiceDirective::EnvBlock { vars, span })
                } else {
                    self.push(self.span(), "expected `{` or `.KEY =` after `env`");
                    self.skip_line();
                    None
                }
            }
            "build_env" => {
                self.bump();
                if !self.at(&Tok::LBrace) {
                    self.push(self.span(), "expected `{` after `build_env`");
                    self.skip_line();
                    return None;
                }
                let (vars, span) = self.parse_env_block();
                Some(ServiceDirective::BuildEnv { vars, span })
            }
            "run" => {
                self.bump();
                let argv = self.capture_argv_line();
                Some(ServiceDirective::Run(argv))
            }
            "setup" => self.parse_hook(HookPhase::Setup),
            "pre-deploy" => self.parse_hook(HookPhase::PreDeploy),
            "post-deploy" => self.parse_hook(HookPhase::PostDeploy),
            "failed-deploy" => self.parse_hook(HookPhase::FailedDeploy),
            "on" => {
                let keyword_token = self.bump();
                let cond = self.parse_condition();
                let mut span = self.token_span(&keyword_token).merge(cond.span());
                if self.eat(&Tok::LBrace) {
                    let body = self.parse_service_directives();
                    if self.at(&Tok::RBrace) {
                        let close = self.bump();
                        span = span.merge(self.token_span(&close));
                    }
                    Some(ServiceDirective::On { cond, body, span })
                } else if self.eat(&Tok::Colon) {
                    let body = match self.peek().clone() {
                        Tok::Ident(inner) => {
                            let start = self.pos;
                            let directive = self.parse_service_directive(&inner);
                            if self.pos == start {
                                self.bump();
                            }
                            directive.into_iter().collect()
                        }
                        _ => {
                            self.push(self.span(), "expected a service directive after `:`");
                            self.skip_line();
                            Vec::new()
                        }
                    };
                    Some(ServiceDirective::On { cond, body, span })
                } else {
                    self.push(self.span(), "expected `{` or `:` after an `on` condition");
                    self.skip_line();
                    None
                }
            }
            other => {
                let span = self.span();
                self.push(span, format!("unknown service directive `{other}`"));
                self.skip_line();
                None
            }
        }
    }

    /// Parse a `{ KEY = <expr> ... }` block, consuming both braces.
    pub(crate) fn parse_env_block(&mut self) -> (Vec<EnvAssignment>, Span) {
        let open = self.bump();
        let mut span = self.token_span(&open);
        let mut vars = Vec::new();
        loop {
            self.skip_separators();
            match self.peek().clone() {
                Tok::RBrace => {
                    let close = self.bump();
                    span = span.merge(self.token_span(&close));
                    break;
                }
                Tok::Eof => {
                    self.push(self.span(), "unterminated `env` block");
                    break;
                }
                Tok::Ident(key) => {
                    let token = self.bump();
                    let key_span = self.token_span(&token);
                    if self.expect(&Tok::Equals, "`=`").is_some() {
                        let expr = self.parse_expr();
                        let declaration = key_span.merge(expr.span());
                        vars.push(EnvAssignment {
                            key: Spanned::new(key, key_span),
                            expr,
                            span: declaration,
                        });
                    }
                    self.finish_line();
                }
                _ => {
                    let error_span = self.span();
                    self.push(error_span, "expected an env variable name");
                    self.skip_line();
                }
            }
        }
        (vars, span)
    }

    fn parse_hook(&mut self, phase: HookPhase) -> Option<ServiceDirective> {
        let keyword = self.bump();
        let mut span = self.token_span(&keyword);
        let commands = if self.eat(&Tok::Colon) {
            vec![self.capture_argv_line()]
        } else if self.at(&Tok::LBrace) {
            self.capture_brace_lines()
        } else {
            self.push(
                self.span(),
                format!("expected `:` or `{{` after `{}`", phase.as_str()),
            );
            self.skip_line();
            return None;
        };
        if let Some(last) = commands.last() {
            span = span.merge(last.span);
        }
        Some(ServiceDirective::Hook {
            phase,
            commands,
            span,
        })
    }

    fn parse_port(&mut self, what: &str) -> Option<Spanned<u32>> {
        match self.peek().clone() {
            Tok::Int(value) => {
                let token = self.bump();
                Some(Spanned::new(
                    value.clamp(0, i64::from(u32::MAX)) as u32,
                    self.token_span(&token),
                ))
            }
            _ => {
                let span = self.span();
                self.push(span, format!("expected {what}"));
                self.skip_line();
                None
            }
        }
    }

    fn parse_internal_ports(&mut self) -> Option<ServiceDirective> {
        let open = self.bump();
        let mut span = self.token_span(&open);
        let mut ports = Vec::new();
        loop {
            match self.peek().clone() {
                Tok::RBracket => {
                    let close = self.bump();
                    span = span.merge(self.token_span(&close));
                    break;
                }
                Tok::Comma | Tok::Newline | Tok::Semi => {
                    self.bump();
                }
                Tok::Int(value) => {
                    let token = self.bump();
                    ports.push(Spanned::new(
                        value.clamp(0, i64::from(u32::MAX)) as u32,
                        self.token_span(&token),
                    ));
                }
                Tok::Eof => {
                    self.push(self.span(), "unterminated `internal_ports` list");
                    break;
                }
                _ => {
                    let error_span = self.span();
                    self.push(error_span, "expected a port number or `]`");
                    self.skip_line();
                    break;
                }
            }
        }
        Some(ServiceDirective::InternalPorts { ports, span })
    }

    /// Capture the rest of the current line as one [`Argv`].
    pub(crate) fn capture_argv_line(&mut self) -> Argv {
        let (text, span) = self.capture_raw(CaptureStop::Line);
        let (argv, mut diags) =
            crate::argv::parse_argv(&text, span.offset, self.source, &self.index);
        self.diags.append(&mut diags);
        argv
    }
}
