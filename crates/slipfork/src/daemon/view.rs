//! The evaluated-`Forkfile` projection behind `ps`, built on the canonical document visitors.
//!
//! Ports and default resource versions come from `slipfork-world`'s [`ResourceKind`], so the status
//! rows cannot drift from the pods that actually boot. No `on`-block recursion lives here.

use std::collections::BTreeSet;

use slipfork_forkfile::ast::{DomainFilter, Forkfile, RouteKind, TopItem};
use slipfork_forkfile::{BucketPolicy, FeatureSet, ResourceDeclKind};
use slipfork_world::ResourceKind;

use crate::ps::{PsBucketPolicy, PsKind, PsRoute, PsRouteMode, PsService};

/// The first declared app `domain <id>`; it **is** the primary `<world>.localhost` authority.
pub(super) fn primary_domain(doc: &Forkfile, features: &FeatureSet) -> Option<String> {
    doc.primary_domain_with(features).map(str::to_owned)
}

/// Projects every declared service and backing resource into typed rows.
pub(super) fn services(doc: &Forkfile, features: &FeatureSet) -> Vec<PsService> {
    let mut out: Vec<PsService> = doc
        .services_with(features)
        .into_iter()
        .map(|service| PsService {
            id: service.id.to_owned(),
            kind: PsKind::Service,
            running: false,
            port: service.port,
            version: None,
            cdn: None,
            policy: None,
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
            orphaned: false,
            image: service.image.map(str::to_owned),
            command: service.command.clone(),
        })
        .collect();
    for resource in doc.resources_with(features) {
        let policy = match resource.kind {
            ResourceDeclKind::S3 => Some(PsBucketPolicy::from(bucket_policy(doc, resource.id))),
            ResourceDeclKind::Postgres | ResourceDeclKind::Redis => None,
        };
        let (kind, backend) = match resource.kind {
            ResourceDeclKind::Postgres => (PsKind::Postgres, ResourceKind::Postgres),
            ResourceDeclKind::Redis => (PsKind::Redis, ResourceKind::Redis),
            ResourceDeclKind::S3 => (PsKind::S3, ResourceKind::S3),
        };
        out.push(PsService {
            id: resource.id.to_owned(),
            kind,
            running: false,
            port: Some(backend.port()),
            version: resource
                .version
                .map(str::to_owned)
                .or_else(|| backend.image().map(resource_version)),
            cdn: resource.cdn.map(str::to_owned),
            policy,
            pid: None,
            routes: Vec::new(),
            orphaned: false,
            image: None,
            command: None,
        });
    }
    out
}

/// The bucket policy the `Forkfile` declares for `id`, resolving through active `on` branches.
fn bucket_policy(doc: &Forkfile, id: &str) -> BucketPolicy {
    doc.items
        .items
        .iter()
        .find_map(|item| match item {
            TopItem::S3(s3) if s3.bucket == id => Some(s3.policy_at(&BTreeSet::new())),
            _ => None,
        })
        .unwrap_or_default()
}

fn mode(kind: &RouteKind) -> PsRouteMode {
    match kind {
        RouteKind::Preserve => PsRouteMode::Preserve,
        RouteKind::StripPrefix => PsRouteMode::StripPrefix,
        RouteKind::Rewrite(_) => PsRouteMode::Rewrite,
        RouteKind::Exact(_) => PsRouteMode::Exact,
    }
}

fn domain(filter: &DomainFilter) -> String {
    match filter {
        DomainFilter::Domain { name, .. } => name.clone(),
    }
}

fn resource_version(image: &str) -> String {
    image
        .rsplit_once(':')
        .map_or_else(|| image.to_owned(), |(_, version)| version.to_owned())
}
