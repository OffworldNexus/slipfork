//! Mode-aware `[resources]` `ResourceStrategy` schema and the single
//! [`ResourcePlan::resolve`] decision point (`ADR-0008` §3).
//!
//! `slipfork.toml` declares deliberate overrides only. The mode default comes from
//! [`ResourcesSpec::defaults_for`]: `local` worlds (`Canonical` / `UserFork`) prefer OSPP dev
//! resources, while `TestSlot` worlds are hermetically local by construction. [`ResourcePlan`]
//! owns every S3-bucket and PostgreSQL-seed decision consumed by `slipforkd`, the `OsppClient`
//! offer mapper, and M3's `TestCoordinator`; there is no second precedence ladder.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

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
    /// Use the OSPP offer when present, otherwise fall back to local with a diagnostic.
    PreferRemote,
    /// Require the OSPP offer; fail closed when it is absent.
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
    /// `--refresh-dump`: require live OSPP dump metadata; fail closed when local or offline.
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

/// Redacted S3 coordinates offered by OSPP (`ADR-0008` §2–§3).
///
/// `Debug` never prints credentials; the consuming `slipforkd` builds `S3Coordinates` from the
/// accessors without ever crossing a second resolution layer.
#[derive(Clone, PartialEq, Eq)]
pub struct RemoteS3Bucket {
    endpoint_url: String,
    region: String,
    bucket: String,
    access_key: String,
    secret_key: String,
}

impl RemoteS3Bucket {
    /// Construct an offer from typed OSPP `s3_buckets.<id>` fields.
    pub fn new(
        endpoint_url: impl Into<String>,
        region: impl Into<String>,
        bucket: impl Into<String>,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Self {
        Self {
            endpoint_url: endpoint_url.into(),
            region: region.into(),
            bucket: bucket.into(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
        }
    }

    /// S3-compatible endpoint URL.
    pub fn endpoint_url(&self) -> &str {
        &self.endpoint_url
    }

    /// S3 region.
    pub fn region(&self) -> &str {
        &self.region
    }

    /// Bucket name.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Access key id.
    pub fn access_key(&self) -> &str {
        &self.access_key
    }

    /// Secret access key.
    pub fn secret_key(&self) -> &str {
        &self.secret_key
    }
}

impl fmt::Debug for RemoteS3Bucket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteS3Bucket")
            .field("endpoint_url", &self.endpoint_url)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key", &"[REDACTED]")
            .field("secret_key", &"[REDACTED]")
            .finish()
    }
}

/// Digest-pinned remote PostgreSQL seed dump offered by OSPP (`ADR-0008` §2–§3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePgDump {
    sha256_compressed: String,
    compressed_bytes: u64,
}

impl RemotePgDump {
    /// Construct an offer from typed `PgDumpMetadata` fields.
    pub fn new(sha256_compressed: impl Into<String>, compressed_bytes: u64) -> Self {
        Self {
            sha256_compressed: sha256_compressed.into(),
            compressed_bytes,
        }
    }

    /// SHA-256 of the `zstd`-compressed `pg_dump --format=custom` archive.
    pub fn sha256_compressed(&self) -> &str {
        &self.sha256_compressed
    }

    /// Compressed archive size in bytes.
    pub fn compressed_bytes(&self) -> u64 {
        self.compressed_bytes
    }
}

/// One redacted `ResourcesDescribe` offer produced by the single
/// `OsppClient::resource_offer` mapper (`ADR-0008` §3).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RemoteResourceOffer {
    s3_buckets: BTreeMap<String, RemoteS3Bucket>,
    pg_databases: BTreeMap<String, RemotePgDump>,
}

impl RemoteResourceOffer {
    /// Build an offer from its complete coordinate maps.
    pub fn new(
        s3_buckets: BTreeMap<String, RemoteS3Bucket>,
        pg_databases: BTreeMap<String, RemotePgDump>,
    ) -> Self {
        Self {
            s3_buckets,
            pg_databases,
        }
    }

    /// Add one offered S3 bucket.
    pub fn with_s3_bucket(mut self, id: impl Into<String>, bucket: RemoteS3Bucket) -> Self {
        self.s3_buckets.insert(id.into(), bucket);
        self
    }

    /// Add one offered PostgreSQL seed dump.
    pub fn with_pg_dump(mut self, id: impl Into<String>, dump: RemotePgDump) -> Self {
        self.pg_databases.insert(id.into(), dump);
        self
    }

    /// Offered coordinates for `id`, if OSPP described that bucket.
    pub fn s3_bucket(&self, id: &str) -> Option<&RemoteS3Bucket> {
        self.s3_buckets.get(id)
    }

    /// Offered seed dump for `id`, if OSPP described that database.
    pub fn pg_dump(&self, id: &str) -> Option<&RemotePgDump> {
        self.pg_databases.get(id)
    }

    /// `true` when OSPP offered no resources at all.
    pub fn is_empty(&self) -> bool {
        self.s3_buckets.is_empty() && self.pg_databases.is_empty()
    }

    fn s3_ids(&self) -> impl Iterator<Item = &str> {
        self.s3_buckets.keys().map(String::as_str)
    }

    fn pg_ids(&self) -> impl Iterator<Item = &str> {
        self.pg_databases.keys().map(String::as_str)
    }
}

/// Where a planned resource resolves to (`ADR-0008` §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceTarget {
    /// World-local coordinates / clean `initdb`.
    Local,
    /// OSPP-offered shared dev coordinates / seed dump.
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
    /// `prefer_remote` had no OSPP offer, so it fell back to local with this diagnostic.
    NotOffered,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S3Resolution {
    /// Use world-local `127.0.0.1:9000` coordinates inside the world `netns`.
    Local,
    /// Use OSPP-offered shared dev bucket coordinates.
    Remote(RemoteS3Bucket),
}

impl S3Resolution {
    /// Local vs remote target for diagnostics.
    pub fn target(&self) -> ResourceTarget {
        match self {
            Self::Local => ResourceTarget::Local,
            Self::Remote(_) => ResourceTarget::Remote,
        }
    }
}

/// Resolution of one PostgreSQL database seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PgSeed {
    /// Clean `initdb`, then migrations and `[fixtures.*]` (never touches shared dev state).
    CleanInitdb,
    /// Restore from an OSPP-offered `zstd` custom dump.
    RemoteDump {
        /// Digest-pinned dump metadata.
        dump: RemotePgDump,
        /// `--refresh-dump` bypasses any cached copy and requires a live metadata check.
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
    /// `mode` is derived from [`WorldTarget`]; `offer` is the single
    /// `OsppClient::resource_offer` mapper's output. The plan covers every id declared in the
    /// spec and every id present in the offer, sorted by id for deterministic diagnostics.
    pub fn resolve(
        spec: &ResourcesSpec,
        mode: ResourceMode,
        dump_policy: DumpPolicy,
        offer: Option<&RemoteResourceOffer>,
    ) -> Result<Self, DagError> {
        let mut s3_ids: BTreeSet<&str> = spec.s3_buckets.keys().map(String::as_str).collect();
        let mut pg_ids: BTreeSet<&str> = spec.pg_databases.keys().map(String::as_str).collect();
        if let Some(offer) = offer {
            s3_ids.extend(offer.s3_ids());
            pg_ids.extend(offer.pg_ids());
        }

        let mut s3_buckets = Vec::with_capacity(s3_ids.len());
        for id in s3_ids {
            let (strategy, reason) = effective_strategy(&spec.s3_buckets, id, mode);
            let (resolution, reason) = match strategy {
                ResourceStrategy::Local => (S3Resolution::Local, reason),
                ResourceStrategy::Remote => (
                    S3Resolution::Remote(required_s3_bucket(offer, id, mode)?),
                    reason,
                ),
                ResourceStrategy::PreferRemote => match offered_s3_bucket(offer, id) {
                    Some(bucket) => (S3Resolution::Remote(bucket), reason),
                    None => (S3Resolution::Local, ResourceReason::NotOffered),
                },
            };
            s3_buckets.push(PlannedS3Bucket {
                id: id.to_owned(),
                strategy,
                reason,
                resolution,
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
                    let dump = required_pg_dump(offer, id, mode)?;
                    (
                        configured_strategy,
                        PgSeed::RemoteDump {
                            dump,
                            refresh: true,
                        },
                        ResourceReason::RefreshedDump,
                    )
                }
                DumpPolicy::Default => match configured_strategy {
                    ResourceStrategy::Local => (configured_strategy, PgSeed::CleanInitdb, reason),
                    ResourceStrategy::Remote => (
                        configured_strategy,
                        PgSeed::RemoteDump {
                            dump: required_pg_dump(offer, id, mode)?,
                            refresh: false,
                        },
                        reason,
                    ),
                    ResourceStrategy::PreferRemote => match offered_pg_dump(offer, id) {
                        Some(dump) => (
                            configured_strategy,
                            PgSeed::RemoteDump {
                                dump,
                                refresh: false,
                            },
                            reason,
                        ),
                        None => (
                            configured_strategy,
                            PgSeed::CleanInitdb,
                            ResourceReason::NotOffered,
                        ),
                    },
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

fn required_s3_bucket(
    offer: Option<&RemoteResourceOffer>,
    id: &str,
    mode: ResourceMode,
) -> Result<RemoteS3Bucket, DagError> {
    offered_s3_bucket(offer, id).ok_or_else(|| DagError::RemoteResourceUnavailable {
        kind: "s3_bucket",
        id: id.to_owned(),
        mode,
    })
}

fn offered_s3_bucket(offer: Option<&RemoteResourceOffer>, id: &str) -> Option<RemoteS3Bucket> {
    offer.and_then(|offer| offer.s3_bucket(id)).cloned()
}

fn required_pg_dump(
    offer: Option<&RemoteResourceOffer>,
    id: &str,
    mode: ResourceMode,
) -> Result<RemotePgDump, DagError> {
    offered_pg_dump(offer, id).ok_or_else(|| DagError::RemoteResourceUnavailable {
        kind: "pg_database",
        id: id.to_owned(),
        mode,
    })
}

fn offered_pg_dump(offer: Option<&RemoteResourceOffer>, id: &str) -> Option<RemotePgDump> {
    offer.and_then(|offer| offer.pg_dump(id)).cloned()
}
