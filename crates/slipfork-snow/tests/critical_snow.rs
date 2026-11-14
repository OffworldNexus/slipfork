//! Protected critical tests for `slipfork-snow` (`R1..R8`).

use slipfork_snow::conformance::assert_secret_provider_conformance;
use slipfork_snow::{
    EntropySource, FileVaultProvider, LayeredSecretProvider, MemorySecretProvider, Namespace,
    ProviderError, ResourceCoordinates, SecretProvider, SnowError, SnowFile,
};

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
fn r1_r3_rejects_envs_plus_modes_and_asterisk_mode() {
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
        Err(SnowError::EnvsAndModesMutuallyExclusive)
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
    let coords = ResourceCoordinates::local_defaults(&snow, "acme");
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
    let coords = ResourceCoordinates::local_defaults(&snow_bad, "acme");
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
