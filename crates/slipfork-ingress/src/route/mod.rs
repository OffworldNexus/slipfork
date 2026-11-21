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
        let entries = rules::entries_for(&world, &declared, routes)?;
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

#[cfg(test)]
mod tests {
    use super::rules::is_authority_label;
    use super::{IngressRegistry, IngressRoute, IngressTable, RouteError, TableError};
    use slipfork_forkfile::{DomainFilter, Position, Route, RouteKind, Span};

    fn route(path: Option<&str>, kind: RouteKind) -> Route {
        Route {
            path: path.map(str::to_owned),
            kind,
            domain: None,
            span: Span::at(Position::new(1, 1, 0)),
        }
    }

    fn domain_route(path: &str, domain: DomainFilter) -> Route {
        Route {
            path: Some(path.to_owned()),
            kind: RouteKind::Preserve,
            domain: Some(domain),
            span: Span::at(Position::new(1, 1, 0)),
        }
    }

    fn declared(name: &str) -> DomainFilter {
        DomainFilter::Domain {
            name: name.to_owned(),
            span: Span::at(Position::new(1, 1, 0)),
        }
    }

    fn domains(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn duplicate_ambiguous_and_undeclared_tables_are_refused() {
        let duplicate = IngressTable::new(
            "demo",
            Vec::new(),
            [
                IngressRoute::new(80, route(Some("/api"), RouteKind::Preserve)),
                IngressRoute::new(81, route(Some("/api"), RouteKind::StripPrefix)),
            ],
        );
        assert!(matches!(duplicate, Err(TableError::Duplicate { .. })));

        let ambiguous = IngressTable::new(
            "demo",
            Vec::new(),
            [
                IngressRoute::new(80, route(None, RouteKind::Preserve)),
                IngressRoute::new(81, route(Some("/"), RouteKind::Preserve)),
            ],
        );
        assert!(matches!(ambiguous, Err(TableError::Ambiguous { .. })));

        let zero = IngressTable::new(
            "demo",
            Vec::new(),
            [IngressRoute::new(0, route(Some("/"), RouteKind::Preserve))],
        );
        assert!(matches!(zero, Err(TableError::PortZero { .. })));

        let undeclared = IngressTable::new(
            "demo",
            domains(&["main"]),
            [IngressRoute::new(80, domain_route("/", declared("ghost")))],
        );
        assert!(matches!(
            undeclared,
            Err(TableError::UndeclaredDomain { .. })
        ));

        let world = IngressTable::new(
            "Demo",
            Vec::new(),
            [IngressRoute::new(80, route(None, RouteKind::Preserve))],
        );
        assert!(matches!(world, Err(TableError::InvalidWorld { .. })));
    }

    #[test]
    fn the_longest_prefix_wins_and_modes_rewrite() {
        let table = IngressTable::new(
            "demo",
            Vec::new(),
            [
                IngressRoute::new(80, route(Some("/"), RouteKind::Preserve)),
                IngressRoute::new(81, route(Some("/api"), RouteKind::Preserve)),
                IngressRoute::new(
                    82,
                    route(Some("/old"), RouteKind::Rewrite("/v1".to_owned())),
                ),
                IngressRoute::new(83, route(Some("/files"), RouteKind::StripPrefix)),
                IngressRoute::new(84, route(None, RouteKind::Exact("/healthz".to_owned()))),
                IngressRoute::new(
                    85,
                    route(Some("/api/v2"), RouteKind::Rewrite("/".to_owned())),
                ),
                IngressRoute::new(86, route(Some("/"), RouteKind::StripPrefix)),
            ],
        );
        assert!(
            matches!(table, Err(TableError::Duplicate { .. })),
            "a catch-all preserve and a catch-all strip-prefix collide"
        );

        let table = IngressTable::new(
            "demo",
            Vec::new(),
            [
                IngressRoute::new(80, route(Some("/"), RouteKind::Preserve)),
                IngressRoute::new(81, route(Some("/api"), RouteKind::Preserve)),
                IngressRoute::new(
                    82,
                    route(Some("/old"), RouteKind::Rewrite("/v1".to_owned())),
                ),
                IngressRoute::new(83, route(Some("/files"), RouteKind::StripPrefix)),
                IngressRoute::new(84, route(None, RouteKind::Exact("/healthz".to_owned()))),
                IngressRoute::new(
                    85,
                    route(Some("/api/v2"), RouteKind::Rewrite("/".to_owned())),
                ),
            ],
        )
        .expect("table is routable");
        assert_eq!(
            table.resolve("", "/api/users?page=2").unwrap().target,
            "/api/users?page=2"
        );
        assert_eq!(table.resolve("", "/apifoo").unwrap().port, 80);
        assert_eq!(table.resolve("", "/old/users").unwrap().target, "/v1/users");
        assert_eq!(
            table.resolve("", "/files/a/b.css?x=1").unwrap().target,
            "/a/b.css?x=1"
        );
        assert_eq!(table.resolve("", "/files").unwrap().target, "/");
        assert_eq!(
            table.resolve("", "/healthz?probe=1").unwrap().target,
            "/healthz?probe=1"
        );
        assert_eq!(table.resolve("", "/healthz/x").unwrap().port, 80);
        assert_eq!(
            table.resolve("", "/api/v2/users?page=2").unwrap().target,
            "/users?page=2"
        );
        assert_eq!(table.resolve("", "/api/v2").unwrap().target, "/");
        assert!(matches!(
            table.resolve("other", "/api"),
            Err(RouteError::UnknownPath { .. })
        ));
        assert!(matches!(
            table.resolve("", "/../etc/passwd"),
            Err(RouteError::PathTraversal { .. })
        ));
        assert!(matches!(
            table.resolve("", "http://demo.localhost/api"),
            Err(RouteError::NotOriginForm { .. })
        ));

        let strip_all = IngressTable::new(
            "demo",
            Vec::new(),
            [IngressRoute::new(
                90,
                route(Some("/"), RouteKind::StripPrefix),
            )],
        )
        .expect("table is routable");
        assert_eq!(strip_all.resolve("", "/files").unwrap().target, "/files");
    }

    #[test]
    fn an_unfiltered_route_serves_the_primary_and_every_declared_domain() {
        let table = IngressTable::new(
            "demo",
            domains(&["main", "static_cdn"]),
            [
                IngressRoute::new(80, route(Some("/api"), RouteKind::Preserve)),
                IngressRoute::new(81, domain_route("/", declared("main"))),
                IngressRoute::new(
                    82,
                    domain_route("/", DomainFilter::Prefix("mail".to_owned())),
                ),
            ],
        )
        .expect("table is routable");
        for authority in ["", "main", "static_cdn"] {
            assert_eq!(table.resolve(authority, "/api/users").unwrap().port, 80);
        }
        assert_eq!(table.resolve("main", "/").unwrap().port, 81);
        assert!(matches!(
            table.resolve("static_cdn", "/"),
            Err(RouteError::UnknownPath { .. })
        ));
        assert_eq!(table.resolve("mail", "/").unwrap().port, 82);
    }

    #[test]
    fn the_registry_dispatches_forks_and_reports_unknown_hosts() {
        let mut registry = IngressRegistry::new([
            IngressTable::new(
                "demo",
                domains(&["main"]),
                [IngressRoute::new(80, route(Some("/"), RouteKind::Preserve))],
            )
            .expect("demo table"),
            IngressTable::new(
                "feat-auth--demo",
                Vec::new(),
                [IngressRoute::new(81, route(Some("/"), RouteKind::Preserve))],
            )
            .expect("fork table"),
        ])
        .expect("registry");
        assert_eq!(registry.route("demo.localhost", "/x").unwrap().port, 80);
        assert_eq!(registry.route("demo.localhost:80", "/x").unwrap().port, 80);
        assert_eq!(
            registry.route("main.demo.localhost", "/x").unwrap().port,
            80
        );
        assert_eq!(
            registry
                .route("feat-auth--demo.localhost", "/x")
                .unwrap()
                .port,
            81
        );
        assert!(matches!(
            registry.route("nowhere.localhost", "/x"),
            Err(RouteError::UnknownHost { .. })
        ));
        assert!(matches!(
            registry.route("example.com", "/x"),
            Err(RouteError::HostOutsideLocalhost { .. })
        ));
        assert!(matches!(
            registry
                .insert(IngressTable::new("demo", Vec::new(), Vec::new()).expect("empty table")),
            Err(TableError::DuplicateWorld { .. })
        ));
    }

    #[test]
    fn the_authority_label_grammar_is_strict() {
        assert!(is_authority_label("demo"));
        assert!(is_authority_label("feat-auth--demo"));
        assert!(is_authority_label("static_cdn"));
        assert!(!is_authority_label("Demo"));
        assert!(!is_authority_label("-demo"));
        assert!(!is_authority_label("demo-"));
        assert!(!is_authority_label("demo.localhost"));
    }
}
