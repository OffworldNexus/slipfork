//! The evaluated-`Forkfile` projection behind `ps`, built on the canonical document visitors.
//!
//! Ports and default resource versions come from `slipfork-world`'s [`ResourceKind`], so the status
//! rows cannot drift from the pods that actually boot. No `on`-block recursion lives here.

use slipfork_forkfile::ResourceDeclKind;
use slipfork_forkfile::ast::{DomainFilter, Forkfile, RouteKind};
use slipfork_world::ResourceKind;

use crate::ps::{PsDomain, PsKind, PsRoute, PsRouteMode, PsService};

/// Projects every declared service and backing resource into typed rows.
pub(super) fn services(doc: &Forkfile) -> Vec<PsService> {
    let mut out: Vec<PsService> = doc
        .services()
        .into_iter()
        .map(|service| PsService {
            id: service.id.to_owned(),
            kind: PsKind::Service,
            running: false,
            port: service.port,
            version: None,
            cdn: None,
            pid: None,
            routes: service
                .routes
                .iter()
                .filter_map(|route| {
                    let port = service.port?;
                    Some(PsRoute {
                        prefix: route.path.clone().unwrap_or_else(|| "/".to_owned()),
                        port,
                        mode: mode(&route.kind),
                        domain: route.domain.as_ref().map(domain),
                    })
                })
                .collect(),
        })
        .collect();
    for resource in doc.resources() {
        let (kind, backend) = match resource.kind {
            ResourceDeclKind::Postgres => (PsKind::Postgres, ResourceKind::Postgres),
            ResourceDeclKind::Redis => (PsKind::Redis, ResourceKind::Redis),
            ResourceDeclKind::S3 => (PsKind::S3, ResourceKind::Seaweedfs),
        };
        out.push(PsService {
            id: resource.id.to_owned(),
            kind,
            running: false,
            port: Some(backend.port()),
            version: resource
                .version
                .map(str::to_owned)
                .or_else(|| Some(resource_version(backend))),
            cdn: resource.cdn.map(str::to_owned),
            pid: None,
            routes: Vec::new(),
        });
    }
    out
}

fn mode(kind: &RouteKind) -> PsRouteMode {
    match kind {
        RouteKind::Preserve => PsRouteMode::Preserve,
        RouteKind::StripPrefix => PsRouteMode::StripPrefix,
        RouteKind::Rewrite(_) => PsRouteMode::Rewrite,
        RouteKind::Exact(_) => PsRouteMode::Exact,
    }
}

fn domain(filter: &DomainFilter) -> PsDomain {
    match filter {
        DomainFilter::Domain { name, .. } => PsDomain::Domain(name.clone()),
        DomainFilter::Prefix(prefix) => PsDomain::Prefix(prefix.clone()),
    }
}

fn resource_version(kind: ResourceKind) -> String {
    kind.image().rsplit_once(':').map_or_else(
        || kind.image().to_owned(),
        |(_, version)| version.to_owned(),
    )
}
