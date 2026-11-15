//! Protected critical tests for `slipfork-dag` (`R9`: orthogonal invalidation & cycle refusal).

use std::collections::BTreeMap;

use slipfork_dag::{
    CacheAction, DagError, DagPlanner, MapInputHasher, SetDigestCache, SlipforkConfig, TestSpec,
};

const SAMPLE_TOML: &str = r#"
[artifacts.front]
inputs = ["front/src/**", "front/package.json"]
build = "pnpm --dir front build"
mount = "front/build"

[fixtures.migrate]
inputs = ["api/**/migrations/*.py"]
run = "uv run python manage.py migrate"

[fixtures.seed_core]
parent = "migrate"
inputs = ["api/core/seed.py"]
run = "uv run python manage.py seed --core"

[fixtures.seed_catalog]
parent = "seed_core"
inputs = ["api/catalog/seed.py"]
run = "uv run python manage.py seed --catalog"

[fixtures.seed_minimal]
parent = "migrate"
inputs = ["api/minimal/seed.py"]
run = "uv run python manage.py seed --minimal"
"#;

#[test]
fn r9_orthogonal_invalidation_front_edit_never_invalidates_db_snapshots_and_vice_versa() {
    let cfg = SlipforkConfig::parse_toml(SAMPLE_TOML).expect("valid slipfork.toml");
    let build_envs = BTreeMap::from([("PUBLIC_API".to_owned(), "/back".to_owned())]);
    let runtime_envs = BTreeMap::from([(
        "DATABASE_URL".to_owned(),
        "postgres://127.0.0.1/db".to_owned(),
    )]);

    let mut hasher = MapInputHasher::default();
    hasher
        .files
        .insert("front/src/App.svelte".to_owned(), b"<h1>v1</h1>".to_vec());
    hasher
        .files
        .insert("front/package.json".to_owned(), b"{}".to_vec());
    hasher.files.insert(
        "api/shop/migrations/0001_initial.py".to_owned(),
        b"mig1".to_vec(),
    );
    hasher
        .files
        .insert("api/core/seed.py".to_owned(), b"core1".to_vec());
    hasher
        .files
        .insert("api/catalog/seed.py".to_owned(), b"cat1".to_vec());
    hasher
        .files
        .insert("api/minimal/seed.py".to_owned(), b"min1".to_vec());

    let mut cache = SetDigestCache::default();
    let cold = DagPlanner::plan(&cfg, &build_envs, &runtime_envs, &hasher, &cache).expect("plan");
    assert_eq!(
        cold.fixture_waves.len(),
        3,
        "migrate -> [seed_core, seed_minimal] -> [seed_catalog]"
    );
    assert_eq!(
        cold.artifact("front").expect("front").action,
        CacheAction::Rebuild
    );

    // Populate cache from cold build.
    for a in &cold.artifacts {
        cache.artifacts.insert(a.digest.clone());
    }
    for f in cold.fixture_waves.iter().flatten() {
        cache.fixtures.insert(f.digest.clone());
    }

    // 1. Warm run with ZERO changes -> 100% Cached!
    let warm = DagPlanner::plan(&cfg, &build_envs, &runtime_envs, &hasher, &cache).expect("warm");
    assert_eq!(
        warm.artifact("front").expect("front").action,
        CacheAction::Cached
    );
    assert!(
        warm.fixture_waves
            .iter()
            .flatten()
            .all(|f| f.action == CacheAction::Cached)
    );

    // 2. Edit ONLY front/src/App.svelte -> artifact:front Rebuilds, ALL 4 DB fixtures stay Cached!
    hasher
        .files
        .insert("front/src/App.svelte".to_owned(), b"<h1>v2</h1>".to_vec());
    let after_front =
        DagPlanner::plan(&cfg, &build_envs, &runtime_envs, &hasher, &cache).expect("front edit");
    assert_eq!(
        after_front.artifact("front").expect("front").action,
        CacheAction::Rebuild
    );
    for f in after_front.fixture_waves.iter().flatten() {
        assert_eq!(
            f.action,
            CacheAction::Cached,
            "fixture {} must remain Cached!",
            f.id
        );
    }
    cache
        .artifacts
        .insert(after_front.artifact("front").expect("front").digest.clone());

    // 3. Edit ONLY api/core/seed.py -> seed_core and child seed_catalog Rebuild,
    //    while ancestor `migrate`, sibling `seed_minimal`, AND `artifact:front` stay Cached!
    hasher
        .files
        .insert("api/core/seed.py".to_owned(), b"core2".to_vec());
    let after_seed =
        DagPlanner::plan(&cfg, &build_envs, &runtime_envs, &hasher, &cache).expect("seed edit");
    assert_eq!(
        after_seed.artifact("front").expect("front").action,
        CacheAction::Cached
    );
    assert_eq!(
        after_seed.fixture("migrate").expect("migrate").action,
        CacheAction::Cached
    );
    assert_eq!(
        after_seed
            .fixture("seed_minimal")
            .expect("seed_minimal")
            .action,
        CacheAction::Cached
    );
    assert_eq!(
        after_seed.fixture("seed_core").expect("seed_core").action,
        CacheAction::Rebuild
    );
    assert_eq!(
        after_seed
            .fixture("seed_catalog")
            .expect("seed_catalog")
            .action,
        CacheAction::Rebuild
    );
}

#[test]
fn rejects_unknown_parent_and_cycles_closed() {
    let bad_parent = SlipforkConfig::parse_toml(
        r#"
[fixtures.a]
parent = "missing"
run = "echo a"
"#,
    )
    .expect("parse");
    let err = DagPlanner::plan(
        &bad_parent,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &MapInputHasher::default(),
        &SetDigestCache::default(),
    )
    .expect_err("unknown parent");
    assert!(matches!(err, DagError::UnknownParent { .. }));

    let cycle = SlipforkConfig::parse_toml(
        r#"
[fixtures.a]
parent = "b"
run = "echo a"
[fixtures.b]
parent = "a"
run = "echo b"
"#,
    )
    .expect("parse");
    let err2 = DagPlanner::plan(
        &cycle,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &MapInputHasher::default(),
        &SetDigestCache::default(),
    )
    .expect_err("cycle");
    assert!(matches!(err2, DagError::CycleDetected(_)));
}

#[test]
fn slipfork_toml_root_schema_owns_project_and_test_after_suite_without_invalidating_dag() {
    let cfg = SlipforkConfig::parse_toml(
        r#"
project = "acme"

[artifacts.front]
inputs = ["front/src/**"]
build = "pnpm --dir front build"
mount = "front/build"

[fixtures.migrate]
inputs = ["api/migrations/*.sql"]
run = "psql -f api/migrations/0001.sql"

[test]
after_suite = "uv run coverage combine \"$SLIPFORK_RUN_ARTIFACTS_DIR\"/*/.coverage*"
"#,
    )
    .expect("valid slipfork.toml with project and [test].after_suite");
    assert_eq!(
        cfg.project
            .as_ref()
            .map(slipfork_world::ProjectSlug::as_str),
        Some("acme")
    );
    assert_eq!(
        cfg.test,
        TestSpec {
            after_suite: Some(
                "uv run coverage combine \"$SLIPFORK_RUN_ARTIFACTS_DIR\"/*/.coverage*".to_owned()
            ),
        }
    );
    assert_eq!(cfg.artifacts.len(), 1);
    assert_eq!(cfg.fixtures.len(), 1);

    let hasher = MapInputHasher::default();
    let cache = SetDigestCache::default();
    let plan_a =
        DagPlanner::plan(&cfg, &BTreeMap::new(), &BTreeMap::new(), &hasher, &cache).expect("plan");

    let mut cfg_b = cfg.clone();
    cfg_b.project = Some(slipfork_world::ProjectSlug::parse("other-slug").expect("slug"));
    cfg_b.test.after_suite = Some("npx allure generate".to_owned());
    let plan_b = DagPlanner::plan(&cfg_b, &BTreeMap::new(), &BTreeMap::new(), &hasher, &cache)
        .expect("plan b");
    assert_eq!(plan_a, plan_b);
}

#[test]
fn project_override_must_be_a_validated_project_slug() {
    let err = SlipforkConfig::parse_toml("project = \"My_Project\"\n")
        .expect_err("invalid project slug must fail closed at parse time");
    assert!(matches!(err, DagError::Toml(_)));
    assert!(SlipforkConfig::parse_toml("project = \"acme\"\n").is_ok());
}

#[test]
fn unknown_slipfork_toml_fields_fail_closed() {
    assert!(SlipforkConfig::parse_toml("mystery = true\n").is_err());
    let bad_artifact = r#"
[artifacts.front]
inputs = ["front/src/**"]
build = "pnpm --dir front build"
mount = "front/dist"
ghost = "nope"
"#;
    assert!(SlipforkConfig::parse_toml(bad_artifact).is_err());
    let bad_fixture = r#"
[fixtures.migrate]
run = "echo migrate"
ghost = "nope"
"#;
    assert!(SlipforkConfig::parse_toml(bad_fixture).is_err());
    let bad_test = r#"
[test]
after_suite = "echo ok"
ghost = "nope"
"#;
    assert!(SlipforkConfig::parse_toml(bad_test).is_err());
}
