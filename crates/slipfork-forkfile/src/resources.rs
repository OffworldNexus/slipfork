//! Backing resource declarations: `domain`, `postgres`, `redis`, and `s3`.

use std::collections::BTreeSet;

use crate::ast::{ActiveBody, Block, On, OnBody};
use crate::check::SymbolKind;
use crate::expr::{Expr, ExprKind};
use crate::span::Span;

/// `domain <id>`
#[derive(Debug, Clone)]
pub struct Domain {
    pub id: String,
    /// Where the id token was declared.
    pub id_span: Span,
    pub span: Span,
}

ast_eq!(Domain { id });

/// One `database <db>` declaration inside a postgres block.
#[derive(Debug, Clone)]
pub struct DatabaseDecl {
    pub id: String,
    /// Where the database id token was declared.
    pub id_span: Span,
    pub span: Span,
}

ast_eq!(DatabaseDecl { id });

/// One `cdn <id>` declaration inside an s3 block.
#[derive(Debug, Clone)]
pub struct CdnDecl {
    pub id: String,
    /// Where the cdn id token was declared.
    pub id_span: Span,
    pub span: Span,
}

ast_eq!(CdnDecl { id });

/// Anonymous-read access policy for an `s3` bucket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BucketPolicy {
    /// Anonymous `GET`/`HEAD` answer `403`.
    #[default]
    Private,
    /// Anonymous `GET`/`HEAD` answer `200`; writes still require SigV4.
    PublicRead,
}

impl BucketPolicy {
    /// The canonical `Forkfile` keyword.
    #[must_use]
    pub const fn keyword(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::PublicRead => "public-read",
        }
    }

    /// Parses a `policy` keyword.
    #[must_use]
    pub fn parse(keyword: &str) -> Option<Self> {
        match keyword {
            "private" => Some(Self::Private),
            "public-read" => Some(Self::PublicRead),
            _ => None,
        }
    }
}

/// `policy <private|public-read>`
#[derive(Debug, Clone, Copy)]
pub struct PolicyDecl {
    pub policy: BucketPolicy,
    /// The `policy` keyword span.
    pub span: Span,
}

ast_eq!(PolicyDecl { policy });

/// Whether reading `property` from a resource of `kind` yields a secret-tainted value.
///
/// `secret_key` is always secret, `url` is secret only on a `private` bucket, and
/// `access_key`/`endpoint`/`bucket` are never secret.
#[must_use]
pub fn secret_property(kind: SymbolKind, policy: BucketPolicy, property: &str) -> bool {
    match (kind, property) {
        (SymbolKind::Postgres | SymbolKind::Database, "password") => true,
        (SymbolKind::S3, "secret_key") => true,
        (SymbolKind::S3, "url") => policy == BucketPolicy::Private,
        _ => false,
    }
}

/// `postgres <id> { ... }`
#[derive(Debug, Clone)]
pub struct Postgres {
    pub id: String,
    /// Where the postgres id token was declared.
    pub id_span: Span,
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
    /// Where the redis id token was declared.
    pub id_span: Span,
    pub body: Block<RedisItem>,
    pub span: Span,
}

ast_eq!(Redis { id, body });

/// A declaration inside a `redis` block.
// A parsed AST is built once per document; the guarded body is not worth boxing.
#[allow(clippy::large_enum_variant)]
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
    /// Where the bucket id token was declared.
    pub id_span: Span,
    pub body: Block<S3Item>,
    pub span: Span,
}

ast_eq!(S3 { bucket, body });

impl S3 {
    /// The bucket policy in effect for `active` features, resolving through `on` branches.
    #[must_use]
    pub fn policy_at(&self, active: &BTreeSet<String>) -> BucketPolicy {
        let mut policy = BucketPolicy::Private;
        for_each_active_s3_item(&self.body, active, &mut |item| {
            if let S3Item::Policy(decl) = item {
                policy = decl.policy;
            }
        });
        policy
    }
}

/// A declaration inside an `s3` block.
#[derive(Debug, Clone, PartialEq)]
pub enum S3Item {
    /// `policy <private|public-read>`
    Policy(PolicyDecl),
    /// `cdn <id>`
    Cdn(CdnDecl),
    /// `on <expr> ...`
    On(On<S3Item>),
}

/// Visits every item inside an `s3` body, descending through every `on` branch.
pub fn for_each_s3_item<'a>(body: &'a Block<S3Item>, visit: &mut impl FnMut(&'a S3Item)) {
    walk_s3(&body.items, None, visit);
}

/// Visits the items enabled by `active`, descending only through matching `on` branches.
pub(crate) fn for_each_active_s3_item<'a>(
    body: &'a Block<S3Item>,
    active: &BTreeSet<String>,
    visit: &mut impl FnMut(&'a S3Item),
) {
    walk_s3(&body.items, Some(active), visit);
}

/// Every `cdn` declaration with the exact features enclosing it.
pub(crate) fn s3_cdn_branches(body: &Block<S3Item>) -> Vec<(&CdnDecl, BTreeSet<String>)> {
    let mut out = Vec::new();
    cdn_branches(&body.items, &BTreeSet::new(), &mut out);
    out
}

fn walk_s3<'a>(
    items: &'a [S3Item],
    active: Option<&BTreeSet<String>>,
    visit: &mut impl FnMut(&'a S3Item),
) {
    for item in items {
        match item {
            S3Item::On(on) => {
                let mut matches =
                    |expr: &Expr| active.is_none_or(|active| condition_matches(expr, active));
                if let Some(body) = on.active_body(&mut matches) {
                    visit(item);
                    match body {
                        ActiveBody::Inline(inner) => {
                            walk_s3(std::slice::from_ref(inner), active, visit);
                        }
                        ActiveBody::Block(block) => walk_s3(&block.items, active, visit),
                    }
                }
            }
            other => visit(other),
        }
    }
}

fn cdn_branches<'a>(
    items: &'a [S3Item],
    enclosing: &BTreeSet<String>,
    out: &mut Vec<(&'a CdnDecl, BTreeSet<String>)>,
) {
    for item in items {
        match item {
            S3Item::Cdn(cdn) => out.push((cdn, enclosing.clone())),
            S3Item::On(on) => {
                let mut next = enclosing.clone();
                condition_features(&on.cond, &mut next);
                on.for_each_branch(&mut |body| match body {
                    OnBody::Inline(inner) => {
                        cdn_branches(std::slice::from_ref(inner.as_ref()), &next, out);
                    }
                    OnBody::Block(block) => cdn_branches(&block.items, &next, out),
                });
            }
            S3Item::Policy(_) => {}
        }
    }
}

fn condition_features(expr: &Expr, out: &mut BTreeSet<String>) {
    match &expr.kind {
        ExprKind::Feature(name) => {
            out.insert(name.clone());
        }
        ExprKind::Not(inner) => condition_features(inner, out),
        ExprKind::And(left, right) | ExprKind::Or(left, right) => {
            condition_features(left, out);
            condition_features(right, out);
        }
        ExprKind::Fallback(left, _) => condition_features(left, out),
        _ => {}
    }
}

fn condition_matches(expr: &Expr, active: &BTreeSet<String>) -> bool {
    match &expr.kind {
        ExprKind::Bool(value) => *value,
        ExprKind::Feature(name) => active.contains(name),
        ExprKind::Not(inner) => !condition_matches(inner, active),
        ExprKind::And(left, right) => {
            condition_matches(left, active) && condition_matches(right, active)
        }
        ExprKind::Or(left, right) => {
            condition_matches(left, active) || condition_matches(right, active)
        }
        ExprKind::Fallback(left, _) => condition_matches(left, active),
        _ => false,
    }
}
