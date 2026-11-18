//! Mode-aware `[resources]` `ResourceStrategy` schema and the single
//! [`ResourcePlan::resolve`] decision point (`ADR-0008` §3).
//!
//! `slipfork.toml` declares deliberate overrides only. The mode default comes from
//! [`ResourcesSpec::defaults_for`]: `local` worlds (`Canonical` / `UserFork`) prefer OSPP dev
//! resources, while `TestSlot` worlds are hermetically local by construction. [`ResourcePlan`]
//! owns every S3-bucket and PostgreSQL-seed decision consumed by `slipforkd` and M3's
//! `TestCoordinator`; there is no second precedence ladder.
//!
//! OSPP v1 no longer carries a client-visible "resource offer": remote coordinates arrive as
//! ordinary component environment values (`env_resolved` / `env_updated`) and remote PostgreSQL
//! dumps are requested explicitly through `OsppSession::dump`. The only OSPP fact the plan needs
//! is therefore whether an authenticated session exists.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use slipfork_world::WorldTarget;

use crate::DagError;

/// World mode that selects the `[resources]` default strategy (`ADR-0008` §3).
///
/// Derived strictly from [`WorldTarget`] through [`From<&WorldTarget>`]; it is never configured
/// and therefore never implements `Deserialize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceMode {
    /// Canonical or user-fork world: prefers OSPP shared dev resources.
    Local,
    /// `TestSlot` world: hermetically local unless a resource explicitly overrides it.
    Test,
}

impl From<&WorldTarget> for ResourceMode {
    fn from(target: &WorldTarget) -> Self {
        match target {
            WorldTarget::Canonical | WorldTarget::UserFork(_) => Self::Local,
            WorldTarget::TestSlot(_) => Self::Test,
        }
    }
}

/// Effective strategy for one declared S3 bucket or PostgreSQL database (`ADR-0008` §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceStrategy {
    /// World-local coordinates (`127.0.0.1:9000` inside the world `netns`) / clean `initdb`.
    Local,
    /// Use the OSPP session when paired, otherwise fall back to local with a diagnostic.
    PreferRemote,
    /// Require an authenticated OSPP session; fail closed when it is absent.
    Remote,
}

impl ResourceStrategy {
    fn is_local(self) -> bool {
        matches!(self, Self::Local)
    }
}

/// Mutually exclusive dump flags (`--no-dump | --refresh-dump`, `ADR-0008` §3).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DumpPolicy {
    /// No flag: the effective [`ResourceStrategy`] decides.
    #[default]
    Default,
    /// `--no-dump`: skip seed dumps and use a clean `initdb` with a diagnostic.
    NoDump,
    /// `--refresh-dump`: require a live OSPP session; fail closed when local or unpaired.
    RefreshDump,
}

/// Per-mode override declared under `[resources.<kind>.<id>]`.
///
/// Fields are private so an empty override cannot be constructed outside this module; the
/// parser fails closed on an override table that sets neither `local` nor `test`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceOverride {
    local: Option<ResourceStrategy>,
    test: Option<ResourceStrategy>,
}

impl ResourceOverride {
    /// Explicit strategy for `local` worlds, if declared.
    pub fn local(self) -> Option<ResourceStrategy> {
        self.local
    }

    /// Explicit strategy for `TestSlot` worlds, if declared.
    pub fn test(self) -> Option<ResourceStrategy> {
        self.test
    }

    fn for_mode(self, mode: ResourceMode) -> Option<ResourceStrategy> {
        match mode {
            ResourceMode::Local => self.local,
            ResourceMode::Test => self.test,
        }
    }
}

/// Typed `[resources]` table from `slipfork.toml` (`ADR-0008` §3).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ResourcesSpec {
    s3_buckets: BTreeMap<String, ResourceOverride>,
    pg_databases: BTreeMap<String, ResourceOverride>,
}

impl ResourcesSpec {
    /// Declared S3-bucket overrides keyed by `snow.yml` bucket id.
    pub fn s3_buckets(&self) -> &BTreeMap<String, ResourceOverride> {
        &self.s3_buckets
    }

    /// Declared PostgreSQL-database overrides keyed by `snow.yml` database id.
    pub fn pg_databases(&self) -> &BTreeMap<String, ResourceOverride> {
        &self.pg_databases
    }

    /// `true` when no override was declared (the standard workflow).
    pub fn is_empty(&self) -> bool {
        self.s3_buckets.is_empty() && self.pg_databases.is_empty()
    }

    /// Extend this override table with every id declared by the manifest.
    ///
    /// `ResourcePlan::resolve` reports one decision per declared id, so `slipforkd` first covers
    /// the ids declared by `snow.yml`; an id without an explicit override keeps
    /// [`ResourcesSpec::defaults_for`] precedence and reports `ResourceReason::ModeDefault`.
    pub fn with_declared_ids(&self, s3_ids: &[&str], pg_ids: &[&str]) -> Self {
        let mut spec = self.clone();
        for id in s3_ids {
            spec.s3_buckets
                .entry((*id).to_owned())
                .or_insert(ResourceOverride {
                    local: None,
                    test: None,
                });
        }
        for id in pg_ids {
            spec.pg_databases
                .entry((*id).to_owned())
                .or_insert(ResourceOverride {
                    local: None,
                    test: None,
                });
        }
        spec
    }

    /// Force every declared resource to `strategy` for both modes.
    ///
    /// This is the typed `--resources local` per-invocation override (`ADR-0008` §3): it rewrites
    /// only the effective overrides and keeps [`ResourcePlan::resolve`] the single decision point.
    pub fn forced_strategy(&self, strategy: ResourceStrategy) -> Self {
        let override_spec = ResourceOverride {
            local: Some(strategy),
            test: Some(strategy),
        };
        Self {
            s3_buckets: self
                .s3_buckets
                .keys()
                .map(|id| (id.clone(), override_spec))
                .collect(),
            pg_databases: self
                .pg_databases
                .keys()
                .map(|id| (id.clone(), override_spec))
                .collect(),
        }
    }

    /// The single mode-aware default (`ADR-0008` §3): local worlds prefer OSPP dev resources,
    /// while `TestSlot` worlds are hermetically local.
    pub fn defaults_for(mode: ResourceMode) -> ResourceStrategy {
        match mode {
            ResourceMode::Local => ResourceStrategy::PreferRemote,
            ResourceMode::Test => ResourceStrategy::Local,
        }
    }

    /// Fail closed when `[resources]` names an id absent from `snow.yml`.
    ///
    /// `slipfork-dag` never depends on `slipfork-snow`; the Composition Root passes the plain id
    /// slices it already owns.
    pub fn validate_declared_ids(
        &self,
        snow_s3_ids: &[&str],
        snow_pg_ids: &[&str],
    ) -> Result<(), DagError> {
        for id in self.s3_buckets.keys() {
            if !snow_s3_ids.contains(&id.as_str()) {
                return Err(DagError::UndeclaredResourceId {
                    kind: "s3_buckets",
                    id: id.clone(),
                });
            }
        }
        for id in self.pg_databases.keys() {
            if !snow_pg_ids.contains(&id.as_str()) {
                return Err(DagError::UndeclaredResourceId {
                    kind: "pg_databases",
                    id: id.clone(),
                });
            }
        }
        Ok(())
    }

    pub(crate) fn from_raw(raw: RawResourcesSpec) -> Result<Self, DagError> {
        Ok(Self {
            s3_buckets: parse_overrides("s3_buckets", raw.s3_buckets)?,
            pg_databases: parse_overrides("pg_databases", raw.pg_databases)?,
        })
    }
}

fn parse_overrides(
    kind: &'static str,
    raw: BTreeMap<String, RawResourceOverride>,
) -> Result<BTreeMap<String, ResourceOverride>, DagError> {
    raw.into_iter()
        .map(|(id, raw)| {
            let override_spec = ResourceOverride {
                local: raw.local,
                test: raw.test,
            };
            if override_spec.local.is_none() && override_spec.test.is_none() {
                return Err(DagError::EmptyResourceOverride { kind, id });
            }
            Ok((id, override_spec))
        })
        .collect()
}

/// Raw `[resources]` wire table; `deny_unknown_fields` keeps typos hard errors.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawResourcesSpec {
    #[serde(default)]
    s3_buckets: BTreeMap<String, RawResourceOverride>,
    #[serde(default)]
    pg_databases: BTreeMap<String, RawResourceOverride>,
}

/// Raw `[resources.<kind>.<id>]` override; zero boolean mode flags.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawResourceOverride {
    local: Option<ResourceStrategy>,
    test: Option<ResourceStrategy>,
}

/// Where a planned resource resolves to (`ADR-0008` §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceTarget {
    /// World-local coordinates / clean `initdb`.
    Local,
    /// The OSPP session owns the coordinates or supplies the seed dump.
    Remote,
}

/// Why [`ResourcePlan::resolve`] picked the winning strategy and target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceReason {
    /// Explicitly configured under `[resources.<kind>.<id>]`.
    Configured,
    /// Selected by [`ResourcesSpec::defaults_for`] because the resource has no override.
    ModeDefault,
    /// `prefer_remote` had no authenticated OSPP session, so it used local with this diagnostic.
    NoRemoteSession,
    /// `--no-dump` skipped the seed dump and uses a clean `initdb`.
    NoDump,
    /// `--refresh-dump` forced a live remote seed dump.
    RefreshedDump,
}

/// Resource kind reported by [`ResourcePlan::diagnostics`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    /// `[resources.s3_buckets.<id>]`.
    S3Bucket,
    /// `[resources.pg_databases.<id>]`.
    PgDatabase,
}

/// Resolution of one S3 bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S3Resolution {
    /// The world-local `127.0.0.1:9000` SeaweedFS container (the only client-side S3 endpoint;
    /// remote credentials, when used, arrive as ordinary OSPP environment values).
    Local,
}

impl S3Resolution {
    /// Local vs remote target for diagnostics.
    pub const fn target(self) -> ResourceTarget {
        ResourceTarget::Local
    }
}

/// Resolution of one PostgreSQL database seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PgSeed {
    /// Clean `initdb`, then migrations and `[fixtures.*]` (never touches shared dev state).
    CleanInitdb,
    /// Seed from a live OSPP `dump_request` streamed straight into `pg_restore`.
    RemoteDump {
        /// `--refresh-dump` bypasses any cached copy and requires a live dump request.
        refresh: bool,
    },
}

impl PgSeed {
    /// Local vs remote target for diagnostics.
    pub fn target(&self) -> ResourceTarget {
        match self {
            Self::CleanInitdb => ResourceTarget::Local,
            Self::RemoteDump { .. } => ResourceTarget::Remote,
        }
    }
}

/// Planned S3 bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedS3Bucket {
    pub id: String,
    /// Effective strategy for this world (dump policy never changes S3 resolution).
    pub strategy: ResourceStrategy,
    pub reason: ResourceReason,
    pub resolution: S3Resolution,
}

impl PlannedS3Bucket {
    /// Local vs remote target for diagnostics.
    pub fn target(&self) -> ResourceTarget {
        self.resolution.target()
    }
}

/// Planned PostgreSQL database seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPgDatabase {
    pub id: String,
    /// Effective strategy after applying [`DumpPolicy`].
    ///
    /// [`DumpPolicy::NoDump`] forces [`ResourceStrategy::Local`] because the plan seeds from a
    /// clean `initdb`, never from an OSPP dump; [`ResourceReason::NoDump`] records that override.
    /// Every other policy keeps the strategy's target consistent with [`Self::target`].
    pub strategy: ResourceStrategy,
    pub reason: ResourceReason,
    pub seed: PgSeed,
}

impl PlannedPgDatabase {
    /// Local vs remote target for diagnostics.
    pub fn target(&self) -> ResourceTarget {
        self.seed.target()
    }
}

/// Complete S3 + PostgreSQL resolution for one world (`ADR-0008` §3).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ResourcePlan {
    pub s3_buckets: Vec<PlannedS3Bucket>,
    pub pg_databases: Vec<PlannedPgDatabase>,
}

impl ResourcePlan {
    /// Sole decision point for `[resources]` resolution (`ADR-0008` §3).
    ///
    /// `mode` is derived from [`WorldTarget`]; `ospp_paired` reports whether an authenticated
    /// `ospp-v1` session exists right now. The plan covers every id declared in the spec, sorted
    /// by id for deterministic diagnostics.
    pub fn resolve(
        spec: &ResourcesSpec,
        mode: ResourceMode,
        dump_policy: DumpPolicy,
        ospp_paired: bool,
    ) -> Result<Self, DagError> {
        let s3_ids: BTreeSet<&str> = spec.s3_buckets.keys().map(String::as_str).collect();
        let pg_ids: BTreeSet<&str> = spec.pg_databases.keys().map(String::as_str).collect();

        let mut s3_buckets = Vec::with_capacity(s3_ids.len());
        for id in s3_ids {
            let (strategy, reason) = effective_strategy(&spec.s3_buckets, id, mode);
            if strategy == ResourceStrategy::Remote && !ospp_paired {
                // An explicitly remote bucket needs the OSPP session. (A `prefer_remote` bucket
                // falls back to the world-local SeaweedFS with a diagnostic, exactly like a
                // PostgreSQL seed; remote credentials would arrive as component env values.)
                return Err(DagError::RemoteResourceUnavailable {
                    kind: "s3_bucket",
                    id: id.to_owned(),
                    mode,
                });
            }
            let reason = if strategy == ResourceStrategy::PreferRemote && !ospp_paired {
                ResourceReason::NoRemoteSession
            } else {
                reason
            };
            s3_buckets.push(PlannedS3Bucket {
                id: id.to_owned(),
                strategy,
                reason,
                resolution: S3Resolution::Local,
            });
        }

        let mut pg_databases = Vec::with_capacity(pg_ids.len());
        for id in pg_ids {
            let (configured_strategy, reason) = effective_strategy(&spec.pg_databases, id, mode);
            let (strategy, seed, reason) = match dump_policy {
                // `--no-dump` is an explicit per-invocation override: the effective seed strategy
                // becomes local (clean `initdb`), recorded by `ResourceReason::NoDump`, even when
                // the configured strategy was `remote`/`prefer_remote`.
                DumpPolicy::NoDump => (
                    ResourceStrategy::Local,
                    PgSeed::CleanInitdb,
                    ResourceReason::NoDump,
                ),
                DumpPolicy::RefreshDump => {
                    if configured_strategy.is_local() {
                        return Err(DagError::RefreshDumpRequiresRemote { id: id.to_owned() });
                    }
                    if !ospp_paired {
                        return Err(DagError::RemoteResourceUnavailable {
                            kind: "pg_database",
                            id: id.to_owned(),
                            mode,
                        });
                    }
                    (
                        configured_strategy,
                        PgSeed::RemoteDump { refresh: true },
                        ResourceReason::RefreshedDump,
                    )
                }
                DumpPolicy::Default => match configured_strategy {
                    ResourceStrategy::Local => (configured_strategy, PgSeed::CleanInitdb, reason),
                    ResourceStrategy::Remote => {
                        if !ospp_paired {
                            return Err(DagError::RemoteResourceUnavailable {
                                kind: "pg_database",
                                id: id.to_owned(),
                                mode,
                            });
                        }
                        (
                            configured_strategy,
                            PgSeed::RemoteDump { refresh: false },
                            reason,
                        )
                    }
                    ResourceStrategy::PreferRemote => {
                        if ospp_paired {
                            (
                                configured_strategy,
                                PgSeed::RemoteDump { refresh: false },
                                reason,
                            )
                        } else {
                            (
                                configured_strategy,
                                PgSeed::CleanInitdb,
                                ResourceReason::NoRemoteSession,
                            )
                        }
                    }
                },
            };
            pg_databases.push(PlannedPgDatabase {
                id: id.to_owned(),
                strategy,
                reason,
                seed,
            });
        }

        Ok(Self {
            s3_buckets,
            pg_databases,
        })
    }

    /// Planned S3 bucket by id.
    pub fn s3_bucket(&self, id: &str) -> Option<&PlannedS3Bucket> {
        self.s3_buckets.iter().find(|bucket| bucket.id == id)
    }

    /// Planned PostgreSQL database by id.
    pub fn pg_database(&self, id: &str) -> Option<&PlannedPgDatabase> {
        self.pg_databases.iter().find(|database| database.id == id)
    }

    /// Typed `[resources]` diagnostic lines and `--json` rows (`ADR-0008` §3).
    pub fn diagnostics(&self) -> Vec<ResourceDiagnostic> {
        let s3 = self.s3_buckets.iter().map(|bucket| ResourceDiagnostic {
            kind: ResourceKind::S3Bucket,
            id: bucket.id.clone(),
            strategy: bucket.strategy,
            resolved: bucket.target(),
            reason: bucket.reason,
        });
        let pg = self.pg_databases.iter().map(|database| ResourceDiagnostic {
            kind: ResourceKind::PgDatabase,
            id: database.id.clone(),
            strategy: database.strategy,
            resolved: database.target(),
            reason: database.reason,
        });
        s3.chain(pg).collect()
    }
}

/// One `[resources]` diagnostic row naming effective strategy, resolved target, and reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResourceDiagnostic {
    pub kind: ResourceKind,
    pub id: String,
    pub strategy: ResourceStrategy,
    pub resolved: ResourceTarget,
    pub reason: ResourceReason,
}

fn effective_strategy(
    overrides: &BTreeMap<String, ResourceOverride>,
    id: &str,
    mode: ResourceMode,
) -> (ResourceStrategy, ResourceReason) {
    match overrides.get(id).and_then(|spec| spec.for_mode(mode)) {
        Some(strategy) => (strategy, ResourceReason::Configured),
        None => (
            ResourcesSpec::defaults_for(mode),
            ResourceReason::ModeDefault,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn override_spec(local: ResourceStrategy) -> ResourceOverride {
        ResourceOverride {
            local: Some(local),
            test: None,
        }
    }

    fn spec() -> ResourcesSpec {
        ResourcesSpec {
            s3_buckets: BTreeMap::from([(
                "media".to_owned(),
                override_spec(ResourceStrategy::PreferRemote),
            )]),
            pg_databases: BTreeMap::from([(
                "main".to_owned(),
                override_spec(ResourceStrategy::PreferRemote),
            )]),
        }
    }

    #[test]
    fn declared_ids_join_the_plan_with_mode_defaults() {
        let spec = spec().with_declared_ids(&["media", "assets"], &["main", "audit"]);
        assert_eq!(spec.s3_buckets().len(), 2);
        assert_eq!(spec.pg_databases().len(), 2);

        let plan = ResourcePlan::resolve(&spec, ResourceMode::Local, DumpPolicy::Default, false)
            .expect("local plan");
        assert_eq!(plan.s3_buckets.len(), 2);
        let assets = plan.s3_bucket("assets").expect("declared bucket");
        assert_eq!(assets.strategy, ResourceStrategy::PreferRemote);
        assert_eq!(assets.reason, ResourceReason::NoRemoteSession);
        assert_eq!(assets.target(), ResourceTarget::Local);
        assert_eq!(plan.pg_databases.len(), 2);
        let audit = plan.pg_database("audit").expect("declared database");
        assert_eq!(audit.reason, ResourceReason::NoRemoteSession);
        assert_eq!(audit.target(), ResourceTarget::Local);
    }

    #[test]
    fn forced_strategy_wins_over_configured_remote_overrides() {
        let spec = spec()
            .with_declared_ids(&["media"], &["main"])
            .forced_strategy(ResourceStrategy::Local);
        let plan = ResourcePlan::resolve(&spec, ResourceMode::Local, DumpPolicy::Default, false)
            .expect("forced local plan");
        for bucket in &plan.s3_buckets {
            assert_eq!(bucket.strategy, ResourceStrategy::Local);
            assert_eq!(bucket.reason, ResourceReason::Configured);
            assert_eq!(bucket.target(), ResourceTarget::Local);
        }
        for database in &plan.pg_databases {
            assert_eq!(database.strategy, ResourceStrategy::Local);
            assert_eq!(database.reason, ResourceReason::Configured);
            assert_eq!(database.target(), ResourceTarget::Local);
        }
    }

    #[test]
    fn a_remote_strategy_fails_closed_without_a_session() {
        let spec = spec().forced_strategy(ResourceStrategy::Remote);
        let err = ResourcePlan::resolve(&spec, ResourceMode::Local, DumpPolicy::Default, false)
            .expect_err("remote without a session must fail closed");
        assert!(matches!(err, DagError::RemoteResourceUnavailable { .. }));
    }

    #[test]
    fn a_paired_session_selects_the_remote_seed() {
        let spec = spec().forced_strategy(ResourceStrategy::PreferRemote);
        let plan = ResourcePlan::resolve(&spec, ResourceMode::Local, DumpPolicy::Default, true)
            .expect("paired plan");
        assert_eq!(
            plan.pg_database("main").expect("main").seed,
            PgSeed::RemoteDump { refresh: false }
        );
        assert_eq!(
            plan.pg_database("main").expect("main").target(),
            ResourceTarget::Remote
        );
    }
}
