//! Protected critical tests for `slipfork-dag` (`R9`: orthogonal invalidation & cycle refusal).

use std::collections::{BTreeMap, BTreeSet};

use slipfork_dag::{
    CacheAction, CoverageConfig, CoverageReportFormat, CoverageThreshold, DagError, DagPlanner,
    DumpPolicy, MapInputHasher, PgSeed, ResourceMode, ResourcePlan, ResourceReason,
    ResourceStrategy, ResourceTarget, S3Resolution, SetDigestCache, SlipforkConfig,
};
use slipfork_world::WorldTarget;

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

const PER_WORLD_TOML: &str = r#"
[fixtures.sites]
per_world = true
run = "python manage.py seed_sites"

[fixtures.sites_child]
parent = "sites"
per_world = true
run = "python manage.py seed_sites_child"

[fixtures.shared]
run = "python manage.py migrate"
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
fn slipfork_toml_root_schema_owns_project_and_typed_test_coverage_without_invalidating_dag() {
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
coverage = true
coverage_report = ["html", "xml"]
coverage_fail_under = 85
"#,
    )
    .expect("valid slipfork.toml with project and typed [test] coverage");
    assert_eq!(
        cfg.project
            .as_ref()
            .map(slipfork_world::ProjectSlug::as_str),
        Some("acme")
    );
    let coverage = cfg.test.coverage.as_ref().expect("coverage enabled");
    assert_eq!(
        coverage.reports,
        BTreeSet::from([CoverageReportFormat::Html, CoverageReportFormat::Xml])
    );
    assert_eq!(
        coverage.fail_under.map(CoverageThreshold::percent),
        Some(85)
    );
    assert_eq!(cfg.artifacts.len(), 1);
    assert_eq!(cfg.fixtures.len(), 1);

    let hasher = MapInputHasher::default();
    let cache = SetDigestCache::default();
    let plan_a =
        DagPlanner::plan(&cfg, &BTreeMap::new(), &BTreeMap::new(), &hasher, &cache).expect("plan");

    let mut cfg_b = cfg.clone();
    cfg_b.project = Some(slipfork_world::ProjectSlug::parse("other-slug").expect("slug"));
    cfg_b.test.coverage = Some(CoverageConfig {
        reports: BTreeSet::from([CoverageReportFormat::Text]),
        fail_under: None,
    });
    let plan_b = DagPlanner::plan(&cfg_b, &BTreeMap::new(), &BTreeMap::new(), &hasher, &cache)
        .expect("plan b");
    assert_eq!(plan_a, plan_b);
}

#[test]
fn typed_test_coverage_is_single_way_and_fail_closed() {
    // `coverage = true` alone defaults reports to `{text}` and no threshold.
    let cfg = SlipforkConfig::parse_toml("[test]\ncoverage = true\n").expect("coverage defaults");
    let coverage = cfg.test.coverage.expect("coverage enabled");
    assert_eq!(
        coverage.reports,
        BTreeSet::from([CoverageReportFormat::Text])
    );
    assert!(coverage.fail_under.is_none());

    // `coverage_report` / `coverage_fail_under` cannot exist without `coverage = true`.
    for raw in [
        "[test]\ncoverage_report = [\"html\"]\n",
        "[test]\ncoverage_report = []\n",
        "[test]\ncoverage_fail_under = 85\n",
        "[test]\ncoverage = false\ncoverage_report = [\"html\"]\n",
    ] {
        let err = SlipforkConfig::parse_toml(raw)
            .expect_err("coverage options require `coverage = true`");
        assert!(
            matches!(err, DagError::CoverageOptionsRequireEnabled),
            "{raw}: {err:?}"
        );
    }

    // An explicitly empty report list fails closed: coverage cannot be enabled with zero formats.
    for raw in [
        "[test]\ncoverage = true\ncoverage_report = []\n",
        "[test]\ncoverage = true\ncoverage_report = []\ncoverage_fail_under = 85\n",
    ] {
        let err = SlipforkConfig::parse_toml(raw).expect_err("empty coverage_report fails closed");
        assert!(
            matches!(err, DagError::EmptyCoverageReport),
            "{raw}: {err:?}"
        );
    }

    // Out-of-range and non-integer thresholds fail closed with the typed error.
    for raw in [
        "[test]\ncoverage = true\ncoverage_fail_under = 101\n",
        "[test]\ncoverage = true\ncoverage_fail_under = -1\n",
        "[test]\ncoverage = true\ncoverage_fail_under = \"eighty\"\n",
        "[test]\ncoverage = true\ncoverage_fail_under = 85.5\n",
    ] {
        let err = SlipforkConfig::parse_toml(raw).expect_err("invalid threshold fails closed");
        assert!(
            matches!(err, DagError::InvalidCoverageThreshold { .. }),
            "{raw}: {err:?}"
        );
    }

    // Boundary values are accepted.
    for (raw, expected) in [
        ("[test]\ncoverage = true\ncoverage_fail_under = 0\n", 0),
        ("[test]\ncoverage = true\ncoverage_fail_under = 100\n", 100),
    ] {
        let cfg = SlipforkConfig::parse_toml(raw).expect("boundary threshold");
        assert_eq!(
            cfg.test
                .coverage
                .expect("coverage enabled")
                .fail_under
                .map(CoverageThreshold::percent),
            Some(expected)
        );
    }

    // `[test]` has no hook surface: unknown keys are rejected.
    assert!(SlipforkConfig::parse_toml("[test]\ncoverage = true\nghost = \"nope\"\n").is_err());
}

#[test]
fn per_world_fixtures_are_typed_and_cannot_parent_origin_independent_nodes() {
    let cfg = SlipforkConfig::parse_toml(PER_WORLD_TOML).expect("per_world schema parses");
    assert!(cfg.fixtures["sites"].per_world);
    assert!(cfg.fixtures["sites_child"].per_world);
    assert!(!cfg.fixtures["shared"].per_world);

    let plan = DagPlanner::plan(
        &cfg,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &MapInputHasher::default(),
        &SetDigestCache::default(),
    )
    .expect("plan");
    assert!(plan.fixture("sites").expect("sites").per_world);
    assert!(plan.fixture("sites_child").expect("sites_child").per_world);
    assert!(!plan.fixture("shared").expect("shared").per_world);

    // An origin-independent child of a per-world parent is rejected (`ADR-0003` §1).
    let illegal = SlipforkConfig::parse_toml(
        r#"
[fixtures.sites]
per_world = true
run = "python manage.py seed_sites"

[fixtures.shared_child]
parent = "sites"
run = "python manage.py migrate"
"#,
    )
    .expect("schema parses; the planner rejects the lineage");
    let err = DagPlanner::plan(
        &illegal,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &MapInputHasher::default(),
        &SetDigestCache::default(),
    )
    .expect_err("per-world parent of an origin-independent child");
    assert!(
        matches!(err, DagError::OriginIndependentChildOfPerWorld { .. }),
        "{err:?}"
    );

    // Flipping `per_world` changes the digest (a per-world snapshot is never silently shared).
    let shared = SlipforkConfig::parse_toml("[fixtures.seed]\nrun = \"seed\"\n").expect("shared");
    let per_world =
        SlipforkConfig::parse_toml("[fixtures.seed]\nper_world = true\nrun = \"seed\"\n")
            .expect("per-world");
    let shared_plan = DagPlanner::plan(
        &shared,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &MapInputHasher::default(),
        &SetDigestCache::default(),
    )
    .expect("plan shared");
    let per_world_plan = DagPlanner::plan(
        &per_world,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &MapInputHasher::default(),
        &SetDigestCache::default(),
    )
    .expect("plan per-world");
    assert_ne!(
        shared_plan.fixture("seed").expect("seed").digest,
        per_world_plan.fixture("seed").expect("seed").digest
    );
}

#[test]
fn per_world_fixtures_are_never_served_from_the_shared_digest_cache() {
    let cfg = SlipforkConfig::parse_toml(PER_WORLD_TOML).expect("per_world schema parses");

    let hasher = MapInputHasher::default();
    let cold = DagPlanner::plan(
        &cfg,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &hasher,
        &SetDigestCache::default(),
    )
    .expect("cold plan");

    // Simulate a world-independent cache that already holds every digest (e.g. world A sealed
    // these snapshots). Only the shared node may be reused.
    let mut cache = SetDigestCache::default();
    for fixture in cold.fixture_waves.iter().flatten() {
        cache.fixtures.insert(fixture.digest.clone());
    }
    let warm = DagPlanner::plan(&cfg, &BTreeMap::new(), &BTreeMap::new(), &hasher, &cache)
        .expect("warm plan");
    assert_eq!(
        warm.fixture("sites").expect("sites").action,
        CacheAction::Rebuild,
        "a per-world snapshot bakes this world's origin URLs and must never be reused"
    );
    assert_eq!(
        warm.fixture("sites_child").expect("sites_child").action,
        CacheAction::Rebuild,
        "a per-world child inherits the same origin-bearing snapshot"
    );
    assert_eq!(
        warm.fixture("shared").expect("shared").action,
        CacheAction::Cached,
        "origin-independent snapshots stay cacheable across worlds"
    );
    // The digest is still world-independent (the `DigestCache` port has no world discriminator),
    // which is exactly why the planner must refuse to consult it for per-world nodes.
    assert_eq!(
        warm.fixture("sites").expect("sites").digest,
        cold.fixture("sites").expect("sites").digest
    );
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
coverage = true
ghost = "nope"
"#;
    assert!(SlipforkConfig::parse_toml(bad_test).is_err());
}

#[test]
fn retired_world_table_is_rejected() {
    let err = SlipforkConfig::parse_toml("[world]\nimage = \"api\"\n")
        .expect_err("the retired [world] table must fail closed");
    assert!(matches!(err, DagError::Toml(_)));
}

#[test]
fn resources_defaults_are_mode_aware_hermetic_and_overrideable() {
    let cfg = SlipforkConfig::parse_toml(
        r#"
[resources.s3_buckets.media]
local = "remote"

[resources.pg_databases.main]
test = "remote"
"#,
    )
    .expect("[resources] schema parses");
    assert!(!cfg.resources.is_empty());
    assert_eq!(
        cfg.resources.s3_buckets()["media"].local(),
        Some(ResourceStrategy::Remote)
    );
    assert_eq!(cfg.resources.s3_buckets()["media"].test(), None);
    assert_eq!(
        cfg.resources.pg_databases()["main"].test(),
        Some(ResourceStrategy::Remote)
    );

    assert_eq!(
        ResourceMode::from(&WorldTarget::Canonical),
        ResourceMode::Local
    );
    assert_eq!(
        ResourceMode::from(&WorldTarget::UserFork("agent".to_owned())),
        ResourceMode::Local
    );
    assert_eq!(
        ResourceMode::from(&WorldTarget::TestSlot(3)),
        ResourceMode::Test
    );

    // `local` worlds default to PreferRemote: with a paired session they seed remotely, while
    // S3 stays on the world-local SeaweedFS container (remote credentials arrive as env values).
    let local = ResourcePlan::resolve(
        &cfg.resources,
        ResourceMode::Local,
        DumpPolicy::Default,
        true,
    )
    .expect("local plan");
    assert_eq!(
        local.s3_bucket("media").expect("media").target(),
        ResourceTarget::Local
    );
    assert_eq!(
        local.pg_database("main").expect("main").seed,
        PgSeed::RemoteDump { refresh: false }
    );
    assert_eq!(
        local.pg_database("main").expect("main").target(),
        ResourceTarget::Remote
    );

    // `TestSlot` worlds are hermetically local by construction even with a paired session,
    // unless the resource explicitly sets `test = "remote"`.
    let test = ResourcePlan::resolve(
        &cfg.resources,
        ResourceMode::Test,
        DumpPolicy::Default,
        true,
    )
    .expect("test plan");
    assert_eq!(
        test.s3_bucket("media").expect("media").resolution,
        S3Resolution::Local
    );
    assert_eq!(
        test.pg_database("main").expect("main").seed,
        PgSeed::RemoteDump { refresh: false }
    );
    let diagnostics = test.diagnostics();
    let media = diagnostics
        .iter()
        .find(|row| row.id == "media")
        .expect("media diagnostic");
    assert_eq!(media.strategy, ResourceStrategy::Local);
    assert_eq!(media.resolved, ResourceTarget::Local);
    assert_eq!(media.reason, ResourceReason::ModeDefault);

    // An unpaired `prefer_remote` falls back to local with the `NoRemoteSession` diagnostic,
    // never a silent third mode.
    let fallback_spec = SlipforkConfig::parse_toml(
        r#"
[resources.s3_buckets.media]
test = "local"

[resources.pg_databases.main]
test = "local"
"#,
    )
    .expect("fallback spec");
    let offline = ResourcePlan::resolve(
        &fallback_spec.resources,
        ResourceMode::Local,
        DumpPolicy::Default,
        false,
    )
    .expect("unpaired plan");
    for (target, reason) in [
        (
            offline.s3_bucket("media").expect("media").target(),
            offline.s3_bucket("media").expect("media").reason,
        ),
        (
            offline.pg_database("main").expect("main").target(),
            offline.pg_database("main").expect("main").reason,
        ),
    ] {
        assert_eq!(target, ResourceTarget::Local);
        assert_eq!(reason, ResourceReason::NoRemoteSession);
    }
}

#[test]
fn resources_schema_rejects_unknown_keys_modes_and_empty_overrides() {
    for raw in [
        "[resources]\nghost = true\n",
        "[resources.s3_buckets.media]\nghost = \"remote\"\n",
        "[resources.s3_buckets.media]\nlocal = \"sometimes\"\n",
        "[resources.s3_buckets.media]\nlocal = true\n",
        "[resources.s3_buckets.media]\n",
        "[resources.pg_databases.main]\n",
    ] {
        assert!(
            SlipforkConfig::parse_toml(raw).is_err(),
            "{raw:?} must fail closed"
        );
    }

    let error =
        SlipforkConfig::parse_toml("[resources.s3_buckets.media]\n").expect_err("empty override");
    assert!(
        matches!(
            error,
            DagError::EmptyResourceOverride {
                kind: "s3_buckets",
                ..
            }
        ),
        "{error:?}"
    );

    let cfg = SlipforkConfig::parse_toml(
        r#"
[resources.s3_buckets.media]
local = "local"

[resources.pg_databases.main]
local = "local"
"#,
    )
    .expect("declared ids");
    let error = cfg
        .resources
        .validate_declared_ids(&["logs"], &["analytics"])
        .expect_err("unknown s3 id");
    assert!(
        matches!(error, DagError::UndeclaredResourceId { kind: "s3_buckets", ref id } if id == "media"),
        "{error:?}"
    );
    let error = cfg
        .resources
        .validate_declared_ids(&["media"], &["analytics"])
        .expect_err("unknown pg id");
    assert!(
        matches!(error, DagError::UndeclaredResourceId { kind: "pg_databases", ref id } if id == "main"),
        "{error:?}"
    );
    cfg.resources
        .validate_declared_ids(&["media"], &["main"])
        .expect("declared ids resolve");
}

#[test]
fn resources_single_resolve_enforces_remote_and_refresh_dump_policy() {
    let pg_only = SlipforkConfig::parse_toml(
        r#"
[resources.pg_databases.main]
local = "remote"
"#,
    )
    .expect("remote pg spec");

    let error = ResourcePlan::resolve(
        &pg_only.resources,
        ResourceMode::Local,
        DumpPolicy::Default,
        false,
    )
    .expect_err("remote pg requires an OSPP session");
    assert!(
        matches!(
            error,
            DagError::RemoteResourceUnavailable {
                kind: "pg_database",
                ..
            }
        ),
        "{error:?}"
    );

    let default = ResourcePlan::resolve(
        &pg_only.resources,
        ResourceMode::Local,
        DumpPolicy::Default,
        true,
    )
    .expect("remote dump");
    assert_eq!(
        default.pg_database("main").expect("main").seed,
        PgSeed::RemoteDump { refresh: false }
    );

    let refreshed = ResourcePlan::resolve(
        &pg_only.resources,
        ResourceMode::Local,
        DumpPolicy::RefreshDump,
        true,
    )
    .expect("refresh dump");
    assert_eq!(
        refreshed.pg_database("main").expect("main").seed,
        PgSeed::RemoteDump { refresh: true }
    );

    // `--no-dump` skips the dump and uses a clean initdb without requiring a session.
    let no_dump = ResourcePlan::resolve(
        &pg_only.resources,
        ResourceMode::Local,
        DumpPolicy::NoDump,
        false,
    )
    .expect("no dump");
    let no_dump_main = no_dump.pg_database("main").expect("main");
    assert_eq!(no_dump_main.seed, PgSeed::CleanInitdb);
    assert_eq!(no_dump_main.reason, ResourceReason::NoDump);
    // `NoDump` forces the effective strategy local even though `local = "remote"` is configured,
    // so the plan's `strategy` and `target` never contradict each other.
    assert_eq!(no_dump_main.strategy, ResourceStrategy::Local);
    assert_eq!(no_dump_main.target(), ResourceTarget::Local);
    let no_dump_diag = no_dump
        .diagnostics()
        .into_iter()
        .find(|row| row.id == "main")
        .expect("main diagnostic");
    assert_eq!(no_dump_diag.strategy, ResourceStrategy::Local);
    assert_eq!(no_dump_diag.resolved, ResourceTarget::Local);
    assert_eq!(no_dump_diag.reason, ResourceReason::NoDump);

    // `RefreshDump` is a configuration conflict with the hermetic `Test` default (`Local`).
    let error = ResourcePlan::resolve(
        &pg_only.resources,
        ResourceMode::Test,
        DumpPolicy::RefreshDump,
        true,
    )
    .expect_err("refresh dump requires a non-local strategy");
    assert!(
        matches!(error, DagError::RefreshDumpRequiresRemote { ref id } if id == "main"),
        "{error:?}"
    );

    // An explicit `test = "remote"` override makes the same refresh legal.
    let test_remote = SlipforkConfig::parse_toml(
        r#"
[resources.pg_databases.main]
test = "remote"
"#,
    )
    .expect("test remote spec");
    let plan = ResourcePlan::resolve(
        &test_remote.resources,
        ResourceMode::Test,
        DumpPolicy::RefreshDump,
        true,
    )
    .expect("explicit test remote refresh");
    assert_eq!(
        plan.pg_database("main").expect("main").seed,
        PgSeed::RemoteDump { refresh: true }
    );
}
