//! Protected critical tests for `slipfork-snow` (`R1..R8`, M2.4 provider & mode contracts).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU8, Ordering};

use slipfork_snow::conformance::assert_secret_provider_conformance;
use slipfork_snow::{
    EntropySource, FileVaultProvider, LayeredSecretProvider, MemorySecretProvider, Namespace,
    ProviderError, ResourceCoordinates, S3Coordinates, SecretProvider, SecretValue, SnowError,
    SnowFile,
};
use slipfork_world::{
    DNS_LABEL_REASON, LocalHostRoute, ProjectSlug, WorldError, WorldOrigin, WorldTarget,
};

fn project_slug(slug: &str) -> ProjectSlug {
    ProjectSlug::parse(slug).expect("valid project slug")
}

fn canonical_coords(snow: &SnowFile) -> ResourceCoordinates {
    ResourceCoordinates::local_for_world(snow, &project_slug("acme"), &WorldTarget::Canonical)
        .expect("canonical coordinates")
}

/// Deterministic [`EntropySource`] adapter: `Sync`, so one instance can be shared by reference.
struct FixedCounterEntropy(AtomicU8);

impl FixedCounterEntropy {
    fn new(seed: u8) -> Self {
        Self(AtomicU8::new(seed))
    }
}

impl EntropySource for FixedCounterEntropy {
    fn fill_bytes(&self, buf: &mut [u8]) -> Result<(), ProviderError> {
        for b in buf {
            let next = self.0.load(Ordering::Relaxed);
            self.0.store(next.wrapping_add(7), Ordering::Relaxed);
            *b = next;
        }
        Ok(())
    }
}

/// Both DI ports and every shipped adapter are shareable across threads.
#[test]
fn providers_are_send_sync_and_usable_through_shared_references() {
    fn assert_send_sync<T: Send + Sync + ?Sized>() {}
    assert_send_sync::<dyn SecretProvider>();
    assert_send_sync::<dyn EntropySource>();
    assert_send_sync::<FileVaultProvider>();
    assert_send_sync::<MemorySecretProvider>();
    assert_send_sync::<LayeredSecretProvider<MemorySecretProvider, MemorySecretProvider>>();
    assert_send_sync::<slipfork_snow::OsCsprngEntropy>();
    assert_send_sync::<FixedCounterEntropy>();
}

#[test]
fn secret_provider_conformance_holds_for_memory_and_file_vault() {
    assert_secret_provider_conformance(MemorySecretProvider::default);
    let dir = tempfile::tempdir().expect("tempdir");
    assert_secret_provider_conformance(|| {
        let p = dir.path().join(format!("vault-{}.json", fastrand_u64()));
        FileVaultProvider::open(p).expect("open vault")
    });
}

fn fastrand_u64() -> u64 {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("rng");
    u64::from_le_bytes(b)
}

#[test]
fn file_vault_ensure_secret_is_a_compare_and_set_under_contention() {
    const RACERS: usize = 8;
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = FileVaultProvider::open(dir.path().join("acme.json")).expect("open vault");
    let barrier = std::sync::Barrier::new(RACERS);
    let proposals: Vec<String> = (0..RACERS).map(|i| format!("proposed-{i}")).collect();
    let shared: &dyn SecretProvider = &vault;

    let observed = std::thread::scope(|scope| {
        let handles: Vec<_> = proposals
            .iter()
            .map(|proposed| {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    shared
                        .ensure_secret("SECRET_KEY", proposed)
                        .expect("concurrent ensure_secret")
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("racer thread"))
            .collect::<Vec<_>>()
    });

    let winner = observed.first().expect("observed value").clone();
    assert!(
        observed.iter().all(|value| *value == winner),
        "every racer must observe the same authoritative value, got {observed:?}"
    );
    assert!(
        proposals.contains(&winner),
        "the winner must be one generated proposal, got {winner:?}"
    );
    assert_eq!(
        vault
            .get(Namespace::Secrets, "SECRET_KEY")
            .expect("read back"),
        Some(winner.clone())
    );
    // Exactly one write: a re-opened provider sees the same single value and no temp file remains.
    let reopened = FileVaultProvider::open(dir.path().join("acme.json")).expect("reopen vault");
    assert_eq!(
        reopened
            .get(Namespace::Secrets, "SECRET_KEY")
            .expect("read reopened"),
        Some(winner)
    );
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .map(|e| e.expect("dir entry").file_name())
        .collect();
    assert_eq!(leftovers, vec![std::ffi::OsString::from("acme.json")]);
}

#[test]
fn file_vault_recovers_from_a_crashed_writer_temp_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("acme.json");
    std::fs::write(path.with_extension("json.tmp"), b"{ truncated").expect("stale temp");
    let vault = FileVaultProvider::open(&path).expect("open vault");
    assert_eq!(
        vault.ensure_secret("SECRET_KEY", "first").expect("ensure"),
        "first"
    );
    assert!(!path.with_extension("json.tmp").exists());
}

#[test]
fn top_level_forbidden_keys_name_only_the_key_and_never_their_values() {
    let with_secret = r#"
envs:
  django:
    SECRET_KEY: "literal-should-never-be-printed"
modes:
  local:
    envs:
      django:
        B: "2"
"#;
    match SnowFile::parse_yaml(with_secret) {
        Err(SnowError::ForbiddenTopLevelKey { key }) => assert_eq!(key, "envs"),
        other => panic!("expected ForbiddenTopLevelKey, got {other:?}"),
    }
    let Err(err) = SnowFile::parse_yaml(with_secret) else {
        panic!("top-level envs must fail closed");
    };
    assert!(format!("{err}").contains("envs"));
    assert!(format!("{err:?}").contains("envs"));
    assert!(!format!("{err:?}").contains("literal-should-never-be-printed"));
    assert!(!format!("{err}").contains("literal-should-never-be-printed"));

    // An explicitly null top-level `envs:` is still a present forbidden key, not a missing one.
    assert!(matches!(
        SnowFile::parse_yaml("envs:\n"),
        Err(SnowError::ForbiddenTopLevelKey { key }) if key == "envs"
    ));

    assert!(matches!(
        SnowFile::parse_yaml("project: acme\nmodes:\n  local: {}\n"),
        Err(SnowError::ForbiddenTopLevelKey { key }) if key == "project"
    ));
    // An explicitly null `project:` is still a present forbidden key.
    assert!(matches!(
        SnowFile::parse_yaml("project:\nmodes:\n  local: {}\n"),
        Err(SnowError::ForbiddenTopLevelKey { key }) if key == "project"
    ));

    let star = r#"
modes:
  "*":
    envs:
      django:
        A: "1"
"#;
    assert!(matches!(
        SnowFile::parse_yaml(star),
        Err(SnowError::AsteriskModeForbidden)
    ));
}

#[test]
fn r2_r4_r5_r8_modes_all_merge_write_once_secret_and_redaction() {
    let yaml = r#"
images:
  - id: api
    context: api
components:
  - id: api
    image: api
    envs: [django]
resources:
  pg_clusters: [{ id: psql, version: "18" }]
  pg_databases: [{ id: psql, cluster: psql }]
  s3_buckets: [{ id: media }]
modes:
  all:
    envs:
      django:
        SECRET_KEY: '${{ secrets::SECRET_KEY !! random_string(50) }}'
        DATABASE_URL: '${{ resources::pg_databases.psql.url }}'
        LOG_LEVEL: '${{ vars::LOG_LEVEL ?? "INFO" }}'
        OVERRIDE_ME: 'from-all'
  local:
    envs:
      django:
        OVERRIDE_ME: 'from-local'
        AWS_S3_ENDPOINT_URL: '${{ resources::s3_buckets.media.endpoint_url }}'
  cloud:
    envs:
      django:
        OVERRIDE_ME: 'from-cloud'
        SENTRY_DSN: '${{ vars::SENTRY_DSN }}'
"#;
    let snow = SnowFile::parse_yaml(yaml).expect("valid snow.yml");
    let coords = canonical_coords(&snow);
    let dir = tempfile::tempdir().expect("tempdir");
    let vault_path = dir.path().join("acme.json");
    let vault = FileVaultProvider::open(&vault_path).expect("open vault");
    let entropy = FixedCounterEntropy::new(1);

    let resolved1 = snow
        .resolve_mode("local", Some("api"), &vault, &coords, &entropy)
        .expect("resolve local");

    let secret_key = resolved1.get("SECRET_KEY").expect("SECRET_KEY");
    assert_eq!(secret_key.expose().len(), 50);
    assert!(secret_key.is_secret());
    // R8: Debug and Display MUST redact secrets!
    assert_eq!(format!("{secret_key}"), "[REDACTED]");
    assert_eq!(format!("{secret_key:?}"), "SecretValue([REDACTED])");
    assert!(!format!("{secret_key:?}").contains(secret_key.expose()));

    assert_eq!(resolved1["OVERRIDE_ME"].expose(), "from-local");
    assert_eq!(resolved1["LOG_LEVEL"].expose(), "INFO");
    assert_eq!(
        resolved1["DATABASE_URL"].expose(),
        "postgres://127.0.0.1:5432/psql"
    );
    assert_eq!(
        resolved1["AWS_S3_ENDPOINT_URL"].expose(),
        "http://127.0.0.1:9000"
    );

    // Re-open vault from disk with a DIFFERENT entropy stream: SECRET_KEY stays identical!
    let vault2 = FileVaultProvider::open(&vault_path).expect("reopen vault");
    let entropy2 = FixedCounterEntropy::new(99);
    let resolved2 = snow
        .resolve_mode("local", Some("api"), &vault2, &coords, &entropy2)
        .expect("re-resolve local");
    assert_eq!(resolved2["SECRET_KEY"].expose(), secret_key.expose());

    // Unix permissions on vault file must be 0600.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&vault_path)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn coordinates_and_s3_debug_redact_every_secret() {
    let s3 = S3Coordinates {
        endpoint_url: "https://s3.example".to_owned(),
        bucket: "media".to_owned(),
        access_key: "access".to_owned(),
        secret_key: "coord-secret-must-not-leak".to_owned(),
        region: "us-east-1".to_owned(),
    };
    let s3_debug = format!("{s3:?}");
    assert!(s3_debug.contains("[REDACTED]"));
    assert!(!s3_debug.contains("coord-secret-must-not-leak"));

    let coords = ResourceCoordinates {
        app_url: "http://acme.localhost".to_owned(),
        app_domain: "acme.localhost".to_owned(),
        env_name: "local".to_owned(),
        is_production: false,
        pg_urls: BTreeMap::new(),
        redis_urls: BTreeMap::new(),
        s3_endpoints: BTreeMap::from([("media".to_owned(), s3)]),
        component_urls: BTreeMap::new(),
    };
    let coords_debug = format!("{coords:?}");
    assert!(coords_debug.contains("[REDACTED]"));
    assert!(!coords_debug.contains("coord-secret-must-not-leak"));

    let value = SecretValue::secret("value-secret-must-not-leak");
    assert_eq!(format!("{value:?}"), "SecretValue([REDACTED])");
    assert!(!format!("{value:?}").contains("value-secret-must-not-leak"));

    // A provider's `Debug` must never expose the store: no `{:?}` path may print plaintext.
    let provider = MemorySecretProvider::new();
    provider.insert(
        Namespace::Secrets,
        "SECRET_KEY",
        "provider-secret-must-not-leak",
    );
    assert_eq!(format!("{provider:?}"), "MemorySecretProvider(<redacted>)");
    assert!(!format!("{provider:?}").contains("provider-secret-must-not-leak"));
    let layered = LayeredSecretProvider::new(provider, MemorySecretProvider::new());
    assert!(!format!("{layered:?}").contains("provider-secret-must-not-leak"));
}

#[test]
fn r6_r7_rejects_non_secret_ensure_and_collects_all_missing_keys_at_once() {
    let bad_ensure = r#"
modes:
  local:
    envs:
      django:
        BAD: '${{ vars::BAD !! random_string(16) }}'
"#;
    let snow_bad = SnowFile::parse_yaml(bad_ensure).expect("parse");
    let coords = canonical_coords(&snow_bad);
    let mem = MemorySecretProvider::new();
    let rng = FixedCounterEntropy::new(1);
    assert!(matches!(
        snow_bad.resolve_mode("local", None, &mem, &coords, &rng),
        Err(SnowError::EnsureOnlyAllowedOnSecrets(_))
    ));

    let missing_yaml = r#"
modes:
  all:
    envs:
      django:
        A: '${{ secrets::STRIPE_KEY }}'
  local:
    envs:
      django:
        B: '${{ vars::MAP_STYLE }}'
"#;
    let snow_missing = SnowFile::parse_yaml(missing_yaml).expect("parse");
    let err = snow_missing
        .resolve_mode("local", None, &mem, &coords, &rng)
        .expect_err("must fail closed on missing values");
    match err {
        SnowError::MissingValues { missing, .. } => {
            assert_eq!(
                missing.len(),
                2,
                "must report ALL missing values in a single pass"
            );
            assert_eq!(missing[0].expression, "secrets::STRIPE_KEY");
            assert_eq!(missing[1].expression, "vars::MAP_STYLE");
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn layered_provider_reads_primary_then_fallback_and_writes_primary_only() {
    let local = MemorySecretProvider::new();
    let remote = MemorySecretProvider::new();
    remote.insert(Namespace::Secrets, "SHARED_API_KEY", "remote-shared-secret");
    let layered = LayeredSecretProvider::new(local, remote.clone());

    assert_eq!(
        layered
            .get(Namespace::Secrets, "SHARED_API_KEY")
            .expect("get")
            .as_deref(),
        Some("remote-shared-secret")
    );
    let gen_val = layered
        .ensure_secret("LOCAL_SECRET", "generated-locally")
        .expect("ensure");
    assert_eq!(gen_val, "generated-locally");
    // Remote provider was never mutated!
    assert_eq!(
        remote
            .get(Namespace::Secrets, "LOCAL_SECRET")
            .expect("remote untouched"),
        None
    );

    // A remote-only secret converges without writing anything locally.
    let local = MemorySecretProvider::new();
    let layered = LayeredSecretProvider::new(local.clone(), remote.clone());
    assert_eq!(
        layered
            .ensure_secret("SHARED_API_KEY", "freshly-generated")
            .expect("converge"),
        "remote-shared-secret"
    );
    assert_eq!(
        local
            .get(Namespace::Secrets, "SHARED_API_KEY")
            .expect("local untouched"),
        None
    );
}

#[test]
fn local_for_world_and_for_world_coordinates_hold() {
    let yaml = r#"
images:
  - id: api
    context: api
components:
  - id: api
    image: api
domains:
  - id: admin
  - id: docs
resources:
  s3_buckets: [{ id: media }]
modes:
  local: {}
"#;
    let snow = SnowFile::parse_yaml(yaml).expect("valid snow.yml");
    let project = project_slug("acme");
    let coords = canonical_coords(&snow);
    assert_eq!(coords.app_domain, "acme.localhost");
    assert_eq!(coords.app_url, "http://acme.localhost");

    let fork_coords = ResourceCoordinates::local_for_world(
        &snow,
        &project,
        &WorldTarget::UserFork("feat-auth".to_owned()),
    )
    .expect("valid fork coordinates");
    assert_eq!(fork_coords.app_domain, "feat-auth.acme.localhost");
    assert_eq!(fork_coords.app_url, "http://feat-auth.acme.localhost");

    // `local_for_world` wires `snow.domain_ids()` into `slipfork-world` and propagates `SnowError::World`:
    assert!(matches!(
        ResourceCoordinates::local_for_world(
            &snow,
            &project,
            &WorldTarget::UserFork("docs".to_owned())
        ),
        Err(SnowError::World(WorldError::InvalidForkName { .. }))
    ));

    // Coordinator-minted ephemeral test slots format cleanly through `local_for_world`:
    let slot_coords =
        ResourceCoordinates::local_for_world(&snow, &project, &WorldTarget::TestSlot(3))
            .expect("test slot coordinates");
    assert_eq!(slot_coords.app_domain, "test-3.acme.localhost");
    assert_eq!(slot_coords.app_url, "http://test-3.acme.localhost");

    // `for_world` renders `app_url`/`app_domain` strictly from the world's single `WorldOrigin`.
    let target = WorldTarget::Canonical;
    let route = LocalHostRoute::new(project.clone(), target.clone(), None, &snow.domain_ids())
        .expect("canonical route");
    let port_origin = WorldOrigin::local(&route, 8080).expect("port-qualified origin");
    let port_coords =
        ResourceCoordinates::for_world(&snow, &project, &target, &port_origin, &BTreeMap::new())
            .expect("port-qualified coordinates");
    assert_eq!(port_coords.app_url, "http://acme.localhost:8080");
    assert_eq!(port_coords.app_domain, "acme.localhost");
    assert_eq!(
        port_coords.resolve("resources", &["app".to_owned(), "url".to_owned()]),
        Some(("http://acme.localhost:8080".to_owned(), false))
    );

    let shared_origin = WorldOrigin::shared("https://acme.tunnel.example:8443").expect("shared");
    let shared_coords =
        ResourceCoordinates::for_world(&snow, &project, &target, &shared_origin, &BTreeMap::new())
            .expect("shared coordinates");
    assert_eq!(shared_coords.app_url, "https://acme.tunnel.example:8443");
    assert_eq!(shared_coords.app_domain, "acme.tunnel.example");

    // `ResourcePlan` overrides replace bucket coordinates; undeclared buckets fail closed.
    let overrides = BTreeMap::from([(
        "media".to_owned(),
        S3Coordinates {
            endpoint_url: "https://s3.example".to_owned(),
            bucket: "acme-media".to_owned(),
            access_key: "access".to_owned(),
            secret_key: "override-secret".to_owned(),
            region: "eu-west-1".to_owned(),
        },
    )]);
    let override_coords =
        ResourceCoordinates::for_world(&snow, &project, &target, &port_origin, &overrides)
            .expect("override coordinates");
    assert_eq!(
        override_coords.s3_endpoints["media"].endpoint_url,
        "https://s3.example"
    );
    assert!(matches!(
        ResourceCoordinates::for_world(
            &snow,
            &project,
            &target,
            &port_origin,
            &BTreeMap::from([(
                "ghost".to_owned(),
                S3Coordinates {
                    endpoint_url: "https://ghost.example".to_owned(),
                    bucket: "ghost".to_owned(),
                    access_key: "a".to_owned(),
                    secret_key: "b".to_owned(),
                    region: "c".to_owned(),
                },
            )])
        ),
        Err(SnowError::UnknownS3Bucket(bucket)) if bucket == "ghost"
    ));

    // `domains[].id` may never claim the reserved `test-*` namespace or violate DNS-label rules.
    let reserved_domain = r#"
domains:
  - id: test-3
modes:
  local: {}
"#;
    assert!(matches!(
        SnowFile::parse_yaml(reserved_domain),
        Err(SnowError::Validation(_))
    ));

    let invalid_domain = r#"
domains:
  - id: admin.sub
modes:
  local: {}
"#;
    match SnowFile::parse_yaml(invalid_domain) {
        Err(SnowError::Validation(errs)) => {
            assert!(
                errs.iter().any(|e| e.contains(DNS_LABEL_REASON)),
                "expected shared DNS_LABEL_REASON in validation errors, got {errs:?}"
            );
        }
        other => panic!("expected Validation error for dotted domain id, got {other:?}"),
    }
}

#[test]
fn mode_component_add_remove_override_resolves_through_one_path() {
    let yaml = r#"
images:
  - id: api
    context: api
  - id: worker
    context: api
components:
  - id: api
    image: api
    run_command: gunicorn app.wsgi:application --bind 0.0.0.0:8000
  - id: worker
    image: worker
    run_command: celery -A app worker --loglevel=info
modes:
  all: {}
  cloud:
    components:
      worker:
        disabled: true
  local:
    components:
      api:
        run_command: blastit dev -- uv run uvicorn app.asgi:application --port 8000
      frontend:
        run_command: pnpm dev
"#;
    let snow = SnowFile::parse_yaml(yaml).expect("parse");
    assert_eq!(
        snow.effective_run_command("cloud", "api")
            .expect("cloud api"),
        "gunicorn app.wsgi:application --bind 0.0.0.0:8000"
    );
    assert_eq!(
        snow.effective_run_command("local", "api")
            .expect("local api"),
        "blastit dev -- uv run uvicorn app.asgi:application --port 8000"
    );
    assert_eq!(
        snow.effective_run_command("local", "worker")
            .expect("local worker inherits top-level components[]"),
        "celery -A app worker --loglevel=info"
    );
    // Mode-only additions are additive per mode and ordered after the declared components.
    assert_eq!(
        snow.effective_run_command("local", "frontend")
            .expect("mode-only addition"),
        "pnpm dev"
    );
    let local = snow
        .effective_components("local")
        .expect("local components");
    let ids: Vec<&str> = local.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, vec!["api", "worker", "frontend"]);
    assert!(!local.iter().any(|(_, comp)| comp.disabled));
    assert!(
        snow.effective_components("cloud")
            .expect("cloud components")
            .iter()
            .all(|(id, _)| id != "frontend"),
        "a mode-only addition must not leak into another mode"
    );

    // `disabled: true` is the sole removal spelling and keeps the entry visible as disabled.
    let cloud = snow
        .effective_components("cloud")
        .expect("cloud components");
    let (_, worker) = cloud
        .iter()
        .find(|(id, _)| id == "worker")
        .expect("worker resolved");
    assert!(worker.disabled);
    assert!(matches!(
        snow.effective_run_command("cloud", "worker"),
        Err(SnowError::Validation(_))
    ));
    assert!(matches!(
        snow.effective_run_command("cloud", "frontend"),
        Err(SnowError::Validation(_))
    ));
    assert!(matches!(
        snow.effective_components("staging"),
        Err(SnowError::UnknownMode { .. })
    ));

    let all_comp_forbidden = r#"
images:
  - id: api
    context: api
components:
  - id: api
    image: api
modes:
  all:
    components:
      api:
        run_command: echo redundant
  local: {}
"#;
    assert!(matches!(
        SnowFile::parse_yaml(all_comp_forbidden),
        Err(SnowError::Validation(_))
    ));

    // `disabled: true` on `modes.all.components` is refused too: `all` never removes components.
    let all_disabled_forbidden = r#"
images:
  - id: api
    context: api
components:
  - id: api
    image: api
modes:
  all:
    components:
      api:
        disabled: true
  local: {}
"#;
    assert!(matches!(
        SnowFile::parse_yaml(all_disabled_forbidden),
        Err(SnowError::Validation(_))
    ));

    // A mode-only id is an addition, so it must carry a non-empty `run_command` (fail closed).
    let addition_without_command = r#"
images:
  - id: api
    context: api
components:
  - id: api
    image: api
modes:
  local:
    components:
      ghost:
        disabled: true
"#;
    assert!(matches!(
        SnowFile::parse_yaml(addition_without_command),
        Err(SnowError::Validation(_))
    ));

    // Unknown override fields fail closed at deserialization (`deny_unknown_fields`).
    let unknown_override_field = r#"
images:
  - id: api
    context: api
components:
  - id: api
    image: api
modes:
  local:
    components:
      api:
        command: echo typo
"#;
    assert!(matches!(
        SnowFile::parse_yaml(unknown_override_field),
        Err(SnowError::Yaml(_))
    ));

    for removed_expr in [
        "resources::pg_cluster.psql.url",
        "components::api.public_url",
        "components::api.private_port", // api has neither ingress nor internal_ports here -> fail closed!
    ] {
        let bad_alias = format!(
            r#"
images:
  - id: api
    context: api
components:
  - id: api
    image: api
    envs: [app]
resources:
  pg_clusters: [{{ id: psql, version: "18" }}]
  pg_databases: [{{ id: psql, cluster: psql }}]
modes:
  local:
    envs:
      app:
        BAD: '${{{{ {removed_expr} }}}}'
"#
        );
        let snow_alias = SnowFile::parse_yaml(&bad_alias).expect("parse");
        let coords = canonical_coords(&snow_alias);
        let mem = MemorySecretProvider::new();
        let rng = FixedCounterEntropy::new(1);
        assert!(matches!(
            snow_alias.resolve_mode("local", Some("api"), &mem, &coords, &rng),
            Err(SnowError::UnknownResourcePath(_))
        ));
    }
}

#[test]
fn whole_mode_resolution_skips_disabled_component_env_groups() {
    let yaml = r#"
images:
  - id: api
    context: api
  - id: worker
    context: api
components:
  - id: api
    image: api
    envs: [app]
  - id: worker
    image: worker
    envs: [jobs]
modes:
  all:
    envs:
      app:
        API_KEY: 'api-value'
      jobs:
        WORKER_KEY: 'worker-value'
      ops:
        OPS_TOKEN: 'ops-value'
  local: {}
  cloud:
    components:
      worker:
        disabled: true
"#;
    let snow = SnowFile::parse_yaml(yaml).expect("parse");
    let coords = canonical_coords(&snow);
    let mem = MemorySecretProvider::new();
    let rng = FixedCounterEntropy::new(1);

    let cloud = snow
        .resolve_mode("cloud", None, &mem, &coords, &rng)
        .expect("cloud sweep");
    assert!(cloud.contains_key("API_KEY"));
    assert!(
        !cloud.contains_key("WORKER_KEY"),
        "a disabled component's env groups must not resolve in a whole-mode sweep"
    );
    // Groups no component references (for example image build envs) still resolve.
    assert!(cloud.contains_key("OPS_TOKEN"));

    let local = snow
        .resolve_mode("local", None, &mem, &coords, &rng)
        .expect("local sweep");
    assert!(local.contains_key("WORKER_KEY"));
    assert!(local.contains_key("API_KEY"));

    // Naming the disabled component explicitly still fails closed.
    assert!(matches!(
        snow.resolve_mode("cloud", Some("worker"), &mem, &coords, &rng),
        Err(SnowError::Validation(_))
    ));
}
