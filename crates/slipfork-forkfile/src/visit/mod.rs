//! Read-only `on`-aware visitors: services with resolved ports/routes, resources, authorities,
//! and every runtime variable a component declares.
//!
//! Every consumer that needs the declared surface walks these visitors instead of re-implementing
//! the `on <expr> { ... }` recursion. The walk is AST-only and never reads source text.

use std::collections::BTreeMap;

use crate::ast::{
    EnvItem, Forkfile, ImageItem, On, OnBody, PostgresItem, RedisItem, Route, S3Item, Service,
    ServiceItem, TopItem,
};
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
    /// Every service declaration, with `on` blocks flattened.
    #[must_use]
    pub fn services(&self) -> Vec<ServiceDecl<'_>> {
        let mut services = Vec::new();
        let mut resources = Vec::new();
        let mut domains = Vec::new();
        for item in &self.items.items {
            top(item, &mut services, &mut resources, &mut domains);
        }
        services
    }

    /// Every backing resource, with `on` blocks flattened.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceDecl<'_>> {
        let mut services = Vec::new();
        let mut resources = Vec::new();
        let mut domains = Vec::new();
        for item in &self.items.items {
            top(item, &mut services, &mut resources, &mut domains);
        }
        resources
    }

    /// Every declared authority: `domain <id>`, `s3 <bucket>`, and `cdn <id>`.
    #[must_use]
    pub fn domains(&self) -> Vec<&str> {
        let mut services = Vec::new();
        let mut resources = Vec::new();
        let mut domains = Vec::new();
        for item in &self.items.items {
            top(item, &mut services, &mut resources, &mut domains);
        }
        domains
    }

    /// Every runtime variable per component.
    ///
    /// A component is a top-level `env <group>` (keyed by group id), an image, or a service that
    /// inherits its image's inline `env { … }` blocks and `use env <group>` groups. Nested `on`
    /// blocks are flattened at every level.
    #[must_use]
    pub fn env_bindings(&self) -> BTreeMap<String, BTreeMap<String, EnvBinding<'_>>> {
        let mut groups: BTreeMap<String, BTreeMap<String, EnvBinding<'_>>> = BTreeMap::new();
        for item in &self.items.items {
            env_groups(item, &mut groups);
        }
        let mut components = groups.clone();
        for item in &self.items.items {
            env_images(item, &groups, &mut components);
        }
        components
    }
}

fn top<'a>(
    item: &'a TopItem,
    services: &mut Vec<ServiceDecl<'a>>,
    resources: &mut Vec<ResourceDecl<'a>>,
    domains: &mut Vec<&'a str>,
) {
    match item {
        TopItem::Domain(domain) => domains.push(&domain.id),
        TopItem::Postgres(postgres) => resources.push(ResourceDecl {
            id: &postgres.id,
            kind: ResourceDeclKind::Postgres,
            version: postgres_version(&postgres.body.items),
            cdn: None,
            databases: postgres_databases(&postgres.body.items),
        }),
        TopItem::Redis(redis) => resources.push(ResourceDecl {
            id: &redis.id,
            kind: ResourceDeclKind::Redis,
            version: redis_version(&redis.body.items),
            cdn: None,
            databases: Vec::new(),
        }),
        TopItem::S3(s3) => {
            domains.push(&s3.bucket);
            let mut cdns = Vec::new();
            s3_cdns(&s3.body.items, domains, &mut cdns);
            resources.push(ResourceDecl {
                id: &s3.bucket,
                kind: ResourceDeclKind::S3,
                version: None,
                cdn: cdns.first().copied(),
                databases: Vec::new(),
            });
        }
        TopItem::Image(image) => {
            for item in &image.body.items {
                image_item(item, &image.id, services);
            }
        }
        TopItem::On(on) => top_on(on, services, resources, domains),
        _ => {}
    }
}

fn top_on<'a>(
    on: &'a On<TopItem>,
    services: &mut Vec<ServiceDecl<'a>>,
    resources: &mut Vec<ResourceDecl<'a>>,
    domains: &mut Vec<&'a str>,
) {
    on.for_each_branch(&mut |body| match body {
        OnBody::Inline(inner) => top(inner.as_ref(), services, resources, domains),
        OnBody::Block(block) => {
            for item in &block.items {
                top(item, services, resources, domains);
            }
        }
    });
}

fn image_item<'a>(item: &'a ImageItem, image: &'a str, services: &mut Vec<ServiceDecl<'a>>) {
    match item {
        ImageItem::Service(service) => services.push(service_decl(service, image)),
        ImageItem::On(on) => on.for_each_branch(&mut |body| match body {
            OnBody::Inline(inner) => image_item(inner.as_ref(), image, services),
            OnBody::Block(block) => {
                for item in &block.items {
                    image_item(item, image, services);
                }
            }
        }),
        _ => {}
    }
}

fn service_decl<'a>(service: &'a Service, image: &'a str) -> ServiceDecl<'a> {
    let mut port = None;
    let mut routes = Vec::new();
    service_items(&service.body.items, &mut port, &mut routes);
    ServiceDecl {
        id: &service.id,
        port,
        routes,
        image: Some(image),
    }
}

fn service_items<'a>(
    items: &'a [ServiceItem],
    port: &mut Option<u16>,
    routes: &mut Vec<&'a Route>,
) {
    for item in items {
        match item {
            ServiceItem::Port(value) => *port = Some(*value),
            ServiceItem::Route(route) => routes.push(route),
            ServiceItem::On(on) => service_on(on, port, routes),
            _ => {}
        }
    }
}

fn service_on<'a>(on: &'a On<ServiceItem>, port: &mut Option<u16>, routes: &mut Vec<&'a Route>) {
    on.for_each_branch(&mut |body| match body {
        OnBody::Inline(inner) => service_items(std::slice::from_ref(inner.as_ref()), port, routes),
        OnBody::Block(block) => service_items(&block.items, port, routes),
    });
}

fn postgres_version(items: &[PostgresItem]) -> Option<&str> {
    for item in items {
        match item {
            PostgresItem::Version(version) => return Some(version.as_str()),
            PostgresItem::On(on) => {
                let mut found = None;
                on.for_each_branch(&mut |body| {
                    if found.is_none() {
                        found = match body {
                            OnBody::Inline(inner) => {
                                postgres_version(std::slice::from_ref(inner.as_ref()))
                            }
                            OnBody::Block(block) => postgres_version(&block.items),
                        };
                    }
                });
                if found.is_some() {
                    return found;
                }
            }
            PostgresItem::Database(_) => {}
        }
    }
    None
}

fn postgres_databases(items: &[PostgresItem]) -> Vec<&str> {
    let mut databases = Vec::new();
    for item in items {
        match item {
            PostgresItem::Database(database) => databases.push(database.id.as_str()),
            PostgresItem::On(on) => on.for_each_branch(&mut |body| match body {
                OnBody::Inline(inner) => {
                    databases.extend(postgres_databases(std::slice::from_ref(inner.as_ref())));
                }
                OnBody::Block(block) => databases.extend(postgres_databases(&block.items)),
            }),
            PostgresItem::Version(_) => {}
        }
    }
    databases
}

fn redis_version(items: &[RedisItem]) -> Option<&str> {
    for item in items {
        match item {
            RedisItem::Version(version) => return Some(version.as_str()),
            RedisItem::On(on) => {
                let mut found = None;
                on.for_each_branch(&mut |body| {
                    if found.is_none() {
                        found = match body {
                            OnBody::Inline(inner) => {
                                redis_version(std::slice::from_ref(inner.as_ref()))
                            }
                            OnBody::Block(block) => redis_version(&block.items),
                        };
                    }
                });
                if found.is_some() {
                    return found;
                }
            }
        }
    }
    None
}

fn s3_cdns<'a>(items: &'a [S3Item], domains: &mut Vec<&'a str>, cdns: &mut Vec<&'a str>) {
    for item in items {
        match item {
            S3Item::Cdn(cdn) => {
                domains.push(&cdn.id);
                cdns.push(&cdn.id);
            }
            S3Item::On(on) => on.for_each_branch(&mut |body| match body {
                OnBody::Inline(inner) => {
                    s3_cdns(std::slice::from_ref(inner.as_ref()), domains, cdns);
                }
                OnBody::Block(block) => s3_cdns(&block.items, domains, cdns),
            }),
            S3Item::Policy(_) => {}
        }
    }
}

fn env_groups<'a>(
    item: &'a TopItem,
    groups: &mut BTreeMap<String, BTreeMap<String, EnvBinding<'a>>>,
) {
    match item {
        TopItem::Env(env) => {
            let entry = groups.entry(env.group.clone()).or_default();
            env_items(&env.entries.items, entry);
        }
        TopItem::On(on) => on.for_each_branch(&mut |body| match body {
            OnBody::Inline(inner) => env_groups(inner, groups),
            OnBody::Block(block) => {
                for item in &block.items {
                    env_groups(item, groups);
                }
            }
        }),
        _ => {}
    }
}

fn env_images<'a>(
    item: &'a TopItem,
    groups: &BTreeMap<String, BTreeMap<String, EnvBinding<'a>>>,
    components: &mut BTreeMap<String, BTreeMap<String, EnvBinding<'a>>>,
) {
    match item {
        TopItem::Image(image) => {
            let mut bindings = BTreeMap::new();
            image_env(&image.body.items, groups, &mut bindings);
            components.insert(image.id.clone(), bindings.clone());
            image_services(&image.body.items, &bindings, components);
        }
        TopItem::On(on) => on.for_each_branch(&mut |body| match body {
            OnBody::Inline(inner) => env_images(inner, groups, components),
            OnBody::Block(block) => {
                for item in &block.items {
                    env_images(item, groups, components);
                }
            }
        }),
        _ => {}
    }
}

fn image_env<'a>(
    items: &'a [ImageItem],
    groups: &BTreeMap<String, BTreeMap<String, EnvBinding<'a>>>,
    bindings: &mut BTreeMap<String, EnvBinding<'a>>,
) {
    for item in items {
        match item {
            ImageItem::Env(block) => env_items(&block.entries.items, bindings),
            ImageItem::UseEnv(group) => {
                if let Some(inherited) = groups.get(group) {
                    bindings.extend(inherited.clone());
                }
            }
            ImageItem::On(on) => on.for_each_branch(&mut |body| match body {
                OnBody::Inline(inner) => {
                    image_env(std::slice::from_ref(inner.as_ref()), groups, bindings);
                }
                OnBody::Block(block) => image_env(&block.items, groups, bindings),
            }),
            _ => {}
        }
    }
}

fn image_services<'a>(
    items: &'a [ImageItem],
    bindings: &BTreeMap<String, EnvBinding<'a>>,
    components: &mut BTreeMap<String, BTreeMap<String, EnvBinding<'a>>>,
) {
    for item in items {
        match item {
            ImageItem::Service(service) => {
                components.insert(service.id.clone(), bindings.clone());
            }
            ImageItem::On(on) => on.for_each_branch(&mut |body| match body {
                OnBody::Inline(inner) => {
                    image_services(std::slice::from_ref(inner.as_ref()), bindings, components);
                }
                OnBody::Block(block) => image_services(&block.items, bindings, components),
            }),
            _ => {}
        }
    }
}

fn env_items<'a>(items: &'a [EnvItem], bindings: &mut BTreeMap<String, EnvBinding<'a>>) {
    for item in items {
        match item {
            EnvItem::Entry(entry) => {
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
            EnvItem::On(on) => on.for_each_branch(&mut |body| match body {
                OnBody::Inline(inner) => {
                    env_items(std::slice::from_ref(inner.as_ref()), bindings);
                }
                OnBody::Block(block) => env_items(&block.items, bindings),
            }),
        }
    }
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
