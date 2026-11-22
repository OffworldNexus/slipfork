//! Projecting one `ps` world into the domain-first rows the tree renders.
//!
//! Every routed row is the same shape — `<path> -> <target>`, an optional kind tag, and one
//! lifecycle word — so an S3 bucket is just a `/` row on its own `<bucket>.<world>.localhost`
//! domain. Services, resources, and orphaned pods without a route land in `support`.

use crate::ps::{PsBucketPolicy, PsKind, PsService, PsWorld};
use slipfork_ingress::route_authorities;

/// One domain authority and the sub-paths mapped onto it.
#[derive(Debug, Clone)]
pub struct DomainRows {
    /// Host without the `http://` scheme (`demo.localhost`).
    pub host: String,
    /// Routed sub-paths in declaration order.
    pub rows: Vec<RoutedRow>,
}

/// One `<path> -> <target>` row under a domain.
#[derive(Debug, Clone)]
pub struct RoutedRow {
    /// Leading path prefix.
    pub path: String,
    /// Service or bucket the path maps to.
    pub target: String,
    /// `s3 (private)`/`s3 (public-read)` tag, when the target is not a service.
    pub kind: Option<String>,
    /// Lifecycle word.
    pub state: State,
}

/// One unrouted service, resource, or orphaned pod.
#[derive(Debug, Clone)]
pub struct SupportRow {
    /// Service or resource id.
    pub id: String,
    /// `service`, `postgres 18`, or `redis 7`.
    pub descriptor: String,
    /// Lifecycle word.
    pub state: State,
}

/// The lifecycle word shown in the final column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    /// The word a human reads.
    pub word: &'static str,
}

impl State {
    const RUNNING: Self = Self { word: "running" };
    const FAILED: Self = Self { word: "failed" };
    const STARTING: Self = Self { word: "starting" };
    const ORPHANED: Self = Self { word: "orphaned" };
}

/// One world's domains and support group.
#[derive(Debug, Clone)]
pub struct WorldRows<'a> {
    /// The world or fork this projection came from.
    pub world: &'a PsWorld,
    /// Routed domains in declaration order.
    pub domains: Vec<DomainRows>,
    /// Unrouted rows, always rendered last.
    pub support: Vec<SupportRow>,
}

/// Projects every declared service, bucket, and resource of one world.
#[must_use]
pub fn project(world: &PsWorld) -> WorldRows<'_> {
    let primary = world
        .url
        .strip_prefix("http://")
        .unwrap_or(&world.url)
        .to_owned();
    let mut domains: Vec<DomainRows> = Vec::new();
    let mut support = Vec::new();
    for service in &world.services {
        let state = state(service, world.booting);
        if service.kind == PsKind::S3 {
            let host = format!("{}.{primary}", service.id);
            let row = RoutedRow {
                path: "/".to_owned(),
                target: service.id.clone(),
                kind: Some(descriptor(service)),
                state,
            };
            push_row(&mut domains, host, row);
        } else if !service.routes.is_empty() {
            for route in &service.routes {
                for authority in
                    route_authorities(world.primary_domain.as_deref(), route.domain.as_deref())
                {
                    let host = authority
                        .map_or_else(|| primary.clone(), |name| format!("{name}.{primary}"));
                    let row = RoutedRow {
                        path: route.prefix.clone(),
                        target: service.id.clone(),
                        kind: None,
                        state,
                    };
                    push_row(&mut domains, host, row);
                }
            }
        } else {
            support.push(SupportRow {
                id: service.id.clone(),
                descriptor: descriptor(service),
                state,
            });
        }
    }
    WorldRows {
        world,
        domains,
        support,
    }
}

fn push_row(domains: &mut Vec<DomainRows>, host: String, row: RoutedRow) {
    match domains.iter_mut().find(|domain| domain.host == host) {
        Some(domain) => domain.rows.push(row),
        None => domains.push(DomainRows {
            host,
            rows: vec![row],
        }),
    }
}

fn state(service: &PsService, booting: bool) -> State {
    if booting {
        State::STARTING
    } else if !service.running {
        State::FAILED
    } else if service.orphaned {
        State::ORPHANED
    } else {
        State::RUNNING
    }
}

fn descriptor(service: &PsService) -> String {
    match service.kind {
        PsKind::Postgres => format!("postgres {}", service.version.as_deref().unwrap_or("18")),
        PsKind::Redis => format!("redis {}", service.version.as_deref().unwrap_or("7")),
        PsKind::S3 if service.policy == Some(PsBucketPolicy::PublicRead) => {
            "s3 (public-read)".to_owned()
        }
        PsKind::S3 => "s3 (private)".to_owned(),
        PsKind::Cdn => "cdn".to_owned(),
        PsKind::Image => "image".to_owned(),
        PsKind::Service => "service".to_owned(),
    }
}
