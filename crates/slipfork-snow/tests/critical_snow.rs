//! Protected critical tests for `slipfork-snow` (`R1..R8`).

use slipfork_snow::conformance::assert_secret_provider_conformance;
use slipfork_snow::{
    EntropySource, FileVaultProvider, LayeredSecretProvider, MemorySecretProvider, Namespace,
    ProviderError, ResourceCoordinates, SecretProvider, SnowError, SnowFile,
};
use slipfork_world::{DNS_LABEL_REASON, ProjectSlug, WorldError, WorldTarget};

fn project_slug(slug: &str) -> ProjectSlug {
    ProjectSlug::parse(slug).expect("valid project slug")
}

fn canonical_coords(snow: &SnowFile) -> ResourceCoordinates {
    ResourceCoordinates::local_for_world(snow, &project_slug("acme"), &WorldTarget::Canonical)
        .expect("canonical coordinates")
}

struct FixedCounterEntropy(u8);

impl EntropySource for FixedCounterEntropy {
    fn fill_bytes(&mut self, buf: &mut [u8]) -> Result<(), ProviderError> {
        for b in buf {
            *b = self.0;
            self.0 = self.0.wrapping_add(7);
        }
        Ok(())
    }
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
fn top_level_envs_and_asterisk_mode_are_rejected() {
    let both = r#"
envs:
  django:
    A: "1"
modes:
  local:
    envs:
      django:
        B: "2"
"#;
    assert!(matches!(
        SnowFile::parse_yaml(both),
        Err(SnowError::TopLevelEnvsUnsupported)
    ));

    let top_level_envs_only = r#"
envs:
  django:
    A: "1"
"#;
    assert!(matches!(
        SnowFile::parse_yaml(top_level_envs_only),
        Err(SnowError::TopLevelEnvsUnsupported)
    ));

    // An explicitly null top-level `envs:` is still a present forbidden key, not a missing one.
    assert!(matches!(
        SnowFile::parse_yaml("envs:\n"),
        Err(SnowError::TopLevelEnvsUnsupported)
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
    let mut vault = FileVaultProvider::open(&vault_path).expect("open vault");
    let mut entropy = FixedCounterEntropy(1);

    let resolved1 = snow
        .resolve_mode("local", Some("api"), &mut vault, &coords, &mut entropy)
        .expect("resolve local");

    let secret_key = resolved1.get("SECRET_KEY").expect("SECRET_KEY");
    assert_eq!(secret_key.expose().len(), 50);
    assert!(secret_key.is_secret());
    // R8: Debug and Display MUST redact secrets!
    assert_eq!(format!("{secret_key}"), "[REDACTED]");
    assert!(format!("{secret_key:?}").contains("[REDACTED]"));
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
    let mut vault2 = FileVaultProvider::open(&vault_path).expect("reopen vault");
    let mut entropy2 = FixedCounterEntropy(99);
    let resolved2 = snow
        .resolve_mode("local", Some("api"), &mut vault2, &coords, &mut entropy2)
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
    let mut mem = MemorySecretProvider::default();
    let mut rng = FixedCounterEntropy(1);
    assert!(matches!(
        snow_bad.resolve_mode("local", None, &mut mem, &coords, &mut rng),
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
        .resolve_mode("local", None, &mut mem, &coords, &mut rng)
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
fn layered_provider_reads_remote_and_persists_generated_secrets_locally_only() {
    let mut remote = MemorySecretProvider::default();
    remote.insert(Namespace::Secrets, "SHARED_API_KEY", "remote-shared-secret");
    let local = MemorySecretProvider::default();
    let mut layered = LayeredSecretProvider::new(remote.clone(), local);

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
}

#[test]
fn top_level_project_in_snow_yml_is_rejected_in_favour_of_slipfork_toml() {
    let yaml = "project: acme\nmodes:\n  local: {}\n";
    assert!(matches!(
        SnowFile::parse_yaml(yaml),
        Err(SnowError::Validation(_))
    ));
    // An explicitly null `project:` is still a present forbidden key.
    assert!(matches!(
        SnowFile::parse_yaml("project:\nmodes:\n  local: {}\n"),
        Err(SnowError::Validation(_))
    ));
}

#[test]
fn local_for_world_coordinates_world_error_propagation_and_domain_validation_hold() {
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
fn mode_component_run_command_overrides_and_unknown_component_refusal() {
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
  cloud: {}
  local:
    components:
      api:
        run_command: blastit dev -- uv run uvicorn app.asgi:application --port 8000
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

    let bad_override = r#"
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
        run_command: echo ghost
"#;
    assert!(matches!(
        SnowFile::parse_yaml(bad_override),
        Err(SnowError::Validation(_))
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
        let mut mem = MemorySecretProvider::default();
        let mut rng = FixedCounterEntropy(1);
        assert!(matches!(
            snow_alias.resolve_mode("local", Some("api"), &mut mem, &coords, &mut rng),
            Err(SnowError::UnknownResourcePath(_))
        ));
    }
}
