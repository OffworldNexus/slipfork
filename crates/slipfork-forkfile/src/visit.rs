//! Read-only `on`-aware visitors: services with resolved ports/routes, resources, authorities.
//!
//! Every consumer that needs the declared surface walks these visitors instead of re-implementing
//! the `on <expr> { ... }` recursion. The walk is AST-only and never reads source text.

use crate::ast::{
    Forkfile, ImageItem, On, OnBody, PostgresItem, RedisItem, Route, S3Item, Service, ServiceItem,
    TopItem,
};

/// One service declaration with the port and routes that apply to it.
#[derive(Debug, Clone)]
pub struct ServiceDecl<'a> {
    /// Service id.
    pub id: &'a str,
    /// Public `port`, when declared.
    pub port: Option<u16>,
    /// Declared routes, in source order.
    pub routes: Vec<&'a Route>,
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
        }),
        TopItem::Redis(redis) => resources.push(ResourceDecl {
            id: &redis.id,
            kind: ResourceDeclKind::Redis,
            version: redis_version(&redis.body.items),
            cdn: None,
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
            });
        }
        TopItem::Image(image) => {
            for item in &image.body.items {
                image_item(item, services);
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

fn image_item<'a>(item: &'a ImageItem, services: &mut Vec<ServiceDecl<'a>>) {
    match item {
        ImageItem::Service(service) => services.push(service_decl(service)),
        ImageItem::On(on) => match &on.body {
            OnBody::Inline(inner) => image_item(inner.as_ref(), services),
            OnBody::Block(block) => {
                for item in &block.items {
                    image_item(item, services);
                }
            }
        },
        _ => {}
    }
}

fn service_decl(service: &Service) -> ServiceDecl<'_> {
    let mut port = None;
    let mut routes = Vec::new();
    service_items(&service.body.items, &mut port, &mut routes);
    ServiceDecl {
        id: &service.id,
        port,
        routes,
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
        }
    }
}
