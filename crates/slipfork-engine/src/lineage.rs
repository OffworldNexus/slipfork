//! Per-world build lineage and service-row data shared by the engine and the daemon wire.
//!
//! These are the engine's own records: a world's rootfs/base/seed/service-image facts and one row
//! per supervised service. They carry no OS handles, no paths, and no clock: `millis`,
//! `uptime_secs`, and every other measured fact are stamped by the engine from its injected clock
//! before the value leaves the engine. `slipfork-edge` re-exports them for the `DaemonControl`
//! wire, so there is exactly one model (no mapping between two identical structs).

use serde::{Deserialize, Serialize};

use crate::ServiceState;

/// One world's build lineage.
///
/// Every fact is recorded on **that world's** engine handle when the world boots: the rootfs and
/// per-service image facts come from the [`crate::WorldSpec`]'s materialized
/// [`crate::WorldRootfs`] (cache outcome and build `millis` measured on the shared injected
/// clock), and the base/seed facts come from [`BootLineage`], which the seeder measured. An
/// unknown fact is `None` and renders `-` in human output, never `0` and never an invented value;
/// there is no placeholder constructor, so a world with no recorded fact is the all-`None` value
/// built from its own (empty) records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldLineage {
    /// Rootfs digest plus its cache outcome, when the world's boot measured one.
    pub rootfs: Option<RootfsLineage>,
    /// One row per backing-service image the world's boot resolved.
    pub services: Vec<ServiceImageLineage>,
    /// Seed digest and whether the seed was rebuilt, when the boot recorded one.
    pub seed: Option<SeedLineage>,
    /// Base snapshot key plus its seal duration, when the boot recorded one.
    pub base: Option<BaseLineage>,
}

/// The base-snapshot and seed facts a world boots from, recorded on that world's handle.
///
/// `WorldEngine::clone_world` copies them verbatim into the cloned world's [`WorldLineage`]; it
/// never estimates a seal duration or a seed verdict. The seeder fills them: `base.millis` is
/// `Some(ms)` when it sealed the base for this world and `None` when the sealed snapshot already
/// existed, and `seed.rebuilt` is `Some(true)` when it restored a dump into the base, `Some(false)`
/// when it reused an already-sealed base, and `None` when the world ships no seed dump at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootLineage {
    /// Base snapshot key (full 64-hex) and its seal duration.
    pub base: Option<BaseLineage>,
    /// Seed dump digest, rebuild verdict, and measured duration.
    pub seed: Option<SeedLineage>,
}

/// The world rootfs build's digest, cache outcome, and measured duration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootfsLineage {
    /// Full 64-hex rootfs digest.
    pub digest: String,
    /// Whether the rootfs came from cache or was rebuilt.
    pub cache: CacheOutcome,
    /// Measured build milliseconds (`None` when the boot recorded no duration).
    pub millis: Option<u64>,
}

/// Cache outcome of one materialized tree (`hit` served from the content-addressed cache,
/// `miss` rebuilt it). One enum for the rootfs and for every backing-service image, because the
/// store observes exactly one outcome per tree; a host that cannot fetch or build fails closed
/// instead of recording a third verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheOutcome {
    /// The tree was already cached.
    Hit,
    /// The tree was rebuilt or fetched.
    Miss,
}

/// One backing-service image's cache outcome and measured duration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceImageLineage {
    /// Image reference (`postgres:18`).
    pub id: String,
    /// Whether the image was already cached or fetched/extracted.
    pub cache: CacheOutcome,
    /// Measured resolution milliseconds (`None` when the boot recorded no duration).
    pub millis: Option<u64>,
}

/// The seed dump's digest and rebuild verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedLineage {
    /// Full 64-hex seed digest, when the boot recorded one.
    pub digest: Option<String>,
    /// `Some(true)` rebuilt the seed, `Some(false)` reused it, `None` recorded no verdict.
    pub rebuilt: Option<bool>,
    /// Measured seed milliseconds (`None` when the boot recorded no duration).
    pub millis: Option<u64>,
}

/// The base snapshot's key and seal duration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseLineage {
    /// Full 64-hex base snapshot key.
    pub key: String,
    /// Seal duration in milliseconds (`None` when the snapshot already existed).
    pub millis: Option<u64>,
}

/// Which kind of supervised service one row describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    /// A `snow.yml` component's run command.
    Component,
    /// A `crun`-backed resource service (`postgres:18`, `redis:7`).
    BackingService,
}

/// One supervised service's engine record: identity, kind, lifecycle state, and measured facts.
///
/// `id` is unique within one world. `pid`, `port`, and `uptime_secs` are `None` while the engine
/// has no such record; a renderer prints `-` for `None`, never `0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldServiceInfo {
    /// Stable service id (`api`, `pg_databases.main`), unique within the world.
    pub id: String,
    /// Component or `crun` backing service.
    pub kind: ServiceKind,
    /// The engine's lifecycle state for this service.
    pub state: ServiceState,
    /// Supervised process id (`None` while the engine does not report one).
    pub pid: Option<u32>,
    /// In-world serving/readiness port (`None` while the engine does not report one).
    pub port: Option<u16>,
    /// Uptime in seconds from the engine's injected monotonic clock.
    pub uptime_secs: Option<u64>,
}
