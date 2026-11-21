//! Declarations for `features`, `env`, and backing resources.

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::parser::Parser;
use crate::span::Span;
use crate::token::TokenKind;

impl<'a> Parser<'a> {
    pub(crate) fn parse_features(&mut self, span: Span) -> Result<Features, Diagnostic> {
        let groups = self.parse_block(|parser| parser.parse_feature_group())?;
        Ok(Features { groups, span })
    }

    fn parse_feature_group(&mut self) -> Result<FeatureGroup, Diagnostic> {
        let span = self.current_span()?;
        let name = self.expect_ident(
            "a feature name",
            "write a feature such as `default = [dev]` or `mail`",
        )?;
        let features = if self.eat(&TokenKind::Eq)? {
            self.expect_punct(
                TokenKind::LBracket,
                "`[`",
                "write the feature list as `[a, b]`",
            )?;
            let mut list = Vec::new();
            loop {
                if self.eat(&TokenKind::RBracket)? {
                    break;
                }
                list.push(
                    self.expect_ident("a feature name", "add a feature name inside the list")?,
                );
                if self.eat(&TokenKind::Comma)? {
                    continue;
                }
                self.expect_punct(
                    TokenKind::RBracket,
                    "`]`",
                    "close the feature list with `]`",
                )?;
                break;
            }
            Some(list)
        } else {
            None
        };
        Ok(FeatureGroup {
            name,
            features,
            span,
        })
    }

    pub(crate) fn parse_env(&mut self, span: Span) -> Result<Env, Diagnostic> {
        let group =
            self.expect_ident("an env group name", "write `env <group> { KEY = <expr> }`")?;
        let entries = self.parse_block(|parser| parser.parse_env_item())?;
        Ok(Env {
            group,
            entries,
            span,
        })
    }

    pub(crate) fn parse_env_item(&mut self) -> Result<EnvItem, Diagnostic> {
        let token = self.bump()?;
        let span = token.span;
        match token.kind {
            TokenKind::Ident(name) if name == "on" => self
                .parse_on_after_keyword(span, |parser| parser.parse_env_item())
                .map(EnvItem::On),
            TokenKind::Ident(key) => {
                self.expect_punct(TokenKind::Eq, "`=`", "write `KEY = <expr>`")?;
                let value = self.parse_expr()?;
                Ok(EnvItem::Entry(EnvEntry { key, value, span }))
            }
            other => Err(Diagnostic::new(
                span,
                format!(
                    "expected an environment variable, found {}",
                    other.describe()
                ),
                "write `KEY = <expr>` or `on <expr>: KEY = <expr>`",
            )),
        }
    }

    pub(crate) fn parse_postgres(&mut self, span: Span) -> Result<Postgres, Diagnostic> {
        let id = self.expect_ident("a postgres id", "write `postgres <id> { database <db> }`")?;
        let body = self.parse_optional_block(|parser| parser.parse_postgres_item())?;
        Ok(Postgres { id, body, span })
    }

    fn parse_postgres_item(&mut self) -> Result<PostgresItem, Diagnostic> {
        let token = self.bump()?;
        let name = match token.kind {
            TokenKind::Ident(name) => name,
            other => {
                return Err(Diagnostic::new(
                    token.span,
                    format!("expected a postgres property, found {}", other.describe()),
                    "use `database <db>`, `version <n>`, or `on <expr>`",
                ));
            }
        };
        match name.as_str() {
            "database" => self
                .expect_ident("a database name", "write `database <db>`")
                .map(PostgresItem::Database),
            "version" => self.parse_version().map(PostgresItem::Version),
            "on" => self
                .parse_on_after_keyword(token.span, |parser| parser.parse_postgres_item())
                .map(PostgresItem::On),
            other => Err(Diagnostic::new(
                token.span,
                format!("unknown postgres property `{other}`"),
                "use `database <db>`, `version <n>`, or `on <expr>`",
            )),
        }
    }

    pub(crate) fn parse_redis(&mut self, span: Span) -> Result<Redis, Diagnostic> {
        let id = self.expect_ident("a redis id", "write `redis <id>`")?;
        let body = self.parse_optional_block(|parser| parser.parse_redis_item())?;
        Ok(Redis { id, body, span })
    }

    fn parse_redis_item(&mut self) -> Result<RedisItem, Diagnostic> {
        let token = self.bump()?;
        let name = match token.kind {
            TokenKind::Ident(name) => name,
            other => {
                return Err(Diagnostic::new(
                    token.span,
                    format!("expected a redis property, found {}", other.describe()),
                    "use `version <n>` or `on <expr>`",
                ));
            }
        };
        match name.as_str() {
            "version" => self.parse_version().map(RedisItem::Version),
            "on" => self
                .parse_on_after_keyword(token.span, |parser| parser.parse_redis_item())
                .map(RedisItem::On),
            other => Err(Diagnostic::new(
                token.span,
                format!("unknown redis property `{other}`"),
                "use `version <n>` or `on <expr>`",
            )),
        }
    }

    pub(crate) fn parse_s3(&mut self, span: Span) -> Result<S3, Diagnostic> {
        let bucket = self.expect_ident("a bucket name", "write `s3 <bucket> { cdn <id> }`")?;
        let body = self.parse_optional_block(|parser| parser.parse_s3_item())?;
        Ok(S3 { bucket, body, span })
    }

    fn parse_s3_item(&mut self) -> Result<S3Item, Diagnostic> {
        let token = self.bump()?;
        let name = match token.kind {
            TokenKind::Ident(name) => name,
            other => {
                return Err(Diagnostic::new(
                    token.span,
                    format!("expected an s3 property, found {}", other.describe()),
                    "use `cdn <id>` or `on <expr>`",
                ));
            }
        };
        match name.as_str() {
            "cdn" => self
                .expect_ident("a cdn id", "write `cdn <id>`")
                .map(S3Item::Cdn),
            "on" => self
                .parse_on_after_keyword(token.span, |parser| parser.parse_s3_item())
                .map(S3Item::On),
            other => Err(Diagnostic::new(
                token.span,
                format!("unknown s3 property `{other}`"),
                "use `cdn <id>` or `on <expr>`",
            )),
        }
    }

    fn parse_version(&mut self) -> Result<String, Diagnostic> {
        let token = self.bump()?;
        match token.kind {
            TokenKind::Int(value) => Ok(value.to_string()),
            TokenKind::Str(value) => Ok(value),
            other => Err(Diagnostic::new(
                token.span,
                format!("expected a version, found {}", other.describe()),
                "write `version 18` or `version \"18-alpine\"`",
            )),
        }
    }
}
