//! Claimed (`OSPP`) local evaluation: only locally owned `${{ ... }}` expressions resolve, every
//! other expression stays verbatim for the server, and a claimed secret taints its value.

use std::collections::BTreeSet;
use std::path::PathBuf;

use slipfork_snow::{
    ClaimedEvaluator, FileVaultProvider, Namespace, OsCsprngEntropy, SecretProvider, SnowError,
    SnowFile,
};
use slipfork_world::{ProjectSlug, WorldTarget};

fn disk_tempdir(label: &str) -> tempfile::TempDir {
    let root = PathBuf::from(std::env::var("HOME").expect("HOME is set"))
        .join(".cache")
        .join("slipfork")
        .join("test-tmp");
    std::fs::create_dir_all(&root).expect("create disk scratch root");
    tempfile::Builder::new()
        .prefix(label)
        .tempdir_in(root)
        .expect("disk tempdir")
}

fn owned(namespaces: &[(&str, &[&str])]) -> impl Fn(&str, &[String]) -> bool + use<> {
    let owned: BTreeSet<(String, String)> = namespaces
        .iter()
        .flat_map(|(namespace, keys)| {
            keys.iter()
                .map(move |key| ((*namespace).to_owned(), (*key).to_owned()))
        })
        .collect();
    move |namespace: &str, path: &[String]| {
        // A resource claim is a `kind.id` prefix: every property of a claimed resource belongs
        // to the caller that owns the resource itself.
        let key = if namespace == "resources" && path.len() >= 2 {
            path[..2].join(".")
        } else {
            path.join(".")
        };
        owned.contains(&(namespace.to_owned(), key))
    }
}

const YAML: &str = r#"
images:
  - id: api
    context: api
components:
  - id: api
    image: api
    envs: [app]
resources:
  pg_clusters: [{ id: psql, version: "18" }]
  pg_databases: [{ id: psql, cluster: psql }]
modes:
  all:
    envs:
      app:
        LOCAL: '${{ vars::LOCAL }}'
        REMOTE: '${{ vars::REMOTE }}'
        MIXED: '${{ secrets::API_KEY }}@${{ resources::pg_databases.psql.url }}'
        GENERATED: '${{ secrets::GENERATED !! random_string(16) }}'
        SERVER_GENERATED: '${{ secrets::SERVER_OWNED !! random_string(16) }}'
"#;

#[test]
fn claimed_evaluation_resolves_only_owned_expressions_and_taints_claimed_secrets() {
    let snow = SnowFile::parse_yaml(YAML).expect("valid snow.yml");
    let coords = slipfork_snow::ResourceCoordinates::local_for_world(
        &snow,
        &ProjectSlug::parse("acme").expect("slug"),
        &WorldTarget::Canonical,
    )
    .expect("coordinates");
    let _dir = disk_tempdir("snow-claimed-eval");
    let vault = FileVaultProvider::open(_dir.path().join("vault.json")).expect("open vault");
    vault
        .put(Namespace::Vars, "LOCAL", "local-value")
        .expect("put local var");
    vault
        .ensure_secret("API_KEY", "api-key-value")
        .expect("ensure api key");
    vault
        .ensure_secret("SERVER_OWNED", "server-owned-value")
        .expect("ensure server secret");

    let claims = owned(&[
        ("vars", &["LOCAL"][..]),
        ("secrets", &["API_KEY", "GENERATED"][..]),
        ("resources", &["pg_databases.psql"][..]),
    ]);
    let evaluator = ClaimedEvaluator::new(&vault, &coords, &OsCsprngEntropy, &claims);
    let resolved = evaluator
        .mode(&snow, "all", Some("api"))
        .expect("claimed resolution");

    assert_eq!(resolved["LOCAL"].expose(), "local-value");
    assert!(!resolved["LOCAL"].is_secret());
    assert_eq!(resolved["REMOTE"].expose(), "${{ vars::REMOTE }}");
    assert_eq!(
        resolved["SERVER_GENERATED"].expose(),
        "${{ secrets::SERVER_OWNED !! random_string(16) }}"
    );
    assert_eq!(
        vault.get(Namespace::Secrets, "SERVER_OWNED").expect("read"),
        Some("server-owned-value".to_owned()),
        "an unclaimed !! must never be regenerated or persisted locally"
    );

    assert!(resolved["MIXED"].is_secret());
    assert!(
        resolved["MIXED"]
            .expose()
            .starts_with("api-key-value@postgres://")
    );
    assert_eq!(
        format!("{:?}", resolved["MIXED"]),
        "SecretValue([REDACTED])"
    );

    let generated = resolved["GENERATED"].expose().to_owned();
    assert_eq!(generated.len(), 16);
    assert!(resolved["GENERATED"].is_secret());
    assert_eq!(
        vault.get(Namespace::Secrets, "GENERATED").expect("read"),
        Some(generated)
    );
}

#[test]
fn generated_secret_keys_lists_only_ensure_expressions_sorted() {
    let snow = SnowFile::parse_yaml(YAML).expect("valid snow.yml");
    assert_eq!(
        snow.generated_secret_keys("all").expect("keys"),
        vec!["GENERATED".to_owned(), "SERVER_OWNED".to_owned()]
    );
}

#[test]
fn an_owned_expression_without_a_local_value_fails_closed() {
    let snow = SnowFile::parse_yaml(YAML).expect("valid snow.yml");
    let coords = slipfork_snow::ResourceCoordinates::local_for_world(
        &snow,
        &ProjectSlug::parse("acme").expect("slug"),
        &WorldTarget::Canonical,
    )
    .expect("coordinates");
    let _dir = disk_tempdir("snow-claimed-missing");
    let vault = FileVaultProvider::open(_dir.path().join("vault.json")).expect("open vault");
    let claims = owned(&[("vars", &["LOCAL", "REMOTE"][..])]);
    let err = ClaimedEvaluator::new(&vault, &coords, &OsCsprngEntropy, &claims)
        .mode(&snow, "all", Some("api"))
        .expect_err("claimed missing value");
    assert!(matches!(err, SnowError::ClaimedValueMissing { .. }));
    assert!(err.to_string().contains("slipfork secret set"));
}

#[test]
fn an_undeclared_component_is_a_typed_error() {
    let snow = SnowFile::parse_yaml(YAML).expect("valid snow.yml");
    let coords = slipfork_snow::ResourceCoordinates::local_for_world(
        &snow,
        &ProjectSlug::parse("acme").expect("slug"),
        &WorldTarget::Canonical,
    )
    .expect("coordinates");
    let _dir = disk_tempdir("snow-claimed-component");
    let vault = FileVaultProvider::open(_dir.path().join("vault.json")).expect("open vault");
    let claims = owned(&[]);
    let err = ClaimedEvaluator::new(&vault, &coords, &OsCsprngEntropy, &claims)
        .mode(&snow, "all", Some("worker"))
        .expect_err("undeclared component");
    assert!(matches!(&err, SnowError::UnknownComponent(id) if id == "worker"));
    assert_eq!(
        err.to_string(),
        "component 'worker' is not defined in snow.yml"
    );
}
