//! Canonical `Forkfile` -> ingress-table extraction.
//!
//! Declared authorities and service routes are read from the evaluated AST once through the
//! `slipfork-forkfile` visitors; S3 buckets and `cdn <id>` authorities rewrite cleanly to the
//! world's SeaweedFS S3 endpoint (`:8333/<bucket>`).

use std::collections::BTreeSet;

use slipfork_forkfile::ResourceDeclKind;
use slipfork_forkfile::ast::{DomainFilter, Forkfile, Route, RouteKind};
use slipfork_world::ResourceKind;

use super::{IngressRoute, IngressTable, TableError};

impl IngressTable {
    /// Builds a world's table directly from an evaluated `Forkfile` document.
    ///
    /// Unfiltered service routes serve `<world>.localhost` and every declared app `domain <id>`;
    /// S3 bucket and `cdn <id>` authorities route to SeaweedFS (`:8333/<bucket>`) unless a service
    /// explicitly claimed that `domain` filter.
    ///
    /// # Errors
    ///
    /// Returns [`TableError`] for an ambiguous, duplicate, or undeclared-domain table.
    pub fn from_forkfile(world: impl Into<String>, doc: &Forkfile) -> Result<Self, TableError> {
        let app_domains = doc.app_domains().into_iter().map(str::to_owned);
        // Bucket and `cdn <id>` authorities are declared by the resource, not by a top-level
        // `domain`; the S3 routes below own them and must pass the table's declared check.
        let mut declared: BTreeSet<String> = doc.domains().into_iter().map(str::to_owned).collect();
        let mut claimed = BTreeSet::new();
        let mut routes = Vec::new();
        for service in doc.services() {
            let Some(port) = service.port else {
                continue;
            };
            for route in service.routes {
                if let Some(DomainFilter::Domain { name, .. }) = &route.domain {
                    claimed.insert(name.clone());
                }
                routes.push(IngressRoute::new(port, route.clone()));
            }
        }
        for resource in doc.resources() {
            if resource.kind != ResourceDeclKind::S3 {
                continue;
            }
            let bucket = resource.id;
            for authority in [Some(bucket), resource.cdn].into_iter().flatten() {
                declared.insert(authority.to_owned());
                if claimed.insert(authority.to_owned()) {
                    routes.push(IngressRoute::new(
                        ResourceKind::Seaweedfs.port(),
                        Route {
                            path: None,
                            kind: RouteKind::Rewrite(format!("/{bucket}")),
                            domain: Some(DomainFilter::Domain {
                                name: authority.to_owned(),
                                span: doc.span,
                            }),
                            span: doc.span,
                        },
                    ));
                }
            }
        }
        Self::from_parts(world, app_domains, declared, routes)
    }
}
