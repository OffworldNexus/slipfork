//! Canonical `Forkfile` -> ingress-table extraction.
//!
//! Declared authorities and service routes are read from the evaluated AST once through the
//! `slipfork-forkfile` visitors; every S3 bucket owns `<bucket>.<world>.localhost` and rewrites to
//! the world's native S3 endpoint (`:8333/<bucket>`). A local `cdn <id>` only aliases that bucket
//! domain, so it never registers a second authority.

use std::collections::BTreeSet;

use slipfork_forkfile::FeatureSet;
use slipfork_forkfile::ResourceDeclKind;
use slipfork_forkfile::ast::{DomainFilter, Forkfile, Route, RouteKind};
use slipfork_world::ResourceKind;

use super::{IngressRoute, IngressTable, TableError};

impl IngressTable {
    /// Builds a world's table directly from a `Forkfile` evaluated under `features`.
    ///
    /// Unfiltered service routes serve `<world>.localhost`, and the first enabled app
    /// `domain <id>` *is* that primary authority; each enabled S3 bucket authority routes to the
    /// native S3 endpoint
    /// (`:8333/<bucket>`) unless a service explicitly claimed that `domain` filter. A `cdn <id>`
    /// never adds an authority.
    ///
    /// # Errors
    ///
    /// Returns [`TableError`] for an ambiguous, duplicate, or undeclared-domain table.
    pub fn from_forkfile_with(
        world: impl Into<String>,
        doc: &Forkfile,
        features: &FeatureSet,
    ) -> Result<Self, TableError> {
        let primary_domain = doc.primary_domain_with(features);
        // Bucket authorities are declared by the resource, not by a top-level `domain`; the S3
        // routes below own them and must pass the table's declared check.
        let mut declared: BTreeSet<String> = doc
            .domains_with(features)
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut claimed = BTreeSet::new();
        let mut routes = Vec::new();
        for service in doc.services_with(features) {
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
        for resource in doc.resources_with(features) {
            if resource.kind != ResourceDeclKind::S3 {
                continue;
            }
            let bucket = resource.id;
            declared.insert(bucket.to_owned());
            if claimed.insert(bucket.to_owned()) {
                routes.push(IngressRoute::new(
                    ResourceKind::S3.port(),
                    Route {
                        path: None,
                        kind: RouteKind::Rewrite(format!("/{bucket}")),
                        domain: Some(DomainFilter::Domain {
                            name: bucket.to_owned(),
                            span: doc.span,
                        }),
                        span: doc.span,
                    },
                ));
            }
        }
        Self::from_parts(world, primary_domain, declared, routes)
    }

    /// Builds a world's table directly from a `Forkfile` document under default features.
    ///
    /// # Errors
    ///
    /// Returns [`TableError`] for an ambiguous, duplicate, or undeclared-domain table.
    pub fn from_forkfile(world: impl Into<String>, doc: &Forkfile) -> Result<Self, TableError> {
        Self::from_forkfile_with(world, doc, &doc.default_features())
    }
}
