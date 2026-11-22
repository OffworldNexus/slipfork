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
    /// The first `secret(KEY)` reachable through fallbacks or concatenation.
    pub secret: Option<&'a str>,
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
    match &on.body {
        OnBody::Inline(inner) => top(inner.as_ref(), services, resources, domains),
        OnBody::Block(block) => {
            for item in &block.items {
                top(item, services, resources, domains);
            }
        }
    }
}

fn image_item<'a>(item: &'a ImageItem, image: &'a str, services: &mut Vec<ServiceDecl<'a>>) {
    match item {
        ImageItem::Service(service) => services.push(service_decl(service, image)),
        ImageItem::On(on) => match &on.body {
            OnBody::Inline(inner) => image_item(inner.as_ref(), image, services),
            OnBody::Block(block) => {
                for item in &block.items {
                    image_item(item, image, services);
                }
            }
        },
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
    match &on.body {
        OnBody::Inline(inner) => service_items(std::slice::from_ref(inner.as_ref()), port, routes),
        OnBody::Block(block) => service_items(&block.items, port, routes),
    }
}

fn postgres_version(items: &[PostgresItem]) -> Option<&str> {
    items.iter().find_map(|item| match item {
        PostgresItem::Version(version) => Some(version.as_str()),
        PostgresItem::On(on) => match &on.body {
            OnBody::Inline(inner) => postgres_version(std::slice::from_ref(inner.as_ref())),
            OnBody::Block(block) => postgres_version(&block.items),
        },
        PostgresItem::Database(_) => None,
    })
}

fn postgres_databases(items: &[PostgresItem]) -> Vec<&str> {
    let mut databases = Vec::new();
    for item in items {
        match item {
            PostgresItem::Database(database) => databases.push(database.id.as_str()),
            PostgresItem::On(on) => match &on.body {
                OnBody::Inline(inner) => {
                    databases.extend(postgres_databases(std::slice::from_ref(inner.as_ref())));
                }
                OnBody::Block(block) => databases.extend(postgres_databases(&block.items)),
            },
            PostgresItem::Version(_) => {}
        }
    }
    databases
}

fn redis_version(items: &[RedisItem]) -> Option<&str> {
    items.iter().find_map(|item| match item {
        RedisItem::Version(version) => Some(version.as_str()),
        RedisItem::On(on) => match &on.body {
            OnBody::Inline(inner) => redis_version(std::slice::from_ref(inner.as_ref())),
            OnBody::Block(block) => redis_version(&block.items),
        },
    })
}

fn s3_cdns<'a>(items: &'a [S3Item], domains: &mut Vec<&'a str>, cdns: &mut Vec<&'a str>) {
    for item in items {
        match item {
            S3Item::Cdn(cdn) => {
                domains.push(&cdn.id);
                cdns.push(&cdn.id);
            }
            S3Item::On(on) => match &on.body {
                OnBody::Inline(inner) => {
                    s3_cdns(std::slice::from_ref(inner.as_ref()), domains, cdns);
                }
                OnBody::Block(block) => s3_cdns(&block.items, domains, cdns),
            },
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
        TopItem::On(on) => match &on.body {
            OnBody::Inline(inner) => env_groups(inner, groups),
            OnBody::Block(block) => {
                for item in &block.items {
                    env_groups(item, groups);
                }
            }
        },
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
        TopItem::On(on) => match &on.body {
            OnBody::Inline(inner) => env_images(inner, groups, components),
            OnBody::Block(block) => {
                for item in &block.items {
                    env_images(item, groups, components);
                }
            }
        },
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
            ImageItem::On(on) => match &on.body {
                OnBody::Inline(inner) => {
                    image_env(std::slice::from_ref(inner.as_ref()), groups, bindings);
                }
                OnBody::Block(block) => image_env(&block.items, groups, bindings),
            },
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
            ImageItem::On(on) => match &on.body {
                OnBody::Inline(inner) => {
                    image_services(std::slice::from_ref(inner.as_ref()), bindings, components);
                }
                OnBody::Block(block) => image_services(&block.items, bindings, components),
            },
            _ => {}
        }
    }
}

fn env_items<'a>(items: &'a [EnvItem], bindings: &mut BTreeMap<String, EnvBinding<'a>>) {
    for item in items {
        match item {
            EnvItem::Entry(entry) => {
                bindings.insert(
                    entry.key.clone(),
                    EnvBinding {
                        secret: secret_key(&entry.value),
                        literal: literal(&entry.value),
                    },
                );
            }
            EnvItem::On(on) => match &on.body {
                OnBody::Inline(inner) => {
                    env_items(std::slice::from_ref(inner.as_ref()), bindings);
                }
                OnBody::Block(block) => env_items(&block.items, bindings),
            },
        }
    }
}

/// The first `secret(...)` reachable through fallbacks or concatenation.
fn secret_key(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Secret(key) => Some(key),
        ExprKind::Fallback(left, right) => secret_key(left).or_else(|| secret_key(right)),
        ExprKind::Concat(left, right) => secret_key(left).or_else(|| secret_key(right)),
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
mod tests {
    const SOURCE: &str = r#"# forkfile: 0.1

env base {
  LOG_LEVEL = "info"
  SECRET_KEY = secret(SECRET_KEY)
  TURN = 2
}

image app . {
  use env base
  env {
    EXTRA = true
  }
  service api {
    route /
  }
  on dev {
    service worker {
      run: serve
    }
  }
}
"#;

    #[test]
    fn env_bindings_flatten_groups_images_services_and_on_blocks() {
        let document = slipfork_forkfile_parse(SOURCE);
        let bindings = document.env_bindings();

        assert!(bindings["base"]["SECRET_KEY"].is_secret());
        assert_eq!(
            bindings["base"]["LOG_LEVEL"].literal.as_deref(),
            Some("info")
        );
        assert_eq!(bindings["base"]["TURN"].literal.as_deref(), Some("2"));

        // The image and both of its services inherit the group plus the inline block.
        for component in ["app", "api", "worker"] {
            assert_eq!(
                bindings[component]["LOG_LEVEL"].literal.as_deref(),
                Some("info"),
                "{component} must inherit `use env base`"
            );
            assert_eq!(
                bindings[component]["TURN"].literal.as_deref(),
                Some("2"),
                "{component}"
            );
            assert_eq!(
                bindings[component]["EXTRA"].literal.as_deref(),
                Some("true"),
                "{component} must inherit the inline env block"
            );
            assert!(bindings[component]["SECRET_KEY"].is_secret(), "{component}");
        }
    }

    fn slipfork_forkfile_parse(source: &str) -> crate::ast::Forkfile {
        crate::parse(source).expect("the visitor fixture must be a valid Forkfile")
    }

    #[test]
    fn services_name_their_enclosing_image_and_postgres_owns_its_databases() {
        let document = slipfork_forkfile_parse(
            "# forkfile: 0.1\n\
             postgres pg { database app-db }\n\
             image app . {\n\
             \x20 service api { port 8000 }\n\
             }\n",
        );
        let services = document.services();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].id, "api");
        assert_eq!(services[0].image, Some("app"));
        let resources = document.resources();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].id, "pg");
        assert_eq!(resources[0].databases, vec!["app-db"]);
    }
}
