//! `slipfork-dag`: Orthogonal Build Artifact Volumes + Stateful Fixture Snapshot DAG.
//!
//! Separates stateless build outputs (`[artifacts.<id>]`) from stateful CoW service snapshots
//! (`[fixtures.<id>]`) so frontend asset changes never invalidate seeded database snapshots
//! and schema/seed changes never rebuild frontend bundles (`ADR-0003`).

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use slipfork_world::ProjectSlug;
use thiserror::Error;

/// Errors produced when parsing or planning a `slipfork.toml` DAG.
#[derive(Debug, Error)]
pub enum DagError {
    #[error("invalid slipfork.toml: {0}")]
    Toml(String),
    #[error("fixture '{fixture}' references unknown parent '{parent}'")]
    UnknownParent { fixture: String, parent: String },
    #[error("dependency cycle detected in fixtures involving '{0}'")]
    CycleDetected(String),
    #[error("input hashing I/O error at {path}: {message}")]
    Io { path: String, message: String },
}

/// One stateless build artifact specification (`[artifacts.<id>]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSpec {
    pub inputs: Vec<String>,
    pub build: String,
    pub mount: String,
}

/// One stateful CoW fixture snapshot specification (`[fixtures.<id>]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FixtureSpec {
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub inputs: Vec<String>,
    pub run: String,
}

/// Suite-level test runner configuration (`[test]` in `<vcs_root>/slipfork.toml`, `ADR-0006` §4).
///
/// Parsed here as part of the single [`SlipforkConfig`] root schema; runtime hook execution ships
/// in Milestone 3 (`crates/slipfork-test`).
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestSpec {
    #[serde(default)]
    pub after_suite: Option<String>,
}

/// Root `<vcs_root>/slipfork.toml` manifest owning `project`, `[artifacts]`, `[fixtures]`, and `[test]`.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SlipforkConfig {
    /// Optional canonical project slug override (defaults to the canonical git/jj root name).
    #[serde(default)]
    pub project: Option<ProjectSlug>,
    #[serde(default)]
    pub artifacts: BTreeMap<String, ArtifactSpec>,
    #[serde(default)]
    pub fixtures: BTreeMap<String, FixtureSpec>,
    #[serde(default)]
    pub test: TestSpec,
}

impl SlipforkConfig {
    /// Parse and validate a `slipfork.toml` document.
    pub fn parse_toml(raw: &str) -> Result<Self, DagError> {
        toml::from_str(raw).map_err(|e| DagError::Toml(e.to_string()))
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
            if let Some(parent) = &spec.parent
                && !config.fixtures.contains_key(parent)
            {
                return Err(DagError::UnknownParent {
                    fixture: id.clone(),
                    parent: parent.clone(),
                });
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
                feed_env_map(&mut h, runtime_envs);
                h.update(input_merkle.as_bytes());
                let digest = hex::encode(h.finalize());
                let action = if cache.has_fixture(&digest) {
                    CacheAction::Cached
                } else {
                    CacheAction::Rebuild
                };
                digests.insert(id.clone(), digest.clone());
                wave.push(PlannedFixture {
                    id,
                    parent: spec.parent.clone(),
                    digest,
                    input_merkle,
                    action,
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
        let mut h = Sha256::new();
        for (rel_path, content) in &self.files {
            if globs.iter().any(|g| matches_glob(g, rel_path)) {
                h.update(rel_path.as_bytes());
                h.update(b"\0");
                let file_hash = Sha256::digest(content);
                h.update(file_hash);
            }
        }
        Ok(hex::encode(h.finalize()))
    }
}

/// Filesystem [`InputHasher`] walking a repository root deterministically.
#[derive(Debug, Clone)]
pub struct FsMerkleHasher {
    root: PathBuf,
}

impl FsMerkleHasher {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }
}

impl InputHasher for FsMerkleHasher {
    fn hash_globs(&self, globs: &[String]) -> Result<String, DagError> {
        if globs.is_empty() {
            return Ok(hex::encode(Sha256::digest(b"")));
        }
        let mut matched: BTreeMap<String, PathBuf> = BTreeMap::new();
        collect_files(&self.root, &self.root, globs, &mut matched)?;
        let mut h = Sha256::new();
        for (rel, full) in matched {
            let bytes = fs::read(&full).map_err(|e| DagError::Io {
                path: full.display().to_string(),
                message: e.to_string(),
            })?;
            h.update(rel.as_bytes());
            h.update(b"\0");
            h.update(Sha256::digest(&bytes));
        }
        Ok(hex::encode(h.finalize()))
    }
}

fn collect_files(
    root: &Path,
    dir: &Path,
    globs: &[String],
    out: &mut BTreeMap<String, PathBuf>,
) -> Result<(), DagError> {
    let entries = fs::read_dir(dir).map_err(|e| DagError::Io {
        path: dir.display().to_string(),
        message: e.to_string(),
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| DagError::Io {
            path: dir.display().to_string(),
            message: e.to_string(),
        })?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if matches!(
            name_str.as_ref(),
            ".git" | ".worktrees" | ".slipfork" | "target" | "node_modules" | ".venv"
        ) {
            continue;
        }
        let path = entry.path();
        let ft = entry.file_type().map_err(|e| DagError::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        if ft.is_dir() {
            collect_files(root, &path, globs, out)?;
        } else if ft.is_file()
            && let Ok(rel) = path.strip_prefix(root)
        {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if globs.iter().any(|g| matches_glob(g, &rel_str)) {
                out.insert(rel_str, path);
            }
        }
    }
    Ok(())
}

/// Simple, dependency-free repo glob matcher supporting `**/`, `**`, `*`, and `?`.
pub fn matches_glob(pattern: &str, path: &str) -> bool {
    let pat_segs: Vec<&str> = pattern
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let path_segs: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    match_segments(&pat_segs, &path_segs)
}

fn match_segments(pat: &[&str], path: &[&str]) -> bool {
    match (pat.first(), path.first()) {
        (None, None) => true,
        (Some(&"**"), _) => {
            match_segments(&pat[1..], path) || (!path.is_empty() && match_segments(pat, &path[1..]))
        }
        (Some(p), Some(s)) if match_segment(p, s) => match_segments(&pat[1..], &path[1..]),
        _ => false,
    }
}

fn match_segment(pat: &str, text: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star_pi, mut star_ti) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_pi = Some(pi);
            pi += 1;
            star_ti = ti;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
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

/// Directory-backed [`DigestCache`] checking `<cache_root>/artifacts/<digest>` and `<cache_root>/fixtures/<digest>`.
#[derive(Debug, Clone)]
pub struct FsDigestCache {
    root: PathBuf,
}

impl FsDigestCache {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }
}

impl DigestCache for FsDigestCache {
    fn has_artifact(&self, digest: &str) -> bool {
        self.root.join("artifacts").join(digest).exists()
    }

    fn has_fixture(&self, digest: &str) -> bool {
        self.root.join("fixtures").join(digest).exists()
    }
}
