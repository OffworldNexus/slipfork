//! Protected critical tests for `slipfork-world` (`R17`: disjoint `.localhost` world namespace).

use slipfork_world::conformance::{InMemoryTunnelRegistrar, assert_tunnel_registrar_conformance};
use slipfork_world::{
    DNS_LABEL_REASON, LocalHostRoute, ProjectSlug, WorldError, WorldTarget,
    is_reserved_test_slot_label, validate_fork_name,
};

fn slug(project: &str) -> ProjectSlug {
    ProjectSlug::parse(project).expect("valid project slug")
}

fn route(project: &str, target: WorldTarget, snow_domain: Option<&str>) -> LocalHostRoute {
    LocalHostRoute::new(slug(project), target, snow_domain, &["admin", "docs"])
        .expect("valid route")
}

#[test]
fn one_two_three_label_routes_parse_and_format_round_trip() {
    let domains = ["admin", "docs"];
    let cases = [
        (
            route("acme", WorldTarget::Canonical, None),
            "acme.localhost",
        ),
        (
            route("acme", WorldTarget::Canonical, Some("admin")),
            "admin.acme.localhost",
        ),
        (
            route("acme", WorldTarget::UserFork("feat-auth".to_owned()), None),
            "feat-auth.acme.localhost",
        ),
        (
            route(
                "acme",
                WorldTarget::UserFork("feat-auth".to_owned()),
                Some("admin"),
            ),
            "admin.feat-auth.acme.localhost",
        ),
        (
            route("acme", WorldTarget::TestSlot(3), None),
            "test-3.acme.localhost",
        ),
        (
            route("acme", WorldTarget::TestSlot(3), Some("admin")),
            "admin.test-3.acme.localhost",
        ),
    ];

    for (expected, host) in cases {
        assert_eq!(expected.host(), host);
        assert_eq!(expected.url(), format!("http://{host}"));
        assert_eq!(
            LocalHostRoute::parse(&slug("acme"), &domains, host),
            Some(expected.clone()),
            "round-trip failed for {host}"
        );
        // A trailing dot and an explicit port are equivalent HTTP/1.1 Host spellings.
        assert_eq!(
            LocalHostRoute::parse(&slug("acme"), &domains, &format!("{host}:80")),
            Some(expected.clone()),
            "port-80 spelling failed for {host}"
        );
        assert_eq!(
            LocalHostRoute::parse(&slug("acme"), &domains, &format!("{host}.:443")),
            Some(expected),
            "normalized spelling failed for {host}"
        );
    }
}

#[test]
fn admin_admin_collision_and_fork_domain_collisions_fail_closed() {
    let domains = ["admin", "docs"];
    // Two labels that both resolve to declared domains are ambiguous, not a fork.
    assert_eq!(
        LocalHostRoute::parse(&slug("acme"), &domains, "admin.admin.acme.localhost"),
        None
    );
    assert_eq!(
        LocalHostRoute::parse(&slug("acme"), &domains, "docs.admin.acme.localhost"),
        None
    );
    for fork in ["admin", "docs"] {
        assert!(matches!(
            validate_fork_name(fork, &domains),
            Err(WorldError::InvalidForkName { .. })
        ));
    }
    for bad_fork in [
        "",
        "feat.auth",
        "My_Project",
        "-feat",
        "feat-",
        &"a".repeat(64),
    ] {
        match validate_fork_name(bad_fork, &domains) {
            Err(WorldError::InvalidForkName { reason, .. }) => {
                assert_eq!(reason, DNS_LABEL_REASON, "fork '{bad_fork}' reason");
            }
            other => panic!("expected InvalidForkName for '{bad_fork}', got {other:?}"),
        }
    }
    assert!(validate_fork_name("feat-auth", &domains).is_ok());
}

#[test]
fn test_slot_namespace_is_reserved_and_cannot_be_a_user_fork_or_domain() {
    let domains = ["admin"];
    assert!(is_reserved_test_slot_label("test-3"));
    assert!(matches!(
        validate_fork_name("test-3", &domains),
        Err(WorldError::InvalidForkName { .. })
    ));
    assert!(matches!(
        validate_fork_name("test-feat", &domains),
        Err(WorldError::InvalidForkName { .. })
    ));

    // `test-<slot>` parses into the typed slot variant, never a `UserFork("test-3")`.
    assert_eq!(
        LocalHostRoute::parse(&slug("acme"), &domains, "test-3.acme.localhost"),
        Some(route("acme", WorldTarget::TestSlot(3), None))
    );
    assert_eq!(
        LocalHostRoute::parse(&slug("acme"), &domains, "admin.test-3.acme.localhost"),
        Some(route("acme", WorldTarget::TestSlot(3), Some("admin")))
    );

    // Non-numeric `test-*` labels are neither slots nor user forks: fail closed, never an
    // unroutable `UserFork("test-feat")`.
    assert_eq!(
        LocalHostRoute::parse(&slug("acme"), &domains, "test-feat.acme.localhost"),
        None
    );
    assert_eq!(
        LocalHostRoute::parse(&slug("acme"), &domains, "admin.test-feat.acme.localhost"),
        None
    );

    // TestSlot targets validate without touching the user-fork rules.
    assert!(WorldTarget::TestSlot(3).validate(&domains).is_ok());

    // Direct construction via LocalHostRoute::new enforces the exact same invariants.
    let valid_route = LocalHostRoute::new(
        slug("acme"),
        WorldTarget::UserFork("feat-auth".to_owned()),
        Some("admin"),
        &domains,
    )
    .expect("valid route");
    assert_eq!(valid_route.project().as_str(), "acme");
    assert_eq!(
        valid_route.target(),
        &WorldTarget::UserFork("feat-auth".to_owned())
    );
    assert_eq!(valid_route.snow_domain(), Some("admin"));

    assert!(matches!(
        LocalHostRoute::new(
            slug("acme"),
            WorldTarget::UserFork("test-feat".to_owned()),
            None,
            &domains
        ),
        Err(WorldError::InvalidForkName { .. })
    ));
    assert!(matches!(
        LocalHostRoute::new(
            slug("acme"),
            WorldTarget::Canonical,
            Some("ghost"),
            &domains
        ),
        Err(WorldError::InvalidSnowDomain { .. })
    ));
    assert!(matches!(
        LocalHostRoute::new(
            slug("acme"),
            WorldTarget::Canonical,
            Some("test-3"),
            &["test-3"]
        ),
        Err(WorldError::InvalidSnowDomain { .. })
    ));
    match LocalHostRoute::new(
        slug("acme"),
        WorldTarget::Canonical,
        Some("Admin.Sub"),
        &["Admin.Sub"],
    ) {
        Err(WorldError::InvalidSnowDomain { reason, .. }) => assert_eq!(reason, DNS_LABEL_REASON),
        other => panic!("expected InvalidSnowDomain with DNS_LABEL_REASON, got {other:?}"),
    }
}

#[test]
fn invalid_project_slugs_fail_closed() {
    let err = ProjectSlug::parse("My_Project").expect_err("My_Project must fail closed");
    assert!(matches!(err, WorldError::InvalidProjectSlug { .. }));
    assert!(err.to_string().contains(DNS_LABEL_REASON));
    for project in [
        "",
        "Project",
        "my_project",
        "-acme",
        "acme-",
        &"a".repeat(64),
    ] {
        assert!(
            ProjectSlug::parse(project).is_err(),
            "slug '{project}' must be rejected"
        );
    }
    let parsed = slug("my-project");
    assert_eq!(parsed.as_str(), "my-project");
    assert_eq!(parsed.to_string(), "my-project");
}

#[test]
fn ipv6_port_corrupted_foreign_and_malformed_hosts_are_rejected_cleanly() {
    let domains = ["admin"];
    for host in [
        "[::1]:80",
        "[2001:db8::1]",
        "acme.localhost:http",
        "acme.localhost:notaport",
        "acme.localhost:80:81",
        "feat-auth.acme.localhost:notaport",
        "undeclared.feat-auth.acme.localhost",
        "a.b.c.acme.localhost",
        "acme.nip.io",
        "other.localhost",
        "acme.localhost.evil.example",
        "",
        ".",
    ] {
        assert_eq!(
            LocalHostRoute::parse(&slug("acme"), &domains, host),
            None,
            "host '{host}' must be rejected"
        );
    }
}

#[test]
fn tunnel_registrar_conformance_holds_for_the_in_memory_fake() {
    let registrar = InMemoryTunnelRegistrar::new();
    assert_tunnel_registrar_conformance(&registrar, |lease_id| registrar.is_lease_active(lease_id));
}
