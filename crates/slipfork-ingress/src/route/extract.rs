//! Canonical `Forkfile` -> ingress-table extraction.
//!
//! Declared authorities and service routes are read from the evaluated AST once; no call site
//! re-parses route syntax or guesses an authority from raw text.

use slipfork_forkfile::Route;
use slipfork_forkfile::ast::{
    Forkfile, ImageItem, On, OnBody, S3Item, Service, ServiceItem, TopItem,
};

use super::error::TableError;
use super::{IngressRoute, IngressTable};

impl IngressTable {
    /// Builds a world's table directly from an evaluated `Forkfile` document.
    ///
    /// Every `domain <id>`, `s3` bucket, and `cdn <id>` becomes a declared authority; each service's
    /// `port` plus `route` items become one [`IngressRoute`]. `on` blocks are walked exactly like the
    /// boot planner walks them.
    ///
    /// # Errors
    ///
    /// Returns [`TableError`] for an ambiguous, duplicate, or undeclared-domain table.
    pub fn from_forkfile(world: impl Into<String>, doc: &Forkfile) -> Result<Self, TableError> {
        let mut declared = Vec::new();
        for item in &doc.items.items {
            domains_of_top(item, &mut declared);
        }
        let mut routes = Vec::new();
        for item in &doc.items.items {
            routes_of_top(item, &mut routes);
        }
        Self::new(world, declared, routes)
    }
}

fn domains_of_top(item: &TopItem, out: &mut Vec<String>) {
    match item {
        TopItem::Domain(domain) => out.push(domain.id.clone()),
        TopItem::S3(s3) => {
            out.push(s3.bucket.clone());
            for item in &s3.body.items {
                match item {
                    S3Item::Cdn(cdn) => out.push(cdn.id.clone()),
                    S3Item::On(on) => domains_of_on(on, out),
                }
            }
        }
        TopItem::On(on) => match &on.body {
            OnBody::Inline(inner) => domains_of_top(inner, out),
            OnBody::Block(block) => {
                for item in &block.items {
                    domains_of_top(item, out);
                }
            }
        },
        _ => {}
    }
}

fn domains_of_on(on: &On<S3Item>, out: &mut Vec<String>) {
    match &on.body {
        OnBody::Inline(inner) => {
            if let S3Item::Cdn(cdn) = inner.as_ref() {
                out.push(cdn.id.clone());
            }
        }
        OnBody::Block(block) => {
            for item in &block.items {
                match item {
                    S3Item::Cdn(cdn) => out.push(cdn.id.clone()),
                    S3Item::On(nested) => domains_of_on(nested, out),
                }
            }
        }
    }
}

fn routes_of_top(item: &TopItem, out: &mut Vec<IngressRoute>) {
    match item {
        TopItem::Image(image) => {
            for item in &image.body.items {
                routes_of_image_item(item, out);
            }
        }
        TopItem::On(on) => match &on.body {
            OnBody::Inline(inner) => routes_of_top(inner, out),
            OnBody::Block(block) => {
                for item in &block.items {
                    routes_of_top(item, out);
                }
            }
        },
        _ => {}
    }
}

fn routes_of_image_item(item: &ImageItem, out: &mut Vec<IngressRoute>) {
    match item {
        ImageItem::Service(service) => service_routes(service, out),
        ImageItem::On(on) => match &on.body {
            OnBody::Inline(inner) => routes_of_image_item(inner, out),
            OnBody::Block(block) => {
                for item in &block.items {
                    routes_of_image_item(item, out);
                }
            }
        },
        _ => {}
    }
}

fn service_routes(service: &Service, out: &mut Vec<IngressRoute>) {
    let mut port = None;
    let mut routes = Vec::new();
    service_block(&service.body.items, &mut port, &mut routes);
    let Some(port) = port else {
        return;
    };
    for route in routes {
        out.push(IngressRoute::new(port, route));
    }
}

fn service_block(items: &[ServiceItem], port: &mut Option<u16>, routes: &mut Vec<Route>) {
    for item in items {
        match item {
            ServiceItem::Port(value) => {
                *port = Some(*value);
            }
            ServiceItem::Route(route) => routes.push(route.clone()),
            ServiceItem::On(on) => service_on(on, port, routes),
            _ => {}
        }
    }
}

fn service_on(on: &On<ServiceItem>, port: &mut Option<u16>, routes: &mut Vec<Route>) {
    match &on.body {
        OnBody::Inline(inner) => service_block(std::slice::from_ref(inner.as_ref()), port, routes),
        OnBody::Block(block) => service_block(&block.items, port, routes),
    }
}
