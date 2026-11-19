//! Resource, env-group, task, and feature declarations.

use crate::diag::Span;

use crate::lexer::Tok;
use crate::parser::Parser;
use crate::syntax::{
    DomainDef, EnvGroup, FeatureDef, PgClusterDef, PgDatabaseDef, PgOn, RedisClusterDef, RedisOn,
    S3Def, Spanned, TaskDef,
};

impl Parser<'_> {
    pub(crate) fn parse_postgres(&mut self) -> Option<PgClusterDef> {
        let keyword = self.bump();
        let id = self.expect_ident("a postgres cluster id")?;
        let mut span = self.token_span(&keyword).merge(id.span);
        let mut cluster = PgClusterDef {
            id,
            version: None,
            databases: Vec::new(),
            on: Vec::new(),
            span,
        };
        if self.eat(&Tok::LBrace) {
            loop {
                self.skip_separators();
                match self.peek().clone() {
                    Tok::RBrace => {
                        let close = self.bump();
                        span = span.merge(self.token_span(&close));
                        break;
                    }
                    Tok::Eof => {
                        self.push(self.span(), "unterminated `postgres` block");
                        break;
                    }
                    Tok::Ident(directive) => match directive.as_str() {
                        "version" => {
                            self.bump();
                            cluster.version = self.parse_scalar("a postgres version");
                            self.finish_line();
                        }
                        "database" => {
                            if let Some(database) = self.parse_pg_database() {
                                cluster.databases.push(database);
                            }
                        }
                        "on" => {
                            if let Some(block) = self.parse_pg_on() {
                                cluster.on.push(block);
                            }
                        }
                        other => {
                            let error_span = self.span();
                            self.push(error_span, format!("unknown postgres directive `{other}`"));
                            self.skip_line();
                        }
                    },
                    _ => {
                        let error_span = self.span();
                        self.push(error_span, "expected a postgres directive");
                        self.skip_line();
                    }
                }
            }
        } else {
            self.finish_line();
        }
        cluster.span = span;
        Some(cluster)
    }
    fn parse_pg_database(&mut self) -> Option<PgDatabaseDef> {
        let keyword = self.bump();
        let id = self.expect_ident("a database id")?;
        let mut span = self.token_span(&keyword).merge(id.span);
        let mut database = PgDatabaseDef {
            id,
            pool: None,
            span,
        };
        if self.eat(&Tok::LBrace) {
            loop {
                self.skip_separators();
                match self.peek().clone() {
                    Tok::RBrace => {
                        let close = self.bump();
                        span = span.merge(self.token_span(&close));
                        break;
                    }
                    Tok::Eof => {
                        self.push(self.span(), "unterminated `database` block");
                        break;
                    }
                    Tok::Ident(directive) if directive == "pool" => {
                        self.bump();
                        if self.expect(&Tok::Equals, "`=`").is_some()
                            && let Some(pool) = self.parse_bool("`true` or `false`")
                        {
                            database.pool = Some(pool);
                        }
                        self.finish_line();
                    }
                    _ => {
                        let error_span = self.span();
                        self.push(error_span, "expected `pool = true|false`");
                        self.skip_line();
                    }
                }
            }
        } else {
            self.finish_line();
        }
        database.span = span;
        Some(database)
    }
    fn parse_pg_on(&mut self) -> Option<PgOn> {
        let keyword = self.bump();
        let cond = self.parse_condition();
        let mut span = self.token_span(&keyword).merge(cond.span());
        let mut version = None;
        let mut databases = Vec::new();
        if self.expect(&Tok::LBrace, "`{`").is_some() {
            loop {
                self.skip_separators();
                match self.peek().clone() {
                    Tok::RBrace => {
                        let close = self.bump();
                        span = span.merge(self.token_span(&close));
                        break;
                    }
                    Tok::Eof => {
                        self.push(self.span(), "unterminated `on` block");
                        break;
                    }
                    Tok::Ident(directive) if directive == "version" => {
                        self.bump();
                        version = self.parse_scalar("a postgres version");
                        self.finish_line();
                    }
                    Tok::Ident(directive) if directive == "database" => {
                        if let Some(database) = self.parse_pg_database() {
                            databases.push(database);
                        }
                    }
                    _ => {
                        let error_span = self.span();
                        self.push(error_span, "expected `version` or `database`");
                        self.skip_line();
                    }
                }
            }
        } else {
            self.push(self.span(), "expected `{` after an `on` condition");
            self.skip_line();
        }
        Some(PgOn {
            cond,
            version,
            databases,
            span,
        })
    }
    pub(crate) fn parse_redis(&mut self) -> Option<RedisClusterDef> {
        let keyword = self.bump();
        let id = self.expect_ident("a redis cluster id")?;
        let mut span = self.token_span(&keyword).merge(id.span);
        let mut cluster = RedisClusterDef {
            id,
            version: None,
            on: Vec::new(),
            span,
        };
        if self.eat(&Tok::LBrace) {
            loop {
                self.skip_separators();
                match self.peek().clone() {
                    Tok::RBrace => {
                        let close = self.bump();
                        span = span.merge(self.token_span(&close));
                        break;
                    }
                    Tok::Eof => {
                        self.push(self.span(), "unterminated `redis` block");
                        break;
                    }
                    Tok::Ident(directive) if directive == "version" => {
                        self.bump();
                        cluster.version = self.parse_scalar("a redis version");
                        self.finish_line();
                    }
                    Tok::Ident(directive) if directive == "on" => {
                        let keyword_token = self.bump();
                        let cond = self.parse_condition();
                        let mut block_span = self.token_span(&keyword_token).merge(cond.span());
                        let mut version = None;
                        if self.expect(&Tok::LBrace, "`{`").is_some() {
                            loop {
                                self.skip_separators();
                                match self.peek().clone() {
                                    Tok::RBrace => {
                                        let close = self.bump();
                                        block_span = block_span.merge(self.token_span(&close));
                                        break;
                                    }
                                    Tok::Eof => {
                                        self.push(self.span(), "unterminated `on` block");
                                        break;
                                    }
                                    Tok::Ident(inner) if inner == "version" => {
                                        self.bump();
                                        version = self.parse_scalar("a redis version");
                                        self.finish_line();
                                    }
                                    _ => {
                                        let error_span = self.span();
                                        self.push(error_span, "expected `version`");
                                        self.skip_line();
                                    }
                                }
                            }
                        } else {
                            self.push(self.span(), "expected `{` after an `on` condition");
                            self.skip_line();
                        }
                        cluster.on.push(RedisOn {
                            cond,
                            version,
                            span: block_span,
                        });
                    }
                    _ => {
                        let error_span = self.span();
                        self.push(error_span, "expected `version` or `on`");
                        self.skip_line();
                    }
                }
            }
        } else {
            self.finish_line();
        }
        cluster.span = span;
        Some(cluster)
    }
    pub(crate) fn parse_s3(&mut self) -> Option<S3Def> {
        let keyword = self.bump();
        let id = self.expect_ident("an s3 bucket id")?;
        let span = self.token_span(&keyword).merge(id.span);
        self.finish_line();
        Some(S3Def { id, span })
    }
    pub(crate) fn parse_domain(&mut self) -> Option<DomainDef> {
        let keyword = self.bump();
        let id = self.expect_ident("a domain id")?;
        let span = self.token_span(&keyword).merge(id.span);
        self.finish_line();
        Some(DomainDef { id, span })
    }
    pub(crate) fn parse_env_group(&mut self) -> Option<EnvGroup> {
        let keyword = self.bump();
        let name = self.expect_ident("an env group name")?;
        let mut span = self.token_span(&keyword).merge(name.span);
        let vars = if self.at(&Tok::LBrace) {
            let (vars, block_span) = self.parse_env_block();
            span = span.merge(block_span);
            vars
        } else {
            self.push(self.span(), "expected `{` after an env group name");
            self.skip_line();
            Vec::new()
        };
        Some(EnvGroup { name, vars, span })
    }
    pub(crate) fn parse_task(&mut self) -> Option<TaskDef> {
        let keyword = self.bump();
        let name = self.expect_ident("a task name")?;
        if !self.at_ident("in") {
            self.push(self.span(), "expected `in <service>` after a task name");
            self.skip_line();
            return None;
        }
        self.bump();
        let service = self.expect_ident("a service id")?;
        let mut span = self.token_span(&keyword).merge(service.span);
        let (commands, braced) = if self.eat(&Tok::Colon) {
            (vec![self.capture_argv_line()], false)
        } else if self.at(&Tok::LBrace) {
            (self.capture_brace_lines(), true)
        } else {
            self.push(self.span(), "expected `:` or `{` after a task header");
            self.skip_line();
            return None;
        };
        if let Some(last) = commands.last() {
            span = span.merge(last.span);
        }
        let multi = braced && commands.len() > 1;
        Some(TaskDef {
            name,
            service,
            commands,
            multi,
            span,
        })
    }
    pub(crate) fn parse_scalar(&mut self, what: &str) -> Option<Spanned<String>> {
        match self.peek().clone() {
            Tok::Ident(value) | Tok::Str(value) => {
                let token = self.bump();
                Some(Spanned::new(value, self.token_span(&token)))
            }
            Tok::Int(value) => {
                let token = self.bump();
                Some(Spanned::new(value.to_string(), self.token_span(&token)))
            }
            _ => {
                let span = self.span();
                self.push(span, format!("expected {what}"));
                self.skip_line();
                None
            }
        }
    }
    fn parse_bool(&mut self, what: &str) -> Option<Spanned<bool>> {
        match self.peek().clone() {
            Tok::Ident(value) if value == "true" || value == "false" => {
                let token = self.bump();
                Some(Spanned::new(value == "true", self.token_span(&token)))
            }
            _ => {
                let span = self.span();
                self.push(span, format!("expected {what}"));
                self.skip_line();
                None
            }
        }
    }
    pub(crate) fn parse_features(&mut self) -> Option<(Vec<FeatureDef>, Span)> {
        let keyword = self.bump();
        let mut span = self.token_span(&keyword);
        let mut definitions = Vec::new();
        self.expect(&Tok::LBrace, "`{`")?;
        loop {
            self.skip_separators();
            match self.peek().clone() {
                Tok::RBrace => {
                    let close = self.bump();
                    span = span.merge(self.token_span(&close));
                    break;
                }
                Tok::Eof => {
                    self.push(self.span(), "unterminated `features` block");
                    break;
                }
                Tok::Ident(name) => {
                    let token = self.bump();
                    let name_span = self.token_span(&token);
                    let mut enables = Vec::new();
                    if self.eat(&Tok::Equals) && self.expect(&Tok::LBracket, "`[`").is_some() {
                        loop {
                            self.skip_separators();
                            match self.peek().clone() {
                                Tok::RBracket => {
                                    self.bump();
                                    break;
                                }
                                Tok::Ident(_) => {
                                    if let Some(enabled) = self.expect_ident("a feature name") {
                                        enables.push(enabled);
                                    }
                                    if !self.eat(&Tok::Comma) && !self.at(&Tok::RBracket) {
                                        let error_span = self.span();
                                        self.push(error_span, "expected `,` or `]`");
                                    }
                                }
                                Tok::Eof => {
                                    self.push(self.span(), "unterminated feature list");
                                    break;
                                }
                                _ => {
                                    let error_span = self.span();
                                    self.push(error_span, "expected a feature name");
                                    self.bump();
                                }
                            }
                        }
                    }
                    let declaration_span = enables
                        .last()
                        .map_or(name_span, |enabled| name_span.merge(enabled.span));
                    definitions.push(FeatureDef {
                        name: Spanned::new(name, name_span),
                        enables,
                        span: declaration_span,
                    });
                    self.eat(&Tok::Comma);
                    self.finish_line();
                }
                _ => {
                    let error_span = self.span();
                    self.push(error_span, "expected a feature name");
                    self.skip_line();
                }
            }
        }
        Some((definitions, span))
    }
}
