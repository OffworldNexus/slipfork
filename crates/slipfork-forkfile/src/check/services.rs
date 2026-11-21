//! Service port, route, and domain semantics across every `on` branch.

use std::collections::BTreeSet;

use super::{SymbolKind, SymbolTable};
use crate::ast::{Block, DomainFilter, OnBody, Route, RouteKind, Service, ServiceItem};
use crate::diag::{Diagnostic, Diagnostics};
use crate::expr::{Expr, ExprKind};

const MAX_ENUMERATED_FEATURES: usize = 12;

pub(super) fn check_service(service: &Service, symbols: &SymbolTable, diags: &mut Diagnostics) {
    let mut names = BTreeSet::new();
    collect_feature_names(&service.body, &mut names);
    let names: Vec<String> = names.into_iter().collect();
    if names.len() > MAX_ENUMERATED_FEATURES {
        let active: BTreeSet<String> = names.iter().map(|name| (*name).to_string()).collect();
        check_branch(service, &active, symbols, diags);
        return;
    }
    let total = 1u32 << names.len();
    for mask in 0..total {
        let mut active = BTreeSet::new();
        for (index, name) in names.iter().enumerate() {
            if mask & (1u32 << index) != 0 {
                active.insert((*name).to_string());
            }
        }
        check_branch(service, &active, symbols, diags);
    }
}

fn check_branch(
    service: &Service,
    active: &BTreeSet<String>,
    symbols: &SymbolTable,
    diags: &mut Diagnostics,
) {
    let mut items = Vec::new();
    effective_block(&service.body, active, false, &mut items);

    let mut ports: Vec<(u16, bool)> = Vec::new();
    let mut internal: Vec<u16> = Vec::new();
    let mut routes: Vec<&Route> = Vec::new();
    for (item, guarded) in &items {
        match item {
            ServiceItem::Port(port) => ports.push((*port, *guarded)),
            ServiceItem::InternalPorts(list) => internal.extend(list.iter().copied()),
            ServiceItem::Route(route) => routes.push(route),
            _ => {}
        }
    }

    if ports.iter().filter(|(_, guarded)| !guarded).count() > 1 {
        diags.push(Diagnostic::new(
            service.span,
            "service declares more than one public `port`",
            "keep one unconditional `port` and override it only inside `on`",
        ));
    }
    let public = ports.last().map(|(port, _)| *port);

    let mut internal_seen = BTreeSet::new();
    for port in &internal {
        if !internal_seen.insert(*port) {
            diags.push(Diagnostic::new(
                service.span,
                format!("duplicate `internal_ports` value {port}"),
                "list each internal port once",
            ));
        }
    }
    if let Some(public) = public
        && internal_seen.contains(&public)
    {
        diags.push(Diagnostic::new(
            service.span,
            format!("`internal_ports` must not repeat the public port {public}"),
            "pick a distinct private port",
        ));
    }

    if !routes.is_empty() && public.is_none() {
        diags.push(Diagnostic::new(
            service.span,
            format!(
                "service `{}` exposes an HTTP route without a public `port`",
                service.id
            ),
            "add `port <u16>` when the service is routed",
        ));
    }

    let mut seen_routes = BTreeSet::new();
    for route in &routes {
        validate_route(route, symbols, diags);
        let key = format!("{:?}|{:?}", route.path, route.domain);
        if !seen_routes.insert(key) {
            diags.push(Diagnostic::new(
                route.span,
                "duplicate `route` for this path and domain",
                "give each route a distinct path or `domain` filter",
            ));
        }
    }
}

fn validate_route(route: &Route, symbols: &SymbolTable, diags: &mut Diagnostics) {
    if let Some(path) = &route.path
        && !path.starts_with('/')
    {
        diags.push(Diagnostic::new(
            route.span,
            format!("route path `{path}` must start with `/`"),
            "write `route /api`",
        ));
    }
    let target = match &route.kind {
        RouteKind::Preserve | RouteKind::StripPrefix => None,
        RouteKind::Rewrite(target) => Some(("rewrite", target)),
        RouteKind::Exact(target) => Some(("exact", target)),
    };
    if let Some((keyword, target)) = target
        && !target.starts_with('/')
    {
        diags.push(Diagnostic::new(
            route.span,
            format!("`{keyword}` target `{target}` must start with `/`"),
            format!("write `route /api {keyword} /v1`"),
        ));
    }
    if let Some(DomainFilter::Domain(id)) = &route.domain
        && !symbols
            .get(id)
            .is_some_and(|symbol| symbol.kind == SymbolKind::Domain)
    {
        diags.push(Diagnostic::new(
            route.span,
            format!("route domain `{id}` is not a declared `domain`"),
            format!("add `domain {id}` or filter with `domain {id}.*`"),
        ));
    }
}

fn collect_feature_names(block: &Block<ServiceItem>, out: &mut BTreeSet<String>) {
    for item in &block.items {
        if let ServiceItem::On(on) = item {
            collect_condition(&on.cond, out);
            match &on.body {
                OnBody::Inline(inner) => collect_item_features(inner, out),
                OnBody::Block(nested) => collect_feature_names(nested, out),
            }
        }
    }
}

fn collect_item_features(item: &ServiceItem, out: &mut BTreeSet<String>) {
    if let ServiceItem::On(on) = item {
        collect_condition(&on.cond, out);
        match &on.body {
            OnBody::Inline(inner) => collect_item_features(inner, out),
            OnBody::Block(block) => collect_feature_names(block, out),
        }
    }
}

fn collect_condition(expr: &Expr, out: &mut BTreeSet<String>) {
    match &expr.kind {
        ExprKind::Feature(name) => {
            out.insert(name.clone());
        }
        ExprKind::Not(inner) => collect_condition(inner, out),
        ExprKind::And(left, right) | ExprKind::Or(left, right) => {
            collect_condition(left, out);
            collect_condition(right, out);
        }
        _ => {}
    }
}

fn effective_block<'a>(
    block: &'a Block<ServiceItem>,
    active: &BTreeSet<String>,
    guarded: bool,
    out: &mut Vec<(&'a ServiceItem, bool)>,
) {
    for item in &block.items {
        match item {
            ServiceItem::On(on) => {
                if eval(&on.cond, active) {
                    match &on.body {
                        OnBody::Inline(inner) => effective_item(inner, active, out),
                        OnBody::Block(nested) => effective_block(nested, active, true, out),
                    }
                }
            }
            other => out.push((other, guarded)),
        }
    }
}

fn effective_item<'a>(
    item: &'a ServiceItem,
    active: &BTreeSet<String>,
    out: &mut Vec<(&'a ServiceItem, bool)>,
) {
    match item {
        ServiceItem::On(on) => {
            if eval(&on.cond, active) {
                match &on.body {
                    OnBody::Inline(inner) => effective_item(inner, active, out),
                    OnBody::Block(block) => effective_block(block, active, true, out),
                }
            }
        }
        other => out.push((other, true)),
    }
}

fn eval(expr: &Expr, active: &BTreeSet<String>) -> bool {
    match &expr.kind {
        ExprKind::Bool(value) => *value,
        ExprKind::Feature(name) => active.contains(name),
        ExprKind::Not(inner) => !eval(inner, active),
        ExprKind::And(left, right) => eval(left, active) && eval(right, active),
        ExprKind::Or(left, right) => eval(left, active) || eval(right, active),
        ExprKind::Fallback(left, _) => eval(left, active),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::check;
    use crate::parse;

    fn diagnostics(source: &str) -> Diagnostics {
        check(&parse(source).expect("parses")).expect_err("expected diagnostics")
    }

    #[test]
    fn internal_ports_must_be_distinct_and_private() {
        let source = "# forkfile: 0.1\n\nimage backend . {\n  service api {\n    port 8000\n    internal_ports [8000, 8001, 8001]\n  }\n}\n";
        let diags = diagnostics(source);
        assert!(
            diags
                .items
                .iter()
                .any(|d| d.message.contains("duplicate `internal_ports`"))
        );
        assert!(
            diags
                .items
                .iter()
                .any(|d| d.message.contains("must not repeat the public port"))
        );
    }

    #[test]
    fn routed_service_needs_a_port() {
        let source =
            "# forkfile: 0.1\n\nimage backend . {\n  service api {\n    route /api\n  }\n}\n";
        let diags = diagnostics(source);
        assert!(
            diags
                .items
                .iter()
                .any(|d| d.message.contains("without a public `port`"))
        );
    }

    #[test]
    fn domain_filters_must_reference_declared_domains() {
        let source = "# forkfile: 0.1\n\ndomain main\n\nimage backend . {\n  service api {\n    port 8000\n    route / domain admin\n  }\n}\n";
        let diags = diagnostics(source);
        assert!(
            diags
                .items
                .iter()
                .any(|d| d.message.contains("not a declared `domain`"))
        );
    }

    #[test]
    fn guarded_port_override_is_allowed() {
        let source = "# forkfile: 0.1\n\nfeatures {\n  dev\n}\n\nimage frontend ./front {\n  service front {\n    port 4173\n    route /\n    on dev {\n      port 5173\n      run: serve\n    }\n  }\n}\n";
        check(&parse(source).expect("parses")).expect("guarded override is valid");
    }
}
