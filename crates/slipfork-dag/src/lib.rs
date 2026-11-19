//! `slipfork-dag`: Orthogonal Build Artifact Volumes + Stateful Fixture Snapshot DAG.
//!
//! Separates stateless build outputs (`[artifacts.<id>]`) from stateful CoW service snapshots
//! (`[fixtures.<id>]`) so frontend asset changes never invalidate seeded database snapshots
//! and schema/seed changes never rebuild frontend bundles (`ADR-0003`). Also owns the typed
//! `[test]` coverage schema (`ADR-0006` §5), and mode-aware `[resources]` resolution
//! (`ADR-0008` §3). Project identity and per-service containers come from `Forkfile`; this crate
//! owns no world-image selection.
//!
//! The domain core is filesystem-free: concrete filesystem adapters live behind the `fs` feature.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use slipfork_world::ProjectSlug;
use slipfork_world::merkle::{file_digest, glob_matches, merkle_hex};
use thiserror::Error;

mod resources;

#[cfg(feature = "fs")]
mod fs;

pub use resources::{
    DumpPolicy, PgSeed, PlannedPgDatabase, PlannedS3Bucket, ResourceDiagnostic, ResourceKind,
    ResourceMode, ResourceOverride, ResourcePlan, ResourceReason, ResourceStrategy, ResourceTarget,
    ResourcesSpec, S3Resolution,
};

#[cfg(feature = "fs")]
pub use fs::{FsDigestCache, FsMerkleHasher};

/// Errors produced when parsing or planning a `Forkfile` DAG.
#[derive(Debug, Error)]
pub enum DagError {
    #[error("invalid Forkfile: {0}")]
    Toml(String),
    #[error("fixture '{fixture}' references unknown parent '{parent}'")]
    UnknownParent { fixture: String, parent: String },
    #[error(
        "fixture '{fixture}' is origin-independent but its parent '{parent}' is `per_world = true`"
    )]
    OriginIndependentChildOfPerWorld { fixture: String, parent: String },
    #[error("dependency cycle detected in fixtures involving '{0}'")]
    CycleDetected(String),
    #[error("`coverage_report` / `coverage_fail_under` require `coverage = true` under [test]")]
    CoverageOptionsRequireEnabled,
    #[error("`coverage_report` must list at least one report format when `coverage = true`")]
    EmptyCoverageReport,
    #[error("`coverage_fail_under` must be an integer in 0..=100, got {value}")]
    InvalidCoverageThreshold { value: String },
    #[error("[resources.{kind}.{id}] must declare at least one of `local` or `test`")]
    EmptyResourceOverride { kind: &'static str, id: String },
    #[error("[resources] declares {kind} '{id}' which is absent from the Forkfile")]
    UndeclaredResourceId { kind: &'static str, id: String },
    #[error("resource '{kind}.{id}' requires an authenticated OSPP session in {mode:?} mode")]
    RemoteResourceUnavailable {
        kind: &'static str,
        id: String,
        mode: ResourceMode,
    },
    #[error("`--refresh-dump` cannot be combined with a local strategy for pg_databases '{id}'")]
    RefreshDumpRequiresRemote { id: String },
    #[error("input hashing I/O error at {path}: {message}")]
    Io { path: String, message: String },
}

/// One stateless build artifact specification (`[artifacts.<id>]`).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSpec {
    pub inputs: Vec<String>,
    pub build: String,
    pub mount: String,
}

/// One stateful CoW fixture snapshot specification (`[fixtures.<id>]`).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FixtureSpec {
    pub parent: Option<String>,
    #[serde(default)]
    pub inputs: Vec<String>,
    /// `true` when this step writes absolute origin URLs into persistent state (e.g. Django
    /// `django.contrib.sites` rows), so it can never be sealed into a snapshot shared across
    /// worlds. A `per_world = true` fixture cannot be the `parent` of an origin-independent one
    /// (`DagError::OriginIndependentChildOfPerWorld`, `ADR-0003` §1), and the planner never reports
    /// it `CacheAction::Cached` because the digest cache is world-independent.
    #[serde(default)]
    pub per_world: bool,
    pub run: String,
}

/// One coverage report format requested through `[test].coverage_report` (`ADR-0006` §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageReportFormat {
    Html,
    Xml,
    Json,
    Lcov,
    Text,
}

/// Validated integer coverage percentage in `0..=100` (`DagError::InvalidCoverageThreshold`).
///
/// Constructed only by parse-time validation of `[test].coverage_fail_under`; the inner value is
/// private so no caller can smuggle an out-of-range percentage into a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverageThreshold(u8);

impl CoverageThreshold {
    /// Highest accepted percentage.
    pub const MAX: u8 = 100;

    /// The validated percentage.
    pub fn percent(self) -> u8 {
        self.0
    }

    fn parse(value: &toml::Value) -> Result<Self, DagError> {
        let invalid = || DagError::InvalidCoverageThreshold {
            value: value.to_string(),
        };
        let percent =
            u8::try_from(value.as_integer().ok_or_else(invalid)?).map_err(|_| invalid())?;
        if percent > Self::MAX {
            return Err(invalid());
        }
        Ok(Self(percent))
    }
}

/// Typed coverage intent parsed from `[test]` (`ADR-0006` §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageConfig {
    /// Requested report formats; defaults to `{Text}` when `coverage_report` is omitted.
    pub reports: BTreeSet<CoverageReportFormat>,
    /// Optional threshold enforced by `TestCoordinator` in M3.
    pub fail_under: Option<CoverageThreshold>,
}

/// Suite-level test runner configuration (`[test]` in `<vcs_root>/Forkfile`, `ADR-0006` §5).
///
/// `coverage = true` is the sole TOML activation switch: it parses into `Some(CoverageConfig)`
/// with `coverage_report` defaulting to `{text}`. `coverage_report` / `coverage_fail_under`
/// without `coverage = true` fail parse-time validation
/// (`DagError::CoverageOptionsRequireEnabled`), an explicitly empty `coverage_report` fails closed
/// (`DagError::EmptyCoverageReport`), and `coverage_fail_under` is validated into
/// [`CoverageThreshold`] (`DagError::InvalidCoverageThreshold`). Runtime execution ships in
/// Milestone 3 (`crates/slipfork-test`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TestSpec {
    pub coverage: Option<CoverageConfig>,
}

/// Raw `[test]` wire table; validated into [`TestSpec`] by [`SlipforkConfig::parse_toml`].
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTestSpec {
    coverage: Option<bool>,
    coverage_report: Option<Vec<CoverageReportFormat>>,
    coverage_fail_under: Option<toml::Value>,
}

impl RawTestSpec {
    fn into_test_spec(self) -> Result<TestSpec, DagError> {
        if self.coverage != Some(true) {
            if self.coverage_report.is_some() || self.coverage_fail_under.is_some() {
                return Err(DagError::CoverageOptionsRequireEnabled);
            }
            return Ok(TestSpec::default());
        }
        let reports = match self.coverage_report {
            Some(reports) if reports.is_empty() => return Err(DagError::EmptyCoverageReport),
            Some(reports) => reports.into_iter().collect(),
            None => BTreeSet::from([CoverageReportFormat::Text]),
        };
        let fail_under = self
            .coverage_fail_under
            .as_ref()
            .map(CoverageThreshold::parse)
            .transpose()?;
        Ok(TestSpec {
            coverage: Some(CoverageConfig {
                reports,
                fail_under,
            }),
        })
    }
}

/// Root `<vcs_root>/Forkfile` manifest owning `project`, `[artifacts]`, `[fixtures]`,
/// `[test]`, and `[resources]`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SlipforkConfig {
    /// Optional canonical project slug override (defaults to the canonical git/jj root name).
    pub project: Option<ProjectSlug>,
    pub artifacts: BTreeMap<String, ArtifactSpec>,
    pub fixtures: BTreeMap<String, FixtureSpec>,
    pub test: TestSpec,
    pub resources: ResourcesSpec,
}

/// Raw root wire document; validated into [`SlipforkConfig`] by [`SlipforkConfig::parse_toml`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSlipforkConfig {
    project: Option<ProjectSlug>,
    #[serde(default)]
    artifacts: BTreeMap<String, ArtifactSpec>,
    #[serde(default)]
    fixtures: BTreeMap<String, FixtureSpec>,
    #[serde(default)]
    test: RawTestSpec,
    #[serde(default)]
    resources: resources::RawResourcesSpec,
}

impl SlipforkConfig {
    /// Parse and validate a `Forkfile` document.
    pub fn parse_toml(raw: &str) -> Result<Self, DagError> {
        let raw: RawSlipforkConfig =
            toml::from_str(raw).map_err(|e| DagError::Toml(e.to_string()))?;
        Ok(Self {
            project: raw.project,
            artifacts: raw.artifacts,
            fixtures: raw.fixtures,
            test: raw.test.into_test_spec()?,
            resources: ResourcesSpec::from_raw(raw.resources)?,
        })
    }
}

/// Port for computing deterministic SHA-256 Merkle digests over repo input globs.
pub trait InputHasher {
    /// Compute a hex SHA-256 digest of all matching files (sorted by relative path).
    fn hash_globs(&self, globs: &[String]) -> Result<String, DagError>;
}

/// Port for querying whether a content-addressed artifact or snapshot already exists on disk.
pub trait DigestCache {
    /// Return `true` if `artifact_digest` is already materialized.
    fn has_artifact(&self, digest: &str) -> bool;
    /// Return `true` if `fixture_digest` is already materialized.
    fn has_fixture(&self, digest: &str) -> bool;
}

/// Cache disposition of a planned artifact or fixture node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheAction {
    Cached,
    Rebuild,
}

/// Planned execution record for one stateless artifact volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedArtifact {
    pub id: String,
    pub digest: String,
    pub input_merkle: String,
    pub action: CacheAction,
    pub build: String,
    pub mount: String,
}

/// Planned execution record for one stateful fixture snapshot node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedFixture {
    pub id: String,
    pub parent: Option<String>,
    pub digest: String,
    pub input_merkle: String,
    pub action: CacheAction,
    pub per_world: bool,
    pub run: String,
}

/// Complete orthogonal execution plan (parallel artifacts + topological fixture waves).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DagPlan {
    pub artifacts: Vec<PlannedArtifact>,
    pub fixture_waves: Vec<Vec<PlannedFixture>>,
}

impl DagPlan {
    /// Look up a planned fixture by `id`.
    pub fn fixture(&self, id: &str) -> Option<&PlannedFixture> {
        self.fixture_waves.iter().flatten().find(|f| f.id == id)
    }

    /// Look up a planned artifact by `id`.
    pub fn artifact(&self, id: &str) -> Option<&PlannedArtifact> {
        self.artifacts.iter().find(|a| a.id == id)
    }
}

/// Pure topological planner for orthogonal artifacts and fixture snapshots.
pub struct DagPlanner;

impl DagPlanner {
    /// Compute digests, cache hits/misses, and parallel topological waves.
    pub fn plan(
        config: &SlipforkConfig,
        build_envs: &BTreeMap<String, String>,
        runtime_envs: &BTreeMap<String, String>,
        hasher: &dyn InputHasher,
        cache: &dyn DigestCache,
    ) -> Result<DagPlan, DagError> {
        // 1. Validate fixture parent links and compute topological waves.
        for (id, spec) in &config.fixtures {
            if let Some(parent) = &spec.parent {
                let Some(parent_spec) = config.fixtures.get(parent) else {
                    return Err(DagError::UnknownParent {
                        fixture: id.clone(),
                        parent: parent.clone(),
                    });
                };
                if parent_spec.per_world && !spec.per_world {
                    return Err(DagError::OriginIndependentChildOfPerWorld {
                        fixture: id.clone(),
                        parent: parent.clone(),
                    });
                }
            }
        }

        let waves = topo_waves(&config.fixtures)?;

        // 2. Plan orthogonal artifacts (depend ONLY on build_envs + their own inputs).
        let mut artifacts = Vec::with_capacity(config.artifacts.len());
        for (id, spec) in &config.artifacts {
            let input_merkle = hasher.hash_globs(&spec.inputs)?;
            let mut h = Sha256::new();
            h.update(b"slipfork-artifact:v1\0");
            h.update(id.as_bytes());
            h.update(b"\0");
            h.update(spec.build.as_bytes());
            h.update(b"\0");
            h.update(spec.mount.as_bytes());
            h.update(b"\0");
            feed_env_map(&mut h, build_envs);
            h.update(input_merkle.as_bytes());
            let digest = hex::encode(h.finalize());
            let action = if cache.has_artifact(&digest) {
                CacheAction::Cached
            } else {
                CacheAction::Rebuild
            };
            artifacts.push(PlannedArtifact {
                id: id.clone(),
                digest,
                input_merkle,
                action,
                build: spec.build.clone(),
                mount: spec.mount.clone(),
            });
        }

        // 3. Plan stateful fixture waves (depend on parent fixture digest + runtime_envs + inputs).
        let mut digests: BTreeMap<String, String> = BTreeMap::new();
        let mut planned_waves = Vec::with_capacity(waves.len());
        for wave_ids in waves {
            let mut wave = Vec::with_capacity(wave_ids.len());
            for id in wave_ids {
                let spec = &config.fixtures[&id];
                let parent_digest = match &spec.parent {
                    Some(p) => digests[p].as_str(),
                    None => "root:base-v1",
                };
                let input_merkle = hasher.hash_globs(&spec.inputs)?;
                let mut h = Sha256::new();
                h.update(b"slipfork-fixture:v1\0");
                h.update(id.as_bytes());
                h.update(b"\0");
                h.update(parent_digest.as_bytes());
                h.update(b"\0");
                h.update(spec.run.as_bytes());
                h.update(b"\0");
                h.update(if spec.per_world {
                    &b"per-world"[..]
                } else {
                    &b"shared"[..]
                });
                h.update(b"\0");
                feed_env_map(&mut h, runtime_envs);
                h.update(input_merkle.as_bytes());
                let digest = hex::encode(h.finalize());
                // `DigestCache` is world-independent by design (its port shape carries no world
                // discriminator), so a `per_world = true` snapshot can never be served from it:
                // it bakes this world's absolute origin URLs into persistent state (`ADR-0003` §1).
                let action = if spec.per_world || !cache.has_fixture(&digest) {
                    CacheAction::Rebuild
                } else {
                    CacheAction::Cached
                };
                digests.insert(id.clone(), digest.clone());
                wave.push(PlannedFixture {
                    id,
                    parent: spec.parent.clone(),
                    digest,
                    input_merkle,
                    action,
                    per_world: spec.per_world,
                    run: spec.run.clone(),
                });
            }
            planned_waves.push(wave);
        }

        Ok(DagPlan {
            artifacts,
            fixture_waves: planned_waves,
        })
    }
}

fn feed_env_map(hasher: &mut Sha256, envs: &BTreeMap<String, String>) {
    for (k, v) in envs {
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
        hasher.update(b"\0");
    }
}

fn topo_waves(fixtures: &BTreeMap<String, FixtureSpec>) -> Result<Vec<Vec<String>>, DagError> {
    let mut remaining: BTreeSet<String> = fixtures.keys().cloned().collect();
    let mut placed: BTreeSet<String> = BTreeSet::new();
    let mut waves = Vec::new();

    while !remaining.is_empty() {
        let ready: Vec<String> = remaining
            .iter()
            .filter(|id| match &fixtures[*id].parent {
                None => true,
                Some(p) => placed.contains(p),
            })
            .cloned()
            .collect();
        if ready.is_empty() {
            let first = remaining.iter().next().cloned().unwrap_or_default();
            return Err(DagError::CycleDetected(first));
        }
        for id in &ready {
            remaining.remove(id);
            placed.insert(id.clone());
        }
        waves.push(ready);
    }
    Ok(waves)
}

/// Pure in-memory [`InputHasher`] for deterministic unit & critical tests.
#[derive(Debug, Default, Clone)]
pub struct MapInputHasher {
    pub files: BTreeMap<String, Vec<u8>>,
}

impl InputHasher for MapInputHasher {
    fn hash_globs(&self, globs: &[String]) -> Result<String, DagError> {
        let entries = self
            .files
            .iter()
            .filter(|(rel_path, _)| globs.iter().any(|glob| glob_matches(glob, rel_path)))
            .map(|(rel_path, content)| (rel_path.as_str(), file_digest(content)));
        Ok(merkle_hex(entries))
    }
}

/// In-memory [`DigestCache`] for tests.
#[derive(Debug, Default, Clone)]
pub struct SetDigestCache {
    pub artifacts: BTreeSet<String>,
    pub fixtures: BTreeSet<String>,
}

impl DigestCache for SetDigestCache {
    fn has_artifact(&self, digest: &str) -> bool {
        self.artifacts.contains(digest)
    }

    fn has_fixture(&self, digest: &str) -> bool {
        self.fixtures.contains(digest)
    }
}
