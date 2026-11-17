//! Protected critical tests for `slipfork-world` (`R17`: disjoint `.localhost` world namespace,
//! single-origin worlds, and deterministic `<= 31`-byte Tunnel Weaver service names).

mod support;

use serde_json::json;
use slipfork_world::{
    DNS_LABEL_REASON, LocalHostRoute, ProjectSlug, SERVICE_NAME_MAX, WeaverServiceName, WorldError,
    WorldOrigin, WorldTarget, derive_weaver_service_name, validate_fork_name,
};
use support::{
    InMemoryTunnelRegistrar, assert_tunnel_registrar_conformance, canonical_route, route,
};

fn slug(project: &str) -> ProjectSlug {
    ProjectSlug::parse(project).expect("valid project slug")
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
        // `WorldOrigin::local` is the only portless-origin constructor: a default-ingress route
        // renders `http://<host>` at port 80, while a named `domains[].id` route is never an origin.
        match expected.snow_domain() {
            None => assert_eq!(
                WorldOrigin::local(&expected, 80)
                    .expect("port-80 origin")
                    .url(),
                format!("http://{host}")
            ),
            Some(_) => assert!(matches!(
                WorldOrigin::local(&expected, 80),
                Err(WorldError::OriginNotDefaultIngress { .. })
            )),
        }
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

    // Host normalization is case-insensitive and accepts the absolute `Host:port` spelling.
    assert_eq!(
        LocalHostRoute::parse(&slug("acme"), &domains, "ACME.localhost.:80"),
        Some(route("acme", WorldTarget::Canonical, None))
    );
    assert_eq!(
        LocalHostRoute::parse(
            &slug("acme"),
            &domains,
            "ADMIN.FEAT-AUTH.ACME.localhost.:443"
        ),
        Some(route(
            "acme",
            WorldTarget::UserFork("feat-auth".to_owned()),
            Some("admin")
        ))
    );
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

    // An empty or `u32`-overflowing slot is rejected rather than wrapping or degrading to a fork.
    assert_eq!(
        LocalHostRoute::parse(&slug("acme"), &domains, "test-.acme.localhost"),
        None
    );
    assert_eq!(
        LocalHostRoute::parse(&slug("acme"), &domains, "test-99999999999.acme.localhost"),
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

fn weaver(label: &str) -> String {
    derive_weaver_service_name(label)
        .expect("label must derive")
        .to_string()
}

#[test]
fn weaver_service_name_is_verbatim_up_to_the_31_byte_boundary() {
    let boundary = "a".repeat(SERVICE_NAME_MAX);
    assert_eq!(weaver(&boundary), boundary);
    assert_eq!(weaver("acme"), "acme");
    assert_eq!(weaver("acme-feat-auth"), "acme-feat-auth");

    // One byte past the cap: exactly 18 prefix bytes + '-' + 12 hash bytes = 31.
    let over = "a".repeat(SERVICE_NAME_MAX + 1);
    let derived = weaver(&over);
    assert_eq!(derived, format!("{}-3ba3f5f43b92", "a".repeat(18)));
    assert_eq!(derived.len(), SERVICE_NAME_MAX);
    assert_eq!(weaver(&over), derived, "derivation is stable across runs");
    assert_eq!(
        WeaverServiceName::parse(&derived)
            .expect("derived name is valid")
            .as_str(),
        derived
    );
}

#[test]
fn weaver_service_name_is_deterministic_but_not_idempotent() {
    let over = "a".repeat(SERVICE_NAME_MAX + 1);
    let once = weaver(&over);
    assert_eq!(
        weaver(&over),
        once,
        "the same full label always derives the same name"
    );

    // The input is a raw full label, never a previous output: a truncated name re-enters the hash
    // path by design (suffix-collision guard), so `f(f(x)) != f(x)`.
    let twice = weaver(&once);
    assert_ne!(twice, once);
    assert_eq!(twice.len(), SERVICE_NAME_MAX);
}

#[test]
fn weaver_service_name_truncation_strips_dashes_and_never_joins_bad_dashes() {
    // The 18-byte cut lands on a dash: it is stripped before the hash suffix is joined.
    let label = format!("{}-{}", "b".repeat(17), "c".repeat(20));
    let derived = weaver(&label);
    assert_eq!(derived, format!("{}-2a0052359bde", "b".repeat(17)));
    assert_eq!(derived.len(), 17 + 1 + 12);
    assert!(!derived.contains("--") && !derived.ends_with('-'));

    // Normalization folds separators and case, so `a-` / `a--b` joins are impossible.
    assert_eq!(weaver("a--b"), "a-b");
    assert_eq!(weaver("  ACME  "), "acme");
    assert_eq!(weaver("-a-"), "a");
    for raw in ["a--b", "A__B", "-a-", "a___", "  a--b  ", "acme.feat/auth"] {
        let name = weaver(raw);
        WeaverServiceName::parse(&name).expect("every derived name satisfies the charset");
        assert!(
            !name.starts_with('-') && !name.ends_with('-') && !name.contains("--"),
            "derived name '{name}' for '{raw}' must be dash-separated runs"
        );
    }

    assert!(matches!(
        derive_weaver_service_name(""),
        Err(WorldError::InvalidWeaverServiceName { .. })
    ));
    assert!(matches!(
        derive_weaver_service_name("---"),
        Err(WorldError::InvalidWeaverServiceName { .. })
    ));
}

#[test]
fn weaver_collision_suffix_shaped_labels_are_hashed_not_used_verbatim() {
    // A short label shaped like a truncated name must not be byte-identical to any truncation.
    let collision = "foo-0123456789ab";
    assert_eq!(collision.len(), 16);
    assert_eq!(weaver(collision), "foo-0123456789ab-25dbe45e8a2f");

    // The whole-label shape (exactly 12 lowercase hex bytes) is rewritten too.
    assert_eq!(weaver("0123456789ab"), "0123456789ab-d407ad901895");

    // The guard is the dash-delimited suffix shape, not "ends with hex": a 31-byte label whose
    // last 12 bytes are hex but which has no separator there stays verbatim.
    let not_shaped = format!("{}{}", "a".repeat(19), "0123456789ab");
    assert_eq!(not_shaped.len(), SERVICE_NAME_MAX);
    assert_eq!(weaver(&not_shaped), not_shaped);

    // Two long labels sharing the first 18 bytes never share a suffix.
    let first = weaver(&format!("{}-{}", "a".repeat(18), "x".repeat(20)));
    let second = weaver(&format!("{}-{}", "a".repeat(18), "y".repeat(20)));
    assert_eq!(first.len(), SERVICE_NAME_MAX);
    assert_eq!(second.len(), SERVICE_NAME_MAX);
    assert_eq!(&first[..18], &second[..18], "shared truncated prefix");
    assert_ne!(first, second, "suffix must depend on the full label");
}

#[test]
fn weaver_service_name_rejects_invalid_charset_and_length() {
    for bad in [
        "",
        "A",
        "Web",
        "-leading",
        "trailing-",
        "a--b",
        "with.dot",
        "with_underscore",
        "with/slash",
        " with-space",
        &"a".repeat(SERVICE_NAME_MAX + 1),
    ] {
        assert!(
            matches!(
                WeaverServiceName::parse(bad),
                Err(WorldError::InvalidWeaverServiceName { .. })
            ),
            "service name '{bad}' must fail closed"
        );
    }
    assert_eq!(
        WeaverServiceName::parse(&"a".repeat(SERVICE_NAME_MAX))
            .expect("31 bytes is the inclusive cap")
            .as_str(),
        "a".repeat(SERVICE_NAME_MAX)
    );
    assert_eq!(
        WeaverServiceName::parse("app-2026")
            .expect("valid name")
            .to_string(),
        "app-2026"
    );

    // Deserialization goes through the same sole constructor and fails closed.
    assert_eq!(
        serde_json::from_str::<WeaverServiceName>("\"app-2026\"")
            .expect("valid service name")
            .as_str(),
        "app-2026"
    );
    for bad in ["\"\"", "\"Web\"", "\"a--b\"", "\"trailing-\""] {
        assert!(
            serde_json::from_str::<WeaverServiceName>(bad).is_err(),
            "deserializing {bad} must fail closed"
        );
    }
}

#[test]
fn world_origin_local_omits_port_80_and_renders_other_ports() {
    let origin = WorldOrigin::local(&canonical_route(), 80).expect("port-80 origin");
    assert!(matches!(origin, WorldOrigin::Local(_)));
    assert_eq!(origin.url(), "http://acme.localhost");
    assert_eq!(origin.host(), "acme.localhost");

    let origin = WorldOrigin::local(&canonical_route(), 7080).expect("ephemeral ingress origin");
    assert_eq!(origin.url(), "http://acme.localhost:7080");
    assert_eq!(origin.host(), "acme.localhost");

    let fork = route("acme", WorldTarget::UserFork("feat-auth".to_owned()), None);
    let fork_origin = WorldOrigin::local(&fork, 65_535).expect("fork origin");
    assert_eq!(fork_origin.url(), "http://feat-auth.acme.localhost:65535");
    assert_eq!(fork_origin.host(), "feat-auth.acme.localhost");

    let slot = route("acme", WorldTarget::TestSlot(3), None);
    let slot_origin = WorldOrigin::local(&slot, 8000).expect("slot origin");
    assert_eq!(slot_origin.url(), "http://test-3.acme.localhost:8000");
    assert_eq!(slot_origin.host(), "test-3.acme.localhost");
}

#[test]
fn world_origin_local_never_renders_port_zero() {
    // Port 0 is an unbound `--listen` wildcard, not an origin: the daemon must report the port it
    // actually bound, so the renderer fails closed instead of printing `:0` or a port-less URL.
    match WorldOrigin::local(&canonical_route(), 0) {
        Err(WorldError::OriginPortZero) => {}
        other => panic!("expected OriginPortZero, got {other:?}"),
    }
}

#[test]
fn world_origin_host_is_the_port_less_authority_for_shared_origins_too() {
    let shared =
        WorldOrigin::shared("https://acme.tunnel.example:8443/app").expect("shared origin");
    assert_eq!(shared.host(), "acme.tunnel.example");
    assert_eq!(shared.url(), "https://acme.tunnel.example:8443/app");

    // A bracketed IPv6 literal keeps its brackets so `host()` stays a valid `Host` authority.
    let v6 = WorldOrigin::shared("http://[::1]:8080").expect("ipv6 origin");
    assert_eq!(v6.host(), "[::1]");

    let bare = WorldOrigin::shared("http://acme.tunnel.example").expect("host-only origin");
    assert_eq!(bare.host(), "acme.tunnel.example");
}

#[test]
fn world_origin_local_rejects_named_snow_domain_routes() {
    let named = route("acme", WorldTarget::Canonical, Some("admin"));
    match WorldOrigin::local(&named, 80) {
        Err(WorldError::OriginNotDefaultIngress { domain }) => assert_eq!(domain, "admin"),
        other => panic!("expected OriginNotDefaultIngress, got {other:?}"),
    }

    // The default-ingress route (snow_domain == None) is the only legal Local origin.
    assert!(WorldOrigin::local(&canonical_route(), 80).is_ok());
}

#[test]
fn world_origin_serializes_with_a_kind_tag_and_deserializes_fail_closed() {
    let local = WorldOrigin::local(&canonical_route(), 7080).expect("local origin");
    assert_eq!(
        serde_json::to_value(&local).expect("serialize"),
        json!({"kind": "local", "url": "http://acme.localhost:7080"})
    );
    let shared = WorldOrigin::shared("https://acme.tunnel.invalid").expect("shared origin");
    assert_eq!(
        serde_json::to_value(&shared).expect("serialize"),
        json!({"kind": "shared", "url": "https://acme.tunnel.invalid"})
    );

    assert_eq!(
        serde_json::from_str::<WorldOrigin>(r#"{"kind":"local","url":"http://acme.localhost"}"#)
            .expect("valid local origin"),
        WorldOrigin::local(&canonical_route(), 80).expect("port-80 origin")
    );
    assert_eq!(
        serde_json::from_str::<WorldOrigin>(r#"{"kind":"shared","url":"https://acme.invalid"}"#)
            .expect("valid shared origin"),
        WorldOrigin::shared("https://acme.invalid").expect("shared origin")
    );

    for bad in [
        r#"{"kind":"local","url":""}"#,
        r#"{"kind":"local","url":"acme.localhost"}"#,
        r#"{"kind":"local","url":"ftp://acme.localhost"}"#,
        r#"{"kind":"shared","url":""}"#,
        r#"{"kind":"shared","url":"http://"}"#,
        r#"{"kind":"shared","url":"ws://acme.invalid"}"#,
        r#"{"kind":"cloud","url":"https://acme.invalid"}"#,
    ] {
        assert!(
            serde_json::from_str::<WorldOrigin>(bad).is_err(),
            "deserializing {bad} must fail closed"
        );
    }

    assert!(matches!(
        WorldOrigin::shared(""),
        Err(WorldError::InvalidOriginUrl { .. })
    ));
    assert!(matches!(
        WorldOrigin::shared("http://"),
        Err(WorldError::InvalidOriginUrl { .. })
    ));
    assert!(matches!(
        WorldOrigin::shared("https://"),
        Err(WorldError::InvalidOriginUrl { .. })
    ));
    assert!(WorldOrigin::shared("https://acme.invalid").is_ok());
}
