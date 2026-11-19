//! Protected end-to-end contract for the `Forkfile` manifest engine.
//!
//! One fixture exercises parse → static checking (all branches, all errors) → canonical JSON
//! round-trip → feature evaluation → environment resolution against a real `0600` vault on disk.

use std::collections::BTreeMap;

use slipfork_forkfile::{
    ArgPart, ArgRef, ArgRefKind, EvaluatedFork, ExternalCheck, FeatureSet, FileVaultProvider,
    Namespace, OsCsprngEntropy, Resolvers, parse_forkfile,
};

const INVALID: &str = "\
features {
    default = [dev]
    dev     = [mail]
    mail
}

postgres pg {
    database app_db
}

redis cache

env common {
    DATABASE_URL = pg.url
}

features {
    staging
}

on staging {
    env staged {
        STAGED = pg.url
    }
}
";

const EXTERNAL_SOURCE: &str = "\
env common {
    KEY = secret(MISSING_KEY)
}
";

/// A slice-backed external catalog, standing in for a CI/daemon variable catalog.
struct Catalog(&'static [&'static str]);

impl ExternalCheck for Catalog {
    fn check(&self, key: &str) -> Result<(), String> {
        if self.0.contains(&key) {
            Ok(())
        } else {
            Err(format!("`{key}` is not available in this environment"))
        }
    }
}

const EXPECTED_FIRST_ERROR: &str = "\
error: `pg` is a postgres cluster, not a database
  --> Forkfile:14:20
   |
14 |     DATABASE_URL = pg.url
   |                    ^^^^^^ did you mean database `app_db.url`?";

const VALID: &str = "\
project demo
features {
    default = [dev]
    dev     = [mail]
    mail
}

postgres pg {
    database app_db
}
redis cache
domain main

env common {
    DATABASE_URL = app_db.url
    REDIS_URL    = cache.url
    SECRET_KEY   = secret(SECRET_KEY) ?? random_hex(32)
    PLAIN        = \"hello\" + \"-world\"
}

service api ./api {
    use env common
    port 8000
    route /api
    volume .venv
    setup: uv sync --frozen
    pre-deploy: uv run alembic upgrade head
    run uv run gunicorn app.main:app --bind 0.0.0.0:$port
    on dev: run uv run uvicorn app.main:app --host 0.0.0.0 --port $port --reload
}

on mail {
    service mailpit from axllent/mailpit:latest {
        port 8025
        route mail.*
    }
}

task migrate in api: uv run alembic upgrade head
";

#[test]
fn every_branch_reports_all_static_errors_with_rustc_carets() {
    let diagnostics = parse_forkfile(INVALID).expect_err("`pg.url` must fail");
    let rendered: Vec<String> = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.render(INVALID, "Forkfile"))
        .collect();
    assert!(
        rendered.contains(&EXPECTED_FIRST_ERROR.to_owned()),
        "unexpected diagnostics:\n{}",
        rendered.join("\n\n")
    );
    assert_eq!(
        rendered[0], EXPECTED_FIRST_ERROR,
        "diagnostics must be ordered by source offset"
    );
    // `staging` is declared but unreachable from `default`, so this failure sits in an inactive
    // `on staging` branch: the whole-file checker still reports it, with its own span.
    assert_eq!(rendered.len(), 2, "expected both branches to report");
    assert!(
        rendered
            .iter()
            .all(|snippet| snippet.contains("did you mean database `app_db.url`?"))
    );
    assert!(
        rendered[1].contains("  --> Forkfile:23:18"),
        "second diagnostic must carry its own span:\n{}",
        rendered[1]
    );
}

#[test]
fn external_catalog_rejects_and_accepts_keys_with_real_spans() {
    let parsed = parse_forkfile(EXTERNAL_SOURCE).expect("valid manifest");

    let rejected = parsed.validate(&Catalog(&["OTHER_KEY"]));
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].span.line, 2);
    assert_eq!(rejected[0].span.col, 11);
    assert_eq!(
        rejected[0].message,
        "`MISSING_KEY` is not available in this environment"
    );

    assert!(parsed.validate(&Catalog(&["MISSING_KEY"])).is_empty());
}

#[test]
fn forkfile_round_trips_losslessly_through_canonical_json() {
    let parsed = parse_forkfile(VALID).expect("valid manifest");
    let json = serde_json::to_string(&parsed).expect("serialize");
    let reparsed: slipfork_forkfile::ForkFile =
        serde_json::from_str(&json).expect("canonical JSON deserializes");
    assert_eq!(parsed, reparsed);
    let value: serde_json::Value = serde_json::from_str(&json).expect("json value");
    assert!(value.get("statements").is_some());
}

#[test]
fn evaluation_and_resolution_keep_secret_taint_off_disk_and_out_of_logs() {
    let parsed = parse_forkfile(VALID).expect("valid manifest");

    // The default feature pulls in `mail`, so the mailpit service is active.
    let evaluated: EvaluatedFork = parsed.evaluate(&FeatureSet::new()).expect("evaluate");
    assert_eq!(evaluated.features, vec!["default", "dev", "mail"]);
    assert!(
        evaluated
            .services
            .iter()
            .any(|service| service.id == "mailpit")
    );
    let api = evaluated
        .services
        .iter()
        .find(|service| service.id == "api")
        .expect("api service");
    assert_eq!(api.port, Some(8000));
    assert_eq!(api.routes, vec!["/api"]);
    let run = api.run.as_ref().expect("run command");
    assert!(
        run.slots
            .iter()
            .any(|slot| slot.parts.iter().any(|part| matches!(
                part,
                ArgPart::Ref(ArgRef {
                    kind: ArgRefKind::Port,
                    ..
                })
            ))),
        "`$port` must be captured as a structured reference"
    );

    // A real 0600 vault on disk: generated secrets are write-once and redacted.
    let dir = tempfile::Builder::new()
        .prefix("forkfile-e2e")
        .tempdir_in("/var/tmp")
        .expect("disk scratch");
    let vault = FileVaultProvider::open(dir.path().join("vault.json")).expect("open vault");
    let resolvers = Resolvers::new(&vault, &OsCsprngEntropy);

    let env: BTreeMap<String, _> = evaluated
        .resolve_env("api", &resolvers)
        .expect("resolve env");
    // Without a declared local password the coordinate stays a plaintext, passwordless URL.
    assert_eq!(
        env["DATABASE_URL"].expose(),
        "postgres://slipfork@127.0.0.1:5432/app_db",
        "the world projection supplies `$app_db.url` with no injected bag"
    );
    assert!(
        !env["DATABASE_URL"].is_tainted(),
        "a passwordless coordinate carries no secret"
    );
    assert_eq!(env["PLAIN"].redacted(), "hello-world");
    assert!(!env["PLAIN"].is_tainted());
    assert_eq!(env["REDIS_URL"].expose(), "redis://127.0.0.1:6379");

    let generated = env["SECRET_KEY"].expose().to_owned();
    assert_eq!(generated.len(), 64, "random_hex(32) is 32 bytes");
    assert!(env["SECRET_KEY"].is_tainted());
    assert!(!generated.contains("[REDACTED]"));

    // A declared local password is composed into the coordinate and taints it end to end.
    vault
        .put(Namespace::Secrets, "APP_DB_PASSWORD", "pg-s3cret")
        .expect("declare the local database password");
    let secret_env: BTreeMap<String, _> = evaluated
        .resolve_env("api", &resolvers)
        .expect("resolve env with a declared password");
    assert_eq!(secret_env["DATABASE_URL"].redacted(), "[REDACTED]");
    assert!(secret_env["DATABASE_URL"].is_tainted());
    assert_eq!(
        secret_env["DATABASE_URL"].expose(),
        "postgres://slipfork:pg-s3cret@127.0.0.1:5432/app_db",
        "the coordinate composes the declared password"
    );

    // Write-once: the second resolution returns the same stored value.
    let second: BTreeMap<String, _> = evaluated.resolve_env("api", &resolvers).expect("resolve");
    assert_eq!(second["SECRET_KEY"].expose(), generated);
    let stored = vault
        .get(Namespace::Secrets, "SECRET_KEY")
        .expect("vault read");
    assert_eq!(stored.as_deref(), Some(generated.as_str()));
}
