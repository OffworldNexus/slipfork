//! Lab journey over the ingress route table: real `Forkfile` route rules resolved against
//! origin-form request targets and `.localhost` authorities, with every refusal branch pinned.

use slipfork_forkfile::{DomainFilter, Position, Route, RouteKind, Span};
use slipfork_ingress::{IngressRegistry, IngressRoute, IngressTable, RouteError, TableError};

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

#[test]
fn ambiguous_and_undeclared_route_tables_fail_closed() {
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
        vec!["main".to_owned()],
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
fn longest_prefix_wins_and_each_mode_rewrites_as_declared() {
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
    .expect("the table is routable");
    assert_eq!(
        table
            .resolve("", "/api/users?page=2")
            .expect("preserve")
            .target,
        "/api/users?page=2"
    );
    assert_eq!(table.resolve("", "/apifoo").expect("catch-all").port, 80);
    assert_eq!(
        table.resolve("", "/old/users").expect("rewrite").target,
        "/v1/users"
    );
    assert_eq!(
        table
            .resolve("", "/files/a/b.css?x=1")
            .expect("strip")
            .target,
        "/a/b.css?x=1"
    );
    assert_eq!(table.resolve("", "/files").expect("strip root").target, "/");
    assert_eq!(
        table.resolve("", "/healthz?probe=1").expect("exact").target,
        "/healthz?probe=1"
    );
    assert_eq!(
        table.resolve("", "/healthz/x").expect("exact miss").port,
        80
    );
    assert_eq!(
        table
            .resolve("", "/api/v2/users?page=2")
            .expect("nested rewrite")
            .target,
        "/users?page=2"
    );
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
}

#[test]
fn the_registry_dispatches_primary_domains_and_forks_over_localhost() {
    let mut registry = IngressRegistry::new([
        IngressTable::new(
            "demo",
            vec!["main".to_owned()],
            [
                IngressRoute::new(80, route(Some("/api"), RouteKind::Preserve)),
                IngressRoute::new(81, domain_route("/", declared("main"))),
                IngressRoute::new(
                    82,
                    domain_route("/", DomainFilter::Prefix("mail".to_owned())),
                ),
            ],
        )
        .expect("demo table"),
        IngressTable::new(
            "feat-auth--demo",
            Vec::new(),
            [IngressRoute::new(90, route(Some("/"), RouteKind::Preserve))],
        )
        .expect("fork table"),
    ])
    .expect("registry");
    assert_eq!(
        registry
            .route("demo.localhost", "/api")
            .expect("primary")
            .port,
        80
    );
    assert_eq!(
        registry
            .route("demo.localhost:80", "/api")
            .expect("explicit :80")
            .port,
        80
    );
    assert_eq!(
        registry
            .route("main.demo.localhost", "/")
            .expect("domain")
            .port,
        81
    );
    assert_eq!(
        registry
            .route("mail.demo.localhost", "/")
            .expect("prefix")
            .port,
        82
    );
    assert_eq!(
        registry
            .route("feat-auth--demo.localhost", "/")
            .expect("fork")
            .port,
        90
    );
    assert!(matches!(
        registry.route("nowhere.localhost", "/"),
        Err(RouteError::UnknownHost { .. })
    ));
    assert!(matches!(
        registry.route("example.com", "/"),
        Err(RouteError::HostOutsideLocalhost { .. })
    ));
    assert!(matches!(
        registry.insert(IngressTable::new("demo", Vec::new(), Vec::new()).expect("empty table")),
        Err(TableError::DuplicateWorld { .. })
    ));
}

/// `s3 uploads` stays private while `s3 assets { cdn static_cdn }` publishes its CDN authority, and
/// the compiler-owned symbol table hands back the matching `.domain`/`.url` values.
const MULTI_S3: &str = r#"# forkfile: 0.1
env common {
  CDN_DOMAIN = static_cdn.domain
  CDN_URL = static_cdn.url
}

s3 uploads
s3 assets { cdn static_cdn }

image app . {
  dockerfile Dockerfile
  service api {
    port 8000
    route /
  }
}
"#;

#[test]
fn s3_buckets_and_cdn_authorities_rewrite_to_seaweedfs() {
    let doc = slipfork_forkfile::parse(MULTI_S3).expect("the multi-S3 Forkfile parses");
    assert!(
        slipfork_forkfile::check(&doc).is_ok(),
        "the multi-S3 Forkfile passes static validation"
    );
    let table = IngressTable::from_forkfile("demo", &doc).expect("the multi-S3 table builds");

    let private = table
        .resolve("uploads", "/notes/a.txt")
        .expect("`uploads` owns its private bucket authority");
    assert_eq!(private.port, 8333);
    assert_eq!(private.target, "/uploads/notes/a.txt");

    let cdn = table
        .resolve("static_cdn", "/logo.png")
        .expect("`cdn static_cdn` mounts its own authority");
    assert_eq!(cdn.port, 8333);
    assert_eq!(cdn.target, "/assets/logo.png");

    let primary = table
        .resolve("", "/")
        .expect("the service still serves the primary world authority");
    assert_eq!(primary.port, 8000);

    let env = doc.eval_all_env("demo", &|_, _| None, &|_| None);
    let common = env.get("common").expect("`env common` was visited");
    assert_eq!(common["CDN_DOMAIN"].0, "static_cdn.demo.localhost");
    assert_eq!(common["CDN_URL"].0, "http://static_cdn.demo.localhost");
}
