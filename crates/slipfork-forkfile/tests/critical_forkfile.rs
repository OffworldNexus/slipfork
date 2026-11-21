//! Protected end-to-end invariants for `Forkfile` semantics and migration.

use std::fs;
use std::path::{Path, PathBuf};

use slipfork_forkfile::{check, format_check, migrate, parse};

const SNOW: &str = r#"images:
  - id: backend
    context: .
    dockerfile: Dockerfile
    envs: [common]
  - id: frontend
    context: ./front
resources:
  pg_clusters:
    - id: pg
      version: 16
  pg_databases:
    - id: app_db
      cluster: pg
  redis_clusters:
    - id: cache
  s3_buckets:
    - uploads
    - id: assets
domains:
  - main
envs:
  common:
    DATABASE_URL: "postgres://${{ resources::pg_databases.app_db.url }}"
    REDIS_URL: "${{ resources::redis_clusters.cache.url }}"
    PUBLIC_URL: "${{ domains::main }}"
    SECRET_KEY: "${{ secrets::SECRET_KEY ?? random_hex(32) }}"
    COMMIT: "${{ deployment::commit_sha }}"
components:
  - id: api
    image: backend
    run_command: gunicorn app.main:app
  - id: worker
    image: backend
    run_command: celery -A app.worker worker
  - id: front
    image: frontend
    run_command: node server.js
ingresses:
  - match: { prefix: /api }
    target: { component: api, port: 8000 }
  - match: { prefix: / }
    target: { component: front, port: 4173 }
hooks:
  - component: api
    when: before_start
    run_command: alembic upgrade head
modes:
  dev:
    envs:
      dev:
        DEBUG: "1"
    components:
      front:
        envs: [common]
"#;

const MIGRATED: &str = "# forkfile: 0.1
# migrated from snow.yml
features {
  dev
}

postgres pg {
  version 16
  database app_db
}

redis cache

s3 uploads

s3 assets

domain main

env common {
  COMMIT = deployment.commit_sha
  DATABASE_URL = \"postgres://\" + app_db.url
  PUBLIC_URL = main.domain
  REDIS_URL = cache.url
  SECRET_KEY = secret(SECRET_KEY) ?? random_hex(32)
}

on dev {
  env dev {
    DEBUG = \"1\"
  }
}

image backend . {
  dockerfile Dockerfile
  use env common
  service api {
    port 8000
    route /api
    before-start: alembic upgrade head
    run: gunicorn app.main:app
  }
  service worker {
    run: celery -A app.worker worker
  }
}

image frontend ./front {
  on dev: use env common
  service front {
    port 4173
    route /
    run: node server.js
  }
}
";

fn lab_dir(name: &str) -> PathBuf {
    let dir =
        Path::new("/var/tmp").join(format!("slipfork-forkfile-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create lab dir");
    dir
}

#[test]
fn build_time_arg_rejects_secret_native_taint() {
    let source = "# forkfile: 0.1\n\npostgres pg { database app_db }\n\nimage backend . {\n  arg TOKEN = secret(TOKEN)\n  arg PASSWORD = app_db.password\n  env {\n    TOKEN = secret(TOKEN)\n  }\n}\n";
    let document = parse(source).expect("parses");
    let diags = check(&document).expect_err("tainted args are refused");
    assert!(
        diags
            .items
            .iter()
            .any(|d| d.message.contains("bake a secret")),
        "{diags:?}"
    );
    assert_eq!(
        diags
            .items
            .iter()
            .filter(|d| d.message.contains("bake a secret"))
            .count(),
        2,
        "{diags:?}"
    );
}

#[test]
fn flat_namespace_duplicates_name_both_kinds_and_spans() {
    let source = "# forkfile: 0.1\n\ndomain cache\nredis cache\n";
    let diags = check(&parse(source).expect("parses")).expect_err("duplicate symbols");
    let first = diags.first().expect("one diagnostic");
    assert!(first.message.contains("duplicate symbol `cache`"));
    assert!(first.message.contains("`domain`"));
    assert!(first.message.contains("`redis`"));
    assert!(first.message.contains("Forkfile:3:1"), "{}", first.message);
}

#[test]
fn every_on_branch_is_typed_even_when_not_selected() {
    let source = "# forkfile: 0.1\n\nfeatures {\n  dev\n}\n\nimage backend . {\n  on !dev: arg TOKEN = secret(TOKEN)\n}\n";
    let diags = check(&parse(source).expect("parses")).expect_err("branch is typed");
    assert!(
        diags
            .items
            .iter()
            .any(|d| d.message.contains("bake a secret")),
        "{diags:?}"
    );
}

#[test]
fn duplicate_routes_in_one_branch_are_rejected() {
    let source = "# forkfile: 0.1\n\nimage backend . {\n  service api {\n    port 8000\n    route /api\n    route /api\n  }\n}\n";
    let diags = check(&parse(source).expect("parses")).expect_err("route conflict");
    assert!(
        diags
            .items
            .iter()
            .any(|d| d.message.contains("duplicate `route`")),
        "{diags:?}"
    );
}

#[test]
fn migration_is_deterministic_lands_on_disk_and_typechecks() {
    let dir = lab_dir("migrate");
    let input = dir.join("snow.yml");
    let output = dir.join("Forkfile");
    fs::write(&input, SNOW).expect("write snow.yml");

    let first = migrate(&fs::read_to_string(&input).expect("read snow.yml")).expect("migrates");
    let second = migrate(&fs::read_to_string(&input).expect("read snow.yml")).expect("migrates");
    assert_eq!(first, second, "identical input must be byte-identical");
    assert_eq!(first, MIGRATED, "migration golden changed");

    let canonical = format_check(&first).expect("migrated output parses");
    assert!(canonical.is_formatted(), "{}", canonical.diff);

    fs::write(&output, &first).expect("write Forkfile");
    let on_disk = fs::read_to_string(&output).expect("read Forkfile");
    let document = parse(&on_disk).expect("migrated output parses");
    let checked = check(&document).expect("migrated output typechecks");
    assert_eq!(
        checked.features.into_iter().collect::<Vec<_>>(),
        vec!["dev".to_string()]
    );
    fs::remove_dir_all(&dir).expect("clean lab dir");
}

#[test]
fn migration_refuses_unknown_keys_and_disabled_component_overrides() {
    let unknown = migrate("images: []\nmystery: 1\n").expect_err("unknown key refused");
    let first = unknown.first().expect("one diagnostic");
    assert!(first.message.contains("mystery"));
    assert!(first.message.contains("images"));
    assert!(first.next.contains("`images`"));
    assert!(first.next.contains("`resources`"));
    assert_eq!(first.span.line(), 2);

    let disabled = "images:\n  - id: backend\n    context: .\ncomponents:\n  - id: api\n    image: backend\n    run_command: run\nmodes:\n  dev:\n    components:\n      api:\n        disabled: true\n";
    let diags = migrate(disabled).expect_err("disabled override refused");
    let first = diags.first().expect("one diagnostic");
    assert!(first.message.contains("modes.dev.components.api.disabled"));
    assert_eq!(first.span.line(), 12);
    assert!(first.next.contains("on <feature>"));

    let rendered = diags.render_as(disabled, "snow.yml");
    assert!(rendered.contains("snow.yml:12:"), "{rendered}");
    assert!(!rendered.contains("Forkfile:"), "{rendered}");
}

#[test]
fn migration_duplicate_symbols_embed_both_yaml_spans() {
    let source = "domains:\n  - cache\nresources:\n  redis_clusters:\n    - id: cache\n";
    let diags = migrate(source).expect_err("duplicate symbol");
    let first = diags.first().expect("one diagnostic");
    assert!(
        first.message.contains("duplicate symbol `cache`"),
        "{}",
        first.message
    );
    assert!(first.message.contains("snow.yml:2:"), "{}", first.message);
    assert!(first.message.contains("snow.yml:5:"), "{}", first.message);
}

#[test]
fn migration_refuses_shell_pipelines_and_unknown_template_namespaces() {
    let pipeline = "images:\n  - id: backend\n    context: .\ncomponents:\n  - id: api\n    image: backend\n    run_command: cat a | grep b\n";
    let diags = migrate(pipeline).expect_err("pipeline refused");
    assert!(
        diags
            .items
            .iter()
            .any(|d| d.next.contains("scripts/<name>.sh")),
        "{diags:?}"
    );

    let template = "envs:\n  common:\n    URL: \"${{ rollback::x }}\"\n";
    let diags = migrate(template).expect_err("namespace refused");
    assert!(
        diags
            .items
            .iter()
            .any(|d| d.message.contains("envs.common.URL")),
        "{diags:?}"
    );
}
