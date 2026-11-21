//! Backing resource declarations: `domain`, `postgres`, `redis`, and `s3`.

use crate::ast::{Block, On};
use crate::span::Span;

/// `domain <id>`
#[derive(Debug, Clone)]
pub struct Domain {
    pub id: String,
    pub span: Span,
}

ast_eq!(Domain { id });

/// One `database <db>` declaration inside a postgres block.
#[derive(Debug, Clone)]
pub struct DatabaseDecl {
    pub id: String,
    pub span: Span,
}

ast_eq!(DatabaseDecl { id });

/// One `cdn <id>` declaration inside an s3 block.
#[derive(Debug, Clone)]
pub struct CdnDecl {
    pub id: String,
    pub span: Span,
}

ast_eq!(CdnDecl { id });

/// `postgres <id> { ... }`
#[derive(Debug, Clone)]
pub struct Postgres {
    pub id: String,
    pub body: Block<PostgresItem>,
    pub span: Span,
}

ast_eq!(Postgres { id, body });

/// A declaration inside a `postgres` block.
#[derive(Debug, Clone, PartialEq)]
pub enum PostgresItem {
    /// `database <db>`
    Database(DatabaseDecl),
    /// `version <n>`
    Version(String),
    /// `on <expr> ...`
    On(On<PostgresItem>),
}

/// `redis <id>`
#[derive(Debug, Clone)]
pub struct Redis {
    pub id: String,
    pub body: Block<RedisItem>,
    pub span: Span,
}

ast_eq!(Redis { id, body });

/// A declaration inside a `redis` block.
#[derive(Debug, Clone, PartialEq)]
pub enum RedisItem {
    /// `version <n>`
    Version(String),
    /// `on <expr> ...`
    On(On<RedisItem>),
}

/// `s3 <bucket>`
#[derive(Debug, Clone)]
pub struct S3 {
    pub bucket: String,
    pub body: Block<S3Item>,
    pub span: Span,
}

ast_eq!(S3 { bucket, body });

/// A declaration inside an `s3` block.
#[derive(Debug, Clone, PartialEq)]
pub enum S3Item {
    /// `cdn <id>`
    Cdn(CdnDecl),
    /// `on <expr> ...`
    On(On<S3Item>),
}
