//! `snow.yml` → `Forkfile` conversion: a realistic upstream document round-trips through
//! `migrate_snow_yaml` → `parse_forkfile` → `evaluate`, and the migrated output is statically
//! valid by construction.

use slipfork_forkfile::{
    ArgPart, EvaluatedImage, Expr, FeatureSet, HookPhase, MigrateError, migrate_snow_yaml,
    parse_forkfile,
};

const UPSTREAM: &str = r#"
images:
  - id: api
    context: ./api
    dockerfile: docker/Dockerfile.api
    envs: [common, build_only]
  - id: front
    context: ./front
  - id: worker
    registry: ghcr.io
    repository: acme/worker
    tag: v3
resources:
  pg_clusters:
    - id: pg
      version: "18"
  pg_databases:
    - id: app_db
      cluster: pg
    - id: analytics
      cluster: pg
      pool: false
  redis_clusters:
    - id: cache
      version: "7"
  s3_buckets:
    - id: uploads
domains:
  - id: main
  - id: admin
envs:
  common:
    DATABASE_URL: "${{ resources::pg_databases.app_db.url }}"
    CACHE_URL: "${{ resources::redis_clusters.cache.url ?? \"redis://cache:6379\" }}"
    API_BASE: "https://${{ domains::main.name }}/api"
    DEPLOY_ENV: "${{ deployment::env.name }}"
    FLAG: "${{ vars::FEATURE_FLAG ?? \"off\" }}"
    SECRET_KEY: "${{ secrets::SECRET_KEY }}"
  build_only:
    BUILD_ONLY: "1"
modes:
  all:
    envs:
      frontend:
        PUBLIC_API: "${{ resources::app.url }}/api"
components:
  - id: api
    image: api
    run_command: "uv run gunicorn app.main:app --bind 0.0.0.0:8000"
    size_hint: "small"
    envs: [common]
  - id: front
    image: front
    run_command: "node server.js"
    envs: [common, frontend]
    internal_ports: [5173]
  - id: worker
    image: worker
    run_command: "uv run python -m worker"
    envs: [common]
ingresses:
  - match:
      prefix: /api
    target:
      component: api
      port: 8000
  - match:
      prefix: /
    target:
      component: front
      port: 5173
hooks:
  - component: api
    when: pre-deploy
    run_command: "uv run alembic upgrade head"
  - component: front
    when: post-deploy
    run_command: "node scripts/warm-cache.js"
"#;

#[test]
fn a_realistic_upstream_file_round_trips_into_a_valid_forkfile() {
    let output = migrate_snow_yaml(UPSTREAM).expect("migrate");
    assert!(
        output.starts_with("# migrated from snow.yml\n\n"),
        "provenance leads the file"
    );
    assert_eq!(
        migrate_snow_yaml(UPSTREAM).expect("migrate twice"),
        output,
        "migration is idempotent"
    );
    assert!(
        output.contains("    dockerfile docker/Dockerfile.api"),
        "a non-default Dockerfile survives: {output}"
    );
    assert!(
        output.contains("    build_env {\n        BUILD_ONLY = \"1\"\n    }"),
        "build-only keys are emitted: {output}"
    );
    assert_eq!(
        output.matches("BUILD_ONLY =").count(),
        2,
        "the group declaration plus exactly one build_env copy"
    );
    assert_eq!(
        output.matches("DATABASE_URL =").count(),
        1,
        "runtime env is declared once, not copied into build_env"
    );

    let parsed = parse_forkfile(&output).expect("migrated output must parse");
    let evaluated = parsed.evaluate(&FeatureSet::new()).expect("evaluate");

    let ids: Vec<&str> = evaluated.services.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["api", "front", "worker"]);

    let api = &evaluated.services[0];
    assert_eq!(api.port, Some(8000));
    assert_eq!(api.routes, ["/api"]);
    assert_eq!(api.size, "small");
    assert_eq!(api.run.as_ref().expect("run argv").slots.len(), 6);
    let EvaluatedImage::Dockerfile { dockerfile, .. } = &api.image else {
        panic!("api is built from a Dockerfile");
    };
    assert_eq!(dockerfile, "docker/Dockerfile.api");

    let front = &evaluated.services[1];
    assert_eq!(front.port, Some(5173));
    assert_eq!(front.routes, ["/"]);
    assert_eq!(front.internal_ports, [5173]);
    let post = &front.hooks[0];
    assert_eq!(post.phase, HookPhase::PostDeploy);
    let ArgPart::Text(program) = &post.argv.slots[0].parts[0] else {
        panic!("hook argv is literal");
    };
    assert_eq!(program, "node");

    let groups: Vec<&str> = evaluated
        .env_groups
        .iter()
        .map(|group| group.name.as_str())
        .collect();
    assert_eq!(groups, ["build_only", "common", "frontend"]);
    let common = &evaluated.env_groups[1];
    let keys: Vec<&str> = common
        .vars
        .iter()
        .map(|assignment| assignment.key.value.as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "API_BASE",
            "CACHE_URL",
            "DATABASE_URL",
            "DEPLOY_ENV",
            "FLAG",
            "SECRET_KEY"
        ]
    );
    let expr = |key: &str| {
        common
            .vars
            .iter()
            .find(|assignment| assignment.key.value == key)
            .map(|assignment| assignment.expr.clone())
            .expect("declared env key")
    };
    let Expr::Concat { parts, .. } = expr("API_BASE") else {
        panic!("mixed template becomes a concatenation");
    };
    assert_eq!(parts.len(), 3);
    let Expr::Str(prefix) = &parts[0] else {
        panic!("literal prefix");
    };
    assert_eq!(prefix.value, "https://");
    let Expr::Ref(name) = &parts[1] else {
        panic!("domain reference");
    };
    assert_eq!(name.value, ["main", "name"]);
    let Expr::Str(suffix) = &parts[2] else {
        panic!("literal suffix");
    };
    assert_eq!(suffix.value, "/api");
    let Expr::Fallback { lhs, rhs, .. } = expr("CACHE_URL") else {
        panic!("`??` becomes a Forkfile fallback");
    };
    let Expr::Ref(cache) = *lhs else {
        panic!("redis reference");
    };
    assert_eq!(cache.value, ["cache", "url"]);
    let Expr::Str(default) = *rhs else {
        panic!("fallback literal");
    };
    assert_eq!(default.value, "redis://cache:6379");
    let Expr::Secret(secret) = expr("SECRET_KEY") else {
        panic!("`secrets::` becomes `secret(...)`");
    };
    assert_eq!(secret.value, "SECRET_KEY");
    let Expr::Ref(deployment) = expr("DEPLOY_ENV") else {
        panic!("deployment prefix becomes a reference");
    };
    assert_eq!(deployment.value, ["deployment", "env", "name"]);

    assert_eq!(evaluated.postgres.len(), 1);
    assert_eq!(evaluated.postgres[0].id, "pg");
    assert_eq!(evaluated.postgres[0].version, "18");
    let databases: Vec<&str> = evaluated.postgres[0]
        .databases
        .iter()
        .map(|database| database.id.as_str())
        .collect();
    assert_eq!(databases, ["app_db", "analytics"]);
    assert!(!evaluated.postgres[0].databases[1].pool);
    assert_eq!(evaluated.redis.len(), 1);
    assert_eq!(evaluated.redis[0].id, "cache");
    assert_eq!(evaluated.redis[0].version, "7");
    assert_eq!(evaluated.s3, ["uploads"]);
    assert_eq!(evaluated.domains, ["main", "admin"]);
}

#[test]
fn the_demo_fixture_migrates_into_a_valid_forkfile() {
    let demo = include_str!("../../../support/demo/migrate/snow.yml");
    let out = migrate_snow_yaml(demo).expect("demo fixture migrates");
    parse_forkfile(&out)
        .expect("parses")
        .evaluate(&FeatureSet::new())
        .expect("evaluates");
    assert!(out.contains("secret(SECRET_KEY) ?? random_string(50)"));
}

#[test]
fn unsupported_templates_shell_syntax_and_two_ports_fail_closed() {
    let unsupported = UPSTREAM.replace(
        "${{ resources::pg_databases.app_db.url }}",
        "${{ resources::s3_buckets.uploads.url }}",
    );
    assert!(matches!(
        migrate_snow_yaml(&unsupported),
        Err(MigrateError::UnsupportedExpression { .. })
    ));

    let shell = UPSTREAM.replace(
        "run_command: \"node server.js\"",
        "run_command: \"node server.js && node warm.js\"",
    );
    let error = migrate_snow_yaml(&shell).expect_err("shell operators are refused");
    assert!(
        error
            .to_string()
            .contains("next: move the pipeline into scripts/")
    );

    let two_ports = UPSTREAM.replace(
        "  - match:\n      prefix: /\n    target:\n      component: front\n      port: 5173\n",
        "  - match:\n      prefix: /\n    target:\n      component: front\n      port: 5173\n  - match:\n      prefix: /extra\n    target:\n      component: api\n      port: 9000\n",
    );
    let error = migrate_snow_yaml(&two_ports).expect_err("one service exposes one port");
    let MigrateError::Schema { path, message, .. } = error else {
        panic!("expected a schema error");
    };
    assert_eq!(path, "ingresses[2].target.port");
    assert!(message.contains("two ports"), "{message}");
}
