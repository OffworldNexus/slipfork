//! One `.localhost` ingress table per world, plus the registry that dispatches by authority.
//!
//! The route grammar is owned by `slipfork-forkfile`: this module only assigns each declared route
//! to the authorities it owns, resolves the longest matching path prefix, and produces the exact
//! upstream target each mode forwards. A table that could route one request two ways is refused
//! before it serves a connection.

use std::collections::{BTreeMap, BTreeSet};

use slipfork_forkfile::Route;

mod error;
mod extract;
mod rules;

pub use error::{RouteError, TableError};

use rules::Entry;

/// One declared `Forkfile` route paired with the port its service listens on.
#[derive(Debug, Clone)]
pub struct IngressRoute {
    /// Upstream port inside the world.
    pub port: u16,
    /// The parsed `Forkfile` route.
    pub route: Route,
}

impl IngressRoute {
    /// Pairs one parsed route with the port of the service that declared it.
    #[must_use]
    pub const fn new(port: u16, route: Route) -> Self {
        Self { port, route }
    }
}

/// The routing decision for one accepted request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteMatch {
    /// World the request is routed into.
    pub world: String,
    /// Matched authority label (`""` for the primary `<world>.localhost`).
    pub authority: String,
    /// Upstream port inside the world.
    pub port: u16,
    /// Upstream request target (path plus query) after the route mode rewrote it.
    pub target: String,
}

/// The declared ingress table of one world.
#[derive(Debug, Clone)]
pub struct IngressTable {
    world: String,
    entries: Vec<Entry>,
}

impl IngressTable {
    /// Builds the table for one world.
    ///
    /// A route without a `domain` filter owns the primary `<world>.localhost` authority and every
    /// declared app domain; `domain <id>` owns exactly that authority; `domain <prefix>.*` owns only
    /// its subdomain family. Routes are taken verbatim from `slipfork-forkfile`, so the
    /// `strip-prefix`/`rewrite`/`exact` spelling is never parsed twice.
    ///
    /// # Errors
    ///
    /// Returns [`TableError`] when the table would be ambiguous or unroutable.
    pub fn new(
        world: impl Into<String>,
        declared_domains: impl IntoIterator<Item = String>,
        routes: impl IntoIterator<Item = IngressRoute>,
    ) -> Result<Self, TableError> {
        let world = world.into();
        if !rules::is_authority_label(&world) {
            return Err(TableError::InvalidWorld { world });
        }
        let declared: BTreeSet<String> = declared_domains.into_iter().collect();
        Self::from_parts(world, declared.clone(), declared, routes)
    }

    pub(crate) fn from_parts(
        world: impl Into<String>,
        app_domains: impl IntoIterator<Item = String>,
        declared_domains: impl IntoIterator<Item = String>,
        routes: impl IntoIterator<Item = IngressRoute>,
    ) -> Result<Self, TableError> {
        let world = world.into();
        if !rules::is_authority_label(&world) {
            return Err(TableError::InvalidWorld { world });
        }
        let app: BTreeSet<String> = app_domains.into_iter().collect();
        let declared: BTreeSet<String> = declared_domains.into_iter().collect();
        let entries = rules::entries_for(&world, &app, &declared, routes)?;
        Ok(Self { world, entries })
    }

    /// Resolves one request target. `authority` is the label before `<world>.localhost`, or `""`
    /// for the primary `<world>.localhost` authority.
    ///
    /// # Errors
    ///
    /// Returns [`RouteError::UnknownPath`] when no route covers the request, or a target-shape
    /// error when the request target is not a safe origin-form path.
    pub fn resolve(&self, authority: &str, target: &str) -> Result<RouteMatch, RouteError> {
        rules::resolve(&self.world, &self.entries, authority, target)
    }
}

/// Every world's table, dispatched by `<authority>.<world>.localhost`.
#[derive(Debug, Clone, Default)]
pub struct IngressRegistry {
    tables: BTreeMap<String, IngressTable>,
}

impl IngressRegistry {
    /// Builds a registry from one table per world.
    ///
    /// # Errors
    ///
    /// Returns [`TableError::DuplicateWorld`] when two tables name the same world.
    pub fn new(tables: impl IntoIterator<Item = IngressTable>) -> Result<Self, TableError> {
        let mut registry = Self::default();
        for table in tables {
            registry.insert(table)?;
        }
        Ok(registry)
    }

    /// Adds one world's table.
    ///
    /// # Errors
    ///
    /// Returns [`TableError::DuplicateWorld`] when the world is already registered.
    pub fn insert(&mut self, table: IngressTable) -> Result<(), TableError> {
        if self.tables.contains_key(&table.world) {
            return Err(TableError::DuplicateWorld { world: table.world });
        }
        self.tables.insert(table.world.clone(), table);
        Ok(())
    }

    /// Routes one `Host` header plus request target to a world, authority, port, and target.
    ///
    /// Authorities are `<world>.localhost`, `<domain>.<world>.localhost`,
    /// `<prefix>.<world>.localhost`, and `<fork>--<world>.localhost` — the fork form is just the
    /// world id itself. The longest registered world id wins, so a fork never shadows its parent.
    ///
    /// # Errors
    ///
    /// Returns [`RouteError`] for non-`.localhost` hosts, unknown worlds, and unrouted paths.
    pub fn route(&self, host: &str, target: &str) -> Result<RouteMatch, RouteError> {
        rules::validate_target(target)?;
        let host = host.trim();
        let lower = strip_host_port(host).to_ascii_lowercase();
        let Some(name) = lower.strip_suffix(".localhost") else {
            return Err(RouteError::HostOutsideLocalhost {
                host: host.to_owned(),
            });
        };
        let mut best: Option<(&IngressTable, &str)> = None;
        for (world, table) in &self.tables {
            let authority = if name == world {
                Some("")
            } else {
                name.strip_suffix(world.as_str())
                    .and_then(|rest| rest.strip_suffix('.'))
            };
            if let Some(authority) = authority
                && best.is_none_or(|(table, _)| world.len() > table.world.len())
            {
                best = Some((table, authority));
            }
        }
        let Some((table, authority)) = best else {
            return Err(RouteError::UnknownHost {
                host: host.to_owned(),
            });
        };
        table.resolve(authority, target)
    }
}

/// Drop a numeric `:port` suffix from a `Host` header value.
fn strip_host_port(host: &str) -> &str {
    match host.rsplit_once(':') {
        Some((name, port))
            if !name.contains(':')
                && !port.is_empty()
                && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            name
        }
        _ => host,
    }
}
