//! Protected contract for the canonical world projection on [`EvaluatedFork`].
//!
//! These assertions were ported from the retired manifest-engine critical suite: `!!`/`??`
//! secret-taint precedence, feature inheritance, resource ids, and one single-origin world view.
//! Snow-only YAML/mode internals were dropped rather than copied.

use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};
use std::thread;

use slipfork_forkfile::{
    DeploymentFacts, EvaluatedFork, FeatureSet, FileVaultProvider, Namespace, OsCsprngEntropy,
    Resolvers, SecretProvider, TaintedString, parse_forkfile,
};

const WORLD: &str = "\
project acme

features {
    default = [dev]
    dev     = [mail]
    mail
}

postgres pg {
    version 17
    database app_db
    database analytics
}

redis cache
s3 media
domain docs

env base {
    LOG_LEVEL    = \"info\"
    OVERRIDE_ME  = \"from-base\"
    SECRET_KEY   = secret(SECRET_KEY) ?? random_string(50)
    MISSING      = secret(ABSENT) ?? \"literal\"
    FALLBACK     = var(ABSENT) ?? \"off\"
    DATABASE_URL = app_db.url
}

env strict {
    OVERRIDE_ME = \"from-strict\"
}

service api ./api {
    use env base
    use env strict
    port 8000
    route /api
    build_env {
        BUILD_FLAG  = \"release\"
        BUILD_TOKEN = secret(BUILD_TOKEN) ?? random_hex(16)
    }
    setup: /bin/true
    run /bin/true
}

on mail {
    service mailpit from axllent/mailpit:latest {
        port 8025
        route mail.*
    }
}
";

const DEPLOYMENT: &str = "\
project acme
features {}

service api ./api {
    env {
        SHA  = deployment.commit_sha
        WHEN = deployment.date_event
    }
    run /bin/true
}
";

fn evaluated(features: &FeatureSet) -> EvaluatedFork {
    parse_forkfile(WORLD)
        .expect("the world manifest is valid")
        .evaluate(features)
        .expect("the active feature set evaluates")
}

fn scratch(label: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(label)
        .tempdir_in("/var/tmp")
        .expect("disk scratch")
}

fn vault_in(dir: &tempfile::TempDir) -> FileVaultProvider {
    FileVaultProvider::open(dir.path().join("vault.json")).expect("open vault")
}

fn fact(fork: &EvaluatedFork, entity: &str, property: &str) -> String {
    fork.world_facts()
        .into_iter()
        .find(|entry| entry.entity == entity && entry.property == property)
        .unwrap_or_else(|| panic!("`{entity}.{property}` must be published"))
        .value
        .expose()
        .to_owned()
}

#[test]
fn the_projection_is_one_pure_view_of_the_active_feature_set() {
    let fork = evaluated(&FeatureSet::new());

    // Coordinates come from the manifest alone: loopback backing services, `.localhost` names.
    assert_eq!(
        fact(&fork, "app_db", "url"),
        "postgres://slipfork@127.0.0.1:5432/app_db"
    );
    assert_eq!(
        fact(&fork, "analytics", "jdbc"),
        "jdbc:postgresql://127.0.0.1:5432/analytics"
    );
    assert_eq!(fact(&fork, "cache", "url"), "redis://127.0.0.1:6379");
    assert_eq!(fact(&fork, "media", "endpoint"), "http://127.0.0.1:9000");
    assert_eq!(fact(&fork, "app", "url"), "http://acme.localhost");
    assert_eq!(fact(&fork, "docs", "name"), "docs.acme.localhost");
    assert_eq!(
        fact(&fork, "api", "private_url"),
        "http://api.acme.localhost"
    );
    assert_eq!(
        fact(&fork, "api", "public_url"),
        "http://acme.localhost/api"
    );
    assert_eq!(
        fact(&fork, "mailpit", "public_url"),
        "http://mail.acme.localhost",
        "a `sub.*` route publishes a subdomain, not a path"
    );
    assert!(
        fork.world_facts()
            .iter()
            .all(|entry| !entry.property.contains("password")),
        "secret properties never appear in a projection"
    );

    // Resource ids and versions are the whole declaration the daemon plans from.
    let databases: Vec<(String, String, String, bool)> = fork
        .pg_databases()
        .into_iter()
        .map(|database| {
            (
                database.cluster,
                database.version,
                database.database,
                database.pool,
            )
        })
        .collect();
    assert_eq!(
        databases,
        [
            ("pg".to_owned(), "17".to_owned(), "app_db".to_owned(), true),
            (
                "pg".to_owned(),
                "17".to_owned(),
                "analytics".to_owned(),
                true
            ),
        ]
    );

    let routes: Vec<(String, Option<u32>, String)> = fork
        .ingress_routes()
        .into_iter()
        .map(|route| (route.service, route.port, route.route))
        .collect();
    assert_eq!(
        routes,
        [
            ("api".to_owned(), Some(8000), "/api".to_owned()),
            ("mailpit".to_owned(), Some(8025), "mail.*".to_owned()),
        ]
    );

    let images = fork.build_images();
    assert_eq!(images.len(), 1, "`from <image>` needs no build stage");
    assert_eq!(images[0].service, "api");
    assert_eq!(images[0].context_dir, "api");
    assert_eq!(images[0].dockerfile, "Dockerfile");
    assert_eq!(images[0].build_env, ["BUILD_FLAG", "BUILD_TOKEN"]);

    // The world base env is the resolved union of declared groups; a later group wins.
    let dir = scratch("forkfile-world-env");
    let vault = vault_in(&dir);
    let resolvers = Resolvers::new(&vault, &OsCsprngEntropy);
    let world_env = fork
        .world_env(&resolvers)
        .expect("the world base env resolves");
    assert_eq!(world_env["LOG_LEVEL"].expose(), "info");
    assert_eq!(
        world_env["OVERRIDE_ME"].expose(),
        "from-strict",
        "the later `env` group wins the same key"
    );
    assert_eq!(
        world_env["DATABASE_URL"].expose(),
        "postgres://slipfork@127.0.0.1:5432/app_db",
        "the base env is evaluated text, never `Forkfile` source"
    );
    assert_eq!(
        world_env["MISSING"].expose(),
        "literal",
        "a `secret(...) ??` literal fallback is plain text"
    );
    assert_eq!(
        world_env["FALLBACK"].expose(),
        "off",
        "a `var(...) ??` fallback is evaluated, not rendered"
    );
    assert!(world_env["SECRET_KEY"].is_tainted());
    assert_eq!(
        world_env.len(),
        6,
        "every declared group key is present exactly once"
    );
}

#[test]
fn build_env_resolves_through_the_same_projection_as_runtime_env() {
    let fork = evaluated(&FeatureSet::new());
    let dir = scratch("forkfile-world");
    let vault = vault_in(&dir);
    let resolvers = Resolvers::new(&vault, &OsCsprngEntropy);

    let build_env = fork
        .resolve_build_env("api", &resolvers)
        .expect("the build environment resolves");
    assert_eq!(build_env["BUILD_FLAG"].expose(), "release");
    assert!(!build_env["BUILD_FLAG"].is_tainted());
    assert!(
        build_env["BUILD_TOKEN"].is_tainted(),
        "a generated build-env secret stays tainted"
    );
    assert_eq!(build_env["BUILD_TOKEN"].redacted(), "[REDACTED]");

    let runtime_env = fork
        .resolve_env("api", &resolvers)
        .expect("the runtime environment resolves");
    assert_eq!(
        runtime_env["DATABASE_URL"].expose(),
        fact(&fork, "app_db", "url"),
        "runtime and build references read one fact table"
    );
    assert_eq!(
        runtime_env["OVERRIDE_ME"].expose(),
        "from-strict",
        "`use env` layering keeps the later group's value"
    );

    let pulled = fork
        .resolve_build_env("mailpit", &resolvers)
        .expect_err("an OCI image has no build environment");
    assert!(pulled[0].message.contains("pulled from an OCI image"));
}

#[test]
fn secret_taint_and_write_once_generation_never_leak() {
    let fork = evaluated(&FeatureSet::new());
    assert_eq!(
        fork.generated_secret_keys(),
        BTreeSet::from(["BUILD_TOKEN".to_owned(), "SECRET_KEY".to_owned()]),
        "runtime and build-env generators are both pre-warmed"
    );

    let dir = scratch("forkfile-secret");
    let vault_path = dir.path().join("vault.json");
    let vault = FileVaultProvider::open(&vault_path).expect("open vault");
    let resolvers = Resolvers::new(&vault, &OsCsprngEntropy);

    let first = fork.resolve_env("api", &resolvers).expect("first resolve");
    let secret: &TaintedString = &first["SECRET_KEY"];
    assert_eq!(secret.expose().len(), 50, "`random_string(50)` is 50 chars");
    assert!(secret.is_tainted(), "`secret(...) ?? <gen>` stays tainted");
    assert_eq!(secret.redacted(), "[REDACTED]");
    assert_eq!(format!("{secret:?}"), "TaintedString([REDACTED])");
    assert!(!format!("{secret:?}").contains(secret.expose()));

    // Write-once: a second resolution (and a reopened vault) returns the stored value.
    let second = fork.resolve_env("api", &resolvers).expect("second resolve");
    assert_eq!(second["SECRET_KEY"].expose(), secret.expose());
    let reopened = FileVaultProvider::open(&vault_path).expect("reopen vault");
    assert_eq!(
        reopened
            .get(Namespace::Secrets, "SECRET_KEY")
            .expect("read back")
            .as_deref(),
        Some(secret.expose())
    );

    // A `??` literal fallback is a plain value, not a secret.
    assert!(
        !first["OVERRIDE_ME"].is_tainted(),
        "only a resolved secret taints a value"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&vault_path)
            .expect("vault metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "the local vault stays 0600");
    }
}

#[test]
fn feature_inheritance_selects_the_projected_world() {
    let fork = evaluated(&FeatureSet::new());
    assert_eq!(fork.features, ["default", "dev", "mail"]);
    assert!(
        fork.services.iter().any(|service| service.id == "mailpit"),
        "`on mail` is active through the transitive `default = [dev]` closure"
    );

    let bare = evaluated(&FeatureSet::new().without_default_features());
    assert!(bare.features.is_empty());
    assert!(
        !bare.services.iter().any(|service| service.id == "mailpit"),
        "an inactive branch contributes nothing to the projection"
    );
    assert_eq!(
        bare.ingress_routes().len(),
        1,
        "only the unconditional service"
    );
    assert_eq!(
        bare.pg_databases().len(),
        2,
        "unconditional resources survive every feature set"
    );
}

#[test]
fn deployment_facts_are_the_fourth_precedence_tier() {
    let fork = parse_forkfile(DEPLOYMENT)
        .expect("valid deployment manifest")
        .evaluate(&FeatureSet::new())
        .expect("evaluates");
    let dir = scratch("forkfile-deployment");
    let vault = vault_in(&dir);
    let bare = Resolvers::new(&vault, &OsCsprngEntropy);
    let unresolved = fork
        .resolve_env("api", &bare)
        .expect_err("without deployment facts the reference is unavailable");
    assert!(
        unresolved[0]
            .hint
            .as_deref()
            .is_some_and(|hint| hint.contains("with_deployment")),
        "the remedy names the injection point: {unresolved:?}"
    );

    let facts = DeploymentFacts {
        commit_sha: Some("c0ffee1234".to_owned()),
        date_event: Some("2026-11-19T20:40:00Z".to_owned()),
        env_branch_name: Some("main".to_owned()),
        ..DeploymentFacts::default()
    };
    let resolvers = Resolvers::new(&vault, &OsCsprngEntropy).with_deployment(facts);
    let env = fork
        .resolve_env("api", &resolvers)
        .expect("deployment resolves");
    assert_eq!(env["SHA"].expose(), "c0ffee1234");
    assert_eq!(env["WHEN"].expose(), "2026-11-19T20:40:00Z");
    assert!(
        !env["SHA"].is_tainted(),
        "deployment metadata is not a secret"
    );
}

#[test]
fn the_local_vault_is_send_sync_and_never_leaks_through_debug() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<FileVaultProvider>();
    assert_send_sync::<OsCsprngEntropy>();

    let dir = scratch("forkfile-vault-debug");
    let provider = vault_in(&dir);
    provider
        .ensure_secret("LEAKY", "hunter2-value")
        .expect("store");
    let debug = format!("{provider:?}");
    assert!(
        !debug.contains("hunter2-value"),
        "the vault Debug leaked a stored secret: {debug}"
    );
    assert_eq!(
        SecretProvider::get(&provider, Namespace::Secrets, "LEAKY")
            .expect("trait read")
            .as_deref(),
        Some("hunter2-value")
    );

    let tainted = TaintedString::tainted("hunter2-value");
    assert!(!format!("{tainted:?}").contains("hunter2-value"));
    assert_eq!(tainted.redacted(), "[REDACTED]");
}

#[test]
fn concurrent_compare_and_set_elects_one_writer() {
    let dir = scratch("forkfile-vault-race");
    let provider = Arc::new(vault_in(&dir));
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|index| {
            let provider = Arc::clone(&provider);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                provider
                    .ensure_secret("RACE_KEY", &format!("value-{index}"))
                    .expect("compare-and-set")
            })
        })
        .collect();
    let values: BTreeSet<String> = handles
        .into_iter()
        .map(|handle| handle.join().expect("racing thread"))
        .collect();
    assert_eq!(values.len(), 1, "exactly one racer writes: {values:?}");
    assert_eq!(
        provider
            .get(Namespace::Secrets, "RACE_KEY")
            .expect("read back")
            .as_deref(),
        values.iter().next().map(String::as_str)
    );
}

#[test]
fn a_crashed_writer_temp_file_is_recovered() {
    let dir = scratch("forkfile-vault-tmp");
    let path = dir.path().join("vault.json");
    std::fs::write(dir.path().join("vault.json.tmp"), b"{\"broken\":").expect("stale temp file");
    let provider = FileVaultProvider::open(&path).expect("open vault");
    provider
        .put(Namespace::Vars, "RECOVERED", "yes")
        .expect("write past a stale temp file");
    assert_eq!(
        provider
            .get(Namespace::Vars, "RECOVERED")
            .expect("read back")
            .as_deref(),
        Some("yes")
    );
    assert!(
        !path.with_extension("json.tmp").exists(),
        "the crashed writer's temp file is cleared"
    );
}
