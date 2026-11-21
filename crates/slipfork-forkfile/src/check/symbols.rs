//! The flat, globally unique `Forkfile` symbol namespace and its collectors.

use std::collections::BTreeMap;

use crate::ast::*;
use crate::diag::{Diagnostic, Diagnostics};
use crate::span::Span;

/// The kind of declaration a symbol names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    /// `postgres <id>`
    Postgres,
    /// `database <id>` inside a postgres block.
    Database,
    /// `redis <id>`
    Redis,
    /// `s3 <bucket>`
    S3,
    /// `cdn <id>` inside an s3 block.
    Cdn,
    /// `domain <id>`
    Domain,
    /// `image <id>`
    Image,
    /// `service <id>`
    Service,
}

impl SymbolKind {
    /// The keyword this symbol was declared with.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Database => "database",
            Self::Redis => "redis",
            Self::S3 => "s3",
            Self::Cdn => "cdn",
            Self::Domain => "domain",
            Self::Image => "image",
            Self::Service => "service",
        }
    }

    /// Typed properties exposed by this symbol, if any.
    pub(crate) const fn properties(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Postgres | Self::Database => {
                Some(&["url", "host", "port", "name", "user", "password"])
            }
            Self::Redis => Some(&["url", "host", "port"]),
            Self::S3 => Some(&["url", "endpoint", "bucket", "access_key", "secret_key"]),
            Self::Cdn | Self::Domain => Some(&["domain", "url"]),
            Self::Image | Self::Service => None,
        }
    }

    /// Whether reading this property yields a secret-tainted value.
    pub(crate) fn is_secret_property(self, property: &str) -> bool {
        matches!(
            (self, property),
            (Self::Postgres | Self::Database, "password") | (Self::S3, "access_key" | "secret_key")
        )
    }
}

/// One declaration in the flat namespace.
#[derive(Debug, Clone)]
pub struct Symbol {
    /// What kind of declaration this is.
    pub kind: SymbolKind,
    /// Where the id was declared.
    pub span: Span,
}

/// The flat, globally unique symbol table for one document.
#[derive(Debug, Clone, Default)]
pub struct SymbolTable {
    entries: BTreeMap<String, Symbol>,
}

impl SymbolTable {
    /// Declares `name`, returning the duplicate diagnostic when it is taken.
    pub fn declare(&mut self, name: &str, kind: SymbolKind, span: Span) -> Result<(), Diagnostic> {
        if let Some(existing) = self.entries.get(name) {
            let first = existing.span;
            return Err(Diagnostic::new(
                span,
                format!(
                    "duplicate symbol `{name}` declared as `{}` at Forkfile:{}:{} and `{}` here",
                    existing.kind.label(),
                    first.line(),
                    first.col(),
                    kind.label()
                ),
                "rename one declaration so every `postgres`, `database`, `redis`, `s3`, `cdn`, `domain`, `image`, and `service` id stays globally unique",
            ));
        }
        self.entries.insert(name.to_string(), Symbol { kind, span });
        Ok(())
    }

    /// Looks up a symbol by id.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Symbol> {
        self.entries.get(name)
    }

    /// Whether `name` is declared.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// The number of declared symbols.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no symbols are declared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterates symbols in id order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Symbol)> + '_ {
        self.entries
            .iter()
            .map(|(name, symbol)| (name.as_str(), symbol))
    }

    /// Collects every declaration in source order, recording duplicates.
    pub(crate) fn from_document(document: &Forkfile, diagnostics: &mut Diagnostics) -> Self {
        let mut table = Self::default();
        for item in &document.items.items {
            table.collect_top(item, diagnostics);
        }
        table
    }

    fn record(&mut self, diagnostics: &mut Diagnostics, name: &str, kind: SymbolKind, span: Span) {
        if let Err(diagnostic) = self.declare(name, kind, span) {
            diagnostics.push(diagnostic);
        }
    }

    fn collect_top(&mut self, item: &TopItem, diagnostics: &mut Diagnostics) {
        match item {
            TopItem::Features(_) | TopItem::Env(_) => {}
            TopItem::Image(image) => {
                self.record(diagnostics, &image.id, SymbolKind::Image, image.span);
                for item in &image.body.items {
                    self.collect_image_item(item, diagnostics, &image.id);
                }
            }
            TopItem::On(on) => match &on.body {
                OnBody::Inline(inner) => self.collect_top(inner, diagnostics),
                OnBody::Block(block) => {
                    for item in &block.items {
                        self.collect_top(item, diagnostics);
                    }
                }
            },
            TopItem::Domain(domain) => {
                self.record(diagnostics, &domain.id, SymbolKind::Domain, domain.span);
            }
            TopItem::Postgres(postgres) => {
                self.record(
                    diagnostics,
                    &postgres.id,
                    SymbolKind::Postgres,
                    postgres.span,
                );
                self.collect_postgres_items(&postgres.body.items, diagnostics);
            }
            TopItem::Redis(redis) => {
                self.record(diagnostics, &redis.id, SymbolKind::Redis, redis.span);
            }
            TopItem::S3(s3) => {
                self.record(diagnostics, &s3.bucket, SymbolKind::S3, s3.span);
                self.collect_s3_items(&s3.body.items, diagnostics);
            }
        }
    }

    fn collect_image_item(&mut self, item: &ImageItem, diagnostics: &mut Diagnostics, owner: &str) {
        match item {
            ImageItem::Service(service) => {
                // A service may reuse its enclosing image's slot (`image x { service x }`);
                // any other cross-kind collision is still a duplicate.
                if service.id != owner {
                    self.record(diagnostics, &service.id, SymbolKind::Service, service.span);
                }
            }
            ImageItem::On(on) => match &on.body {
                OnBody::Inline(inner) => self.collect_image_item(inner, diagnostics, owner),
                OnBody::Block(block) => {
                    for item in &block.items {
                        self.collect_image_item(item, diagnostics, owner);
                    }
                }
            },
            _ => {}
        }
    }

    fn collect_postgres_items(&mut self, items: &[PostgresItem], diagnostics: &mut Diagnostics) {
        for item in items {
            match item {
                PostgresItem::Database(database) => {
                    self.record(
                        diagnostics,
                        &database.id,
                        SymbolKind::Database,
                        database.span,
                    );
                }
                PostgresItem::On(on) => match &on.body {
                    OnBody::Inline(inner) => {
                        self.collect_postgres_items(
                            std::slice::from_ref(inner.as_ref()),
                            diagnostics,
                        );
                    }
                    OnBody::Block(block) => {
                        self.collect_postgres_items(&block.items, diagnostics);
                    }
                },
                PostgresItem::Version(_) => {}
            }
        }
    }

    fn collect_s3_items(&mut self, items: &[S3Item], diagnostics: &mut Diagnostics) {
        for item in items {
            match item {
                S3Item::Cdn(cdn) => {
                    self.record(diagnostics, &cdn.id, SymbolKind::Cdn, cdn.span);
                }
                S3Item::On(on) => match &on.body {
                    OnBody::Inline(inner) => {
                        self.collect_s3_items(std::slice::from_ref(inner.as_ref()), diagnostics);
                    }
                    OnBody::Block(block) => self.collect_s3_items(&block.items, diagnostics),
                },
            }
        }
    }
}
