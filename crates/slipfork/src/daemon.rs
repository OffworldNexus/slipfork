//! The in-process [`DaemonControl`] implementation `slipforkd` injects into its server.
//!
//! Every collaborator is injected by the `slipforkd` Composition Root: the CoW engine, the HTTP
//! ingress `WorldRegistry`, the VCS repository resolver and workspace factory, the explicit
//! state/data roots, and the inherited allowlist the engine hands to world processes. This module
//! never reads `std::env`, never calls `current_dir()`, and never guesses a path: every request
//! carries its explicit `cwd`.
//!
//! World handles live only here (`H-010` #1): `up`/`fork` clone and attach a world, `down`/`drop`
//! release it, and the thin CLI only ever receives serializable summaries and the direct-entry
//! [`slipfork_engine::WorldEntryCommand`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use sha2::{Digest, Sha256};

use slipfork_dag::{
    DumpPolicy, PgSeed, RemoteResourceOffer, ResourceMode, ResourcePlan, ResourceStrategy,
    S3Resolution, SlipforkConfig,
};
use slipfork_edge::daemon::{WorkspaceRefsRequest, WorkspaceRefsSummary};
use slipfork_edge::forward::ForwardTargetSpec;
use slipfork_edge::{
    BootEvent, DaemonControl, DaemonError, DaemonErrorKind, DaemonStatus, DropForkRequest,
    EdgeError, ExecTargetRequest, ForkRequest, ForwardRequest, ForwardSummary, HookRunRequest,
    IngressRoute, LogsRequest, PruneReport, ResourceSummary, SharedWorldInfo, SnowResolveRequest,
    SnowResolveSummary, UpRequest, WorldExecTarget, WorldRegistry, WorldSelector, WorldState,
    WorldSummary, WorldUpSummary, world_id,
};
use slipfork_engine::rootfs::{
    BackingServiceKey, BaseImageBuilder, BuildEnvVar, HostCacheRequest, RootfsBuildEvent,
    RuntimeAssets, SnowPortContract, WorldRootfs, WorldRootfsRequest,
};
use slipfork_engine::{
    EngineError, ExecEnv, ExecOutput, InheritedVar, LocalIngressBridge, MountMode, ServiceHandle,
    ServiceRuntime, ServiceSpec, SnapshotId, StateSubdirMount, StreamChunk, WorldEngine,
    WorldHandle, WorldMount, WorldSpec,
};
use slipfork_ospp::{
    BrowserOpener, DeviceKeySigner, DumpStreamer, OsppClient, OsppEndpoint, OsppError,
    OsppTimeouts, PGCUSTOM_SUFFIX, PairingUrl, SeedDumpStore, redeem_pairing_url,
};
use slipfork_snow::{
    EntropySource, HookPhase, Image, Namespace, OsCsprngEntropy, ResolvedEnvMap,
    ResourceCoordinates, S3Coordinates, SecretProvider, SnowFile,
};
use slipfork_vcs::{
    RepoRoot, RepoRootResolver, VcsWorkspace, VcsWorkspaceFactory, WORKTREES_DIRECTORY,
    WorkspaceRequest,
};
use slipfork_world::{LocalHostRoute, ProjectSlug, WorldOrigin, WorldTarget, validate_fork_name};

use crate::layout::{Layout, ensure_private_dir};
use crate::ospp::{OsppSessionCoordinates, OsppSessionStore};
use crate::project::load_optional_slipfork_config;
use crate::vaults::ProjectVaults;

/// The world mode this daemon supervises (`snow.yml` `modes.local`, `ADR-0001` §1).
const WORLD_MODE: &str = "local";

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
pub struct DaemonConfig {
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
}

/// One live world owned by this daemon.
struct LiveWorld {
    project: ProjectSlug,
    target: WorldTarget,
    repo: RepoRoot,
    worktree: PathBuf,
    handle: WorldHandle,
    state: WorldState,
    base_env: ExecEnv,
    component_envs: BTreeMap<String, ExecEnv>,
    enabled_components: BTreeSet<String>,
    snow: SnowFile,
    resources: Vec<ResourceSummary>,
    services: Vec<ServiceHandle>,
    /// Retained hook logs by `<component>:<phase>` (`slipfork logs <hook_id>`, `ADR-0008` §4).
    hook_logs: BTreeMap<String, Arc<HookLog>>,
    workspace: Option<Arc<dyn VcsWorkspace>>,
}

/// One `slipfork logs` target: an engine-retained service log or a daemon-retained hook log.
enum RetainedTarget {
    Service(String),
    Hook(Arc<HookLog>),
}

/// Size-capped retained output plus its on-disk log for one hook run.
///
/// Mirrors the engine's `ServiceLogs` retention so `slipfork logs <hook_id> --follow` behaves like
/// a service log, while [`HookLog::path`] gives the rescue block a real file to point at.
struct HookLog {
    path: PathBuf,
    state: Mutex<HookLogState>,
    changed: Condvar,
}

#[derive(Default)]
struct HookLogState {
    chunks: Vec<StreamChunk>,
    closed: bool,
}

impl HookLog {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            state: Mutex::new(HookLogState::default()),
            changed: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HookLogState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn push(&self, chunk: StreamChunk) {
        if chunk.bytes.is_empty() {
            return;
        }
        self.lock().chunks.push(chunk);
        self.changed.notify_all();
    }

    fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }

    fn snapshot(&self) -> Vec<StreamChunk> {
        self.lock().chunks.clone()
    }

    fn follow(&self, on_chunk: &mut dyn FnMut(StreamChunk)) {
        let mut state = self.lock();
        let mut cursor = 0;
        loop {
            while cursor < state.chunks.len() {
                let chunk = state.chunks[cursor].clone();
                cursor += 1;
                drop(state);
                on_chunk(chunk);
                state = self.lock();
            }
            if state.closed {
                return;
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Mutable daemon index, mutated only in short critical sections (never across engine I/O).
#[derive(Default)]
struct DaemonState {
    worlds: Vec<LiveWorld>,
    /// Base state snapshot created on first use per `(project, base digest)` in this daemon's
    /// lifetime. The digest covers backing-service versions and every resolved seed-dump digest, so
    /// `--no-dump` and a restored dump never share a base.
    bases: BTreeMap<(ProjectSlug, String), SnapshotId>,
}

impl DaemonState {
    fn world(&self, project: &ProjectSlug, target: &WorldTarget) -> Option<&LiveWorld> {
        self.worlds
            .iter()
            .find(|world| world.project == *project && world.target == *target)
    }

    fn world_index(&self, project: &ProjectSlug, target: &WorldTarget) -> Option<usize> {
        self.worlds
            .iter()
            .position(|world| world.project == *project && world.target == *target)
    }

    fn world_mut(&mut self, project: &ProjectSlug, target: &WorldTarget) -> Option<&mut LiveWorld> {
        self.worlds
            .iter_mut()
            .find(|world| world.project == *project && world.target == *target)
    }
}

/// A hook resolved once for both interactive entry and non-interactive streaming.
struct ResolvedHook {
    argv: Vec<String>,
    overlay: ExecEnv,
    phase_label: &'static str,
}

/// A resolved world selection: repository identity, target, and the checkout its world mounts.
struct WorldSelection {
    repo: RepoRoot,
    target: WorldTarget,
    worktree: PathBuf,
}

/// Inputs to one world materialization.
struct BootContext {
    repo: RepoRoot,
    target: WorldTarget,
    worktree: PathBuf,
    dump: DumpPolicy,
    force_local_resources: bool,
    host_caches: bool,
    workspace: Option<Box<dyn VcsWorkspace>>,
}

/// Failure of one boot; a world that already exists stays attached for inspection.
struct BootFailure {
    error: EdgeError,
    /// A world that already exists keeps its handle so it stays inspectable; boxed because the
    /// success value is only returned out-of-band from `boot`'s `Result`.
    world: Option<Box<LiveWorld>>,
    workspace: Option<Box<dyn VcsWorkspace>>,
}

/// The composition root's already-constructed ports, injected as one value.
pub struct DaemonPorts {
    /// CoW world engine.
    pub engine: std::sync::Arc<dyn WorldEngine>,
    /// The single `L0` world-rootfs builder (`DockerfileStageBuilder` in production).
    pub builder: std::sync::Arc<dyn BaseImageBuilder>,
    /// HTTP ingress registry and origin source.
    pub registry: std::sync::Arc<WorldRegistry>,
    /// Canonical repository resolver.
    pub repo_roots: std::sync::Arc<dyn RepoRootResolver>,
    /// Fork workspace factory.
    pub workspaces: std::sync::Arc<dyn VcsWorkspaceFactory>,
    /// Project-scoped `0600` vault factory.
    pub vaults: std::sync::Arc<dyn ProjectVaults>,
    /// Persistent OSPP device-key signer (`tpmd:sealed` or the `0600` software fallback).
    pub signer: Arc<dyn DeviceKeySigner>,
    /// Paired OSPP host coordinates (`0600`, zero secrets), written by `auth login`.
    pub ospp_sessions: Arc<dyn OsppSessionStore>,
    /// Digest-pinned seed-dump cache (`DiskSeedDumpStore` in production).
    pub seed_dumps: Arc<dyn SeedDumpStore>,
    /// Injected browser opener for the pairing approval page (never logs the live handle).
    pub browser: Arc<dyn BrowserOpener>,
    /// CSPRNG injected into the pairing handshake nonce.
    pub entropy: Arc<dyn EntropySource>,
    /// Bounds for the pairing WebSocket handshake.
    pub ospp_timeouts: OsppTimeouts,
}

/// The daemon-owned [`DaemonControl`] implementation.
pub struct DaemonService {
    engine: std::sync::Arc<dyn WorldEngine>,
    builder: std::sync::Arc<dyn BaseImageBuilder>,
    registry: std::sync::Arc<WorldRegistry>,
    repo_roots: std::sync::Arc<dyn RepoRootResolver>,
    workspaces: std::sync::Arc<dyn VcsWorkspaceFactory>,
    vaults: std::sync::Arc<dyn ProjectVaults>,
    signer: Arc<dyn DeviceKeySigner>,
    ospp_sessions: Arc<dyn OsppSessionStore>,
    seed_dumps: Arc<dyn SeedDumpStore>,
    /// Lazily connected, daemon-held `ospp-v1` session (`None` while unpaired or offline).
    ospp_client: Mutex<Option<Arc<OsppClient>>>,
    browser: Arc<dyn BrowserOpener>,
    entropy: Arc<dyn EntropySource>,
    ospp_timeouts: OsppTimeouts,
    config: DaemonConfig,
    http_ingress_port: u16,
    forward_port: Option<u16>,
    state: Mutex<DaemonState>,
    /// Serializes world materialization so one project's base snapshot is minted exactly once.
    boot_lock: Mutex<()>,
}

impl std::fmt::Debug for DaemonService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonService")
            .field("http_ingress_port", &self.http_ingress_port)
            .field("forward_port", &self.forward_port)
            .finish_non_exhaustive()
    }
}

impl DaemonService {
    /// Assemble the service over the composition root's already-constructed adapters.
    pub fn new(
        ports: DaemonPorts,
        config: DaemonConfig,
        http_ingress_port: u16,
        forward_port: Option<u16>,
    ) -> Self {
        Self {
            engine: ports.engine,
            builder: ports.builder,
            registry: ports.registry,
            repo_roots: ports.repo_roots,
            workspaces: ports.workspaces,
            vaults: ports.vaults,
            signer: ports.signer,
            ospp_sessions: ports.ospp_sessions,
            seed_dumps: ports.seed_dumps,
            ospp_client: Mutex::new(None),
            browser: ports.browser,
            entropy: ports.entropy,
            ospp_timeouts: ports.ospp_timeouts,
            config,
            http_ingress_port,
            forward_port,
            state: Mutex::new(DaemonState::default()),
            boot_lock: Mutex::new(()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, DaemonState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn boot_guard(&self) -> MutexGuard<'_, ()> {
        self.boot_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The daemon-held `ospp-v1` session: lazily dial the persisted coordinates once, then reuse it.
    ///
    /// An unpaired developer, or a paired host whose OSPP server is unreachable
    /// (`OsppError::Transport`), yields `None` so the `0600` local vault still serves every secret;
    /// `ResourceStrategy::Remote` and `DumpPolicy::RefreshDump` then fail closed inside
    /// `ResourcePlan::resolve`. Every other connect failure (a revoked key, bad signature, TLS or
    /// protocol refusal) is a paired host rejecting us, so it propagates instead of silently
    /// degrading to the local vault.
    fn ospp_client(&self) -> Result<Option<Arc<OsppClient>>, EdgeError> {
        let Some(coordinates) = self.ospp_sessions.load()? else {
            return Ok(None);
        };
        {
            let cache = self
                .ospp_client
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(client) = cache.as_ref() {
                return Ok(Some(Arc::clone(client)));
            }
        }
        let endpoint =
            OsppEndpoint::connect(coordinates.host(), coordinates.port(), coordinates.tls())
                .map_err(|error| {
                    runtime_error(format!(
                        "the paired OSPP endpoint '{}:{}' is invalid: {error}",
                        coordinates.host(),
                        coordinates.port()
                    ))
                })?;
        let client = match OsppClient::connect(
            &endpoint,
            self.signer.as_ref(),
            self.entropy.as_ref(),
            self.ospp_timeouts,
        ) {
            Ok(client) => Arc::new(client),
            Err(OsppError::Transport { .. }) => {
                // A dead socket must never be reused; the next call dials afresh.
                self.invalidate_ospp_client();
                return Ok(None);
            }
            Err(error) => {
                return Err(runtime_error(format!(
                    "the paired OSPP server '{}:{}' refused the session: {error}",
                    coordinates.host(),
                    coordinates.port()
                )));
            }
        };
        let mut cache = self
            .ospp_client
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *cache = Some(Arc::clone(&client));
        Ok(Some(client))
    }

    /// Drop the cached session so the next capability call reconnects from the persisted
    /// coordinates instead of reusing a dead socket.
    fn invalidate_ospp_client(&self) {
        *self
            .ospp_client
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// The project's vault, layered over the paired remote vault when one is reachable.
    fn vault_for(&self, project: &ProjectSlug) -> Result<Arc<dyn SecretProvider>, EdgeError> {
        let client = self.ospp_client()?;
        self.vaults.vault(project, client, WORLD_MODE)
    }

    /// The single `OsppClient::resource_offer` mapper, with offline transport failures treated as
    /// "no offer" so `prefer_remote` falls back to world-local coordinates with a diagnostic.
    fn resource_offer(
        &self,
        project: &ProjectSlug,
        client: Option<&Arc<OsppClient>>,
    ) -> Result<Option<RemoteResourceOffer>, EdgeError> {
        let Some(client) = client else {
            return Ok(None);
        };
        match client.resource_offer(project, WORLD_MODE) {
            Ok(offer) => Ok(offer),
            Err(OsppError::Transport { .. }) => {
                self.invalidate_ospp_client();
                Ok(None)
            }
            Err(error) => Err(runtime_error(format!(
                "cannot describe OSPP resources for '{project}:{WORLD_MODE}': {error}"
            ))),
        }
    }

    /// Resolve the repository, target, and checkout for one `cwd` + `--world` request.
    fn select(&self, cwd: &Path, world: Option<&str>) -> Result<WorldSelection, EdgeError> {
        let repo = self
            .repo_roots
            .resolve(cwd)
            .map_err(|err| runtime_error(err.to_string()))?;
        match world {
            Some(fork) => {
                validate_fork_name(fork, &[]).map_err(|err| rejected(err.to_string()))?;
                let worktree = repo.vcs_root.join(WORKTREES_DIRECTORY).join(fork);
                Ok(WorldSelection {
                    repo,
                    target: WorldTarget::UserFork(fork.to_owned()),
                    worktree,
                })
            }
            None => {
                let worktrees = repo.vcs_root.join(WORKTREES_DIRECTORY);
                match repo.worktree_root.strip_prefix(&worktrees) {
                    Ok(relative) => {
                        let fork = relative
                            .components()
                            .next()
                            .and_then(|component| component.as_os_str().to_str())
                            .ok_or_else(|| {
                                rejected(format!(
                                    "cannot resolve the fork name from '{}' (expected <vcs_root>/{WORKTREES_DIRECTORY}/<fork>)",
                                    repo.worktree_root.display()
                                ))
                            })?;
                        validate_fork_name(fork, &[]).map_err(|err| rejected(err.to_string()))?;
                        let target = WorldTarget::UserFork(fork.to_owned());
                        let worktree = repo.worktree_root.clone();
                        Ok(WorldSelection {
                            repo,
                            target,
                            worktree,
                        })
                    }
                    Err(_) => Ok(WorldSelection {
                        worktree: repo.vcs_root.clone(),
                        repo,
                        target: WorldTarget::Canonical,
                    }),
                }
            }
        }
    }

    fn world_state(&self, selection: &WorldSelection) -> Option<WorldState> {
        self.lock()
            .world(&selection.repo.project, &selection.target)
            .map(|world| world.state)
    }

    fn world_resources(&self, selection: &WorldSelection) -> Option<Vec<ResourceSummary>> {
        self.lock()
            .world(&selection.repo.project, &selection.target)
            .map(|world| world.resources.clone())
    }

    fn summary(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
        resources: Vec<ResourceSummary>,
    ) -> Result<WorldUpSummary, EdgeError> {
        let origin = self.registry.origin(project, target)?;
        Ok(WorldUpSummary {
            world: world_id(project, target),
            origin,
            resources,
        })
    }

    fn load_snow(&self, worktree: &Path) -> Result<SnowFile, EdgeError> {
        let path = worktree.join("snow.yml");
        let raw = fs::read_to_string(&path).map_err(|err| {
            runtime_error(format!(
                "cannot read {}: {err}; every world boot needs the project's snow.yml",
                path.display()
            ))
        })?;
        SnowFile::parse_yaml(&raw)
            .map_err(|err| runtime_error(format!("{}: {err}", path.display())))
    }

    fn load_config(&self, vcs_root: &Path) -> Result<SlipforkConfig, EdgeError> {
        // `slipfork.toml` is optional (`VISION.md`, `docs/getting-started.md`): an absent manifest
        // means the default DAG/resources spec, while a malformed or unreadable one fails closed.
        load_optional_slipfork_config(&vcs_root.join("slipfork.toml")).map_err(runtime_error)
    }

    /// Materialize (or reuse) the selected `snow.yml` `Dockerfile` stage as the world's `L0`
    /// rootfs (`ADR-0009` §1–§3), resolving `[world]` and every requested backing-service image.
    fn materialize_rootfs(
        &self,
        worktree: &Path,
        snow: &SnowFile,
        config: &SlipforkConfig,
        vault: &dyn SecretProvider,
        coords: &ResourceCoordinates,
        host_caches: bool,
    ) -> Result<WorldRootfs, EdgeError> {
        let build_images: Vec<&slipfork_snow::BuildImage> = snow
            .images
            .iter()
            .filter_map(|image| match image {
                Image::Build(build) => Some(build),
                Image::DockerHub { .. } => None,
            })
            .collect();
        let ids: Vec<String> = build_images.iter().map(|image| image.id.clone()).collect();
        let image_id = config
            .world
            .select_image(&ids)
            .map_err(|err| runtime_error(err.to_string()))?
            .to_owned();
        let image = build_images
            .iter()
            .find(|image| image.id == image_id)
            .copied()
            .ok_or_else(|| {
                runtime_error(format!(
                    "snow.yml images: selected image '{image_id}' is not a buildable Dockerfile image"
                ))
            })?;
        let context_dir = worktree.join(&image.context);
        let dockerfile = context_dir.join(&image.dockerfile);

        let groups = snow
            .effective_env_groups(WORLD_MODE)
            .map_err(|err| runtime_error(err.to_string()))?;
        let build_names: BTreeSet<String> = image
            .envs
            .iter()
            .filter_map(|group| groups.get(group))
            .flat_map(|vars| vars.keys().cloned())
            .collect();
        let resolved = snow
            .resolve_mode(WORLD_MODE, None, vault, coords, &OsCsprngEntropy)
            .map_err(|err| runtime_error(err.to_string()))?;
        let build_envs: Vec<BuildEnvVar> = resolved
            .iter()
            .filter(|(name, _)| build_names.contains(*name))
            .map(|(name, value)| BuildEnvVar {
                name: name.clone(),
                value: value.expose().to_owned(),
            })
            .collect();

        let component = snow
            .components
            .iter()
            .find(|component| component.image == image_id);
        let ports = SnowPortContract {
            component: component.map_or_else(|| image_id.clone(), |component| component.id.clone()),
            internal_ports: component
                .map(|component| component.internal_ports.clone())
                .unwrap_or_default(),
            ingress_ports: snow
                .ingresses
                .iter()
                .filter(|ingress| {
                    component.is_some_and(|component| ingress.target.component == component.id)
                })
                .map(|ingress| ingress.target.port)
                .collect(),
        };

        let request = WorldRootfsRequest {
            state_dir: self.config.layout.state_dir().to_path_buf(),
            image_id,
            context_dir,
            context_label: image.context.clone(),
            dockerfile,
            target_stage: config.world.stage.clone(),
            build_envs,
            target_arch: self.config.target_arch.clone(),
            services: requested_service_images(snow),
            ports,
            uid: self.config.uid,
            gid: self.config.gid,
            runtime: self.config.runtime.clone(),
            host_caches: if host_caches {
                HostCacheRequest {
                    host_shell: self.config.host_caches.witness.clone(),
                    mounts: self.config.host_caches.mounts.clone(),
                }
            } else {
                HostCacheRequest::default()
            },
        };
        self.builder
            .materialize(&request, &mut |event: RootfsBuildEvent| {
                // Progress reaches the CLI through `BootEvent`; a cache hit/miss is silent for now.
                let _ = event;
            })
            .map_err(|err| runtime_error(err.to_string()))
    }

    /// Seal the real `<project>-base-<digest>` snapshot (`ADR-0008` §2, `ADR-0009` §2–§3).
    ///
    /// A short-lived seed world boots on an empty root snapshot, starts every backing-service
    /// container (mounting each verified `<sha256>.pgcustom` read-only at `/dumps/...`), restores
    /// the selected dumps with `pg_restore` + `CHECKPOINT;` **inside** the `postgres:<v>`
    /// container, and is then frozen and destroyed. `up`/`fork` clone the sealed base and never
    /// re-run `pg_restore`; `--no-dump` seals the clean `initdb` base through the same path.
    fn ensure_base_snapshot(
        &self,
        project: &ProjectSlug,
        snow: &SnowFile,
        plan: &ResourcePlan,
        rootfs: &WorldRootfs,
        client: Option<&Arc<OsppClient>>,
    ) -> Result<SnapshotId, EdgeError> {
        let digest = base_digest(project, snow, plan);
        let key = (project.clone(), digest.clone());
        if let Some(snapshot) = self.lock().bases.get(&key).cloned() {
            return Ok(snapshot);
        }

        let dumps = self.materialize_seed_dumps(project, snow, plan, client)?;
        let seed = self.config.layout.base_seed_dir(project);
        ensure_private_dir(&seed).map_err(runtime_error)?;
        let empty = SnapshotId(format!("{project}-seed-empty"));
        self.engine.create_snapshot(&seed, &empty).map_err(|err| {
            runtime_error(format!("cannot create the empty base seed snapshot: {err}"))
        })?;

        let mut mounts = Vec::new();
        if let Some(directory) = dumps
            .values()
            .next()
            .and_then(|(_, path)| path.parent())
            .map(Path::to_path_buf)
        {
            mounts.push(WorldMount {
                host_path: directory.clone(),
                guest_path: directory,
                mode: MountMode::ReadOnly,
            });
        }
        let spec = WorldSpec {
            target: WorldTarget::TestSlot(0),
            rootfs: rootfs.clone(),
            snapshot: empty,
            mounts,
            base_env: ExecEnv::world_scoped(self.config.layout.world_home(project, "base-seed")),
            local_ingress: None,
        };
        let handle = self.engine.clone_world(&spec).map_err(|err| {
            runtime_error(format!(
                "cannot boot the base seed world for '{project}': {err}"
            ))
        })?;

        let restored = self.restore_base(&handle, snow, rootfs, &dumps);
        let snapshot = SnapshotId(format!("{project}-base-{digest}"));
        let sealed = restored.and_then(|()| {
            self.engine
                .freeze_world(&handle, &snapshot)
                .map_err(|err| runtime_error(format!("cannot freeze the base seed world: {err}")))
        });
        if let Err(error) = sealed {
            let _ = self.engine.destroy_world(handle);
            return Err(error);
        }
        self.engine
            .destroy_world(handle)
            .map_err(|err| runtime_error(format!("cannot destroy the base seed world: {err}")))?;
        self.lock().bases.insert(key, snapshot.clone());
        Ok(snapshot)
    }

    /// Fetch/materialize every dump the plan selected, keyed by `snow.yml` database id.
    fn materialize_seed_dumps(
        &self,
        project: &ProjectSlug,
        snow: &SnowFile,
        plan: &ResourcePlan,
        client: Option<&Arc<OsppClient>>,
    ) -> Result<BTreeMap<String, (String, PathBuf)>, EdgeError> {
        let mut dumps = BTreeMap::new();
        for database in &plan.pg_databases {
            let PgSeed::RemoteDump { dump, refresh } = &database.seed else {
                continue;
            };
            if !snow
                .resources
                .pg_databases
                .iter()
                .any(|declared| declared.id == database.id)
            {
                continue;
            }
            let sha256 = dump.sha256_compressed().to_owned();
            if self.seed_dumps.lookup(&sha256).is_none() || *refresh {
                let client = client.ok_or_else(|| {
                    runtime_error(format!(
                        "the seed dump for '{}' requires a paired OSPP session; run `slipfork auth login`",
                        database.id
                    ))
                })?;
                DumpStreamer::new(client.as_ref())
                    .fetch(project, WORLD_MODE, &database.id, self.seed_dumps.as_ref())
                    .map_err(|err| {
                        runtime_error(format!(
                            "cannot stream the seed dump for '{}': {err}",
                            database.id
                        ))
                    })?;
            }
            let pgcustom = self.seed_dumps.materialize(&sha256).map_err(|err| {
                runtime_error(format!(
                    "cannot materialize the seed dump for '{}': {err}",
                    database.id
                ))
            })?;
            dumps.insert(database.id.clone(), (sha256, pgcustom));
        }
        Ok(dumps)
    }

    /// Start every backing-service container in the seed world and restore the selected dumps.
    fn restore_base(
        &self,
        handle: &WorldHandle,
        snow: &SnowFile,
        rootfs: &WorldRootfs,
        dumps: &BTreeMap<String, (String, PathBuf)>,
    ) -> Result<(), EdgeError> {
        let mounts = dump_mounts(snow, dumps);
        let mut services: BTreeMap<String, ServiceHandle> = BTreeMap::new();
        for spec in service_specs(snow, rootfs, &mounts)? {
            let service = match self.engine.spawn_service(handle, &spec) {
                Ok(service) => service,
                Err(err) => {
                    let tail = self
                        .engine
                        .service_logs(handle, &spec.id)
                        .map(|logs| {
                            let bytes: Vec<u8> = logs
                                .snapshot()
                                .into_iter()
                                .flat_map(|chunk| chunk.bytes)
                                .collect();
                            let text = String::from_utf8_lossy(&bytes);
                            let lines: Vec<&str> = text.lines().collect();
                            lines[lines.len().saturating_sub(20)..].join("\n")
                        })
                        .unwrap_or_default();
                    return Err(runtime_error(format!(
                        "cannot start backing service '{}' in the base seed world: {err}\n{tail}",
                        spec.id
                    )));
                }
            };
            services.insert(spec.id.clone(), service);
        }
        for database in &snow.resources.pg_databases {
            let Some(service) = services.get(&format!("pg_clusters.{}", database.cluster)) else {
                continue;
            };
            self.ensure_database(service, &database.id)?;
            let Some((sha256, _)) = dumps.get(&database.id) else {
                continue;
            };
            let guest = format!("/dumps/{sha256}{PGCUSTOM_SUFFIX}");
            self.exec_service_checked(
                service,
                &[
                    "pg_restore".to_owned(),
                    "--no-owner".to_owned(),
                    "--no-privileges".to_owned(),
                    "--clean".to_owned(),
                    "--if-exists".to_owned(),
                    "-U".to_owned(),
                    "postgres".to_owned(),
                    "-h".to_owned(),
                    "127.0.0.1".to_owned(),
                    "-d".to_owned(),
                    database.id.clone(),
                    guest,
                ],
                &format!("restoring database '{}'", database.id),
            )?;
            self.exec_service_checked(
                service,
                &[
                    "psql".to_owned(),
                    "-U".to_owned(),
                    "postgres".to_owned(),
                    "-h".to_owned(),
                    "127.0.0.1".to_owned(),
                    "-d".to_owned(),
                    database.id.clone(),
                    "-v".to_owned(),
                    "ON_ERROR_STOP=1".to_owned(),
                    "-c".to_owned(),
                    "CHECKPOINT;".to_owned(),
                ],
                &format!("checkpointing database '{}'", database.id),
            )?;
        }
        Ok(())
    }

    /// Create one declared `snow.yml` database in a freshly `initdb`-ed cluster when absent.
    fn ensure_database(&self, service: &ServiceHandle, database: &str) -> Result<(), EdgeError> {
        validate_database_id(database)?;
        let query = format!("SELECT 1 FROM pg_database WHERE datname = '{database}'");
        let existing = self.exec_service_checked(
            service,
            &[
                "psql".to_owned(),
                "-U".to_owned(),
                "postgres".to_owned(),
                "-h".to_owned(),
                "127.0.0.1".to_owned(),
                "-d".to_owned(),
                "postgres".to_owned(),
                "-tAc".to_owned(),
                query,
            ],
            &format!("checking database '{database}'"),
        )?;
        if existing.stdout.trim() == "1" {
            return Ok(());
        }
        self.exec_service_checked(
            service,
            &[
                "createdb".to_owned(),
                "-U".to_owned(),
                "postgres".to_owned(),
                "-h".to_owned(),
                "127.0.0.1".to_owned(),
                database.to_owned(),
            ],
            &format!("creating database '{database}'"),
        )?;
        Ok(())
    }

    /// Run `argv` inside a running backing-service container, failing closed on a non-zero exit.
    fn exec_service_checked(
        &self,
        service: &ServiceHandle,
        argv: &[String],
        what: &str,
    ) -> Result<ExecOutput, EdgeError> {
        let mut stderr = Vec::new();
        let output = self
            .engine
            .exec_in_service(service, argv, &ExecEnv::default(), &mut |chunk| {
                if chunk.source == slipfork_engine::StreamSource::Stderr {
                    stderr.extend_from_slice(&chunk.bytes);
                }
            })
            .map_err(|err| match err {
                EngineError::ServiceNotRunning { .. } => runtime_error(format!(
                    "the base seed world's backing service '{}' is not running (ServiceNotRunning); cannot {what}",
                    service.id()
                )),
                other => runtime_error(format!("cannot {what}: {other}")),
            })?;
        if output.exit_code != 0 {
            let detail = if output.stderr.trim().is_empty() {
                String::from_utf8_lossy(&stderr).into_owned()
            } else {
                output.stderr.clone()
            };
            return Err(runtime_error(format!(
                "{what} failed with exit code {}: {}",
                output.exit_code,
                detail.trim()
            )));
        }
        Ok(output)
    }

    fn world_env(
        &self,
        snow: &SnowFile,
        vault: &dyn SecretProvider,
        coords: &ResourceCoordinates,
        project: &ProjectSlug,
        target: &WorldTarget,
    ) -> Result<ExecEnv, EdgeError> {
        let mut env = ExecEnv::world_scoped(
            self.config
                .layout
                .world_home(project, &world_id(project, target)),
        );
        for (var, value) in &self.config.inherited {
            env.inherit(*var, value.clone());
        }
        let resolved = snow
            .resolve_mode(WORLD_MODE, None, vault, coords, &OsCsprngEntropy)
            .map_err(|err| runtime_error(err.to_string()))?;
        apply_env(&mut env, resolved)?;
        Ok(env)
    }

    fn component_env(
        &self,
        snow: &SnowFile,
        vault: &dyn SecretProvider,
        coords: &ResourceCoordinates,
        component: &str,
    ) -> Result<ExecEnv, EdgeError> {
        let resolved = snow
            .resolve_mode(WORLD_MODE, Some(component), vault, coords, &OsCsprngEntropy)
            .map_err(|err| runtime_error(err.to_string()))?;
        let mut env = ExecEnv::default();
        apply_env(&mut env, resolved)?;
        Ok(env)
    }

    /// Materialize, attach, and start one world.
    fn boot(
        &self,
        context: BootContext,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<LiveWorld, BootFailure> {
        let BootContext {
            repo,
            target,
            worktree,
            dump,
            force_local_resources,
            host_caches,
            mut workspace,
        } = context;
        let project = repo.project.clone();

        let snow = match self.load_snow(&worktree) {
            Ok(snow) => snow,
            Err(error) => {
                return Err(BootFailure {
                    error,
                    world: None,
                    workspace,
                });
            }
        };
        let effective = match snow.effective_components(WORLD_MODE) {
            Ok(effective) => effective,
            Err(err) => {
                return Err(BootFailure {
                    error: runtime_error(err.to_string()),
                    world: None,
                    workspace,
                });
            }
        };

        let route =
            match LocalHostRoute::new(project.clone(), target.clone(), None, &snow.domain_ids()) {
                Ok(route) => route,
                Err(err) => {
                    return Err(BootFailure {
                        error: runtime_error(err.to_string()),
                        world: None,
                        workspace,
                    });
                }
            };
        let origin = match self.registry.mint_origin(&route, self.http_ingress_port) {
            Ok(origin) => origin,
            Err(error) => {
                return Err(BootFailure {
                    error,
                    world: None,
                    workspace,
                });
            }
        };

        let config = match self.load_config(&repo.vcs_root) {
            Ok(config) => config,
            Err(error) => {
                return Err(BootFailure {
                    error,
                    world: None,
                    workspace,
                });
            }
        };
        let client = match self.ospp_client() {
            Ok(client) => client,
            Err(error) => {
                return Err(BootFailure {
                    error,
                    world: None,
                    workspace,
                });
            }
        };
        let offer = match self.resource_offer(&project, client.as_ref()) {
            Ok(offer) => offer,
            Err(error) => {
                return Err(BootFailure {
                    error,
                    world: None,
                    workspace,
                });
            }
        };
        let plan = match resolve_plan(
            &config,
            &snow,
            &target,
            dump,
            force_local_resources,
            offer.as_ref(),
        ) {
            Ok(plan) => plan,
            Err(error) => {
                return Err(BootFailure {
                    error,
                    world: None,
                    workspace,
                });
            }
        };
        let resources = plan_summaries(&plan);
        for resource in &resources {
            on_event(BootEvent::Resources {
                resource: resource.clone(),
            });
        }

        let coords = match ResourceCoordinates::for_world(
            &snow,
            &project,
            &target,
            &origin,
            &s3_overrides(&plan),
        ) {
            Ok(coords) => coords,
            Err(err) => {
                return Err(BootFailure {
                    error: runtime_error(err.to_string()),
                    world: None,
                    workspace,
                });
            }
        };
        let vault = match self.vault_for(&project) {
            Ok(vault) => vault,
            Err(error) => {
                return Err(BootFailure {
                    error,
                    world: None,
                    workspace,
                });
            }
        };
        let base_env = match self.world_env(&snow, vault.as_ref(), &coords, &project, &target) {
            Ok(env) => env,
            Err(error) => {
                return Err(BootFailure {
                    error,
                    world: None,
                    workspace,
                });
            }
        };

        let rootfs = match self.materialize_rootfs(
            &worktree,
            &snow,
            &config,
            vault.as_ref(),
            &coords,
            host_caches,
        ) {
            Ok(rootfs) => rootfs,
            Err(error) => {
                return Err(BootFailure {
                    error,
                    world: None,
                    workspace,
                });
            }
        };

        let mut component_envs = BTreeMap::new();
        let mut enabled_components = BTreeSet::new();
        for (id, resolved) in &effective {
            if resolved.disabled {
                continue;
            }
            enabled_components.insert(id.clone());
            match self.component_env(&snow, vault.as_ref(), &coords, id) {
                Ok(env) => {
                    component_envs.insert(id.clone(), env);
                }
                Err(error) => {
                    return Err(BootFailure {
                        error,
                        world: None,
                        workspace,
                    });
                }
            }
        }

        let ingresses = match ingress_routes(&snow, &effective) {
            Ok(ingresses) => ingresses,
            Err(error) => {
                return Err(BootFailure {
                    error,
                    world: None,
                    workspace,
                });
            }
        };

        let snapshot =
            match self.ensure_base_snapshot(&project, &snow, &plan, &rootfs, client.as_ref()) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    return Err(BootFailure {
                        error,
                        world: None,
                        workspace,
                    });
                }
            };
        let spec = WorldSpec {
            target: target.clone(),
            rootfs: rootfs.clone(),
            snapshot,
            mounts: vec![WorldMount {
                host_path: worktree.clone(),
                guest_path: worktree.clone(),
                mode: MountMode::ReadWrite,
            }],
            base_env: base_env.clone(),
            local_ingress: Some(LocalIngressBridge {
                port: self.http_ingress_port,
                host_ingress_sock: self.config.layout.http_ingress_socket(),
            }),
        };
        let handle = match self.engine.clone_world(&spec) {
            Ok(handle) => handle,
            Err(err) => {
                return Err(BootFailure {
                    error: runtime_error(format!(
                        "cannot materialize world '{}': {err}",
                        world_id(&project, &target)
                    )),
                    world: None,
                    workspace,
                });
            }
        };

        let domains: Vec<String> = snow.domain_ids().into_iter().map(str::to_owned).collect();
        if let Err(error) =
            self.registry
                .attach_world(&project, &target, domains, handle.clone(), ingresses)
        {
            let _ = self.engine.destroy_world(handle);
            return Err(BootFailure {
                error,
                world: None,
                workspace,
            });
        }

        let mut live = LiveWorld {
            project: project.clone(),
            target: target.clone(),
            repo,
            worktree: worktree.clone(),
            handle: handle.clone(),
            state: WorldState::Running,
            base_env,
            component_envs,
            enabled_components,
            snow: snow.clone(),
            resources,
            services: Vec::new(),
            hook_logs: BTreeMap::new(),
            workspace: workspace.take().map(Arc::from),
        };

        let service_specs = match service_specs(&snow, &rootfs, &BTreeMap::new()) {
            Ok(specs) => specs,
            Err(error) => {
                live.state = WorldState::Failed;
                return Err(BootFailure {
                    error,
                    world: Some(Box::new(live)),
                    workspace: None,
                });
            }
        };
        for spec in service_specs {
            match self.engine.spawn_service(&handle, &spec) {
                Ok(service) => live.services.push(service),
                Err(err) => {
                    live.state = WorldState::Failed;
                    return Err(BootFailure {
                        error: runtime_error(format!(
                            "cannot start backing service '{}' in world '{}': {err}",
                            spec.id,
                            world_id(&project, &target)
                        )),
                        world: Some(Box::new(live)),
                        workspace: None,
                    });
                }
            }
        }

        for (id, resolved) in &effective {
            if resolved.disabled {
                continue;
            }
            if resolved.run_command.trim().is_empty() {
                live.state = WorldState::Failed;
                return Err(BootFailure {
                    error: rejected(format!(
                        "component '{id}' declares an empty run_command in mode '{WORLD_MODE}'"
                    )),
                    world: Some(Box::new(live)),
                    workspace: None,
                });
            }
            let declaration = snow.components.iter().find(|component| component.id == *id);
            let readiness_port =
                declaration.and_then(|component| component.internal_ports.first().copied());
            let spec = ServiceSpec {
                id: id.clone(),
                runtime: slipfork_engine::ServiceRuntime::WorldProcess {
                    argv: vec![
                        "sh".to_owned(),
                        "-eu".to_owned(),
                        "-c".to_owned(),
                        resolved.run_command.clone(),
                    ],
                    cwd: worktree.clone(),
                },
                env: live.component_envs.get(id).cloned().unwrap_or_default(),
                readiness_port,
            };
            match self.engine.spawn_service(&handle, &spec) {
                Ok(service) => {
                    live.services.push(service);
                    if let Some(port) = readiness_port {
                        on_event(BootEvent::Ready {
                            component: id.clone(),
                            port,
                        });
                    }
                }
                Err(err) => {
                    live.state = WorldState::Failed;
                    return Err(BootFailure {
                        error: runtime_error(format!(
                            "cannot start component '{id}' in world '{}': {err}",
                            world_id(&project, &target)
                        )),
                        world: Some(Box::new(live)),
                        workspace: None,
                    });
                }
            }
        }

        if let Err(error) = self.run_hooks(&mut live, HookPhase::PreDeploy, on_event) {
            live.state = WorldState::Failed;
            return Err(BootFailure {
                error,
                world: Some(Box::new(live)),
                workspace: None,
            });
        }
        if let Err(error) = self.run_hooks(&mut live, HookPhase::PostDeploy, on_event) {
            live.state = WorldState::Failed;
            return Err(BootFailure {
                error,
                world: Some(Box::new(live)),
                workspace: None,
            });
        }
        Ok(live)
    }

    fn run_hooks(
        &self,
        live: &mut LiveWorld,
        phase: HookPhase,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<(), EdgeError> {
        let hooks = live.snow.hooks.clone();
        for hook in hooks {
            if hook.when != phase || !live.enabled_components.contains(&hook.component) {
                continue;
            }
            if hook.run_command.trim().is_empty() {
                return Err(rejected(format!(
                    "hook '{}:{}' declares an empty run_command",
                    hook.component,
                    hook_phase_label(hook.when)
                )));
            }
            let id = format!("{}:{}", hook.component, hook_phase_label(hook.when));
            let argv = vec![
                "sh".to_owned(),
                "-eu".to_owned(),
                "-c".to_owned(),
                hook.run_command.clone(),
            ];
            on_event(BootEvent::Hook {
                id: id.clone(),
                argv: argv.clone(),
            });
            let overlay = live
                .component_envs
                .get(&hook.component)
                .cloned()
                .unwrap_or_default();
            let log = self.hook_log(live, &id, hook_phase_label(hook.when))?;
            let mut file = open_hook_log(&log.path)?;
            let output = self
                .engine
                .exec_in_world(
                    &live.handle,
                    &argv,
                    &live.worktree,
                    &overlay,
                    &mut |chunk| {
                        let _ = file.write_all(&chunk.bytes);
                        let _ = file.flush();
                        log.push(chunk.clone());
                        on_event(BootEvent::HookOutput {
                            id: id.clone(),
                            chunk,
                        });
                    },
                )
                .map_err(|err| {
                    log.close();
                    runtime_error(format!(
                        "cannot run hook '{id}' in world '{}': {err}",
                        world_id(&live.project, &live.target)
                    ))
                })?;
            log.close();
            if output.exit_code != 0 {
                let world = world_id(&live.project, &live.target);
                on_event(BootEvent::HookFailed {
                    id: id.clone(),
                    argv,
                    exit_code: output.exit_code,
                    world: world.clone(),
                    log_path: log.path.clone(),
                });
                return Err(runtime_error(format!(
                    "hook '{id}' exited with code {} in world '{world}' (the world is kept for inspection; full output is in {})",
                    output.exit_code,
                    log.path.display()
                )));
            }
        }
        Ok(())
    }

    /// The retained log for one hook, created once per world and reused by `hook run`.
    fn hook_log(
        &self,
        live: &mut LiveWorld,
        id: &str,
        phase: &str,
    ) -> Result<Arc<HookLog>, EdgeError> {
        if let Some(log) = live.hook_logs.get(id) {
            return Ok(Arc::clone(log));
        }
        let directory = self
            .config
            .layout
            .world_logs_dir(&live.project, &world_id(&live.project, &live.target));
        ensure_private_dir(&directory).map_err(runtime_error)?;
        let (component, _) = id
            .rsplit_once(':')
            .ok_or_else(|| rejected(format!("invalid hook id '{id}'")))?;
        let log = Arc::new(HookLog::new(
            directory.join(format!("{component}.{phase}.log")),
        ));
        live.hook_logs.insert(id.to_owned(), Arc::clone(&log));
        Ok(log)
    }

    fn insert_world(&self, live: LiveWorld) {
        let mut state = self.lock();
        if let Some(index) = state.world_index(&live.project, &live.target) {
            state.worlds[index] = live;
        } else {
            state.worlds.push(live);
        }
    }

    fn take_world(&self, project: &ProjectSlug, target: &WorldTarget) -> Option<LiveWorld> {
        let mut state = self.lock();
        state
            .world_index(project, target)
            .map(|index| state.worlds.remove(index))
    }
}

impl DaemonControl for DaemonService {
    fn status(&self) -> Result<DaemonStatus, EdgeError> {
        let worlds = self.list(None)?;
        Ok(DaemonStatus {
            pid: std::process::id(),
            http_ingress_port: self.http_ingress_port,
            forward_port: self.forward_port,
            worlds,
        })
    }

    fn snow_resolve(&self, req: &SnowResolveRequest) -> Result<SnowResolveSummary, EdgeError> {
        let file = if req.file.is_absolute() {
            req.file.clone()
        } else {
            req.cwd.join(&req.file)
        };
        let raw = fs::read_to_string(&file)
            .map_err(|err| runtime_error(format!("cannot read {}: {err}", file.display())))?;
        let snow = SnowFile::parse_yaml(&raw)
            .map_err(|err| runtime_error(format!("{}: {err}", file.display())))?;
        let repository = self
            .repo_roots
            .resolve(&parent_dir(&file))
            .map_err(|err| runtime_error(err.to_string()))?;
        let project = repository.project;
        let vault = self.vault_for(&project)?;
        let coords = ResourceCoordinates::local_for_world(&snow, &project, &WorldTarget::Canonical)
            .map_err(|err| runtime_error(err.to_string()))?;
        let resolved = snow
            .resolve_mode(
                &req.mode,
                req.component.as_deref(),
                vault.as_ref(),
                &coords,
                &OsCsprngEntropy,
            )
            .map_err(|err| runtime_error(err.to_string()))?;
        let vars = resolved
            .iter()
            .map(|(key, value)| {
                let text = if req.expose_secrets {
                    value.expose()
                } else {
                    value.redacted()
                };
                (key.clone(), text.to_owned())
            })
            .collect();
        Ok(SnowResolveSummary { vars })
    }

    fn up(
        &self,
        req: &UpRequest,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let _guard = self.boot_guard();

        if let Some(state) = self.world_state(&selection) {
            if state == WorldState::Running {
                return self.summary(
                    &selection.repo.project,
                    &selection.target,
                    self.world_resources(&selection).unwrap_or_default(),
                );
            }
            // A failed world is preserved for inspection, but an explicit `up` retries it.
            self.release_world(&selection.repo.project, &selection.target)?;
        }
        if !selection.worktree.is_dir() {
            return Err(rejected(format!(
                "world worktree '{}' does not exist; run `slipfork fork {}` first",
                selection.worktree.display(),
                match &selection.target {
                    WorldTarget::UserFork(fork) => fork.as_str(),
                    _ => "<fork>",
                }
            )));
        }

        let context = BootContext {
            repo: selection.repo.clone(),
            target: selection.target.clone(),
            worktree: selection.worktree.clone(),
            dump: dump_policy(req.dump),
            force_local_resources: matches!(req.resources, slipfork_edge::ResourceRequest::Local),
            host_caches: req.host_caches,
            workspace: None,
        };
        match self.boot(context, on_event) {
            Ok(live) => {
                let summary = self.summary(&live.project, &live.target, live.resources.clone())?;
                self.insert_world(live);
                Ok(summary)
            }
            Err(failure) => {
                if let Some(live) = failure.world {
                    self.insert_world(*live);
                }
                Err(failure.error)
            }
        }
    }

    fn down(&self, req: &WorldSelector) -> Result<bool, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        self.release_only(&selection.repo.project, &selection.target)
    }

    fn fork(
        &self,
        req: &ForkRequest,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError> {
        let selection = self.select(&req.cwd, Some(&req.fork))?;
        let _guard = self.boot_guard();
        if self.world_state(&selection).is_some() {
            return Err(rejected(format!(
                "world '{}' already exists; run `slipfork down --world {}` or `slipfork drop {}` first",
                world_id(&selection.repo.project, &selection.target),
                req.fork,
                req.fork
            )));
        }
        let workspace = self
            .workspaces
            .create_workspace(&WorkspaceRequest {
                repo: selection.repo.clone(),
                fork: req.fork.clone(),
                base: req.base.clone(),
            })
            .map_err(|err| runtime_error(err.to_string()))?;
        let context = BootContext {
            repo: selection.repo.clone(),
            target: selection.target.clone(),
            worktree: selection.worktree.clone(),
            dump: dump_policy(req.dump),
            force_local_resources: matches!(req.resources, slipfork_edge::ResourceRequest::Local),
            host_caches: req.host_caches,
            workspace: Some(workspace),
        };
        match self.boot(context, on_event) {
            Ok(live) => {
                let summary = self.summary(&live.project, &live.target, live.resources.clone())?;
                self.insert_world(live);
                Ok(summary)
            }
            Err(failure) => {
                if let Some(live) = failure.world {
                    self.insert_world(*live);
                } else if let Some(workspace) = failure.workspace {
                    workspace
                        .forget()
                        .map_err(|err| runtime_error(err.to_string()))?;
                }
                Err(failure.error)
            }
        }
    }

    fn drop_fork(&self, req: &DropForkRequest) -> Result<bool, EdgeError> {
        let selection = self.select(&req.cwd, Some(&req.fork))?;
        let project = selection.repo.project.clone();
        let target = selection.target.clone();
        let (released, world) = self.release_world(&project, &target)?;
        if let Some(world) = world {
            if let Some(workspace) = world.workspace {
                workspace
                    .forget()
                    .map_err(|err| runtime_error(err.to_string()))?;
            }
            return Ok(true);
        }
        Ok(released)
    }

    fn list(&self, repo_root: Option<&Path>) -> Result<Vec<WorldSummary>, EdgeError> {
        let filter = match repo_root {
            Some(root) => Some(
                self.repo_roots
                    .resolve(root)
                    .map_err(|err| runtime_error(err.to_string()))?
                    .vcs_root,
            ),
            None => None,
        };
        let entries: Vec<(ProjectSlug, WorldTarget, WorldState, PathBuf)> = {
            let state = self.lock();
            state
                .worlds
                .iter()
                .filter(|world| match &filter {
                    Some(vcs_root) => world.repo.vcs_root == *vcs_root,
                    None => true,
                })
                .map(|world| {
                    (
                        world.project.clone(),
                        world.target.clone(),
                        world.state,
                        world.worktree.clone(),
                    )
                })
                .collect()
        };
        let mut summaries = Vec::with_capacity(entries.len());
        for (project, target, state, worktree) in entries {
            summaries.push(WorldSummary {
                world: world_id(&project, &target),
                state,
                origin: self.registry.origin(&project, &target)?,
                worktree,
            });
        }
        summaries.sort_by(|a, b| a.world.cmp(&b.world));
        Ok(summaries)
    }

    fn prune(&self, dry_run: bool) -> Result<PruneReport, EdgeError> {
        let orphans: Vec<(ProjectSlug, WorldTarget, String)> = {
            let state = self.lock();
            state
                .worlds
                .iter()
                .filter(|world| !world.worktree.is_dir())
                .map(|world| {
                    (
                        world.project.clone(),
                        world.target.clone(),
                        world_id(&world.project, &world.target),
                    )
                })
                .collect()
        };
        if !dry_run {
            for (project, target, _) in &orphans {
                self.release_only(project, target)?;
            }
        }
        Ok(PruneReport {
            dry_run,
            worlds: orphans.into_iter().map(|(_, _, id)| id).collect::<Vec<_>>(),
            // `WorldEngine` exposes no snapshot deletion capability yet, so the daemon never claims
            // to have reaped a snapshot it cannot remove. Snapshot GC lands with the engine surface.
            snapshots: Vec::new(),
        })
    }

    fn share(&self, req: &WorldSelector) -> Result<SharedWorldInfo, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        if self.world_state(&selection).is_none() {
            return Err(Self::unknown_world(
                &selection.repo.project,
                &selection.target,
            ));
        }
        self.registry
            .share_world(&selection.repo.project, &selection.target)
    }

    fn unshare(&self, req: &WorldSelector) -> Result<WorldOrigin, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        self.registry
            .unshare_world(&selection.repo.project, &selection.target)
    }

    fn secret_set(
        &self,
        repo_root: &Path,
        key: &str,
        value: &str,
        force: bool,
    ) -> Result<(), EdgeError> {
        validate_secret_key(key)?;
        let repo = self
            .repo_roots
            .resolve(repo_root)
            .map_err(|err| runtime_error(err.to_string()))?;
        let vault = self.vaults.mutable_vault(&repo.project)?;
        let existing = vault
            .get(Namespace::Secrets, key)
            .map_err(|err| runtime_error(err.to_string()))?;
        if existing.is_some() && !force {
            return Err(rejected(format!(
                "secret '{key}' already exists in {}; pass --force to rotate it",
                self.config.layout.vault_path(&repo.project).display()
            )));
        }
        vault
            .put(Namespace::Secrets, key, value)
            .map_err(|err| runtime_error(err.to_string()))
    }

    fn secret_unset(&self, repo_root: &Path, key: &str) -> Result<bool, EdgeError> {
        validate_secret_key(key)?;
        let repo = self
            .repo_roots
            .resolve(repo_root)
            .map_err(|err| runtime_error(err.to_string()))?;
        let vault = self.vaults.mutable_vault(&repo.project)?;
        vault
            .remove(key)
            .map_err(|err| runtime_error(err.to_string()))
    }

    fn secret_list(&self, repo_root: &Path) -> Result<Vec<String>, EdgeError> {
        let repo = self
            .repo_roots
            .resolve(repo_root)
            .map_err(|err| runtime_error(err.to_string()))?;
        let vault = self.vaults.mutable_vault(&repo.project)?;
        vault.keys().map_err(|err| runtime_error(err.to_string()))
    }

    fn logs(
        &self,
        req: &LogsRequest,
        on_chunk: &mut dyn FnMut(StreamChunk),
    ) -> Result<(), EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let world_id_text = world_id(&selection.repo.project, &selection.target);
        let (handle, service_ids, hook_logs) = {
            let state = self.lock();
            let Some(world) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            (
                world.handle.clone(),
                world
                    .services
                    .iter()
                    .map(ServiceHandle::id)
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
                world.hook_logs.clone(),
            )
        };

        let mut retained: Vec<RetainedTarget> = Vec::new();
        let requested: Vec<String> = match &req.target {
            Some(target) => vec![target.clone()],
            None => service_ids
                .iter()
                .cloned()
                .chain(hook_logs.keys().cloned())
                .collect(),
        };
        for target in requested {
            if service_ids.iter().any(|id| id == &target) {
                retained.push(RetainedTarget::Service(target));
            } else if let Some(log) = hook_logs.get(&target) {
                retained.push(RetainedTarget::Hook(Arc::clone(log)));
            } else {
                let mut known = service_ids.clone();
                known.extend(hook_logs.keys().cloned());
                return Err(rejected(format!(
                    "world '{world_id_text}' has no log target '{target}' (known targets: {})",
                    known.join(", ")
                )));
            }
        }

        let wants = |chunk: &StreamChunk| {
            req.stderr == matches!(chunk.source, slipfork_engine::StreamSource::Stderr)
        };
        for target in retained {
            match target {
                RetainedTarget::Service(id) => {
                    let log = self
                        .engine
                        .service_logs(&handle, &id)
                        .map_err(|err| runtime_error(err.to_string()))?;
                    if req.follow {
                        log.follow(&mut |chunk| {
                            if wants(&chunk) {
                                on_chunk(chunk);
                            }
                        });
                    } else {
                        let selected: Vec<StreamChunk> = log
                            .snapshot()
                            .into_iter()
                            .filter(|chunk| wants(chunk))
                            .collect();
                        for chunk in tail_lines(selected, req.lines) {
                            on_chunk(chunk);
                        }
                    }
                }
                RetainedTarget::Hook(log) => {
                    if req.follow {
                        log.follow(&mut |chunk| {
                            if wants(&chunk) {
                                on_chunk(chunk);
                            }
                        });
                    } else {
                        let selected: Vec<StreamChunk> = log
                            .snapshot()
                            .into_iter()
                            .filter(|chunk| wants(chunk))
                            .collect();
                        for chunk in tail_lines(selected, req.lines) {
                            on_chunk(chunk);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn exec_target(
        &self,
        req: &ExecTargetRequest,
    ) -> Result<slipfork_edge::WorldExecTarget, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let world = world_id(&selection.repo.project, &selection.target);
        let (handle, base_env, service) = {
            let state = self.lock();
            let Some(world) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            let service = match &req.service {
                Some(requested) => {
                    let (service_id, argv) =
                        resolve_service_target(&world.snow, requested, &req.argv)?;
                    let handle = world
                        .services
                        .iter()
                        .find(|service| service.id() == service_id.as_str())
                        .cloned()
                        .ok_or_else(|| {
                            service_not_running(
                                &world_id(&world.project, &world.target),
                                &service_id,
                            )
                        })?;
                    Some((handle, argv))
                }
                None => None,
            };
            (world.handle.clone(), world.base_env.clone(), service)
        };
        let origin = self
            .registry
            .origin(&selection.repo.project, &selection.target)?;
        if let Some((service, argv)) = service {
            let entry = self
                .engine
                .service_entry_command(&service)
                .map_err(|err| service_engine_error(&world, service.id(), err))?;
            return Ok(WorldExecTarget {
                world,
                origin,
                entry,
                // The container's own OCI environment is authoritative; never leak the app
                // rootfs PATH into `crun exec` (that would hide the image's `psql`/`redis-cli`).
                env: ExecEnv::default(),
                target_argv: argv,
            });
        }
        let entry = self
            .engine
            .entry_command(&handle, &req.cwd)
            .map_err(|err| runtime_error(err.to_string()))?;
        Ok(slipfork_edge::WorldExecTarget {
            world,
            origin,
            entry,
            env: base_env,
            target_argv: req.argv.clone(),
        })
    }

    fn hook_exec_target(&self, req: &HookRunRequest) -> Result<WorldExecTarget, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let (handle, worktree, snow, component_envs, enabled, base_env) = {
            let state = self.lock();
            let Some(world) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            (
                world.handle.clone(),
                world.worktree.clone(),
                world.snow.clone(),
                world.component_envs.clone(),
                world.enabled_components.clone(),
                world.base_env.clone(),
            )
        };
        let resolved = resolve_hook(&snow, &enabled, &component_envs, &req.hook)?;
        let entry = self
            .engine
            .entry_command(&handle, &worktree)
            .map_err(|err| runtime_error(err.to_string()))?;
        let origin = self
            .registry
            .origin(&selection.repo.project, &selection.target)?;
        Ok(WorldExecTarget {
            world: world_id(&selection.repo.project, &selection.target),
            origin,
            entry,
            env: base_env.overlaid(&resolved.overlay),
            target_argv: resolved.argv,
        })
    }

    fn workspace_refs(
        &self,
        req: &WorkspaceRefsRequest,
    ) -> Result<WorkspaceRefsSummary, EdgeError> {
        let selection = self.select(&req.cwd, Some(&req.fork))?;
        let world = {
            let state = self.lock();
            let Some(world) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            if world.state != WorldState::Running && world.state != WorldState::Failed {
                return Err(rejected(format!(
                    "world '{}' is not live; run `slipfork up --world {}` first",
                    world_id(&selection.repo.project, &selection.target),
                    req.fork
                )));
            }
            world
                .workspace
                .clone()
                .map(|workspace| (workspace, world.worktree.clone()))
        };
        let (workspace, worktree) = world.ok_or_else(|| {
            rejected(format!(
                "fork '{}' has no materialized workspace in this slipforkd; create it with `slipfork fork {}`",
                req.fork, req.fork
            ))
        })?;
        let refs = workspace.refs();
        let base_oid = workspace
            .resolve(&refs.base)
            .map_err(|err| runtime_error(err.to_string()))?;
        let wip_oid = workspace
            .resolve(&refs.wip)
            .map_err(|err| runtime_error(err.to_string()))?;
        Ok(WorkspaceRefsSummary {
            base: refs.base,
            wip: refs.wip,
            base_oid,
            wip_oid,
            worktree,
        })
    }

    fn hook_run(
        &self,
        req: &HookRunRequest,
        on_chunk: &mut dyn FnMut(StreamChunk),
    ) -> Result<i32, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let (handle, worktree, snow, component_envs, enabled) = {
            let state = self.lock();
            let Some(world) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            (
                world.handle.clone(),
                world.worktree.clone(),
                world.snow.clone(),
                world.component_envs.clone(),
                world.enabled_components.clone(),
            )
        };
        let resolved = resolve_hook(&snow, &enabled, &component_envs, &req.hook)?;
        let log = {
            let mut state = self.lock();
            let Some(world) = state.world_mut(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            self.hook_log(world, &req.hook, resolved.phase_label)?
        };
        let mut file = open_hook_log(&log.path)?;
        let output = self
            .engine
            .exec_in_world(
                &handle,
                &resolved.argv,
                &worktree,
                &resolved.overlay,
                &mut |chunk| {
                    let _ = file.write_all(&chunk.bytes);
                    let _ = file.flush();
                    log.push(chunk.clone());
                    on_chunk(chunk);
                },
            )
            .map_err(|err| {
                log.close();
                runtime_error(err.to_string())
            })?;
        log.close();
        Ok(output.exit_code)
    }

    fn forward(
        &self,
        req: &ForwardRequest,
        wait_disconnect: &mut dyn FnMut(ForwardSummary),
    ) -> Result<(), EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let world_id_text = world_id(&selection.repo.project, &selection.target);
        let (snow, enabled) = {
            let state = self.lock();
            let Some(world) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            (world.snow.clone(), world.enabled_components.clone())
        };
        let specs = forward_specs(&snow, &world_id_text, &enabled, &req.targets)?;
        let bridge = self
            .registry
            .forward(&selection.repo.project, &selection.target, &specs)?;
        wait_disconnect(bridge.summary());
        // Dropping the bridge stops every accept loop and closes the host ports.
        Ok(())
    }

    fn auth_login(&self, pairing_url: &str) -> Result<slipfork_edge::AuthSummary, EdgeError> {
        let url = PairingUrl::parse(pairing_url)
            .map_err(|err| rejected(format!("invalid pairing URL: {err}")))?;
        let browser_url = url
            .browser_url()
            .map_err(|err| rejected(format!("invalid pairing URL: {err}")))?;
        self.browser
            .open_pairing_url(browser_url)
            .map_err(|err| runtime_error(format!("cannot open the pairing page: {err}")))?;
        let host = url.host().to_owned();
        let port = url.port();
        let tls = url.scheme().uses_tls();
        let approval = redeem_pairing_url(
            url,
            self.signer.as_ref(),
            self.entropy.as_ref(),
            self.ospp_timeouts,
        )
        .map_err(|err| {
            // OSPP errors never embed the handle; the URL itself is never re-rendered here.
            runtime_error(format!("OSPP pairing failed: {err}"))
        })?;
        self.ospp_sessions.save(&OsppSessionCoordinates::new(
            host,
            port,
            tls,
            approval.account.clone(),
        ))?;
        // The next capability call must dial the freshly paired host, never a stale session.
        self.invalidate_ospp_client();
        Ok(slipfork_edge::AuthSummary {
            key_id: self.signer.key_id().as_str().to_owned(),
            backend: self.signer.backend_label().to_owned(),
            account: Some(approval.account),
        })
    }
}

impl DaemonService {
    /// Release one world from the registry and the daemon index (idempotent).
    ///
    /// Returns the registry's `released` flag plus the removed in-process record, so `drop` can
    /// forget the fork workspace with the same critical section ordering.
    fn release_world(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
    ) -> Result<(bool, Option<LiveWorld>), EdgeError> {
        let released = self.registry.release_world(project, target)?;
        let world = self.take_world(project, target);
        Ok((released, world))
    }

    fn release_only(&self, project: &ProjectSlug, target: &WorldTarget) -> Result<bool, EdgeError> {
        self.release_world(project, target)
            .map(|(released, _)| released)
    }

    /// Reject a request for a world this daemon does not hold.
    fn unknown_world(project: &ProjectSlug, target: &WorldTarget) -> EdgeError {
        rejected(format!(
            "world '{}' is not running in this slipforkd; run `slipfork up` first",
            world_id(project, target)
        ))
    }
}

/// Map one engine failure to a secret-free runtime diagnostic.
fn runtime_error(message: impl Into<String>) -> EdgeError {
    EdgeError::Control(DaemonError::new(DaemonErrorKind::Internal, message))
}

/// Open (truncate) one hook's `0600` retained log for a fresh run.
fn open_hook_log(path: &Path) -> Result<File, EdgeError> {
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|err| runtime_error(format!("cannot write {}: {err}", path.display())))
}

/// The `forward` targets a world exposes: declared resources plus enabled component ports.
fn forward_specs(
    snow: &SnowFile,
    world: &str,
    enabled: &BTreeSet<String>,
    requested: &[String],
) -> Result<Vec<ForwardTargetSpec>, EdgeError> {
    let mut specs: Vec<ForwardTargetSpec> = Vec::new();
    for db in &snow.resources.pg_databases {
        specs.push(ForwardTargetSpec {
            id: format!("pg_databases.{}", db.id),
            port: 5432,
            scheme: "postgres".to_owned(),
            path: format!("/{}", db.id),
        });
    }
    for cluster in &snow.resources.redis_clusters {
        specs.push(ForwardTargetSpec {
            id: format!("redis_clusters.{}", cluster.id),
            port: 6379,
            scheme: "redis".to_owned(),
            path: String::new(),
        });
    }
    for bucket in &snow.resources.s3_buckets {
        specs.push(ForwardTargetSpec {
            id: format!("s3_buckets.{}", bucket.id),
            // `ADR-0009` §2 pins the SeaweedFS container to `-port=9000` on the world loopback;
            // `snow.yml` carries no per-bucket S3 port to read.
            port: 9000,
            scheme: "http".to_owned(),
            path: String::new(),
        });
    }
    for component in &snow.components {
        if !enabled.contains(&component.id) {
            continue;
        }
        if let Some(port) = component.internal_ports.first() {
            specs.push(ForwardTargetSpec {
                id: format!("components.{}", component.id),
                port: *port,
                scheme: "http".to_owned(),
                path: String::new(),
            });
        }
    }
    if requested.is_empty() {
        if specs.is_empty() {
            return Err(rejected(format!(
                "world '{world}' has no forwardable target: declare snow.yml resources or component internal_ports"
            )));
        }
        return Ok(specs);
    }
    let mut selected = Vec::with_capacity(requested.len());
    for id in requested {
        let spec = specs.iter().find(|spec| spec.id == *id).ok_or_else(|| {
            let known = specs
                .iter()
                .map(|spec| spec.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            rejected(format!(
                "world '{world}' has no forward target '{id}' (known targets: {known})"
            ))
        })?;
        if !selected
            .iter()
            .any(|existing: &ForwardTargetSpec| existing.id == *id)
        {
            selected.push(spec.clone());
        }
    }
    Ok(selected)
}

/// Map one invalid request to a typed rejection (the message is the whole diagnostic).
fn rejected(message: impl Into<String>) -> EdgeError {
    EdgeError::Control(DaemonError::new(DaemonErrorKind::Rejected, message))
}

/// Resolve one `snow.yml` resource id to the running service handle id and the argv to run.
///
/// `pg_databases.main` selects the `pg_clusters.<cluster>` container and appends `-d main` to
/// `psql`; `redis_clusters.cache` is its own service. Nothing ever falls back to the app rootfs.
fn resolve_service_target(
    snow: &SnowFile,
    requested: &str,
    argv: &[String],
) -> Result<(String, Vec<String>), EdgeError> {
    if requested.trim().is_empty() {
        return Err(rejected("a backing-service id must not be empty"));
    }
    let (kind, id) = requested.split_once('.').ok_or_else(|| {
        rejected(format!(
            "invalid backing-service id '{requested}' (expected <kind>.<id>, for example pg_databases.main)"
        ))
    })?;
    match kind {
        "pg_databases" => {
            let database = snow
                .resources
                .pg_databases
                .iter()
                .find(|database| database.id == id)
                .ok_or_else(|| unknown_service(snow, requested))?;
            let mut argv = argv.to_vec();
            if argv.first().map(String::as_str) == Some("psql")
                && !argv.iter().any(|arg| arg == "-d" || arg == "--dbname")
            {
                argv.push("-d".to_owned());
                argv.push(database.id.clone());
            }
            Ok((format!("pg_clusters.{}", database.cluster), argv))
        }
        "redis_clusters" => {
            if !snow
                .resources
                .redis_clusters
                .iter()
                .any(|cluster| cluster.id == id)
            {
                return Err(unknown_service(snow, requested));
            }
            Ok((requested.to_owned(), argv.to_vec()))
        }
        "pg_clusters" => {
            if !snow
                .resources
                .pg_clusters
                .iter()
                .any(|cluster| cluster.id == id)
            {
                return Err(unknown_service(snow, requested));
            }
            Ok((requested.to_owned(), argv.to_vec()))
        }
        _ => Err(unknown_service(snow, requested)),
    }
}

/// `ServiceNotRunning` diagnostic naming the world and the stopped container.
fn service_not_running(world: &str, service: &str) -> EdgeError {
    EdgeError::Control(DaemonError::new(
        DaemonErrorKind::ServiceNotRunning,
        format!(
            "world '{world}' has no running service '{service}' (ServiceNotRunning): psql and \
             redis-cli run only inside the service container — run `slipfork up` and retry"
        ),
    ))
}

/// `ServiceNotRunning` remains a typed daemon error; every other engine failure is internal.
fn service_engine_error(world: &str, service: &str, error: EngineError) -> EdgeError {
    match error {
        EngineError::ServiceNotRunning { .. } => service_not_running(world, service),
        other => runtime_error(other.to_string()),
    }
}

fn unknown_service(snow: &SnowFile, requested: &str) -> EdgeError {
    let mut known: Vec<String> = snow
        .resources
        .pg_databases
        .iter()
        .map(|database| format!("pg_databases.{}", database.id))
        .chain(
            snow.resources
                .redis_clusters
                .iter()
                .map(|cluster| format!("redis_clusters.{}", cluster.id)),
        )
        .collect();
    known.sort();
    rejected(format!(
        "world declares no backing service '{requested}' (known services: {})",
        if known.is_empty() {
            "none".to_owned()
        } else {
            known.join(", ")
        }
    ))
}

fn dump_policy(request: slipfork_edge::DumpRequest) -> DumpPolicy {
    match request {
        slipfork_edge::DumpRequest::Default => DumpPolicy::Default,
        slipfork_edge::DumpRequest::NoDump => DumpPolicy::NoDump,
        slipfork_edge::DumpRequest::RefreshDump => DumpPolicy::RefreshDump,
    }
}

/// Resolve `[resources]` through the single `ResourcePlan::resolve` decision point.
fn resolve_plan(
    config: &SlipforkConfig,
    snow: &SnowFile,
    target: &WorldTarget,
    dump: DumpPolicy,
    force_local: bool,
    offer: Option<&RemoteResourceOffer>,
) -> Result<ResourcePlan, EdgeError> {
    let s3_ids: Vec<&str> = snow
        .resources
        .s3_buckets
        .iter()
        .map(|bucket| bucket.id.as_str())
        .collect();
    let pg_ids: Vec<&str> = snow
        .resources
        .pg_databases
        .iter()
        .map(|database| database.id.as_str())
        .collect();
    config
        .resources
        .validate_declared_ids(&s3_ids, &pg_ids)
        .map_err(|err| runtime_error(err.to_string()))?;

    // `--resources local` is a per-invocation override (`ADR-0008` §3): the typed forced-strategy
    // spec keeps `ResourcePlan::resolve` the sole decision point while no seed dumps are used and
    // no OSPP offer is consulted.
    let spec = config.resources.with_declared_ids(&s3_ids, &pg_ids);
    let (spec, dump) = if force_local {
        if dump == DumpPolicy::RefreshDump {
            return Err(rejected(
                "`--resources local` cannot be combined with `--refresh-dump`",
            ));
        }
        (
            spec.forced_strategy(ResourceStrategy::Local),
            DumpPolicy::NoDump,
        )
    } else {
        (spec, dump)
    };
    ResourcePlan::resolve(&spec, ResourceMode::from(target), dump, offer)
        .map_err(|err| runtime_error(err.to_string()))
}

/// Render every `[resources]` decision as the typed `up` / `fork` diagnostic row.
fn plan_summaries(plan: &ResourcePlan) -> Vec<ResourceSummary> {
    plan.diagnostics()
        .into_iter()
        .map(|diagnostic| ResourceSummary {
            id: diagnostic.id,
            strategy: json_label(&diagnostic.strategy),
            resolved: json_label(&diagnostic.resolved),
            reason: json_label(&diagnostic.reason),
        })
        .collect()
}

/// The serde `snake_case` label of a domain enum (the single wire spelling).
fn json_label<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Content key of one base snapshot: backing-service versions plus every resolved dump digest.
fn base_digest(project: &ProjectSlug, snow: &SnowFile, plan: &ResourcePlan) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"slipfork-base:v1\0");
    hasher.update(project.as_str().as_bytes());
    hasher.update(b"\0");
    for image in requested_service_images(snow) {
        hasher.update(image.to_string().as_bytes());
        hasher.update(b"\0");
    }
    for database in &plan.pg_databases {
        hasher.update(database.id.as_bytes());
        hasher.update(b"=");
        match &database.seed {
            PgSeed::CleanInitdb => hasher.update(b"clean"),
            PgSeed::RemoteDump { dump, .. } => hasher.update(dump.sha256_compressed().as_bytes()),
        }
        hasher.update(b"\0");
    }
    hex::encode(hasher.finalize())
}

/// The verified `<sha256>.pgcustom` files one `pg_clusters.<id>` container mounts read-only.
fn dump_mounts(
    snow: &SnowFile,
    dumps: &BTreeMap<String, (String, PathBuf)>,
) -> BTreeMap<String, Vec<WorldMount>> {
    let mut mounts: BTreeMap<String, Vec<WorldMount>> = BTreeMap::new();
    for (database_id, (sha256, pgcustom)) in dumps {
        let Some(database) = snow
            .resources
            .pg_databases
            .iter()
            .find(|database| database.id == *database_id)
        else {
            continue;
        };
        mounts
            .entry(format!("pg_clusters.{}", database.cluster))
            .or_default()
            .push(WorldMount {
                host_path: pgcustom.clone(),
                guest_path: PathBuf::from("/dumps").join(format!("{sha256}{PGCUSTOM_SUFFIX}")),
                mode: MountMode::ReadOnly,
            });
    }
    mounts
}

/// A `snow.yml` database id is interpolated into `psql` SQL, so it stays a plain identifier.
fn validate_database_id(id: &str) -> Result<(), EdgeError> {
    if id.is_empty()
        || id.len() > 63
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Err(rejected(format!(
            "invalid pg_databases id '{id}': expected 1..=63 [A-Za-z0-9_-] characters"
        )));
    }
    Ok(())
}

/// The backing-service OCI images `snow.yml` `resources:` requests (`ADR-0009` §2.2).
fn requested_service_images(snow: &SnowFile) -> Vec<BackingServiceKey> {
    let mut images = BTreeSet::new();
    for cluster in &snow.resources.pg_clusters {
        images.insert(BackingServiceKey::Postgres {
            version: cluster.version.clone(),
        });
    }
    for cluster in &snow.resources.redis_clusters {
        images.insert(BackingServiceKey::Redis {
            version: cluster.version.clone(),
        });
    }
    if !snow.resources.s3_buckets.is_empty() {
        images.insert(BackingServiceKey::SeaweedFs);
    }
    images.into_iter().collect()
}

/// One `ServiceSpec` per declared backing service, all on the single `WorldEngine` supervision
/// surface (`ADR-0009` §2–§3).
///
/// `dump_mounts` maps a `pg_clusters.<id>` service id to the verified `<sha256>.pgcustom` files the
/// base seed world must expose read-only at `/dumps/...`.
fn service_specs(
    snow: &SnowFile,
    rootfs: &WorldRootfs,
    dump_mounts: &BTreeMap<String, Vec<WorldMount>>,
) -> Result<Vec<ServiceSpec>, EdgeError> {
    let mut specs = Vec::new();
    for cluster in &snow.resources.pg_clusters {
        let image = BackingServiceKey::Postgres {
            version: cluster.version.clone(),
        };
        let id = format!("pg_clusters.{}", cluster.id);
        require_image(rootfs, &image, &id)?;
        ensure_container_dirs(rootfs, &image, &["/var/lib/postgresql/data"])?;
        let readonly_mounts = dump_mounts.get(&id).cloned().unwrap_or_default();
        for mount in &readonly_mounts {
            ensure_container_file(rootfs, &image, &mount.guest_path)?;
        }
        specs.push(ServiceSpec {
            id: id.clone(),
            runtime: ServiceRuntime::OciContainer {
                image,
                argv: vec!["docker-entrypoint.sh".to_owned(), "postgres".to_owned()],
                state_mounts: vec![StateSubdirMount {
                    host_subdir: PathBuf::from("pgdata").join(&cluster.id),
                    container_path: PathBuf::from("/var/lib/postgresql/data"),
                }],
                readonly_mounts,
            },
            env: container_env(
                "/tmp",
                &[
                    ("LC_ALL", "C.UTF-8"),
                    ("PGDATA", "/var/lib/postgresql/data"),
                    ("POSTGRES_HOST_AUTH_METHOD", "trust"),
                ],
                &format!(
                    "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/lib/postgresql/{}/bin",
                    cluster.version
                ),
            )?,
            readiness_port: Some(5432),
        });
    }
    for cluster in &snow.resources.redis_clusters {
        let image = BackingServiceKey::Redis {
            version: cluster.version.clone(),
        };
        require_image(rootfs, &image, &format!("redis_clusters.{}", cluster.id))?;
        ensure_container_dirs(rootfs, &image, &["/data"])?;
        specs.push(ServiceSpec {
            id: format!("redis_clusters.{}", cluster.id),
            runtime: ServiceRuntime::OciContainer {
                image,
                argv: vec![
                    "docker-entrypoint.sh".to_owned(),
                    "redis-server".to_owned(),
                    "--dir".to_owned(),
                    "/data".to_owned(),
                ],
                state_mounts: vec![StateSubdirMount {
                    host_subdir: PathBuf::from("redis").join(&cluster.id),
                    container_path: PathBuf::from("/data"),
                }],
                readonly_mounts: Vec::new(),
            },
            env: container_env(
                "/data",
                &[("LC_ALL", "C")],
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )?,
            readiness_port: Some(6379),
        });
    }
    if !snow.resources.s3_buckets.is_empty() {
        let image = BackingServiceKey::SeaweedFs;
        require_image(rootfs, &image, "s3_buckets")?;
        ensure_container_dirs(rootfs, &image, &["/data"])?;
        specs.push(ServiceSpec {
            id: "s3_buckets".to_owned(),
            runtime: ServiceRuntime::OciContainer {
                image,
                argv: vec![
                    "weed".to_owned(),
                    "server".to_owned(),
                    "-dir=/data".to_owned(),
                    // The world netns carries no DNS resolver, so `weed` must never resolve
                    // `localhost` for its master/filer/S3 bind addresses.
                    "-ip=127.0.0.1".to_owned(),
                    "-ip.bind=127.0.0.1".to_owned(),
                    "-s3".to_owned(),
                    "-s3.port=9000".to_owned(),
                    "-s3.ip.bind=127.0.0.1".to_owned(),
                ],
                state_mounts: vec![StateSubdirMount {
                    host_subdir: PathBuf::from("s3"),
                    container_path: PathBuf::from("/data"),
                }],
                readonly_mounts: Vec::new(),
            },
            env: container_env(
                "/data",
                &[],
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )?,
            readiness_port: Some(9000),
        });
    }
    Ok(specs)
}

/// Create the bind-mount destinations a backing-service image declares as volumes.
///
/// `docker export` omits `VOLUME` contents, so the extracted read-only rootfs may lack the
/// directory itself. The engine refuses to invent a destination; the Composition Root prepares it
/// once in the content-addressed cache before the container starts.
fn ensure_container_dirs(
    rootfs: &WorldRootfs,
    image: &BackingServiceKey,
    paths: &[&str],
) -> Result<(), EdgeError> {
    let image = rootfs.service_images.get(image).ok_or_else(|| {
        runtime_error(format!(
            "backing-service image '{image}' is not materialized; re-run `slipfork up`"
        ))
    })?;
    for path in paths {
        let host = image.rootfs.join(path.trim_start_matches('/'));
        std::fs::create_dir_all(&host)
            .map_err(|err| runtime_error(format!("cannot create {}: {err}", host.display())))?;
    }
    Ok(())
}

/// Create the parent directory and an empty destination file a read-only file mount needs.
///
/// `docker export` never carries the `/dumps/<sha256>.pgcustom` path, and `crun` requires the
/// bind-mount destination to exist in the image rootfs.
fn ensure_container_file(
    rootfs: &WorldRootfs,
    image: &BackingServiceKey,
    path: &Path,
) -> Result<(), EdgeError> {
    let image = rootfs.service_images.get(image).ok_or_else(|| {
        runtime_error(format!(
            "backing-service image '{image}' is not materialized; re-run `slipfork up`"
        ))
    })?;
    let host = image.rootfs.join(path.strip_prefix("/").unwrap_or(path));
    if let Some(parent) = host.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| runtime_error(format!("cannot create {}: {err}", parent.display())))?;
    }
    if !host.exists() {
        File::create(&host)
            .map_err(|err| runtime_error(format!("cannot create {}: {err}", host.display())))?;
    }
    Ok(())
}

fn require_image(
    rootfs: &WorldRootfs,
    image: &BackingServiceKey,
    resource: &str,
) -> Result<(), EdgeError> {
    if rootfs.service_images.contains_key(image) {
        return Ok(());
    }
    Err(runtime_error(format!(
        "resource '{resource}' needs backing-service image '{image}', but the materialized rootfs \
         carries no OCI image for it; re-run `slipfork up` so the L0 builder extracts it"
    )))
}

fn container_env(home: &str, vars: &[(&str, &str)], path: &str) -> Result<ExecEnv, EdgeError> {
    let mut env = ExecEnv::world_scoped(home);
    env.inherit(InheritedVar::Path, path);
    // `LANG` is a closed inherited variable, so an OCI image without the host locale gets `C`.
    env.inherit(
        InheritedVar::Lang,
        if home == "/data" { "C" } else { "C.UTF-8" },
    );
    for (name, value) in vars {
        env.set(name, *value)
            .map_err(|err| runtime_error(err.to_string()))?;
    }
    Ok(env)
}

/// OSPP-offered S3 coordinates are the only ones that override the world-local endpoint.
fn s3_overrides(plan: &ResourcePlan) -> BTreeMap<String, S3Coordinates> {
    plan.s3_buckets
        .iter()
        .filter_map(|bucket| match &bucket.resolution {
            S3Resolution::Remote(remote) => Some((
                bucket.id.clone(),
                S3Coordinates {
                    endpoint_url: remote.endpoint_url().to_owned(),
                    bucket: remote.bucket().to_owned(),
                    access_key: remote.access_key().to_owned(),
                    secret_key: remote.secret_key().to_owned(),
                    region: remote.region().to_owned(),
                },
            )),
            S3Resolution::Local => None,
        })
        .collect()
}

/// Build the `IngressRoute` prefix table: declared `snow.yml` ingresses plus the canonical
/// empty-prefix catch-all from the first enabled component's first `internal_ports` entry.
fn ingress_routes(
    snow: &SnowFile,
    effective: &[(String, slipfork_snow::ResolvedComponent)],
) -> Result<Vec<IngressRoute>, EdgeError> {
    let mut routes: Vec<IngressRoute> = snow
        .ingresses
        .iter()
        .map(|ingress| IngressRoute {
            prefix: ingress.match_rule.prefix.clone(),
            port: ingress.target.port,
        })
        .collect();
    if !routes.iter().any(|route| route.prefix.is_empty()) {
        for (id, resolved) in effective {
            if resolved.disabled {
                continue;
            }
            if let Some(port) = snow
                .components
                .iter()
                .find(|component| component.id == *id)
                .and_then(|component| component.internal_ports.first())
            {
                routes.push(IngressRoute {
                    prefix: String::new(),
                    port: *port,
                });
                break;
            }
        }
    }
    if routes.is_empty() {
        return Err(rejected(format!(
            "world has no HTTP ingress: declare `ingresses:` in snow.yml or give a component an \
             `internal_ports` entry (mode '{WORLD_MODE}')"
        )));
    }
    Ok(routes)
}

fn apply_env(env: &mut ExecEnv, resolved: ResolvedEnvMap) -> Result<(), EdgeError> {
    for (key, value) in resolved {
        let outcome = if value.is_secret() {
            env.set_secret(&key, value.expose())
        } else {
            env.set(&key, value.expose())
        };
        outcome
            .map_err(|err| runtime_error(format!("invalid world environment '{key}': {err}")))?;
    }
    Ok(())
}

/// Parent directory of an explicit manifest path (`"."` for a bare file name).
fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn validate_secret_key(key: &str) -> Result<(), EdgeError> {
    if key.is_empty()
        || key
            .chars()
            .any(|character| character.is_control() || character == '=')
    {
        return Err(rejected(format!(
            "invalid secret key '{key}': keys are non-empty printable names without '='"
        )));
    }
    Ok(())
}

fn hook_phase_label(phase: HookPhase) -> &'static str {
    match phase {
        HookPhase::PreDeploy => "pre-deploy",
        HookPhase::PostDeploy => "post-deploy",
        HookPhase::FailedDeploy => "failed-deploy",
    }
}

/// Resolve one `<component>:<phase>` hook through the single validation path used by both
/// interactive `hook run` (entry argv) and `--no-tty` streaming.
fn resolve_hook(
    snow: &SnowFile,
    enabled: &BTreeSet<String>,
    component_envs: &BTreeMap<String, ExecEnv>,
    hook_id: &str,
) -> Result<ResolvedHook, EdgeError> {
    let (component, phase) = parse_hook_id(hook_id).ok_or_else(|| {
        rejected(format!(
            "invalid hook id '{hook_id}' (expected <component>:<phase>)"
        ))
    })?;
    if !enabled.contains(component) {
        return Err(rejected(format!(
            "component '{component}' is not enabled in mode '{WORLD_MODE}'"
        )));
    }
    let hook = snow
        .hooks
        .iter()
        .find(|hook| hook.component == component && hook.when == phase)
        .ok_or_else(|| rejected(format!("world has no hook '{hook_id}'")))?;
    if hook.run_command.trim().is_empty() {
        return Err(rejected(format!(
            "hook '{hook_id}' declares an empty run_command"
        )));
    }
    Ok(ResolvedHook {
        argv: vec![
            "sh".to_owned(),
            "-eu".to_owned(),
            "-c".to_owned(),
            hook.run_command.clone(),
        ],
        overlay: component_envs.get(component).cloned().unwrap_or_default(),
        phase_label: hook_phase_label(phase),
    })
}

fn parse_hook_id(hook: &str) -> Option<(&str, HookPhase)> {
    let (component, phase) = hook.rsplit_once(':')?;
    let phase = match phase {
        "pre-deploy" => HookPhase::PreDeploy,
        "post-deploy" => HookPhase::PostDeploy,
        "failed-deploy" => HookPhase::FailedDeploy,
        _ => return None,
    };
    (!component.is_empty()).then_some((component, phase))
}

/// Keep the last `lines` lines of the retained stdout/stderr selection.
fn tail_lines(chunks: Vec<StreamChunk>, lines: Option<usize>) -> Vec<StreamChunk> {
    let Some(lines) = lines else {
        return chunks;
    };
    let joined: Vec<u8> = chunks
        .iter()
        .flat_map(|chunk| chunk.bytes.iter().copied())
        .collect();
    let text = String::from_utf8_lossy(&joined);
    let kept: Vec<&str> = text
        .lines()
        .rev()
        .take(lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let mut bytes = kept.join("\n").into_bytes();
    if !bytes.is_empty() {
        bytes.push(b'\n');
    }
    let source = chunks
        .first()
        .map_or(slipfork_engine::StreamSource::Stdout, |chunk| chunk.source);
    vec![StreamChunk { source, bytes }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_ids_round_trip_every_phase() {
        assert_eq!(
            parse_hook_id("api:pre-deploy"),
            Some(("api", HookPhase::PreDeploy))
        );
        assert_eq!(
            parse_hook_id("api:post-deploy"),
            Some(("api", HookPhase::PostDeploy))
        );
        assert_eq!(
            parse_hook_id("api:failed-deploy"),
            Some(("api", HookPhase::FailedDeploy))
        );
        assert_eq!(parse_hook_id("api"), None);
        assert_eq!(parse_hook_id("api:unknown"), None);
        assert_eq!(parse_hook_id(":pre-deploy"), None);
    }

    #[test]
    fn secret_keys_reject_separators_and_control_characters() {
        assert!(validate_secret_key("STRIPE_SECRET_KEY").is_ok());
        assert!(validate_secret_key("").is_err());
        assert!(validate_secret_key("A=B").is_err());
        assert!(validate_secret_key("A\nB").is_err());
    }

    #[test]
    fn tail_lines_keeps_the_requested_tail() {
        let chunk = StreamChunk {
            source: slipfork_engine::StreamSource::Stdout,
            bytes: b"one\ntwo\nthree\n".to_vec(),
        };
        let out = tail_lines(vec![chunk], Some(2));
        assert_eq!(out.len(), 1);
        assert_eq!(String::from_utf8_lossy(&out[0].bytes), "two\nthree\n");
    }
}
