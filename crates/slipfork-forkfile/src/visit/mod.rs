//! Read-only `on`-aware visitors: services with resolved ports/routes, resources, authorities,
//! and every runtime variable a component declares.
//!
//! Every consumer that needs the declared surface walks these visitors instead of re-implementing
//! the `on <expr> { ... }` recursion. The walk is AST-only and never reads source text; the
//! `*_with` variants gate every declaration through one [`FeatureSet`].

use std::collections::BTreeMap;

use crate::ast::{
    EnvItem, Forkfile, ImageItem, PostgresItem, RedisItem, Route, S3Item, Service, ServiceItem,
    TopItem,
};
use crate::eval::FeatureSet;
use crate::gate::each_enabled;
use crate::{Expr, ExprKind};

/// One service declaration with the port, routes, and enclosing image that apply to it.
#[derive(Debug, Clone)]
pub struct ServiceDecl<'a> {
    /// Service id.
    pub id: &'a str,
    /// Public `port`, when declared.
    pub port: Option<u16>,
    /// Declared routes, in source order.
    pub routes: Vec<&'a Route>,
    /// The `image <id>` block the service was declared inside.
    pub image: Option<&'a str>,
}

/// Which backing resource an id declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceDeclKind {
    /// `postgres <id>`.
    Postgres,
    /// `redis <id>`.
    Redis,
    /// `s3 <bucket>`.
    S3,
}

/// One backing-resource declaration with its explicit version and CDN, when present.
#[derive(Debug, Clone)]
pub struct ResourceDecl<'a> {
    /// Resource or bucket id.
    pub id: &'a str,
    /// Which backend the id declares.
    pub kind: ResourceDeclKind,
    /// Explicit `version <n>`, when declared.
    pub version: Option<&'a str>,
    /// `cdn <id>` inside an S3 block.
    pub cdn: Option<&'a str>,
    /// Child `database <id>` names inside a postgres block.
    pub databases: Vec<&'a str>,
}

/// One declared runtime variable, classified for the vault without a source re-scan.
#[derive(Debug, Clone)]
pub struct EnvBinding<'a> {
    /// The first `secret(KEY)` reachable through fallbacks, concatenation, or interpolation.
    pub secret: Option<&'a str>,
    /// The generator attached to that `secret(KEY, <generator>)`, when any.
    pub secret_generator: Option<crate::expr::GeneratorSpec>,
    /// The generator attached to a `var(KEY, <generator>)`, when any.
    pub var_generator: Option<crate::expr::GeneratorSpec>,
    /// The static literal the declaration resolves to without the vault.
    pub literal: Option<String>,
}

impl EnvBinding<'_> {
    /// Whether the declaration resolves through the secret vault.
    #[must_use]
    pub fn is_secret(&self) -> bool {
        self.secret.is_some()
    }
}

impl Forkfile {
    /// Every service declaration enabled by `features`, with `on` blocks resolved.
    #[must_use]
    pub fn services_with(&self, features: &FeatureSet) -> Vec<ServiceDecl<'_>> {
        let mut services = Vec::new();
        each_enabled(&self.items.items, features, &mut |item| {
            if let TopItem::Image(image) = item {
                each_enabled(&image.body.items, features, &mut |item| {
                    if let ImageItem::Service(service) = item {
                        services.push(service_decl(service, &image.id, features));
                    }
                });
            }
        });
        services
    }

    /// Every service declaration, with `on` blocks flattened under default features.
    #[must_use]
    pub fn services(&self) -> Vec<ServiceDecl<'_>> {
        self.services_with(&self.default_features())
    }

    /// Every backing resource enabled by `features`, with `on` blocks resolved.
    #[must_use]
    pub fn resources_with(&self, features: &FeatureSet) -> Vec<ResourceDecl<'_>> {
        let mut resources = Vec::new();
        each_enabled(&self.items.items, features, &mut |item| match item {
            TopItem::Postgres(postgres) => resources.push(ResourceDecl {
                id: &postgres.id,
                kind: ResourceDeclKind::Postgres,
                version: postgres_version(&postgres.body.items, features),
                cdn: None,
                databases: postgres_databases(&postgres.body.items, features),
            }),
            TopItem::Redis(redis) => resources.push(ResourceDecl {
                id: &redis.id,
                kind: ResourceDeclKind::Redis,
                version: redis_version(&redis.body.items, features),
                cdn: None,
                databases: Vec::new(),
            }),
            TopItem::S3(s3) => {
                let mut cdns: Vec<&str> = Vec::new();
                s3_cdns(&s3.body.items, features, &mut cdns);
                resources.push(ResourceDecl {
                    id: &s3.bucket,
                    kind: ResourceDeclKind::S3,
                    version: None,
                    cdn: cdns.first().copied(),
                    databases: Vec::new(),
                });
            }
            _ => {}
        });
        resources
    }

    /// Every backing resource, with `on` blocks flattened under default features.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceDecl<'_>> {
        self.resources_with(&self.default_features())
    }

    /// Every authority enabled by `features`: `domain <id>`, `s3 <bucket>`, and `cdn <id>`.
    #[must_use]
    pub fn domains_with(&self, features: &FeatureSet) -> Vec<&str> {
        let mut domains: Vec<&str> = Vec::new();
        each_enabled(&self.items.items, features, &mut |item| match item {
            TopItem::Domain(domain) => domains.push(&domain.id),
            TopItem::S3(s3) => {
                domains.push(&s3.bucket);
                s3_domains(&s3.body.items, features, &mut domains);
            }
            _ => {}
        });
        domains
    }

    /// Every declared authority under default features.
    #[must_use]
    pub fn domains(&self) -> Vec<&str> {
        self.domains_with(&self.default_features())
    }

    /// Every runtime variable per component enabled by `features`.
    ///
    /// A component is a top-level `env <group>` (keyed by group id), an image, or a service that
    /// inherits its image's inline `env { … }` blocks and `use env <group>` groups. Nested `on`
    /// blocks are resolved at every level.
    #[must_use]
    pub fn env_bindings_with(
        &self,
        features: &FeatureSet,
    ) -> BTreeMap<String, BTreeMap<String, EnvBinding<'_>>> {
        let mut groups: BTreeMap<String, BTreeMap<String, EnvBinding<'_>>> = BTreeMap::new();
        each_enabled(&self.items.items, features, &mut |item| {
            if let TopItem::Env(env) = item {
                let entry = groups.entry(env.group.clone()).or_default();
                env_items(&env.entries.items, features, entry);
            }
        });
        let mut components = groups.clone();
        each_enabled(&self.items.items, features, &mut |item| {
            if let TopItem::Image(image) = item {
                let mut bindings = BTreeMap::new();
                image_env(&image.body.items, features, &groups, &mut bindings);
                components.insert(image.id.clone(), bindings.clone());
                let mut services = Vec::new();
                each_enabled(&image.body.items, features, &mut |item| {
                    if let ImageItem::Service(service) = item {
                        services.push(service.id.clone());
                    }
                });
                for id in services {
                    components.insert(id, bindings.clone());
                }
            }
        });
        components
    }

    /// Every runtime variable per component under default features.
    #[must_use]
    pub fn env_bindings(&self) -> BTreeMap<String, BTreeMap<String, EnvBinding<'_>>> {
        self.env_bindings_with(&self.default_features())
    }
}

fn service_decl<'a>(
    service: &'a Service,
    image: &'a str,
    features: &FeatureSet,
) -> ServiceDecl<'a> {
    let mut port = None;
    let mut routes = Vec::new();
    each_enabled(&service.body.items, features, &mut |item| match item {
        ServiceItem::Port(value) => port = Some(*value),
        ServiceItem::Route(route) => routes.push(route),
        _ => {}
    });
    ServiceDecl {
        id: &service.id,
        port,
        routes,
        image: Some(image),
    }
}

fn postgres_version<'a>(items: &'a [PostgresItem], features: &FeatureSet) -> Option<&'a str> {
    let mut version = None;
    each_enabled(items, features, &mut |item| {
        if let PostgresItem::Version(found) = item {
            version = Some(found.as_str());
        }
    });
    version
}

fn postgres_databases<'a>(items: &'a [PostgresItem], features: &FeatureSet) -> Vec<&'a str> {
    let mut databases = Vec::new();
    each_enabled(items, features, &mut |item| {
        if let PostgresItem::Database(database) = item {
            databases.push(database.id.as_str());
        }
    });
    databases
}

fn redis_version<'a>(items: &'a [RedisItem], features: &FeatureSet) -> Option<&'a str> {
    let mut version = None;
    each_enabled(items, features, &mut |item| {
        if let RedisItem::Version(found) = item {
            version = Some(found.as_str());
        }
    });
    version
}

fn s3_domains<'a>(items: &'a [S3Item], features: &FeatureSet, domains: &mut Vec<&'a str>) {
    each_enabled(items, features, &mut |item| {
        if let S3Item::Cdn(cdn) = item {
            domains.push(&cdn.id);
        }
    });
}

fn s3_cdns<'a>(items: &'a [S3Item], features: &FeatureSet, cdns: &mut Vec<&'a str>) {
    each_enabled(items, features, &mut |item| {
        if let S3Item::Cdn(cdn) = item {
            cdns.push(&cdn.id);
        }
    });
}

fn image_env<'a>(
    items: &'a [ImageItem],
    features: &FeatureSet,
    groups: &BTreeMap<String, BTreeMap<String, EnvBinding<'a>>>,
    bindings: &mut BTreeMap<String, EnvBinding<'a>>,
) {
    each_enabled(items, features, &mut |item| match item {
        ImageItem::Env(block) => env_items(&block.entries.items, features, bindings),
        ImageItem::UseEnv(group) => {
            if let Some(inherited) = groups.get(group) {
                bindings.extend(inherited.clone());
            }
        }
        _ => {}
    });
}

fn env_items<'a>(
    items: &'a [EnvItem],
    features: &FeatureSet,
    bindings: &mut BTreeMap<String, EnvBinding<'a>>,
) {
    each_enabled(items, features, &mut |item| {
        if let EnvItem::Entry(entry) = item {
            let (secret, secret_generator) = secret_ref(&entry.value);
            bindings.insert(
                entry.key.clone(),
                EnvBinding {
                    secret,
                    secret_generator,
                    var_generator: var_generator(&entry.value),
                    literal: literal(&entry.value),
                },
            );
        }
    });
}

/// The first `secret(...)` reachable through fallbacks, concatenation, or interpolation.
fn secret_ref(expr: &Expr) -> (Option<&str>, Option<crate::expr::GeneratorSpec>) {
    match &expr.kind {
        ExprKind::Secret(key) => (Some(key), None),
        ExprKind::GeneratedSecret { key, generator } => (Some(key), Some(*generator)),
        ExprKind::Fallback(left, right) | ExprKind::Concat(left, right) => {
            let first = secret_ref(left);
            if first.0.is_some() {
                first
            } else {
                secret_ref(right)
            }
        }
        ExprKind::Interpolated(parts) => parts
            .iter()
            .find_map(|part| match part {
                crate::expr::InterpPart::Expr(inner) => {
                    let found = secret_ref(inner);
                    found.0.map(|_| found)
                }
                crate::expr::InterpPart::Text(_) => None,
            })
            .unwrap_or((None, None)),
        _ => (None, None),
    }
}

/// The first `var(KEY, <generator>)` generator in the expression, when any.
fn var_generator(expr: &Expr) -> Option<crate::expr::GeneratorSpec> {
    match &expr.kind {
        ExprKind::GeneratedVar { generator, .. } => Some(*generator),
        ExprKind::Fallback(left, right) | ExprKind::Concat(left, right) => {
            var_generator(left).or_else(|| var_generator(right))
        }
        ExprKind::Interpolated(parts) => parts.iter().find_map(|part| match part {
            crate::expr::InterpPart::Expr(inner) => var_generator(inner),
            crate::expr::InterpPart::Text(_) => None,
        }),
        _ => None,
    }
}

/// The static literal a declaration would resolve to without the vault.
fn literal(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::String(text) => Some(text.clone()),
        ExprKind::Bool(value) => Some(value.to_string()),
        ExprKind::Int(value) => Some(value.to_string()),
        ExprKind::Fallback(left, right) => literal(left).or_else(|| literal(right)),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
