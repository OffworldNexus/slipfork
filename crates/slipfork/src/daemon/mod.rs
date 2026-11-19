//! The in-process [`DaemonControl`] implementation `slipforkd` injects into its server.
//!
//! Every collaborator is wired by [`compose_daemon`], the one composition function `slipforkd` and
//! the integration harness both call: the CoW engine over the concrete [`KernelBtrfsDriver`], the
//! HTTP ingress `WorldRegistry`, the VCS repository resolver and workspace factory, the explicit
//! state/data roots, and the inherited allowlist the engine hands to world processes.
//! `compose_daemon` is this binary's composition root: it resolves the inherited `PATH`/`TERM`/`LANG`
//! allowlist from `std::env` once, before the engine exists. Nothing else here reads ambient state:
//! the module never calls `current_dir()` and never guesses a path — every request carries its
//! explicit `cwd`.
//!
//! World handles live only here (`H-010` #1): `up`/`fork` clone and attach a world, `down`/`drop`
//! release it, and the thin CLI only ever receives serializable summaries and the direct-entry
//! [`slipfork_world::WorldEntryCommand`].

use crate::browser::Browser;
use crate::layout::Layout;
use crate::project::repository_resolver;
use crate::vaults::{LayoutProjectVaults, ProjectVaults};
use crate::vcs::RepositoryWorkspaces;
use postgres::PostgresReady;
use slipfork_edge::{EdgeError, WorldRegistry};
use slipfork_engine::btrfs::KernelBtrfsDriver;
use slipfork_engine::rootfs::{
    BaseImageBuilder, DockerBuildx, DockerImageStore, DockerfileStageBuilder, ElfProbe,
    FsInputHasher, RuntimeAssets,
};
use slipfork_engine::{BtrfsEngine, InheritedEnv};
use slipfork_ospp::{
    DeviceKeyPath, DeviceKeySigner, FileDeviceKeyStore, OsppClient, OsppEndpoint, OsppTimeouts,
    SoftwareP256Key, SoftwareP256Signer, WsOsppClient,
};
use slipfork_snow::{EntropySource, OsCsprngEntropy, SnowError};
use slipfork_vcs::{RepoRootResolver, VcsWorkspaceFactory};
use slipfork_wire::control::{DaemonError, DaemonErrorKind, IngressInfo, WorldState};
use slipfork_world::{EngineError, InheritedVar, MountMode, WorldEngine, WorldMount};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod backing;
mod base;
mod boot;
mod control;
mod env;
mod ospp_supervisor;
mod pairing_store;
mod postgres;
mod protocol;
mod resolve;
mod select;
mod service;
mod types;
mod vault;
mod world;

pub use pairing_store::{BootEndpoint, PairingStore, resolve_boot_endpoint};
pub use service::DaemonService;

#[cfg(test)]
mod tests;

/// The world mode this daemon supervises (`snow.yml` `modes.local`, `ADR-0001` §1).
pub(super) const WORLD_MODE: &str = "local";

/// Host toolchain caches exposed read-only by `--host-caches` (`ADR-0009` §2.3).
///
/// Resolved once by the Composition Root from the developer's real `HOME`; the daemon only
/// decides whether the per-invocation flag asks for them. Empty `mounts` means the host has no
/// cache directory to share, so no ABI probe runs.
#[derive(Debug, Clone, Default)]
pub struct HostCaches {
    /// Host ELF used as the ABI witness (`/bin/sh`).
    pub witness: PathBuf,
    /// Existing host cache directories, always [`MountMode::ReadOnly`].
    pub mounts: Vec<WorldMount>,
}

/// Everything the daemon needs that the Composition Root resolved once.
pub(super) struct DaemonConfig {
    /// Explicit `--state-dir` / `--runtime-dir` roots and the project-vault data home.
    pub layout: Layout,
    /// `PATH`/`TERM`/`LANG` values the engine hands to world processes (never the whole env).
    pub inherited: Vec<(InheritedVar, String)>,
    /// Host `crun` + `slipfork-world-init` payloads staged into every `L0` rootfs.
    pub runtime: RuntimeAssets,
    /// Target platform (`x86_64`, `aarch64`) for the rootfs Merkle key.
    pub target_arch: String,
    /// Invoking developer's account (World Image Contract #3 diagnostics).
    pub uid: u32,
    /// Invoking developer's group.
    pub gid: u32,
    /// Resolved host toolchain caches for `--host-caches`.
    pub host_caches: HostCaches,
    /// Postmaster-readiness gate for the base seed world's `postgres:18` server.
    pub(in crate::daemon) postgres_ready: PostgresReady,
}

/// Existing developer toolchain caches shared read-only by `--host-caches` (`ADR-0009` §2.3).
///
/// Resolved by each Composition Root (`slipforkd` and the in-process integration harness) from the
/// developer's real `HOME`; the daemon only decides whether the per-invocation flag asks for them.
/// Empty `mounts` means the host has no cache directory to share, so no ABI probe runs.
pub fn host_caches(home: &Path) -> HostCaches {
    let candidates = [
        home.join(".cargo").join("registry"),
        home.join(".cargo").join("git"),
        home.join(".cargo").join("bin"),
        home.join(".rustup"),
        home.join(".cache").join("uv"),
        home.join(".local").join("share").join("uv"),
        home.join(".local").join("share").join("pnpm").join("store"),
        home.join(".cargo"),
    ];
    let mut seen = BTreeSet::new();
    let mounts = candidates
        .into_iter()
        .filter(|path| path.is_dir())
        .filter(|path| seen.insert(path.clone()))
        .map(|path| WorldMount {
            guest_path: path.clone(),
            host_path: path,
            mode: MountMode::ReadOnly,
        })
        .collect();
    HostCaches {
        witness: PathBuf::from("/bin/sh"),
        mounts,
    }
}

/// The composition root's already-constructed ports, injected as one value.
pub(super) struct DaemonPorts {
    /// CoW world engine.
    pub(super) engine: std::sync::Arc<dyn WorldEngine>,
    /// The single `L0` world-rootfs builder (`DockerfileStageBuilder` in production).
    pub(super) builder: std::sync::Arc<dyn BaseImageBuilder>,
    /// HTTP ingress registry and origin source.
    pub(super) registry: std::sync::Arc<WorldRegistry>,
    /// Canonical repository resolver.
    pub(super) repo_roots: std::sync::Arc<dyn RepoRootResolver>,
    /// Fork workspace factory.
    pub(super) workspaces: std::sync::Arc<dyn VcsWorkspaceFactory>,
    /// Project-scoped `0600` local-vault factory.
    pub(super) vaults: std::sync::Arc<dyn ProjectVaults>,
    /// Public OSPP pairing record store (`<state_dir>/ospp.json`, `0600`).
    pub(super) pairing: PairingStore,
    /// The one OSPP v1 client bound to the boot-injected endpoint (`None` = local-only daemon).
    pub(super) ospp: Option<Arc<dyn OsppClient>>,
    /// The machine-authentication identity (`p256` auth only; never subscription encryption).
    pub(super) signer: Arc<dyn DeviceKeySigner>,
    /// Hostname proposed in `pair_begin` (resolved once by the Composition Root).
    pub(super) hostname: String,
    /// The concrete desktop launcher the pairing handle is handed to immediately.
    pub(super) browser: Browser,
    /// CSPRNG injected into the pairing handshake nonce.
    pub(super) entropy: Arc<dyn EntropySource>,
}

/// Everything a Composition Root resolves before [`compose_daemon`] wires the daemon graph.
///
/// `slipforkd` and the in-process integration harness pass the same already-resolved values and
/// differ only in the concrete CoW driver; neither builds a second graph. Nothing in
/// [`compose_daemon`] reads the environment or the working directory, and the only ambient
/// collaborator it installs is the postmaster-readiness clock (`PostgresReady::production`).
pub struct DaemonWiring {
    /// Explicit `--state-dir` / `--runtime-dir` roots and the project-vault data home.
    pub layout: Layout,
    /// CoW subvolume driver: the concrete kernel Btrfs driver ('slipforkd's one adapter).
    pub driver: KernelBtrfsDriver,
    /// The concrete desktop launcher (value, not a trait object) for the pairing approval page.
    pub browser: Browser,
    /// Bound HTTP ingress listeners (`--listen`, `LISTEN_FDS`, or a test-bound listener) and the
    /// single port every minted origin carries.
    pub ingress: IngressInfo,
    /// Bound static forward listener port, when one is bound.
    pub forward_port: Option<u16>,
    /// Resolved host toolchain caches for `--host-caches`.
    pub host_caches: HostCaches,
    /// `PATH`/`TERM`/`LANG` values the engine hands to world processes (never the whole env).
    pub inherited: Vec<(InheritedVar, String)>,
    /// Host `crun` + `slipfork-world-init` payloads staged into every `L0` rootfs.
    pub runtime: RuntimeAssets,
    /// Target platform (`x86_64`, `aarch64`) for the rootfs Merkle key.
    pub target_arch: String,
    /// Invoking developer's account (World Image Contract #3 diagnostics).
    pub uid: u32,
    /// Invoking developer's group.
    pub gid: u32,
    /// The single OSPP v1 endpoint resolved at the Composition Root (`None` = local-only).
    pub ospp_endpoint: Option<OsppEndpoint>,
    /// Hostname proposed to the server in `pair_begin` (never re-read from the environment).
    pub hostname: String,
}

/// The wired daemon graph: the control service and the ingress registry over one engine.
pub struct DaemonGraph {
    /// In-process [`DaemonControl`] implementation the control socket serves.
    pub service: Arc<DaemonService>,
    /// HTTP ingress registry the ingress server binds over the same engine.
    pub registry: Arc<WorldRegistry>,
}

/// Wire the one daemon composition graph from already-resolved inputs.
///
/// `slipforkd` and the in-process integration harness call this; the only difference is the
/// concrete [`KernelBtrfsDriver`]. Every path, port, account id, and environment value arrives
/// through [`DaemonWiring`].
pub fn compose_daemon(wiring: DaemonWiring) -> Result<DaemonGraph, String> {
    let DaemonWiring {
        layout,
        driver,
        browser,
        ingress,
        forward_port,
        host_caches,
        inherited,
        runtime,
        target_arch,
        uid,
        gid,
        ospp_endpoint,
        hostname,
    } = wiring;

    // Every measuring adapter records the plain `Instant` pair around its own work: the rootfs
    // builder, the OCI image store, and the engine's `uptime_secs` need no injected clock.
    let builder: Arc<dyn BaseImageBuilder> = Arc::new(DockerfileStageBuilder::new(
        DockerBuildx::default(),
        FsInputHasher,
        DockerImageStore::new(layout.state_dir()),
        ElfProbe,
    ));
    // The sole world engine: one unified Btrfs subvolume per world over the concrete driver.
    // Construction fails closed with `EngineError::BtrfsRequired` before any snapshot or world
    // directory exists (`ADR-0010` §4), so a non-Btrfs `--state-dir` never reaches a degraded mode.
    let engine: Arc<dyn WorldEngine> = Arc::new(
        BtrfsEngine::new(
            layout.state_dir(),
            uid,
            gid,
            runtime.world_init.clone(),
            driver,
            InheritedEnv::new(std::env::vars().filter_map(|(name, value)| {
                InheritedVar::parse(&name).map(|var| (var.name().to_owned(), value))
            })),
        )
        .map_err(|err| format!("cannot start the Btrfs world engine: {err}"))?,
    );
    let registry = Arc::new(WorldRegistry::new(Arc::clone(&engine), ingress.port()));

    // One machine-authentication P-256 identity: a `0600` scalar at the injected data-home path.
    // Subscription encryption keys are ephemeral and live only in the session thread's memory.
    let entropy: Arc<dyn EntropySource> = Arc::new(OsCsprngEntropy);
    let signer: Arc<dyn DeviceKeySigner> = if ospp_endpoint.is_some() {
        let store = FileDeviceKeyStore::new(DeviceKeyPath(layout.device_key_path()));
        Arc::new(
            SoftwareP256Signer::from_store(&store, entropy.as_ref())
                .map_err(|err| format!("cannot initialize the OSPP device key: {err}"))?,
        )
    } else {
        let key = SoftwareP256Key::generate(entropy.as_ref())
            .map_err(|err| format!("cannot initialize the in-memory OSPP identity: {err}"))?;
        Arc::new(SoftwareP256Signer::new(key))
    };
    let ospp: Option<Arc<dyn OsppClient>> = match ospp_endpoint {
        Some(endpoint) => Some(Arc::new(WsOsppClient::new(
            endpoint,
            Arc::clone(&signer),
            Arc::clone(&entropy),
            OsppTimeouts::default(),
        ))),
        None => None,
    };

    let service = Arc::new(DaemonService::new(
        DaemonPorts {
            engine,
            builder,
            registry: Arc::clone(&registry),
            repo_roots: repository_resolver(),
            workspaces: Arc::new(RepositoryWorkspaces::new()),
            vaults: Arc::new(LayoutProjectVaults::new(layout.clone())),
            pairing: PairingStore::new(layout.pairing_path()),
            ospp,
            signer,
            hostname,
            browser,
            entropy,
        },
        DaemonConfig {
            layout,
            inherited,
            runtime,
            target_arch,
            uid,
            gid,
            host_caches,
            postgres_ready: PostgresReady::production(),
        },
        ingress,
        forward_port,
    ));
    service.start_ospp_supervisor();
    Ok(DaemonGraph { service, registry })
}

/// Map one engine failure to a secret-free runtime diagnostic.
pub(super) fn runtime_error(message: impl Into<String>) -> EdgeError {
    EdgeError::Control(DaemonError::new(DaemonErrorKind::Internal, message))
}

/// Map one `snow.yml` failure to a control error, keeping the author-facing `1 line + next:` text.
///
/// A malformed manifest is the caller's to fix, so the three author-facing kinds stay typed as
/// [`DaemonErrorKind::Rejected`]; everything else is an internal `snow.yml`/provider failure.
pub(super) fn snow_error(err: SnowError) -> EdgeError {
    let kind = match &err {
        SnowError::UnknownComponent(_)
        | SnowError::ClaimedValueMissing { .. }
        | SnowError::ClaimedCoordinateMissing { .. } => DaemonErrorKind::Rejected,
        _ => DaemonErrorKind::Internal,
    };
    EdgeError::Control(DaemonError::new(kind, err.to_string()))
}

/// Render a failed read of one **held** world's engine record (`list`, `status`, and `ps`).
///
/// A world whose last reload tore it down keeps its engine record — still owned, but with no live
/// PID 1 — and every read fails closed with [`EngineError::WorldNotLive`]: name the failed boot and
/// the retry, never a handle the daemon still holds. Only a genuinely absent handle (a released,
/// destroyed, or foreign one) renders "engine handle is gone".
pub(super) fn engine_read_error(world: &str, state: WorldState, err: &EngineError) -> EdgeError {
    match (err, state) {
        (EngineError::WorldNotLive { .. }, WorldState::Failed) => runtime_error(format!(
            "world '{world}' last boot failed; run `slipfork up` to retry: {err}"
        )),
        _ => runtime_error(format!(
            "world '{world}' is attached but its engine handle is gone: {err}"
        )),
    }
}

/// Map one invalid request to a typed rejection (the message is the whole diagnostic).
pub(super) fn rejected(message: impl Into<String>) -> EdgeError {
    EdgeError::Control(DaemonError::new(DaemonErrorKind::Rejected, message))
}
