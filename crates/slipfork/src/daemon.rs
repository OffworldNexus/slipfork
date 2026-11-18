//! The in-process [`DaemonControl`] implementation `slipforkd` injects into its server.
//!
//! Every collaborator is wired by [`compose_daemon`], the one composition function `slipforkd` and
//! the integration harness both call: the CoW engine over the concrete [`KernelBtrfsDriver`], the
//! HTTP ingress `WorldRegistry`, the VCS repository resolver and workspace factory, the explicit
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
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use sha2::{Digest, Sha256};

use slipfork_dag::{
    DumpPolicy, PgSeed, ResourceMode, ResourcePlan, ResourceStrategy, SlipforkConfig,
};
use slipfork_edge::forward::ForwardTargetSpec;
use slipfork_edge::{
    DaemonControl, EdgeError, IngressRoute, WorldRegistry, parent_world, world_id,
};
use slipfork_engine::btrfs::KernelBtrfsDriver;
use slipfork_engine::rootfs::{
    BackingServiceKey, BaseImageBuilder, BuildEnvVar, DockerBuildx, DockerImageStore,
    DockerfileStageBuilder, ElfProbe, FsInputHasher, HostCacheRequest, RootfsBuildEvent,
    RuntimeAssets, SnowPortContract, WorldRootfs, WorldRootfsRequest,
};
use slipfork_engine::{
    BaseLineage, BootLineage, BtrfsEngine, EngineError, ExecEnv, ExecOutput, InheritedVar,
    LocalIngressBridge, LogChunk, MountMode, SeedLineage, ServiceHandle, ServiceKind,
    ServiceRuntime, ServiceSpec, ServiceState, SnapshotId, StateSubdirMount, StreamChunk,
    WorldEngine, WorldHandle, WorldMount, WorldSpec, elapsed_millis,
};
use slipfork_ospp::{
    DeviceKeyPath, DeviceKeySigner, FileDeviceKeyStore, OsppClaims, OsppClient, OsppEndpoint,
    OsppSession, OsppTimeouts, SoftwareP256Key, SoftwareP256Signer, SubscribeRequest,
    SubscriptionEnv, WsOsppClient,
};
use slipfork_snow::{
    ClaimedEvaluator, EntropySource, HookPhase, Image, OsCsprngEntropy, ResolvedEnvMap,
    ResourceCoordinates, SecretProvider, SecretValue, SnowError, SnowFile,
};
use slipfork_vcs::{
    RepoRoot, RepoRootResolver, VcsWorkspace, VcsWorkspaceFactory, WORKTREES_DIRECTORY,
    WorkspaceRequest, sanitize_fork_name,
};
use slipfork_wire::control::{
    BootEvent, DaemonError, DaemonErrorKind, DaemonStatus, DropForkRequest, ExecTargetRequest,
    ForkRequest, ForwardRequest, ForwardSummary, HookRunRequest, IngressInfo, LogsRequest,
    PruneReport, ResourceSummary, SnowResolveRequest, SnowResolveSummary, UpRequest,
    WorkspaceRefsRequest, WorkspaceRefsSummary, WorldExecTarget, WorldProcessReport, WorldSelector,
    WorldServiceSummary, WorldState, WorldSummary, WorldUpSummary,
};
use slipfork_world::{LocalHostRoute, ProjectSlug, WorldTarget, validate_fork_name};

use slipfork_edge::SecretVault;

use crate::browser::{APPROVAL_HINT_PREFIX, Browser};
use crate::layout::{Layout, ensure_private_dir};
use crate::project::{load_optional_slipfork_config, repository_resolver};
use crate::status::world_next_commands;
use crate::vaults::{LayoutProjectVaults, ProjectVaults};
use crate::vcs::RepositoryWorkspaces;

/// The world mode this daemon supervises (`snow.yml` `modes.local`, `ADR-0001` §1).
const WORLD_MODE: &str = "local";

/// Bound on the postmaster answering `pg_isready` after its container accepts `:5432`.
///
/// `postgres:18` binds the port milliseconds before postmaster leaves `PM_STARTUP`, so the
/// engine's TCP-accept readiness probe is not server readiness: the first `psql` must wait for
/// this server-side gate instead of racing crash recovery.
const POSTGRES_READY_DEADLINE: Duration = Duration::from_secs(30);
/// Interval between `pg_isready` probes while the postmaster finishes startup.
const POSTGRES_READY_POLL: Duration = Duration::from_millis(100);

/// Injected postmaster-readiness policy for the base seed world's `postgres:18` server.
///
/// [`compose_daemon`] defaults it once from [`POSTGRES_READY_DEADLINE`]/[`POSTGRES_READY_POLL`];
/// the gate measures its own start [`std::time::Instant`], so no clock is injected and no test
/// needs to wait 30 real seconds to exercise the deadline.
struct PostgresReady {
    /// Total time the postmaster gets to accept `pg_isready`.
    deadline: Duration,
    /// Interval between probes while the postmaster finishes startup.
    poll: Duration,
}

impl PostgresReady {
    /// Production bounds for the postmaster-readiness gate.
    fn production() -> Self {
        Self {
            deadline: POSTGRES_READY_DEADLINE,
            poll: POSTGRES_READY_POLL,
        }
    }
}

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
struct DaemonConfig {
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
    pub postgres_ready: PostgresReady,
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

/// One live world owned by this daemon.
///
/// Two groups of fields, and only the first is frozen while the handle lives:
///
/// - **Frozen until a reload re-roots the world:** `worktree`, `handle` (and the CoW state behind
///   it), `rootfs`, `policy`, `workspace`, `hook_logs`, and `project`/`target`/`repo`. A `restart`
///   keeps the handle and the subvolume (so backing-service data survives) and only re-clones that
///   subvolume when the resolved base snapshot changed; it never discards the worktree.
/// - **Re-derived by every pipeline run** (`resolve_world`): `snow`, `resources`, `coords`,
///   `base_env`, `components`, and the registry's ingress/domain table, so an edited `snow.yml` is
///   what the world actually runs after a `restart`.
///
/// `services` holds the spawn handles this daemon needs for the entry/log paths only; the engine's
/// `world_processes` rows remain the authoritative record of what is running.
struct LiveWorld {
    project: ProjectSlug,
    target: WorldTarget,
    repo: RepoRoot,
    worktree: PathBuf,
    handle: WorldHandle,
    state: WorldState,
    /// Resolved world environment (re-derived by `resolve_world`).
    base_env: ExecEnv,
    /// The one enabled-component list: id, run command, readiness port, and environment
    /// (re-derived by `resolve_world`; `start_world` and the hook paths never re-resolve it).
    components: Vec<ResolvedComponent>,
    /// The policy this world was booted (or last re-run) with; a later `up` may not silently ask
    /// for a different dump policy or rootfs build on top of the retained CoW state.
    policy: BootPolicy,
    /// Resources coordinates for `resources::*`/`deployment::*` values, re-derived with the plan.
    coords: ResourceCoordinates,
    snow: SnowFile,
    /// The resource plan this run resolved (kept so `env_updated` can re-derive ID-only claims).
    plan: ResourcePlan,
    /// The last decrypted server environment applied to this world (never persisted).
    ospp_env: Option<BTreeMap<String, BTreeMap<String, SecretValue>>>,
    /// The `L0` rootfs this world is currently rooted in: `restart` replaces it (and its
    /// subvolume, when the base changed) only when the resolver's digest changed.
    rootfs: WorldRootfs,
    resources: Vec<ResourceSummary>,
    services: Vec<ServiceHandle>,
    /// Retained hook logs by `<component>:<phase>` (`slipfork logs <hook_id>`, `ADR-0008` §4).
    hook_logs: BTreeMap<String, Arc<HookLog>>,
    workspace: Option<Arc<dyn VcsWorkspace>>,
}

/// The one daemon-held `ospp-v1` session and its stable per-project subscriptions.
#[derive(Default)]
struct OsppRuntime {
    session: Option<Arc<dyn OsppSession>>,
    /// Subscription id -> project. One subscription per project while its worlds run.
    subscriptions: BTreeMap<String, ProjectSlug>,
}

/// Stable per-project subscription id, reused on every re-`subscribe`.
fn subscription_id(project: &ProjectSlug) -> String {
    format!("sub-{}", project.as_str())
}

/// The daemon's own record of one held world, copied out of the index so engine reads never run
/// under the state mutex.
struct WorldFacts {
    state: WorldState,
    worktree: PathBuf,
    resources: Vec<ResourceSummary>,
    handle: WorldHandle,
}

impl WorldFacts {
    fn of(world: &LiveWorld) -> Self {
        Self {
            state: world.state,
            worktree: world.worktree.clone(),
            resources: world.resources.clone(),
            handle: world.handle.clone(),
        }
    }
}

/// One `slipfork logs` target: an engine-retained service log or a daemon-retained hook log.
enum RetainedTarget {
    Service(String),
    Hook(Arc<HookLog>),
}

/// Size-capped retained output plus its on-disk log for one hook run.
///
/// Mirrors the engine's `ServiceLogs` retention so `slipfork logs <hook_id> --follow` behaves like
/// a service log, while [`HookLog::path`] gives the rescue block a real file to point at. The
/// `target` is the hook id the record was minted for, so `slipfork logs` attributes every chunk to
/// its own hook record instead of the caller's request string.
struct HookLog {
    target: String,
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
    fn new(target: String, path: PathBuf) -> Self {
        Self {
            target,
            path,
            state: Mutex::new(HookLogState::default()),
            changed: Condvar::new(),
        }
    }

    fn target(&self) -> &str {
        &self.target
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

/// One sealed base: the snapshot a world clones from plus the lineage a later world reuses.
///
/// `lineage` carries the **reuse** facts (base `millis: None`, seed `rebuilt: Some(false)`): the
/// seal duration belongs only to the world whose boot actually performed the seal.
#[derive(Debug, Clone)]
struct SealedBase {
    snapshot: SnapshotId,
    lineage: BootLineage,
}

/// Mutable daemon index, mutated only in short critical sections (never across engine I/O).
#[derive(Default)]
struct DaemonState {
    worlds: Vec<LiveWorld>,
    /// Base state snapshot created on first use per `(project, base digest)` in this daemon.s
    /// lifetime, with the `BootLineage` a later world reuses (base `reused`, seed `cached`). The
    /// digest covers backing-service versions and every resolved seed-dump digest, so `--no-dump`
    /// and a restored dump never share a base.
    bases: BTreeMap<(ProjectSlug, String), SealedBase>,
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

/// The minimal world-id round trip (`ADR-0005` §3, 0.1.0 subset; the typed `WorldId` lands in
/// 0.2.0): accept exactly the forms `slipfork list` prints.
///
/// - `<project>` selects the canonical world of `repo`.
/// - `<project>:<fork>` selects that fork, and the project must be `repo.project`.
/// - bare `<fork>` selects the fork in `repo` (repo-local CLI ergonomics).
///
/// A foreign project prefix, more than one `':'`, an empty half, or an invalid fork label fails
/// closed with a typed `Rejected` diagnostic.
fn resolve_world_selector(raw: &str, repo: &RepoRoot) -> Result<WorldTarget, EdgeError> {
    let selector = raw.trim();
    if selector.is_empty() {
        return Err(rejected(
            "a world selector must not be empty; pass `<fork>`, `<project>:<fork>`, or `<project>`",
        ));
    }
    match selector.split_once(':') {
        Some((project, fork)) => {
            if fork.contains(':') {
                return Err(rejected(format!(
                    "invalid world selector '{raw}': expected at most one ':' (`<project>:<fork>`)"
                )));
            }
            let project = ProjectSlug::parse(project).map_err(|err| rejected(err.to_string()))?;
            if project != repo.project {
                return Err(rejected(format!(
                    "world '{raw}' belongs to project '{project}', while '{}' belongs to project '{}'; cd into '{project}' or pass a world id printed by `slipfork list` for '{}'",
                    repo.worktree_root.display(),
                    repo.project,
                    repo.project
                )));
            }
            validate_fork_name(fork, &[]).map_err(|err| rejected(err.to_string()))?;
            Ok(WorldTarget::UserFork(fork.to_owned()))
        }
        None if selector == repo.project.as_str() => Ok(WorldTarget::Canonical),
        None => {
            validate_fork_name(selector, &[]).map_err(|err| rejected(err.to_string()))?;
            Ok(WorldTarget::UserFork(selector.to_owned()))
        }
    }
}

/// The two verbs that run the one boot pipeline (`docs/design/dx-0.1.0-ux.md` §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BootCommand {
    /// `slipfork up`: boot an absent world, confirm a `Running` one, rescue a held one in place.
    Up,
    /// `slipfork restart`: re-run a world this daemon holds; never boot what was not running.
    Restart,
}

/// What one verb must do to the selected world (`docs/design/dx-0.1.0-ux.md` §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BootVerb {
    /// Already `Running`: report it, touching nothing (`changed: false`).
    Confirm,
    /// No world here: run the boot pipeline from scratch (`changed: true`).
    Boot,
    /// A world this daemon holds: stop its services, then run the same pipeline over its handle
    /// (the `restart` path and the `up` rescue path, `changed: true`).
    Rerun,
}

/// The single verb decision `up` and `restart` obey; `None` means the verb refuses the request.
///
/// `up` boots an absent world and confirms a `Running` one without touching it, while `restart`
/// only ever re-runs a world this daemon already holds — a never-booted or stopped world has no
/// components or `crun` services to stop, so `restart` refuses rather than silently booting it.
fn boot_verb(command: BootCommand, state: Option<WorldState>) -> Option<BootVerb> {
    match (command, state) {
        (BootCommand::Up, None) => Some(BootVerb::Boot),
        (BootCommand::Up, Some(WorldState::Running)) => Some(BootVerb::Confirm),
        (BootCommand::Up, Some(_)) => Some(BootVerb::Rerun),
        // A held, running or failed world is re-run in place (`Failed` is the rescue path); a
        // stopped world has nothing to stop and is refused, exactly like a never-booted one (§4).
        (BootCommand::Restart, Some(WorldState::Running | WorldState::Failed)) => {
            Some(BootVerb::Rerun)
        }
        (BootCommand::Restart, Some(WorldState::Stopped) | None) => None,
    }
}

/// The fork name a zero-argument `slipfork fork` derives from the requesting checkout (§7.1–§7.2).
///
/// Exactly three fail-closed answers — a request from inside `<vcs_root>/.worktrees/<fork>`, a
/// detached `HEAD` / bookmark-less `jj` checkout, and a branch that cannot become a fork name — and
/// never a generated or placeholder name. The name itself is [`sanitize_fork_name`]'s only; the
/// project and `domains[].id` collisions stay [`validate_fork_name`]'s job in the `fork` verb.
fn branch_fork_name(
    workspaces: &dyn VcsWorkspaceFactory,
    repo: &RepoRoot,
) -> Result<String, EdgeError> {
    let worktrees = repo.vcs_root.join(WORKTREES_DIRECTORY);
    if let Ok(relative) = repo.worktree_root.strip_prefix(&worktrees)
        && let Some(fork) = relative
            .components()
            .next()
            .and_then(|component| component.as_os_str().to_str())
    {
        return Err(rejected(format!(
            "already inside fork worktree '{WORKTREES_DIRECTORY}/{fork}'; run `slipfork fork` from the repository root {}",
            repo.vcs_root.display()
        )));
    }
    let branch = workspaces
        .current_branch(repo)
        .map_err(|err| runtime_error(err.to_string()))?
        .ok_or_else(|| {
            rejected(
                "checkout has no active branch; pass an explicit name with `slipfork fork <name>`",
            )
        })?;
    sanitize_fork_name(&branch).ok_or_else(|| {
        rejected(format!(
            "branch '{branch}' cannot become a fork name; pass a distinct name with `slipfork fork <name>`"
        ))
    })
}

/// Reject `restart` of a world this daemon never booted (`code: not_running`, §4).
fn not_running(project: &ProjectSlug, target: &WorldTarget) -> EdgeError {
    EdgeError::Control(DaemonError::new(
        DaemonErrorKind::NotRunning,
        format!(
            "world '{}' is not running; boot it with `slipfork up`",
            world_id(project, target)
        ),
    ))
}

/// The service ids one pipeline run brought to `Running`: the engine's rows after the run, minus
/// the rows that were already `Running` in the snapshot taken when the run began.
///
/// Both guarantees are structural: only [`ServiceState::Running`] rows can be named, and an id is
/// reported once even if the engine ever returns a duplicated row. `restarted` therefore never
/// contradicts `services[]` (an `exited`/`stopped` row is never called restarted) and is empty when
/// the run started nothing.
fn restarted_service_ids(
    before: &[WorldServiceSummary],
    after: &[WorldServiceSummary],
) -> Vec<String> {
    let mut restarted: Vec<String> = Vec::new();
    for row in after
        .iter()
        .filter(|row| row.info.state == ServiceState::Running)
    {
        let was_running = before.iter().any(|previous| {
            previous.info.id == row.info.id && previous.info.state == ServiceState::Running
        });
        if was_running || restarted.contains(&row.info.id) {
            continue;
        }
        restarted.push(row.info.id.clone());
    }
    restarted
}

/// The per-invocation boot policy (`--no-dump | --refresh-dump`, `--resources local`,
/// `--host-caches`), threaded into whichever verb runs the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BootPolicy {
    dump: DumpPolicy,
    force_local_resources: bool,
    host_caches: bool,
}

/// Inputs to one `L0` rootfs materialization.
struct RootfsInputs<'a> {
    worktree: &'a Path,
    snow: &'a SnowFile,
    config: &'a SlipforkConfig,
    vault: &'a dyn SecretProvider,
    coords: &'a ResourceCoordinates,
    claims: &'a OsppClaims,
    host_caches: bool,
}

/// Inputs to one world-level environment resolution.
struct WorldEnvInputs<'a> {
    snow: &'a SnowFile,
    vault: &'a dyn SecretProvider,
    coords: &'a ResourceCoordinates,
    project: &'a ProjectSlug,
    target: &'a WorldTarget,
    claims: &'a OsppClaims,
    ospp_env: Option<&'a BTreeMap<String, BTreeMap<String, SecretValue>>>,
}

struct ResolvedWorld {
    snow: SnowFile,
    config: SlipforkConfig,
    vault: Arc<dyn SecretProvider>,
    plan: ResourcePlan,
    resources: Vec<ResourceSummary>,
    coords: ResourceCoordinates,
    base_env: ExecEnv,
    /// The one enabled-component list this run resolved; `start_world` iterates exactly this.
    components: Vec<ResolvedComponent>,
    ingresses: Vec<IngressRoute>,
    domains: Vec<String>,
    /// The ID-only claims this boot sent (local vault keys, local resources, local domains).
    claims: OsppClaims,
    /// The server-resolved per-component environment (already decrypted in memory), kept so a
    /// pushed `env_updated` can be diffed against what the world runs.
    ospp_env: Option<BTreeMap<String, BTreeMap<String, SecretValue>>>,
}

/// Everything one `up`/`restart` pipeline run resolves **before** the world exists.
///
/// Both verbs call [`DaemonService::prepare_world`], so the `L0` cache verdict (hit vs rebuild),
/// the base-snapshot seal, and every environment are decided by one code path; only what the
/// engine does with the result differs (`clone_world` vs `reload_world`).
struct PreparedWorld {
    resolved: ResolvedWorld,
    rootfs: WorldRootfs,
    snapshot: SnapshotId,
    boot: BootLineage,
}

/// One enabled component of the world: everything `start_world` needs, resolved once.
#[derive(Debug, Clone)]
struct ResolvedComponent {
    id: String,
    run_command: String,
    readiness_port: Option<u16>,
    env: ExecEnv,
}

/// A world a fresh `boot` produced, plus what that boot observably did.
struct BootedWorld {
    live: LiveWorld,
    /// The engine's rows read after the pipeline ran; the summary reports these, not a second read.
    rows: Vec<WorldServiceSummary>,
    changed: bool,
    restarted: Vec<String>,
}

/// Refuse a rerun that asks for a fact only a fresh boot can produce (`docs/design/dx-0.1.0-ux.md`
/// §4): dump policy and `--host-caches` are inputs of the sealed base and the built rootfs, which a
/// `restart`/in-place `up` deliberately keeps.
fn check_rerun_policy(world: &str, held: BootPolicy, policy: BootPolicy) -> Result<(), EdgeError> {
    if policy.dump != held.dump {
        return Err(rejected(format!(
            "world '{world}' keeps its CoW state and its base snapshot; the dump policy re-seals the base, so run `slipfork down --world {world}` first"
        )));
    }
    if policy.host_caches != held.host_caches {
        return Err(rejected(format!(
            "world '{world}' keeps its built rootfs; `--host-caches` rebuilds it, so run `slipfork down --world {world}` first"
        )));
    }
    Ok(())
}

/// The policy one `up`/`fork` request asks for from scratch.
fn request_policy(
    dump: slipfork_wire::control::DumpRequest,
    resources: slipfork_wire::control::ResourceRequest,
    host_caches: bool,
) -> BootPolicy {
    BootPolicy {
        dump: dump_policy(dump),
        force_local_resources: matches!(resources, slipfork_wire::control::ResourceRequest::Local),
        host_caches,
    }
}

/// Merge one `up` request into the policy a held world already runs.
///
/// An absent `--no-dump`/`--refresh-dump` and an absent `--host-caches` keep what the world booted
/// with (a bare `up` of a failed world never silently changes a fresh-boot-only fact), while an
/// explicit flag that a rerun cannot apply is carried through so [`check_rerun_policy`] refuses it
/// with one actionable line instead of dropping it.
fn rerun_policy(
    held: BootPolicy,
    dump: slipfork_wire::control::DumpRequest,
    resources: slipfork_wire::control::ResourceRequest,
    host_caches: bool,
) -> BootPolicy {
    BootPolicy {
        dump: match dump {
            slipfork_wire::control::DumpRequest::Default => held.dump,
            explicit => dump_policy(explicit),
        },
        force_local_resources: matches!(resources, slipfork_wire::control::ResourceRequest::Local)
            || held.force_local_resources,
        host_caches: host_caches || held.host_caches,
    }
}

/// Inputs to one world materialization.
struct BootContext {
    repo: RepoRoot,
    target: WorldTarget,
    worktree: PathBuf,
    policy: BootPolicy,
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
struct DaemonPorts {
    /// CoW world engine.
    engine: std::sync::Arc<dyn WorldEngine>,
    /// The single `L0` world-rootfs builder (`DockerfileStageBuilder` in production).
    builder: std::sync::Arc<dyn BaseImageBuilder>,
    /// HTTP ingress registry and origin source.
    registry: std::sync::Arc<WorldRegistry>,
    /// Canonical repository resolver.
    repo_roots: std::sync::Arc<dyn RepoRootResolver>,
    /// Fork workspace factory.
    workspaces: std::sync::Arc<dyn VcsWorkspaceFactory>,
    /// Project-scoped `0600` local-vault factory.
    vaults: std::sync::Arc<dyn ProjectVaults>,
    /// The one OSPP v1 client bound to the boot-injected endpoint (`None` = local-only daemon).
    ospp: Option<Arc<dyn OsppClient>>,
    /// The machine-authentication identity (`p256` auth only; never subscription encryption).
    signer: Arc<dyn DeviceKeySigner>,
    /// Hostname proposed in `pair_begin` (resolved once by the Composition Root).
    hostname: String,
    /// The concrete desktop launcher the pairing handle is handed to immediately.
    browser: Browser,
    /// CSPRNG injected into the pairing handshake nonce.
    entropy: Arc<dyn EntropySource>,
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

/// The daemon-owned [`DaemonControl`] implementation.
pub struct DaemonService {
    engine: std::sync::Arc<dyn WorldEngine>,
    builder: std::sync::Arc<dyn BaseImageBuilder>,
    registry: std::sync::Arc<WorldRegistry>,
    repo_roots: std::sync::Arc<dyn RepoRootResolver>,
    workspaces: std::sync::Arc<dyn VcsWorkspaceFactory>,
    vaults: std::sync::Arc<dyn ProjectVaults>,
    /// The one boot-injected OSPP client (`None` = local-only daemon).
    ospp: Option<Arc<dyn OsppClient>>,
    /// The machine-authentication identity (`p256` auth only).
    signer: Arc<dyn DeviceKeySigner>,
    /// Hostname proposed in `pair_begin`.
    hostname: String,
    /// The concrete desktop launcher for the pairing approval page.
    browser: Browser,
    entropy: Arc<dyn EntropySource>,
    /// The daemon-owned live `ospp-v1` session and its per-project subscriptions.
    ospp_runtime: Mutex<OsppRuntime>,
    /// Pushed `env_updated` environments, drained by the subscription supervisor thread.
    ospp_updates_tx: Arc<Mutex<mpsc::Sender<SubscriptionEnv>>>,
    ospp_updates_rx: Mutex<Option<mpsc::Receiver<SubscriptionEnv>>>,
    /// One actionable line describing a failed live session (never a secret value).
    ospp_failure: Mutex<Option<String>>,
    config: DaemonConfig,
    ingress: IngressInfo,
    forward_port: Option<u16>,
    state: Mutex<DaemonState>,
    /// Serializes world materialization so one project's base snapshot is minted exactly once.
    boot_lock: Mutex<()>,
}

impl std::fmt::Debug for DaemonService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonService")
            .field("ingress", &self.ingress)
            .field("forward_port", &self.forward_port)
            .finish_non_exhaustive()
    }
}

impl DaemonService {
    /// Assemble the service over the composition root's already-constructed adapters.
    fn new(
        ports: DaemonPorts,
        config: DaemonConfig,
        ingress: IngressInfo,
        forward_port: Option<u16>,
    ) -> Self {
        let (updates_tx, updates_rx) = mpsc::channel();
        Self {
            engine: ports.engine,
            builder: ports.builder,
            registry: ports.registry,
            repo_roots: ports.repo_roots,
            workspaces: ports.workspaces,
            vaults: ports.vaults,
            ospp: ports.ospp,
            signer: ports.signer,
            hostname: ports.hostname,
            browser: ports.browser,
            entropy: ports.entropy,
            ospp_runtime: Mutex::new(OsppRuntime::default()),
            ospp_updates_tx: Arc::new(Mutex::new(updates_tx)),
            ospp_updates_rx: Mutex::new(Some(updates_rx)),
            ospp_failure: Mutex::new(None),
            config,
            ingress,
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

    /// The project's local vault: the narrow read/ensure input to `SnowFile` evaluation.
    fn vault_for(&self, project: &ProjectSlug) -> Result<Arc<dyn SecretProvider>, EdgeError> {
        self.vaults.provider(project)
    }

    /// The project's local vault as the capability-honest `SecretVault` port.
    fn vault_store(&self, project: &ProjectSlug) -> Result<Arc<dyn SecretVault>, EdgeError> {
        self.vaults.store(project)
    }

    /// Record one actionable, secret-free line describing a failed live session.
    fn record_ospp_failure(&self, line: String) {
        let mut failure = self
            .ospp_failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if failure.as_deref() != Some(line.as_str()) {
            eprintln!("slipforkd: {line}");
            *failure = Some(line);
        }
    }

    /// The daemon-held `ospp-v1` session, dialed once and reused while it lives.
    ///
    /// An endpoint that is unreachable or refuses the machine key yields `None` so a purely local
    /// world still boots (`OSPP` is never a prerequisite, `ADR-0005` §1). A resource strategy that
    /// needs the session then fails closed inside `ResourcePlan::resolve`, and the recorded line
    /// names the endpoint and the remedy.
    fn ospp_session(&self) -> Result<Option<Arc<dyn OsppSession>>, EdgeError> {
        let Some(client) = self.ospp.as_ref() else {
            return Ok(None);
        };
        {
            let runtime = self
                .ospp_runtime
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(session) = runtime.session.as_ref() {
                return Ok(Some(Arc::clone(session)));
            }
        }
        let updates = Arc::clone(&self.ospp_updates_tx);
        let on_update = Box::new(move |environment: SubscriptionEnv| {
            if let Ok(sender) = updates.lock() {
                let _ = sender.send(environment);
            }
        });
        let session: Arc<dyn OsppSession> = match client.connect(on_update) {
            Ok(session) => Arc::from(session),
            Err(error) => {
                self.record_ospp_failure(format!(
                    "the OSPP endpoint {} is unavailable ({error}); run `slipfork auth` to pair this machine, then `slipfork restart`",
                    client.endpoint()
                ));
                return Ok(None);
            }
        };
        self.record_ospp_failure_clear();
        let mut runtime = self
            .ospp_runtime
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        runtime.session = Some(Arc::clone(&session));
        Ok(Some(session))
    }

    fn record_ospp_failure_clear(&self) {
        *self
            .ospp_failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// The ID-only claims the local world owns: local vault `vars`/`secrets`, locally provisioned
    /// resources, and Slipfork-assigned domains. Values never cross the wire.
    fn claims_for(
        &self,
        project: &ProjectSlug,
        snow: &SnowFile,
        plan: &ResourcePlan,
    ) -> Result<OsppClaims, EdgeError> {
        let store = self.vault_store(project)?;
        let mut resources: BTreeMap<String, Vec<String>> = BTreeMap::new();
        // Slipfork always assigns the local world's origin, so `resources::app.*` stays local.
        resources.insert(
            "app".to_owned(),
            vec!["url".to_owned(), "domain".to_owned()],
        );
        let pg_ids: Vec<String> = snow
            .resources
            .pg_databases
            .iter()
            .map(|database| database.id.clone())
            .collect();
        if !pg_ids.is_empty() {
            resources.insert("pg_databases".to_owned(), pg_ids);
        }
        let redis_ids: Vec<String> = snow
            .resources
            .redis_clusters
            .iter()
            .map(|cluster| cluster.id.clone())
            .collect();
        if !redis_ids.is_empty() {
            resources.insert("redis_clusters".to_owned(), redis_ids);
        }
        let local_s3: Vec<String> = plan
            .s3_buckets
            .iter()
            .filter(|bucket| matches!(bucket.strategy, ResourceStrategy::Local))
            .map(|bucket| bucket.id.clone())
            .collect();
        if !local_s3.is_empty() {
            resources.insert("s3_buckets".to_owned(), local_s3);
        }
        // A local-mode `!!` secret is generated and persisted by this daemon, so the server must
        // leave it untouched even before the vault file records it.
        let mut secrets: BTreeSet<String> = store.keys(project)?.into_iter().collect();
        secrets.extend(snow.generated_secret_keys(WORLD_MODE).map_err(snow_error)?);
        Ok(OsppClaims {
            vars: store.vars(project)?,
            secrets: secrets.into_iter().collect(),
            resources,
            domains: snow.domain_ids().into_iter().map(str::to_owned).collect(),
            // Components are always started locally, but their *environment* is exactly what the
            // server resolves; claiming one would suppress its `env_resolved` entry.
            components: Vec::new(),
        })
    }

    /// Subscribe (or re-subscribe with the same stable id) for one project.
    fn subscribe_project(
        &self,
        project: &ProjectSlug,
        remote: &str,
        snow_source: &str,
        claims: &OsppClaims,
    ) -> Result<Option<SubscriptionEnv>, EdgeError> {
        let Some(session) = self.ospp_session()? else {
            return Ok(None);
        };
        let id = subscription_id(project);
        let request = SubscribeRequest {
            id: id.clone(),
            remote: remote.to_owned(),
            snow: snow_source.to_owned(),
            claims: claims.clone(),
        };
        match session.subscribe(&request) {
            Ok(environment) => {
                let mut runtime = self
                    .ospp_runtime
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                runtime.subscriptions.insert(id, project.clone());
                Ok(Some(environment))
            }
            Err(error) => {
                self.record_ospp_failure(format!(
                    "the OSPP subscription for project '{project}' failed ({error}); run `slipfork restart` after fixing the server"
                ));
                Err(runtime_error(format!(
                    "cannot subscribe project '{project}' to OSPP: {error}"
                )))
            }
        }
    }

    /// Drop the session when the project's last world stops (`unsubscribe`, then `close`).
    fn note_world_released(&self, project: &ProjectSlug) {
        let remaining = {
            let state = self.lock();
            state.worlds.iter().any(|world| &world.project == project)
        };
        if remaining {
            return;
        }
        let mut runtime = self
            .ospp_runtime
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let id = subscription_id(project);
        let released = runtime.subscriptions.remove(&id).is_some();
        if released && let Some(session) = runtime.session.as_ref() {
            let _ = session.unsubscribe(&id);
        }
        if runtime.subscriptions.is_empty()
            && let Some(session) = runtime.session.take()
        {
            let _ = session.close();
        }
    }

    /// Start the one subscription supervisor that applies pushed `env_updated` environments.
    fn start_ospp_supervisor(self: &Arc<Self>) {
        let Some(receiver) = self
            .ospp_updates_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let service = Arc::downgrade(self);
        let _ = thread::Builder::new()
            .name("ospp-supervisor".to_owned())
            .spawn(move || {
                while let Ok(environment) = receiver.recv() {
                    let Some(service) = service.upgrade() else {
                        break;
                    };
                    if let Err(error) = service.apply_env_update(&environment) {
                        service.record_ospp_failure(error.to_string());
                    }
                }
            });
    }

    /// Apply one already-decrypted `env_updated` environment: recompute each component's
    /// environment from the local vault + coordinates, and restart **only** the components whose
    /// environment actually changed.
    fn apply_env_update(&self, environment: &SubscriptionEnv) -> Result<(), EdgeError> {
        let project = {
            let runtime = self
                .ospp_runtime
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            runtime.subscriptions.get(&environment.id).cloned()
        };
        let Some(project) = project else {
            return Err(runtime_error(format!(
                "OSPP pushed an environment for unknown subscription '{}'",
                environment.id
            )));
        };
        let _guard = self.boot_guard();
        let mut state = self.lock();
        for world in state
            .worlds
            .iter_mut()
            .filter(|world| world.project == project)
        {
            let provider = self.vault_for(&project)?;
            let claims = self.claims_for(&project, &world.snow, &world.plan)?;
            for index in 0..world.components.len() {
                let id = world.components[index].id.clone();
                let server = environment.component(&id);
                let env = resolve_component_env(
                    &world.snow,
                    provider.as_ref(),
                    &world.coords,
                    &id,
                    &claims,
                    server,
                    self.entropy.as_ref(),
                )?;
                if env == world.components[index].env {
                    continue;
                }
                let handle = world
                    .services
                    .iter()
                    .find(|service| service.id() == id)
                    .cloned();
                if let Some(handle) = handle {
                    self.engine.update_service_env(&handle, &env).map_err(|err| {
                        runtime_error(format!(
                            "cannot restart component '{id}' with the pushed OSPP environment: {err}"
                        ))
                    })?;
                }
                world.components[index].env = env;
            }
            world.ospp_env = Some(environment.components.clone());
        }
        Ok(())
    }

    /// Resolve the repository, target, and checkout for one `cwd` + `--world` request.
    fn select(&self, cwd: &Path, world: Option<&str>) -> Result<WorldSelection, EdgeError> {
        let repo = self
            .repo_roots
            .resolve(cwd)
            .map_err(|err| runtime_error(err.to_string()))?;
        match world {
            Some(raw) => {
                let target = resolve_world_selector(raw, &repo)?;
                let worktree = match &target {
                    WorldTarget::Canonical => repo.vcs_root.clone(),
                    WorldTarget::UserFork(fork) => {
                        repo.vcs_root.join(WORKTREES_DIRECTORY).join(fork)
                    }
                    WorldTarget::TestSlot(_) => {
                        return Err(rejected(format!(
                            "world selector '{raw}' names a test slot; `--world` accepts only `<project>`, `<project>:<fork>`, or `<fork>`"
                        )));
                    }
                };
                Ok(WorldSelection {
                    repo,
                    target,
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

    /// The daemon's own record of one held world, copied out of the index so no engine I/O runs
    /// under the state mutex.
    fn world_facts(&self, project: &ProjectSlug, target: &WorldTarget) -> Option<WorldFacts> {
        self.lock().world(project, target).map(WorldFacts::of)
    }

    /// The one `WorldUpSummary` builder: every field is read back from the world's own record, its
    /// engine handle, and the engine's service rows, so only `changed` and `restarted` (both
    /// observed by the pipeline run) are caller-supplied facts.
    fn summary(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
        facts: &WorldFacts,
        services: Vec<WorldServiceSummary>,
        changed: bool,
        restarted: Vec<String>,
    ) -> Result<WorldUpSummary, EdgeError> {
        let world = world_id(project, target);
        let lineage = self.engine.world_lineage(&facts.handle).map_err(|err| {
            runtime_error(format!(
                "world '{world}' is attached but its engine handle is gone: {err}"
            ))
        })?;
        Ok(WorldUpSummary {
            world,
            state: facts.state,
            changed,
            restarted,
            worktree: facts.worktree.clone(),
            origin: self.registry.origin(project, target)?,
            lineage,
            services,
            resources: facts.resources.clone(),
            next: world_next_commands(facts.state),
        })
    }

    /// The engine's own service rows for one world (`world_processes` is the supervision record).
    fn service_rows(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
        handle: &WorldHandle,
    ) -> Result<Vec<WorldServiceSummary>, EdgeError> {
        let world = world_id(project, target);
        Ok(self
            .engine
            .world_processes(handle)
            .map_err(|err| {
                runtime_error(format!(
                    "world '{world}' is attached but its engine handle is gone: {err}"
                ))
            })?
            .into_iter()
            .map(|info| WorldServiceSummary {
                world: world.clone(),
                info,
            })
            .collect())
    }

    /// Load `snow.yml` and keep its raw bytes: OSPP `subscribe` sends the file verbatim.
    fn load_snow(&self, worktree: &Path) -> Result<(SnowFile, String), EdgeError> {
        let path = worktree.join("snow.yml");
        let raw = fs::read_to_string(&path).map_err(|err| {
            runtime_error(format!(
                "cannot read {}: {err}; every world boot needs the project's snow.yml",
                path.display()
            ))
        })?;
        let parsed = SnowFile::parse_yaml(&raw)
            .map_err(|err| runtime_error(format!("{}: {err}", path.display())))?;
        Ok((parsed, raw))
    }

    fn load_config(&self, vcs_root: &Path) -> Result<SlipforkConfig, EdgeError> {
        // `slipfork.toml` is optional (`VISION.md`, `docs/getting-started.md`): an absent manifest
        // means the default DAG/resources spec, while a malformed or unreadable one fails closed.
        load_optional_slipfork_config(&vcs_root.join("slipfork.toml")).map_err(runtime_error)
    }

    /// Materialize (or reuse) the selected `snow.yml` `Dockerfile` stage as the world's `L0`
    /// rootfs (`ADR-0009` §1–§3), resolving `[world]` and every requested backing-service image.
    fn materialize_rootfs(&self, inputs: RootfsInputs<'_>) -> Result<WorldRootfs, EdgeError> {
        let RootfsInputs {
            worktree,
            snow,
            config,
            vault,
            coords,
            claims,
            host_caches,
        } = inputs;
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
        let owns = |namespace: &str, path: &[String]| claims.owns(namespace, path);
        let resolved = ClaimedEvaluator::new(vault, coords, self.entropy.as_ref(), &owns)
            .mode(snow, WORLD_MODE, None)
            .map_err(snow_error)?;
        let mut build_envs: Vec<BuildEnvVar> = Vec::new();
        for (name, value) in resolved
            .iter()
            .filter(|(name, _)| build_names.contains(*name))
        {
            if value.expose().contains("${{") {
                return Err(runtime_error(format!(
                    "`snow.yml` build variable '{name}' has no locally resolvable value; a Dockerfile build cannot receive an OSPP-resolved template"
                )));
            }
            build_envs.push(BuildEnvVar {
                name: name.clone(),
                value: value.expose().to_owned(),
            });
        }

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
    ///
    /// Returns the snapshot plus the [`BootLineage`] the clone records on its own handle: a seal
    /// performed by this call reports its measured duration and seed rebuild verdict, while a base
    /// that already existed reports `base.millis: None` (rendered `reused`) and
    /// `seed.rebuilt: Some(false)` (rendered `cached`). Every duration comes from the daemon's one
    /// own plain [`std::time::Instant`] observations; nothing is estimated.
    fn ensure_base_snapshot(
        &self,
        project: &ProjectSlug,
        snow: &SnowFile,
        plan: &ResourcePlan,
        rootfs: &WorldRootfs,
    ) -> Result<(SnapshotId, BootLineage), EdgeError> {
        let digest = base_digest(project, snow, plan);
        let key = (project.clone(), digest.clone());
        if let Some(sealed) = self.lock().bases.get(&key).cloned() {
            return Ok((sealed.snapshot, sealed.lineage));
        }

        // Pre-flight: a base snapshot already on disk is reused as-is, so the seeding pipeline (seed
        // world boot, `pg_restore`, seal) never runs only to be discarded by `SnapshotExists`.
        let snapshot = SnapshotId(format!("{project}-base-{digest}"));
        let reused = self
            .engine
            .snapshot_exists(&snapshot)
            .map_err(|err| runtime_error(format!("cannot look up the base snapshot: {err}")))?;
        if reused {
            let (has_seed, digest_hex) = selected_seed(snow, plan);
            let lineage = reused_base_lineage(&digest, has_seed, digest_hex);
            self.lock().bases.insert(
                key,
                SealedBase {
                    snapshot: snapshot.clone(),
                    lineage: lineage.clone(),
                },
            );
            return Ok((snapshot, lineage));
        }

        let seed = self.config.layout.base_seed_dir(project);
        ensure_private_dir(&seed).map_err(runtime_error)?;
        let empty = SnapshotId(format!("{project}-seed-empty"));
        // A sealed snapshot is immutable: an existing empty seed is reused as-is, never re-sealed.
        match self.engine.create_snapshot(&seed, &empty) {
            Ok(()) | Err(EngineError::SnapshotExists { .. }) => {}
            Err(err) => {
                return Err(runtime_error(format!(
                    "cannot create the empty base seed snapshot: {err}"
                )));
            }
        }

        let spec = WorldSpec {
            target: WorldTarget::TestSlot(0),
            rootfs: rootfs.clone(),
            snapshot: empty,
            mounts: Vec::new(),
            base_env: ExecEnv::world_scoped(self.config.layout.world_home(project, "base-seed")),
            local_ingress: None,
            boot: BootLineage::default(),
        };
        let handle = self.engine.clone_world(&spec).map_err(|err| {
            runtime_error(format!(
                "cannot boot the base seed world for '{project}': {err}"
            ))
        })?;

        let restore_started = std::time::Instant::now();
        let restored = self.restore_base(project, &handle, snow, plan, rootfs);
        let restore_millis = restored
            .as_ref()
            .ok()
            .map(|()| elapsed_millis(restore_started, std::time::Instant::now()));
        // The seal outcome decides the facts: only a seal performed here may claim a duration.
        let seal_started = std::time::Instant::now();
        let sealed = restored.and_then(|()| {
            match self.engine.freeze_world(&handle, &snapshot) {
                Ok(()) => Ok(true),
                // The base is already sealed on disk: the world reuses it and claims no seal time.
                Err(EngineError::SnapshotExists { .. }) => Ok(false),
                Err(err) => Err(runtime_error(format!(
                    "cannot freeze the base seed world: {err}"
                ))),
            }
        });
        let sealed_now = match sealed {
            Ok(sealed_now) => sealed_now,
            Err(error) => {
                let _ = self.engine.destroy_world(handle);
                return Err(error);
            }
        };
        let seal_millis =
            sealed_now.then(|| elapsed_millis(seal_started, std::time::Instant::now()));
        self.engine
            .destroy_world(handle)
            .map_err(|err| runtime_error(format!("cannot destroy the base seed world: {err}")))?;
        // A seed exists only when a dump was restored; several dumps have no single digest.
        let (has_seed, digest_hex) = selected_seed(snow, plan);
        let seed = has_seed.then_some(SeedLineage {
            digest: digest_hex,
            rebuilt: Some(sealed_now),
            millis: if sealed_now { restore_millis } else { None },
        });
        let facts = BootLineage {
            base: Some(BaseLineage {
                key: digest.clone(),
                millis: seal_millis,
            }),
            seed,
        };
        let reused_facts = reused_base_lineage(
            &digest,
            has_seed,
            facts.seed.as_ref().and_then(|seed| seed.digest.clone()),
        );
        self.lock().bases.insert(
            key,
            SealedBase {
                snapshot: snapshot.clone(),
                lineage: reused_facts,
            },
        );
        Ok((snapshot, facts))
    }

    /// Start every backing-service container in the seed world and restore the selected dumps.
    fn restore_base(
        &self,
        project: &ProjectSlug,
        handle: &WorldHandle,
        snow: &SnowFile,
        plan: &ResourcePlan,
        rootfs: &WorldRootfs,
    ) -> Result<(), EdgeError> {
        let remote_dumps: BTreeMap<&str, bool> = plan
            .pg_databases
            .iter()
            .filter_map(|database| match &database.seed {
                PgSeed::RemoteDump { refresh } => Some((database.id.as_str(), *refresh)),
                PgSeed::CleanInitdb => None,
            })
            .collect();
        let session = if remote_dumps.is_empty() {
            None
        } else {
            Some(self.ospp_session()?.ok_or_else(|| {
                runtime_error(
                    "the selected seed dump requires an authenticated OSPP session; run `slipfork auth` (or `up --no-dump` for a clean database)",
                )
            })?)
        };
        let mut services: BTreeMap<String, ServiceHandle> = BTreeMap::new();
        for spec in service_specs(snow, rootfs)? {
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
            validate_database_id(&database.id)?;
            self.ensure_database(service, &database.id)?;
            let Some(_refresh) = remote_dumps.get(database.id.as_str()) else {
                continue;
            };
            let session = session.as_ref().ok_or_else(|| {
                runtime_error(format!(
                    "the seed dump for '{}' requires an authenticated OSPP session",
                    database.id
                ))
            })?;
            let reader = session
                .dump(
                    &subscription_id(project),
                    &format!("pg_databases.{}", database.id),
                )
                .map_err(|err| {
                    runtime_error(format!(
                        "cannot stream the seed dump for '{}': {err}",
                        database.id
                    ))
                })?;
            // The decrypted stream is piped straight into `zstd -d | pg_restore` inside the
            // container: no plaintext dump byte ever lands on host disk.
            let restore = format!(
                "zstd -d | pg_restore --no-owner --no-privileges --clean --if-exists -U postgres -h 127.0.0.1 -d {}",
                database.id
            );
            let argv = vec!["sh".to_owned(), "-c".to_owned(), restore];
            let mut stderr = Vec::new();
            let output = self
                .engine
                .exec_in_service_with_input(
                    service,
                    &argv,
                    reader,
                    &ExecEnv::default(),
                    &mut |chunk| {
                        if chunk.source == slipfork_engine::StreamSource::Stderr {
                            stderr.extend_from_slice(&chunk.bytes);
                        }
                    },
                )
                .map_err(|err| {
                    runtime_error(format!("cannot restore database '{}': {err}", database.id))
                })?;
            if output.exit_code != 0 {
                let detail = if output.stderr.trim().is_empty() {
                    String::from_utf8_lossy(&stderr).into_owned()
                } else {
                    output.stderr.clone()
                };
                return Err(runtime_error(format!(
                    "restoring database '{}' failed with exit code {}: {}",
                    database.id,
                    output.exit_code,
                    detail.trim()
                )));
            }
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
        self.wait_for_postgres_ready(service)?;
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

    /// Block until the world's postmaster accepts queries, bounded by the injected
    /// [`PostgresReady`] policy.
    ///
    /// The engine's `readiness_port` probe only proves the container accepts TCP; `postgres:18`
    /// binds `:5432` milliseconds before postmaster leaves `PM_STARTUP`, so a `psql` issued right
    /// after it intermittently dies with `FATAL: the database system is starting up`. `pg_isready`
    /// runs inside the service container through the same [`WorldEngine::exec_in_service`] path as
    /// every later client, and the last server-side diagnostic is surfaced on timeout.
    fn wait_for_postgres_ready(&self, service: &ServiceHandle) -> Result<(), EdgeError> {
        let argv = [
            "pg_isready",
            "-U",
            "postgres",
            "-h",
            "127.0.0.1",
            "-d",
            "postgres",
        ]
        .map(str::to_owned)
        .to_vec();
        let what = "probing the base seed world's postgres readiness";
        await_postgres_ready(&self.config.postgres_ready, || {
            let (output, stderr) = self.exec_service(service, &argv, what)?;
            Ok((
                output.exit_code,
                readiness_detail(&output.stdout, &stderr).to_owned(),
            ))
        })
    }

    /// Run `argv` inside a running backing-service container, failing closed on a non-zero exit.
    fn exec_service_checked(
        &self,
        service: &ServiceHandle,
        argv: &[String],
        what: &str,
    ) -> Result<ExecOutput, EdgeError> {
        let (output, detail) = self.exec_service(service, argv, what)?;
        if output.exit_code != 0 {
            return Err(runtime_error(format!(
                "{what} failed with exit code {}: {}",
                output.exit_code,
                detail.trim()
            )));
        }
        Ok(output)
    }

    /// Run `argv` inside a running backing-service container, whatever the exit code.
    ///
    /// Returns the captured output plus the stderr the caller must surface (the process's own
    /// `stderr`, or the streamed chunks when the engine reports none).
    fn exec_service(
        &self,
        service: &ServiceHandle,
        argv: &[String],
        what: &str,
    ) -> Result<(ExecOutput, String), EdgeError> {
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
        let detail = if output.stderr.trim().is_empty() {
            String::from_utf8_lossy(&stderr).into_owned()
        } else {
            output.stderr.clone()
        };
        Ok((output, detail))
    }

    fn world_env(&self, inputs: WorldEnvInputs<'_>) -> Result<ExecEnv, EdgeError> {
        let WorldEnvInputs {
            snow,
            vault,
            coords,
            project,
            target,
            claims,
            ospp_env,
        } = inputs;
        let mut env = ExecEnv::world_scoped(
            self.config
                .layout
                .world_home(project, &world_id(project, target)),
        );
        for (var, value) in &self.config.inherited {
            env.inherit(*var, value.clone());
        }
        let owns = |namespace: &str, path: &[String]| claims.owns(namespace, path);
        let evaluator = ClaimedEvaluator::new(vault, coords, self.entropy.as_ref(), &owns);
        let resolved = evaluator.mode(snow, WORLD_MODE, None).map_err(snow_error)?;
        apply_env(&mut env, resolved)?;
        if let Some(components) = ospp_env {
            for server in components.values() {
                for (name, value) in server {
                    let rendered = evaluator.render(value.expose()).map_err(snow_error)?;
                    let secret = value.is_secret() || rendered.is_secret();
                    let raw = rendered.expose().to_owned();
                    let outcome = if secret {
                        env.set_secret(name, raw)
                    } else {
                        env.set(name, raw)
                    };
                    outcome.map_err(|err| {
                        runtime_error(format!("invalid OSPP environment '{name}': {err}"))
                    })?;
                }
            }
        }
        Ok(env)
    }

    /// Resolve every `snow.yml`- and flag-derived fact one pipeline run needs, before anything is
    /// materialized or started.
    ///
    /// `boot` and `reboot` call this same resolver, so a `restart` re-derives the resource plan,
    /// resource coordinates, origin, environments, enabled components, and ingress/domain table
    /// from the edited file instead of reusing boot-time values.
    fn resolve_world(
        &self,
        repo: &RepoRoot,
        target: &WorldTarget,
        worktree: &Path,
        policy: BootPolicy,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<ResolvedWorld, EdgeError> {
        let project = &repo.project;
        let (snow, snow_source) = self.load_snow(worktree)?;
        let effective = snow
            .effective_components(WORLD_MODE)
            .map_err(|err| runtime_error(err.to_string()))?;
        let route = LocalHostRoute::new(project.clone(), target.clone(), None, &snow.domain_ids())
            .map_err(|err| runtime_error(err.to_string()))?;
        let origin = self.registry.mint_origin(&route, self.ingress.port())?;
        let config = self.load_config(&repo.vcs_root)?;

        // OSPP project identity is the verbatim Git remote and only that string: without an
        // origin there is nothing to subscribe, even when the machine key is paired.
        let remote = self.repo_roots.remote(repo).map_err(|err| {
            runtime_error(format!(
                "cannot read the repository's Git origin for OSPP: {err}"
            ))
        })?;
        // One live session per project while worlds run; the plan only needs to know whether a
        // usable (paired and remote-identified) session exists. OSPP is never a prerequisite.
        let session = self.ospp_session()?;
        let ospp_paired = session.is_some() && remote.is_some();
        let plan = resolve_plan(
            &config,
            &snow,
            target,
            policy.dump,
            policy.force_local_resources,
            ospp_paired,
        )?;
        let resources = plan_summaries(&plan);
        for resource in &resources {
            on_event(BootEvent::Resources {
                resource: resource.clone(),
            });
        }
        // Resource coordinates are always world-local: remote S3 credentials arrive as component
        // environment values, and PostgreSQL is still the local container restored from a stream.
        let coords =
            ResourceCoordinates::for_world(&snow, project, target, &origin, &BTreeMap::new())
                .map_err(|err| runtime_error(err.to_string()))?;
        let vault = self.vault_for(project)?;
        let claims = self.claims_for(project, &snow, &plan)?;
        let ospp_env = match (session.as_ref(), remote.as_deref()) {
            (Some(_), Some(remote)) => {
                self.subscribe_project(project, remote, &snow_source, &claims)?
            }
            _ => None,
        };
        let base_env = self.world_env(WorldEnvInputs {
            snow: &snow,
            vault: vault.as_ref(),
            coords: &coords,
            project,
            target,
            claims: &claims,
            ospp_env: ospp_env.as_ref().map(|env| &env.components),
        })?;
        let mut components = Vec::new();
        for (id, resolved) in &effective {
            if resolved.disabled {
                continue;
            }
            let readiness_port = snow
                .components
                .iter()
                .find(|component| component.id == *id)
                .and_then(|component| component.internal_ports.first().copied());
            components.push(ResolvedComponent {
                id: id.clone(),
                run_command: resolved.run_command.clone(),
                readiness_port,
                env: resolve_component_env(
                    &snow,
                    vault.as_ref(),
                    &coords,
                    id,
                    &claims,
                    ospp_env.as_ref().and_then(|env| env.component(id)),
                    self.entropy.as_ref(),
                )?,
            });
        }
        let ingresses = ingress_routes(&snow, &effective)?;
        let domains = snow.domain_ids().into_iter().map(str::to_owned).collect();
        Ok(ResolvedWorld {
            snow,
            config,
            vault,
            plan,
            resources,
            coords,
            base_env,
            components,
            ingresses,
            domains,
            claims,
            ospp_env: ospp_env.map(|environment| environment.components),
        })
    }

    /// Resolve one run's world inputs and materialize the content-addressed `L0` rootfs and base
    /// snapshot — the one prep path `boot` and `reboot` share.
    ///
    /// The rootfs digest is content-addressed over the whole build context, so an edited
    /// `Dockerfile` or copied input yields a new digest here and the builder rebuilds it; an
    /// unchanged tree is served from the cache. Neither verb can skip this and boot a stale tree.
    fn prepare_world(
        &self,
        repo: &RepoRoot,
        target: &WorldTarget,
        worktree: &Path,
        policy: BootPolicy,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<PreparedWorld, EdgeError> {
        let resolved = self.resolve_world(repo, target, worktree, policy, on_event)?;
        let rootfs = self.materialize_rootfs(RootfsInputs {
            worktree,
            snow: &resolved.snow,
            config: &resolved.config,
            vault: resolved.vault.as_ref(),
            coords: &resolved.coords,
            claims: &resolved.claims,
            host_caches: policy.host_caches,
        })?;
        let (snapshot, boot) =
            self.ensure_base_snapshot(&repo.project, &resolved.snow, &resolved.plan, &rootfs)?;
        Ok(PreparedWorld {
            resolved,
            rootfs,
            snapshot,
            boot,
        })
    }

    /// The one `WorldSpec` both verbs hand the engine: the worktree bind mount, the world-scoped
    /// environment, and the daemon's ingress bridge.
    fn world_spec(
        &self,
        target: &WorldTarget,
        worktree: &Path,
        prepared: &PreparedWorld,
    ) -> WorldSpec {
        WorldSpec {
            target: target.clone(),
            rootfs: prepared.rootfs.clone(),
            snapshot: prepared.snapshot.clone(),
            mounts: vec![WorldMount {
                host_path: worktree.to_path_buf(),
                guest_path: worktree.to_path_buf(),
                mode: MountMode::ReadWrite,
            }],
            base_env: prepared.resolved.base_env.clone(),
            local_ingress: Some(LocalIngressBridge {
                port: self.ingress.port(),
                host_ingress_sock: self.config.layout.http_ingress_socket(),
            }),
            boot: prepared.boot.clone(),
        }
    }

    /// Materialize, attach, and start one world.
    fn boot(
        &self,
        context: BootContext,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<BootedWorld, BootFailure> {
        let BootContext {
            repo,
            target,
            worktree,
            policy,
            mut workspace,
        } = context;
        let project = repo.project.clone();
        let fail = |error: EdgeError, workspace: Option<Box<dyn VcsWorkspace>>| BootFailure {
            error,
            world: None,
            workspace,
        };

        let prepared = match self.prepare_world(&repo, &target, &worktree, policy, on_event) {
            Ok(prepared) => prepared,
            Err(error) => return Err(fail(error, workspace)),
        };
        let spec = self.world_spec(&target, &worktree, &prepared);
        let PreparedWorld {
            resolved, rootfs, ..
        } = prepared;
        let handle = match self.engine.clone_world(&spec) {
            Ok(handle) => handle,
            Err(err) => {
                return Err(fail(
                    runtime_error(format!(
                        "cannot materialize world '{}': {err}",
                        world_id(&project, &target)
                    )),
                    workspace,
                ));
            }
        };

        if let Err(error) = self.registry.attach_world(
            &project,
            &target,
            resolved.domains.clone(),
            handle.clone(),
            resolved.ingresses.clone(),
        ) {
            let _ = self.engine.destroy_world(handle);
            return Err(fail(error, workspace));
        }

        let mut live = LiveWorld {
            project: project.clone(),
            target: target.clone(),
            repo,
            worktree: worktree.clone(),
            handle: handle.clone(),
            state: WorldState::Running,
            base_env: resolved.base_env,
            components: resolved.components,
            policy,
            coords: resolved.coords,
            snow: resolved.snow,
            plan: resolved.plan,
            ospp_env: resolved.ospp_env,
            rootfs: rootfs.clone(),
            resources: resolved.resources,
            services: Vec::new(),
            hook_logs: BTreeMap::new(),
            workspace: workspace.take().map(Arc::from),
        };

        let rows_before = match self.service_rows(&project, &target, &live.handle) {
            Ok(rows) => rows,
            Err(error) => {
                live.state = WorldState::Failed;
                return Err(BootFailure {
                    error,
                    world: Some(Box::new(live)),
                    workspace: None,
                });
            }
        };
        // A fresh boot always changed something: it created the world, so the hook count is not
        // needed here (only `restart` derives `changed` from it).
        if let Err(error) = self.start_world(&mut live, on_event) {
            live.state = WorldState::Failed;
            return Err(BootFailure {
                error,
                world: Some(Box::new(live)),
                workspace: None,
            });
        }
        let rows_after = match self.service_rows(&project, &target, &live.handle) {
            Ok(rows) => rows,
            Err(error) => {
                live.state = WorldState::Failed;
                return Err(BootFailure {
                    error,
                    world: Some(Box::new(live)),
                    workspace: None,
                });
            }
        };
        let restarted = restarted_service_ids(&rows_before, &rows_after);
        Ok(BootedWorld {
            live,
            rows: rows_after,
            changed: true,
            restarted,
        })
    }

    /// Start one world's backing services, its components, and its deploy hooks — the single
    /// pipeline tail `up`, `fork`, and `restart` all run.
    ///
    /// `live` already carries the world's handle (and therefore its CoW state), worktree, resolved
    /// environments, and `snow.yml`; this call owns only what happens *inside* that world. The
    /// services it starts are recorded on `live`, so `restarted` is read back from that record
    /// instead of being counted a second time. Returns the number of hooks that actually ran.
    fn start_world(
        &self,
        live: &mut LiveWorld,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<usize, EdgeError> {
        let world = world_id(&live.project, &live.target);

        let service_specs = service_specs(&live.snow, &live.rootfs)?;
        for spec in service_specs {
            match self.engine.spawn_service(&live.handle, &spec) {
                Ok(service) => live.services.push(service),
                Err(err) => {
                    return Err(runtime_error(format!(
                        "cannot start backing service '{}' in world '{world}': {err}",
                        spec.id
                    )));
                }
            }
        }

        let components = live.components.clone();
        for component in &components {
            let id = &component.id;
            if component.run_command.trim().is_empty() {
                return Err(rejected(format!(
                    "component '{id}' declares an empty run_command in mode '{WORLD_MODE}'"
                )));
            }
            let spec = ServiceSpec {
                kind: ServiceKind::Component,
                id: id.clone(),
                runtime: slipfork_engine::ServiceRuntime::WorldProcess {
                    argv: vec![
                        "sh".to_owned(),
                        "-eu".to_owned(),
                        "-c".to_owned(),
                        component.run_command.clone(),
                    ],
                    cwd: live.worktree.clone(),
                },
                env: component.env.clone(),
                readiness_port: component.readiness_port,
            };
            match self.engine.spawn_service(&live.handle, &spec) {
                Ok(service) => {
                    live.services.push(service);
                    if let Some(port) = component.readiness_port {
                        on_event(BootEvent::Ready {
                            component: id.clone(),
                            port,
                        });
                    }
                }
                Err(err) => {
                    return Err(runtime_error(format!(
                        "cannot start component '{id}' in world '{world}': {err}"
                    )));
                }
            }
        }

        let mut ran = self.run_hooks(live, HookPhase::PreDeploy, on_event)?;
        ran += self.run_hooks(live, HookPhase::PostDeploy, on_event)?;
        Ok(ran)
    }

    /// Run one deploy phase's hooks, returning how many ran.
    fn run_hooks(
        &self,
        live: &mut LiveWorld,
        phase: HookPhase,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<usize, EdgeError> {
        let hooks = live.snow.hooks.clone();
        let mut ran = 0;
        for hook in hooks {
            let Some(component) = live
                .components
                .iter()
                .find(|component| component.id == hook.component)
            else {
                continue;
            };
            if hook.when != phase {
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
            let overlay = component.env.clone();
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
            ran += 1;
        }
        Ok(ran)
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
            id.to_owned(),
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
            ingress: self.ingress.clone(),
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
        let project = selection.repo.project.clone();
        let target = selection.target.clone();
        let _guard = self.boot_guard();

        let verb = boot_verb(BootCommand::Up, self.world_state(&selection))
            .ok_or_else(|| not_running(&project, &target))?;
        match verb {
            BootVerb::Confirm => {
                // Idempotent `up`: the world already runs, so nothing is touched and only its
                // recorded facts are read back (`changed: false`, nothing restarted).
                let facts = self
                    .world_facts(&project, &target)
                    .ok_or_else(|| Self::unknown_world(&project, &target))?;
                let rows = self.service_rows(&project, &target, &facts.handle)?;
                self.summary(&project, &target, &facts, rows, false, Vec::new())
            }
            BootVerb::Rerun => {
                let live = self
                    .take_world(&project, &target)
                    .ok_or_else(|| Self::unknown_world(&project, &target))?;
                // The same in-place pipeline as `restart`, honouring this invocation's flags: an
                // absent flag keeps the world's policy, an explicit one a rerun cannot apply is
                // refused rather than dropped.
                let policy = rerun_policy(live.policy, req.dump, req.resources, req.host_caches);
                self.reboot(live, policy, on_event)
            }
            BootVerb::Boot => {
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
                    policy: request_policy(req.dump, req.resources, req.host_caches),
                    workspace: None,
                };
                match self.boot(context, on_event) {
                    Ok(booted) => self.record_world(
                        booted.live,
                        booted.rows,
                        booted.changed,
                        booted.restarted,
                    ),
                    Err(failure) => {
                        if let Some(live) = failure.world {
                            self.insert_world(*live);
                        }
                        Err(failure.error)
                    }
                }
            }
        }
    }

    fn restart(
        &self,
        req: &WorldSelector,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let project = selection.repo.project.clone();
        let target = selection.target.clone();
        let _guard = self.boot_guard();

        match boot_verb(BootCommand::Restart, self.world_state(&selection)) {
            Some(BootVerb::Rerun) => {
                let live = self
                    .take_world(&project, &target)
                    .ok_or_else(|| not_running(&project, &target))?;
                // `restart` carries no flags: it re-runs the policy the world already holds.
                let policy = live.policy;
                self.reboot(live, policy, on_event)
            }
            // `restart` never boots a world the caller believed was running.
            Some(BootVerb::Confirm | BootVerb::Boot) | None => Err(not_running(&project, &target)),
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
        let repo = self
            .repo_roots
            .resolve(&req.cwd)
            .map_err(|err| runtime_error(err.to_string()))?;
        // A nameless `slipfork fork` is resolved here and only here (§7.1): the client sends `None`
        // and never runs a VCS command.
        let (fork, derived) = match req.fork.as_deref().map(str::trim) {
            Some(name) => (name.to_owned(), false),
            None => (branch_fork_name(self.workspaces.as_ref(), &repo)?, true),
        };
        // The collision checks read the checkout's real `snow.yml` `domains[].id` set — never an
        // empty placeholder, which would let a fork shadow a declared domain.
        let (snow, _) = self.load_snow(&repo.worktree_root)?;
        let domains = snow.domain_ids();
        if fork == repo.project.as_str() {
            return Err(rejected(if derived {
                format!(
                    "branch '{fork}' collides with project '{}'; pass a distinct name with `slipfork fork <name>`",
                    repo.project.as_str()
                )
            } else {
                format!(
                    "cannot create a fork named '{fork}', which is the project's canonical world id; pass a distinct name with `slipfork fork <name>`"
                )
            }));
        }
        validate_fork_name(&fork, &domains).map_err(|err| {
            rejected(format!(
                "{err}; pass a distinct name with `slipfork fork <name>`"
            ))
        })?;
        let selection = self.select(&req.cwd, Some(&fork))?;
        let _guard = self.boot_guard();
        if self.world_state(&selection).is_some() {
            return Err(rejected(format!(
                "fork '{fork}' already exists; boot it with `slipfork up --world {}`",
                world_id(&selection.repo.project, &selection.target)
            )));
        }
        let workspace = self
            .workspaces
            .create_workspace(&WorkspaceRequest {
                repo: selection.repo.clone(),
                fork: fork.to_owned(),
                base: req.base.clone(),
            })
            .map_err(|err| runtime_error(err.to_string()))?;
        let context = BootContext {
            repo: selection.repo.clone(),
            target: selection.target.clone(),
            worktree: selection.worktree.clone(),
            policy: request_policy(req.dump, req.resources, req.host_caches),
            workspace: Some(workspace),
        };
        match self.boot(context, on_event) {
            Ok(booted) => {
                self.record_world(booted.live, booted.rows, booted.changed, booted.restarted)
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
        if selection.target == WorldTarget::Canonical {
            return Err(rejected(format!(
                "'{}' is the canonical world of '{}'; `slipfork drop` destroys a fork world — use `slipfork down --world {}` instead",
                world_id(&selection.repo.project, &selection.target),
                selection.repo.vcs_root.display(),
                world_id(&selection.repo.project, &selection.target)
            )));
        }
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
        let entries: Vec<(ProjectSlug, WorldTarget, WorldState, PathBuf, WorldHandle)> = {
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
                        world.handle.clone(),
                    )
                })
                .collect()
        };
        let mut summaries = Vec::with_capacity(entries.len());
        for (project, target, state, worktree, handle) in entries {
            let world = world_id(&project, &target);
            // Recorded on this world's own engine handle at boot: an unrecorded fact is `None` and
            // renders `-`, never `0` and never another world's value.
            let lineage = self
                .engine
                .world_lineage(&handle)
                .map_err(|err| engine_read_error(&world, state, &err))?;
            summaries.push(WorldSummary {
                world,
                parent: parent_world(&project, &target),
                state,
                origin: self.registry.origin(&project, &target)?,
                worktree,
                lineage,
            });
        }
        summaries.sort_by(|a, b| a.world.cmp(&b.world));
        Ok(summaries)
    }

    fn world_processes(&self, req: &WorldSelector) -> Result<WorldProcessReport, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let (state, handle) = {
            let world_state = self.lock();
            let Some(world) = world_state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            (world.state, world.handle.clone())
        };
        let world_text = world_id(&selection.repo.project, &selection.target);
        let lineage = self
            .engine
            .world_lineage(&handle)
            .map_err(|err| engine_read_error(&world_text, state, &err))?;
        let services = self
            .engine
            .world_processes(&handle)
            .map_err(|err| engine_read_error(&world_text, state, &err))?
            .into_iter()
            .map(|info| WorldServiceSummary {
                world: world_text.clone(),
                info,
            })
            .collect();
        WorldProcessReport::new(state, lineage, services).map_err(EdgeError::Control)
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
        let vault = self.vault_store(&repo.project)?;
        let existing = vault.get(&repo.project, key)?;
        if existing.is_some() && !force {
            return Err(rejected(format!(
                "secret '{key}' already exists in {}; pass --force to rotate it",
                self.config.layout.vault_path(&repo.project).display()
            )));
        }
        vault.put(&repo.project, key, value, true)
    }

    fn secret_unset(&self, repo_root: &Path, key: &str) -> Result<bool, EdgeError> {
        validate_secret_key(key)?;
        let repo = self
            .repo_roots
            .resolve(repo_root)
            .map_err(|err| runtime_error(err.to_string()))?;
        let vault = self.vault_store(&repo.project)?;
        vault.remove(&repo.project, key)
    }

    fn secret_list(&self, repo_root: &Path) -> Result<Vec<String>, EdgeError> {
        let repo = self
            .repo_roots
            .resolve(repo_root)
            .map_err(|err| runtime_error(err.to_string()))?;
        let vault = self.vault_store(&repo.project)?;
        vault.keys(&repo.project)
    }

    fn logs(&self, req: &LogsRequest, on_chunk: &mut dyn FnMut(LogChunk)) -> Result<(), EdgeError> {
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
                    // The engine's own record — never the request string — labels the chunks.
                    let label = log.target().to_owned();
                    let mut emit = |chunk: StreamChunk| {
                        if wants(&chunk) {
                            on_chunk(LogChunk::attribute(label.clone(), chunk));
                        }
                    };
                    if req.follow {
                        log.follow(&mut emit);
                    } else {
                        let selected: Vec<StreamChunk> = log
                            .snapshot()
                            .into_iter()
                            .filter(|chunk| wants(chunk))
                            .collect();
                        for chunk in tail_lines(selected, req.lines) {
                            emit(chunk);
                        }
                    }
                }
                RetainedTarget::Hook(log) => {
                    let label = log.target().to_owned();
                    let mut emit = |chunk: StreamChunk| {
                        if wants(&chunk) {
                            on_chunk(LogChunk::attribute(label.clone(), chunk));
                        }
                    };
                    if req.follow {
                        log.follow(&mut emit);
                    } else {
                        let selected: Vec<StreamChunk> = log
                            .snapshot()
                            .into_iter()
                            .filter(|chunk| wants(chunk))
                            .collect();
                        for chunk in tail_lines(selected, req.lines) {
                            emit(chunk);
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
    ) -> Result<slipfork_wire::control::WorldExecTarget, EdgeError> {
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
        Ok(slipfork_wire::control::WorldExecTarget {
            world,
            origin,
            entry,
            env: base_env,
            target_argv: req.argv.clone(),
        })
    }

    fn hook_exec_target(&self, req: &HookRunRequest) -> Result<WorldExecTarget, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let (handle, worktree, snow, components, base_env) = {
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
                world.components.clone(),
                world.base_env.clone(),
            )
        };
        let resolved = resolve_hook(&snow, &components, &req.hook)?;
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
        let (handle, worktree, snow, components) = {
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
                world.components.clone(),
            )
        };
        let resolved = resolve_hook(&snow, &components, &req.hook)?;
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
        let (snow, components) = {
            let state = self.lock();
            let Some(world) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            (world.snow.clone(), world.components.clone())
        };
        let specs = forward_specs(&snow, &world_id_text, &components, &req.targets)?;
        let bridge = self
            .registry
            .forward(&selection.repo.project, &selection.target, &specs)?;
        wait_disconnect(bridge.summary());
        // Dropping the bridge stops every accept loop and closes the host ports.
        Ok(())
    }

    fn shutdown(&self) -> Result<(), EdgeError> {
        // The transport server stops accepting and releases `daemon.sock` after this ack.
        Ok(())
    }

    fn auth_login(
        &self,
        endpoint_url: Option<&str>,
    ) -> Result<slipfork_wire::control::AuthSummary, EdgeError> {
        let Some(client) = self.ospp.as_ref() else {
            return Err(rejected(
                "this daemon has no OSPP endpoint; start `slipforkd` with `--ospp-endpoint <wss://...>`",
            ));
        };
        if let Some(requested) = endpoint_url
            && requested != client.endpoint().as_str()
        {
            return Err(rejected(format!(
                "the requested OSPP endpoint '{requested}' differs from this daemon's injected endpoint '{}'; one daemon has exactly one server identity",
                client.endpoint()
            )));
        }
        let mut opened: Option<String> = None;
        let mut on_handle = |url: &slipfork_ospp::LivePairingUrl| {
            // The handle is live only for this pairing attempt: hand it to the launcher now,
            // before `pair_approved` blocks the handshake, and show it to the developer.
            let page = url.as_str();
            eprintln!("{page}");
            match self.browser.open(page) {
                Ok(true) => {}
                Ok(false) => eprintln!("{APPROVAL_HINT_PREFIX}{page}"),
                Err(err) => return Err(slipfork_ospp::OsppError::Transport(err)),
            }
            opened = Some(page.to_owned());
            Ok(())
        };
        let approval = client
            .pair(&self.hostname, &mut on_handle)
            .map_err(|err| runtime_error(format!("OSPP pairing failed: {err}")))?;
        // The freshly paired key must be used on the next dial, never a stale session.
        {
            let mut runtime = self
                .ospp_runtime
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(session) = runtime.session.take() {
                let _ = session.close();
            }
        }
        Ok(slipfork_wire::control::AuthSummary {
            key_id: format!("p256:{}", self.signer.public_key_hex()),
            backend: "software:file-0600".to_owned(),
            account: Some(approval.account),
            machine: Some(approval.machine),
            pairing_url: slipfork_wire::control::ControlSecret::new(
                opened.ok_or_else(|| runtime_error("the OSPP server issued no pairing handle"))?,
            ),
        })
    }
}

impl DaemonService {
    /// Record a world a pipeline run just produced and return its truthful summary.
    ///
    /// The world is inserted **before** the summary is read, so an engine-read failure can never
    /// drop a live world and make the next `restart` report `not_running`.
    fn record_world(
        &self,
        live: LiveWorld,
        rows: Vec<WorldServiceSummary>,
        changed: bool,
        restarted: Vec<String>,
    ) -> Result<WorldUpSummary, EdgeError> {
        let facts = WorldFacts::of(&live);
        let project = live.project.clone();
        let target = live.target.clone();
        self.insert_world(live);
        self.summary(&project, &target, &facts, rows, changed, restarted)
    }

    /// Whether the engine still holds a live world behind `handle`.
    ///
    /// [`WorldEngine::reload_world`] refuses a malformed spec, a pruned base, or a world that is
    /// sealing/sealed **before** it mutates anything, so a refused reload leaves the world's own
    /// engine record `Live` with its services bound. A failure past that gate may already have
    /// reaped the world PID 1 while the record still reads `Live` (the `Failed` stamp happens
    /// later), so the status alone is not a liveness proof: [`WorldEngine::world_is_live`] checks
    /// the record's booted init identity against `/proc`. No error variant or message is
    /// interpreted here, and a read error is not evidence of a live PID 1 — it counts as "no live
    /// world", so the daemon never reports a torn-down world as running.
    fn engine_holds_live_world(&self, handle: &WorldHandle) -> bool {
        self.engine.world_is_live(handle).unwrap_or(false)
    }

    /// Re-run one held world over freshly resolved inputs (`slipfork restart`, and `up`'s rescue
    /// path, §4) through the engine's one reload path.
    ///
    /// The world keeps its handle, its worktree, and — when the resolved base snapshot is
    /// unchanged — its one CoW subvolume, so backing-service data survives a rootfs-only change.
    /// Every boot-scope fact is re-derived through [`DaemonService::prepare_world`] (the same path
    /// `up` takes), which also re-materializes the content-addressed `L0` rootfs: an edited
    /// `Dockerfile`/copied input rebuilds and is what the world actually boots, while an unchanged
    /// tree stays a cache hit. A changed base snapshot re-clones the world from that base.
    /// `changed` and `restarted` are observed from the engine's own rows: the run changed the world
    /// only when a service came back `Running` or a hook ran.
    fn reboot(
        &self,
        mut live: LiveWorld,
        policy: BootPolicy,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError> {
        let world = world_id(&live.project, &live.target);

        // Refuse before touching anything; the world goes back into the index so the refusal never
        // loses a live world.
        if let Some(error) = check_rerun_policy(&world, live.policy, policy).err() {
            self.insert_world(live);
            return Err(error);
        }

        // Resolve and materialize first: a resolution or build failure leaves the running world
        // untouched instead of failing a healthy world.
        let prepared =
            match self.prepare_world(&live.repo, &live.target, &live.worktree, policy, on_event) {
                Ok(prepared) => prepared,
                Err(error) => {
                    self.insert_world(live);
                    return Err(error);
                }
            };
        let spec = self.world_spec(&live.target, &live.worktree, &prepared);
        let PreparedWorld {
            resolved, rootfs, ..
        } = prepared;

        // The engine owns the world's lifecycle: it stops every service (group-signalling each
        // `sh -eu -c` wrapper and its children), reaps the PID 1, keeps or re-clones the subvolume
        // for the resolved base, re-materializes the rootfs reference, records this run's lineage,
        // and boots a new PID 1 over the same handle.
        if let Err(err) = self.engine.reload_world(&live.handle, &spec) {
            // The engine's reload gate — the spec, its world-scoped `HOME`, the base snapshot, and
            // the world's lifecycle status — runs **before any mutation**, so a refused reload
            // leaves the world live with every service still bound: the daemon leaves its own
            // record untouched and only reports the error. A failure past that gate stopped every
            // service and left the engine record non-live, so the world is marked `Failed` and the
            // rows the engine already stopped are dropped.
            if !self.engine_holds_live_world(&live.handle) {
                live.state = WorldState::Failed;
                live.services.clear();
            }
            self.insert_world(live);
            return Err(runtime_error(format!(
                "cannot re-run world '{world}': {err}"
            )));
        }
        live.services.clear();

        // The registry serves the world's routes: swap them for the re-resolved table without
        // touching the engine handle, so an edited `ingresses:`/`domains:` takes effect.
        if let Err(error) = self.registry.replace_world(
            &live.project,
            &live.target,
            resolved.domains.clone(),
            resolved.ingresses.clone(),
        ) {
            live.state = WorldState::Failed;
            self.insert_world(live);
            return Err(error);
        }

        live.snow = resolved.snow;
        live.resources = resolved.resources;
        live.coords = resolved.coords;
        live.base_env = resolved.base_env;
        live.components = resolved.components;
        live.plan = resolved.plan;
        live.ospp_env = resolved.ospp_env;
        live.rootfs = rootfs;
        live.policy = policy;

        // The rows as they stand after the reload (the engine cleared them) and before the
        // pipeline: whatever is `Running` afterwards is what this run restarted.
        let rows_before = match self.service_rows(&live.project, &live.target, &live.handle) {
            Ok(rows) => rows,
            Err(error) => {
                live.state = WorldState::Failed;
                self.insert_world(live);
                return Err(error);
            }
        };
        let hooks = match self.start_world(&mut live, on_event) {
            Ok(hooks) => hooks,
            Err(error) => {
                live.state = WorldState::Failed;
                self.insert_world(live);
                return Err(error);
            }
        };
        let rows_after = match self.service_rows(&live.project, &live.target, &live.handle) {
            Ok(rows) => rows,
            Err(error) => {
                live.state = WorldState::Failed;
                self.insert_world(live);
                return Err(error);
            }
        };

        let restarted = restarted_service_ids(&rows_before, &rows_after);
        let changed = !restarted.is_empty() || hooks > 0;
        live.state = WorldState::Running;
        self.record_world(live, rows_after, changed, restarted)
    }

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
        if world.is_some() {
            self.note_world_released(project);
        }
        Ok((released, world))
    }

    fn release_only(&self, project: &ProjectSlug, target: &WorldTarget) -> Result<bool, EdgeError> {
        self.release_world(project, target)
            .map(|(released, _)| released)
    }

    /// Reject a request for a world this daemon does not hold (`code: unknown_world`).
    fn unknown_world(project: &ProjectSlug, target: &WorldTarget) -> EdgeError {
        EdgeError::Control(DaemonError::new(
            DaemonErrorKind::UnknownWorld,
            format!(
                "no world '{}' here; boot it with `slipfork up` or list worlds with `slipfork list`",
                world_id(project, target)
            ),
        ))
    }
}

/// Map one engine failure to a secret-free runtime diagnostic.
fn runtime_error(message: impl Into<String>) -> EdgeError {
    EdgeError::Control(DaemonError::new(DaemonErrorKind::Internal, message))
}

/// Map one `snow.yml` failure to a control error, keeping the author-facing `1 line + next:` text.
///
/// A malformed manifest is the caller's to fix, so the three author-facing kinds stay typed as
/// [`DaemonErrorKind::Rejected`]; everything else is an internal `snow.yml`/provider failure.
fn snow_error(err: SnowError) -> EdgeError {
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
fn engine_read_error(world: &str, state: WorldState, err: &EngineError) -> EdgeError {
    match (err, state) {
        (EngineError::WorldNotLive { .. }, WorldState::Failed) => runtime_error(format!(
            "world '{world}' last boot failed; run `slipfork up` to retry: {err}"
        )),
        _ => runtime_error(format!(
            "world '{world}' is attached but its engine handle is gone: {err}"
        )),
    }
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
    components: &[ResolvedComponent],
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
        if !components.iter().any(|enabled| enabled.id == component.id) {
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

fn dump_policy(request: slipfork_wire::control::DumpRequest) -> DumpPolicy {
    match request {
        slipfork_wire::control::DumpRequest::Default => DumpPolicy::Default,
        slipfork_wire::control::DumpRequest::NoDump => DumpPolicy::NoDump,
        slipfork_wire::control::DumpRequest::RefreshDump => DumpPolicy::RefreshDump,
    }
}

/// Resolve `[resources]` through the single `ResourcePlan::resolve` decision point.
fn resolve_plan(
    config: &SlipforkConfig,
    snow: &SnowFile,
    target: &WorldTarget,
    dump: DumpPolicy,
    force_local: bool,
    ospp_paired: bool,
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
    ResourcePlan::resolve(&spec, ResourceMode::from(target), dump, ospp_paired)
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
/// The seed dumps the plan selected: `(any_seed, single_digest)`.
///
/// `false` means the plan restores no dump (a clean `initdb` base); several dumps are a seed with
/// no single digest, so the digest stays `None` rather than naming one database arbitrarily.
fn selected_seed(snow: &SnowFile, plan: &ResourcePlan) -> (bool, Option<String>) {
    let has_seed = plan.pg_databases.iter().any(|database| {
        snow.resources
            .pg_databases
            .iter()
            .any(|declared| declared.id == database.id)
            && matches!(database.seed, PgSeed::RemoteDump { .. })
    });
    // OSPP v1 no longer advertises a dump digest, so the only honest lineage label is `remote`.
    (has_seed, has_seed.then(|| "remote".to_owned()))
}

/// The facts a world reuses when the base snapshot already existed on disk.
///
/// No seal ran for this world (`base.millis: None`) and no seed was rebuilt
/// (`seed.rebuilt: Some(false)`), so a restarted daemon never claims another boot's duration.
fn reused_base_lineage(key: &str, has_seed: bool, digest_hex: Option<String>) -> BootLineage {
    BootLineage {
        base: Some(BaseLineage {
            key: key.to_owned(),
            millis: None,
        }),
        seed: has_seed.then_some(SeedLineage {
            digest: digest_hex,
            rebuilt: Some(false),
            millis: None,
        }),
    }
}

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
            PgSeed::RemoteDump { .. } => hasher.update(b"remote"),
        }
        hasher.update(b"\0");
    }
    hex::encode(hasher.finalize())
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

/// The last server-side diagnostic a failed postgres readiness probe produced.
///
/// `pg_isready` reports "rejecting connections"/"no response" on stdout; a `psql`-style `FATAL`
/// lands on stderr. Prefer stderr, fall back to stdout, and never invent text.
fn readiness_detail<'a>(stdout: &'a str, stderr: &'a str) -> &'a str {
    if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    }
}

/// Poll `probe` until it reports exit code `0`, bounded by the [`PostgresReady`] policy.
///
/// `probe` yields the `pg_isready` exit code plus the last server-side diagnostic. The gate stamps
/// its own start [`std::time::Instant`] and sleeps the policy's plain [`Duration`], so a zero
/// deadline expires on the first rejection without a real 30 s wait.
fn await_postgres_ready(
    policy: &PostgresReady,
    mut probe: impl FnMut() -> Result<(i32, String), EdgeError>,
) -> Result<(), EdgeError> {
    let started = std::time::Instant::now();
    loop {
        let (exit_code, detail) = probe()?;
        if exit_code == 0 {
            return Ok(());
        }
        if started.elapsed() >= policy.deadline {
            return Err(runtime_error(format!(
                "the base seed world's postgres server did not accept queries within {}s \
                 (pg_isready exit code {exit_code}): {detail}",
                policy.deadline.as_secs(),
            )));
        }
        thread::sleep(policy.poll);
    }
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
fn service_specs(snow: &SnowFile, rootfs: &WorldRootfs) -> Result<Vec<ServiceSpec>, EdgeError> {
    let mut specs = Vec::new();
    for cluster in &snow.resources.pg_clusters {
        let image = BackingServiceKey::Postgres {
            version: cluster.version.clone(),
        };
        let id = format!("pg_clusters.{}", cluster.id);
        require_image(rootfs, &image, &id)?;
        ensure_container_dirs(rootfs, &image, &["/var/lib/postgresql/data"])?;
        specs.push(ServiceSpec {
            kind: ServiceKind::BackingService,
            id: id.clone(),
            runtime: ServiceRuntime::OciContainer {
                image,
                argv: vec!["docker-entrypoint.sh".to_owned(), "postgres".to_owned()],
                state_mounts: vec![StateSubdirMount {
                    host_subdir: PathBuf::from("pgdata").join(&cluster.id),
                    container_path: PathBuf::from("/var/lib/postgresql/data"),
                }],
                readonly_mounts: Vec::new(),
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
            kind: ServiceKind::BackingService,
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
            kind: ServiceKind::BackingService,
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

/// One component's environment: local `snow.yml` resolution first, then the server-resolved
/// values, with only **claimed** `${{ ... }}` expressions substituted locally.
fn resolve_component_env(
    snow: &SnowFile,
    provider: &dyn SecretProvider,
    coords: &ResourceCoordinates,
    component: &str,
    claims: &OsppClaims,
    server: Option<&BTreeMap<String, SecretValue>>,
    entropy: &dyn EntropySource,
) -> Result<ExecEnv, EdgeError> {
    let owns = |namespace: &str, path: &[String]| claims.owns(namespace, path);
    let evaluator = ClaimedEvaluator::new(provider, coords, entropy, &owns);
    let resolved = evaluator
        .mode(snow, WORLD_MODE, Some(component))
        .map_err(snow_error)?;
    let mut env = ExecEnv::default();
    apply_env(&mut env, resolved)?;
    let Some(server) = server else {
        return Ok(env);
    };
    for (name, value) in server {
        let rendered = evaluator.render(value.expose()).map_err(snow_error)?;
        let secret = value.is_secret() || rendered.is_secret();
        let raw = rendered.expose().to_owned();
        let outcome = if secret {
            env.set_secret(name, raw)
        } else {
            env.set(name, raw)
        };
        outcome
            .map_err(|err| runtime_error(format!("invalid OSPP environment '{name}': {err}")))?;
    }
    Ok(env)
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
    components: &[ResolvedComponent],
    hook_id: &str,
) -> Result<ResolvedHook, EdgeError> {
    let (component, phase) = parse_hook_id(hook_id).ok_or_else(|| {
        rejected(format!(
            "invalid hook id '{hook_id}' (expected <component>:<phase>)"
        ))
    })?;
    let Some(enabled) = components
        .iter()
        .find(|candidate| candidate.id == component)
    else {
        return Err(rejected(format!(
            "component '{component}' is not enabled in mode '{WORLD_MODE}'"
        )));
    };
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
        overlay: enabled.env.clone(),
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
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    #[test]
    fn up_twice_on_a_running_world_confirms_without_touching_it() {
        assert_eq!(
            boot_verb(BootCommand::Up, Some(WorldState::Running)),
            Some(BootVerb::Confirm),
            "an `up` of a running world reports `changed: false` and performs no work"
        );
    }

    #[test]
    fn up_boots_an_absent_world_and_rescues_a_failed_one_in_place() {
        assert_eq!(boot_verb(BootCommand::Up, None), Some(BootVerb::Boot));
        assert_eq!(
            boot_verb(BootCommand::Up, Some(WorldState::Failed)),
            Some(BootVerb::Rerun),
            "a failed world is rescued through the same in-place pipeline as `restart`"
        );
        assert_eq!(
            boot_verb(BootCommand::Up, Some(WorldState::Failed)),
            boot_verb(BootCommand::Restart, Some(WorldState::Failed)),
        );
    }

    #[test]
    fn restart_rescues_a_failed_world_and_reruns_a_running_one() {
        assert_eq!(
            boot_verb(BootCommand::Restart, Some(WorldState::Failed)),
            Some(BootVerb::Rerun)
        );
        assert_eq!(
            boot_verb(BootCommand::Restart, Some(WorldState::Running)),
            Some(BootVerb::Rerun)
        );
        assert_eq!(
            boot_verb(BootCommand::Restart, Some(WorldState::Stopped)),
            None,
            "a stopped world has nothing to stop: `restart` refuses it (§4)"
        );
    }

    #[test]
    fn a_rerun_refuses_a_policy_only_a_fresh_boot_can_produce() {
        let held = BootPolicy {
            dump: DumpPolicy::Default,
            force_local_resources: false,
            host_caches: false,
        };
        assert!(check_rerun_policy("acme", held, held).is_ok());
        assert!(
            check_rerun_policy(
                "acme",
                held,
                BootPolicy {
                    force_local_resources: true,
                    ..held
                }
            )
            .is_ok(),
            "`--resources local` re-resolves in place"
        );

        // A bare `up` of a held world keeps the policy it booted with.
        let booted = BootPolicy {
            dump: DumpPolicy::NoDump,
            force_local_resources: true,
            host_caches: true,
        };
        let bare = rerun_policy(
            booted,
            slipfork_wire::control::DumpRequest::Default,
            slipfork_wire::control::ResourceRequest::Default,
            false,
        );
        assert_eq!(bare, booted);
        assert!(check_rerun_policy("acme", booted, bare).is_ok());

        // An explicit flag a rerun cannot apply is carried through and refused, never dropped.
        let explicit = rerun_policy(
            held,
            slipfork_wire::control::DumpRequest::NoDump,
            slipfork_wire::control::ResourceRequest::Local,
            true,
        );
        assert_eq!(explicit.dump, DumpPolicy::NoDump);
        assert!(explicit.force_local_resources);
        for (policy, needle) in [
            (
                BootPolicy {
                    dump: DumpPolicy::RefreshDump,
                    ..held
                },
                "dump policy re-seals the base",
            ),
            (
                BootPolicy {
                    host_caches: true,
                    ..held
                },
                "--host-caches` rebuilds it",
            ),
        ] {
            let error = check_rerun_policy("acme", held, policy)
                .expect_err("a fresh-boot-only fact is refused in place");
            let message = error.to_string();
            assert!(message.contains(needle), "got: {message}");
            assert!(
                message.contains("run `slipfork down --world acme` first"),
                "the refusal must name the next command, got: {message}"
            );
        }
    }

    #[test]
    fn restart_on_a_never_booted_world_fails_closed_naming_up() {
        assert_eq!(
            boot_verb(BootCommand::Restart, None),
            None,
            "`restart` never boots a world the caller believed was running"
        );
        let project = ProjectSlug::parse("acme").expect("valid project slug");
        match not_running(&project, &WorldTarget::Canonical) {
            EdgeError::Control(error) => {
                assert_eq!(error.kind, DaemonErrorKind::NotRunning);
                assert_eq!(
                    error.message,
                    "world 'acme' is not running; boot it with `slipfork up`"
                );
            }
            other => panic!("expected a typed `not_running` refusal, got {other:?}"),
        }
    }

    #[test]
    fn restarted_ids_are_the_engine_rows_this_run_brought_to_running() {
        fn row(world: &str, id: &str, state: slipfork_engine::ServiceState) -> WorldServiceSummary {
            WorldServiceSummary {
                world: world.to_owned(),
                info: slipfork_wire::control::WorldServiceInfo {
                    id: id.to_owned(),
                    kind: ServiceKind::Component,
                    state,
                    pid: None,
                    port: None,
                    uptime_secs: None,
                },
            }
        }
        use slipfork_engine::ServiceState;

        let stopped = vec![
            row("acme", "pg_databases.main", ServiceState::Stopped),
            row("acme", "api", ServiceState::Stopped),
        ];
        let running = vec![
            row("acme", "pg_databases.main", ServiceState::Running),
            row("acme", "api", ServiceState::Running),
        ];

        // After the stop every row is `Stopped`; what is `Running` afterwards is what restarted.
        assert_eq!(
            restarted_service_ids(&stopped, &running),
            vec!["pg_databases.main", "api"]
        );
        // An idempotent run changes nothing.
        assert!(restarted_service_ids(&running, &running).is_empty());
        // An `exited` row is never named beside `restarted`.
        let mut half = running.clone();
        half[1].info.state = ServiceState::Exited;
        assert_eq!(
            restarted_service_ids(&stopped, &half),
            vec!["pg_databases.main"]
        );
        // A service that had crashed before the run and came back counts as restarted.
        let crashed = vec![
            row("acme", "pg_databases.main", ServiceState::Exited),
            row("acme", "api", ServiceState::Stopped),
        ];
        assert_eq!(
            restarted_service_ids(&crashed, &running),
            vec!["pg_databases.main", "api"]
        );
        // Duplicate rows never duplicate an id.
        let mut doubled = running.clone();
        doubled.push(row("acme", "api", ServiceState::Running));
        assert_eq!(
            restarted_service_ids(&stopped, &doubled),
            vec!["pg_databases.main", "api"]
        );
    }

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
    fn postgres_readiness_gate_returns_on_the_first_accepted_probe() {
        let policy = PostgresReady {
            deadline: Duration::from_secs(1),
            poll: Duration::from_millis(1),
        };
        await_postgres_ready(&policy, || Ok((0, String::new())))
            .expect("a postmaster that answers is ready");
    }

    #[test]
    fn postgres_readiness_gate_fails_closed_at_the_deadline() {
        let policy = PostgresReady {
            deadline: Duration::ZERO,
            poll: Duration::ZERO,
        };
        let mut probes = 0;
        let error = await_postgres_ready(&policy, || {
            probes += 1;
            Ok((2, "FATAL:  the database system is starting up".to_owned()))
        })
        .expect_err("a postmaster that never accepts must fail closed");
        assert_eq!(probes, 1, "a zero deadline fails on the first rejection");
        let message = error.to_string();
        assert!(message.contains("within 0s"), "got: {message}");
        assert!(message.contains("starting up"), "got: {message}");
    }

    #[test]
    fn postgres_readiness_gate_polls_until_the_real_deadline() {
        let policy = PostgresReady {
            deadline: Duration::from_millis(120),
            poll: Duration::from_millis(10),
        };
        let mut probes = 0;
        let started = std::time::Instant::now();
        let error = await_postgres_ready(&policy, || {
            probes += 1;
            Ok((2, "FATAL:  the database system is starting up".to_owned()))
        })
        .expect_err("a postmaster that never accepts must fail closed");
        let elapsed = started.elapsed();
        assert!(
            probes >= 2,
            "the gate polls again after each rejection (probes: {probes})"
        );
        assert!(
            elapsed >= Duration::from_millis(120) && elapsed < Duration::from_millis(500),
            "the gate waits the policy deadline in real time (elapsed {elapsed:?}): {error}"
        );
        assert!(
            error.to_string().contains("starting up"),
            "the last server diagnostic survives the deadline: {error}"
        );
    }

    #[test]
    fn readiness_detail_prefers_stderr_and_falls_back_to_stdout() {
        assert_eq!(
            readiness_detail(
                "127.0.0.1:5432 - rejecting connections\n",
                "FATAL:  the database system is starting up\n",
            ),
            "FATAL:  the database system is starting up"
        );
        assert_eq!(
            readiness_detail("127.0.0.1:5432 - rejecting connections\n", ""),
            "127.0.0.1:5432 - rejecting connections"
        );
        assert_eq!(readiness_detail("", "  \n"), "");
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

    /// The world every restart test boots: one enabled component, one pre-deploy hook, one
    /// ingress, and one `modes.local` env value — each editable between runs.
    fn restart_snow(port: u16, hook: &str, greeting: &str) -> String {
        format!(
            r#"
images:
  - id: app
    context: .
    dockerfile: Dockerfile

components:
  - id: api
    image: app
    envs: [app]
    run_command: "exec sleep 300"

ingresses:
  - match:
      prefix: "/"
    target:
      component: api
      port: {port}

hooks:
  - component: api
    when: pre-deploy
    run_command: "{hook}"

modes:
  local:
    envs:
      app:
        GREETING: '{greeting}'
"#
        )
    }

    /// A short, disk-backed scratch root.
    ///
    /// Never `/tmp` (tmpfs: the seed-dump store refuses it) and never the repository path (the
    /// daemon's ingress socket must stay under the 107-byte `AF_UNIX` budget).
    fn disk_scratch(label: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let root = PathBuf::from("/var/tmp").join("sf-m32817").join(format!(
            "{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("scratch dir");
        root
    }

    /// An engine double that models handles, the supervision rows, and every call the daemon makes.
    ///
    /// It returns handles through the public `WorldEngine::mint_service_handle` seam and keeps its
    /// own `WorldServiceInfo` rows, so `world_processes` is the record the daemon reads back (never
    /// the daemon's own handle list). It records the spawn environments and hook argv, which is how
    /// the test proves an edited `snow.yml` is what the world runs.
    struct RecordingEngine {
        live: Mutex<Vec<WorldHandle>>,
        snapshots: Mutex<Vec<SnapshotId>>,
        next_seq: AtomicU64,
        spawns: AtomicUsize,
        world_stops: AtomicUsize,
        destroyed: AtomicUsize,
        hooks: Mutex<Vec<Vec<String>>>,
        greetings: Mutex<Vec<Option<String>>>,
        rows: Mutex<Vec<RecordedRow>>,
        /// The reload failure shape the next `reload_world` produces, if any.
        reload_fault: Mutex<Option<ReloadFault>>,
        /// Handles a reload tore down and stamped non-live (`WorldStatus::Failed`): still owned,
        /// no live PID 1, and every read fails closed with `WorldNotLive`.
        torndown: Mutex<Vec<WorldHandle>>,
        /// Handles a reload tore down while its record still reads `Live` — the real engine's
        /// window between reaping the PID 1 and stamping `Failed`, where only `world_is_live` can
        /// tell the truth.
        init_gone: Mutex<Vec<WorldHandle>>,
    }

    /// The reload failure shapes the real engine distinguishes, and the daemon must not confuse.
    #[derive(Clone, Copy)]
    enum ReloadFault {
        /// A pre-flight refusal: nothing was stopped or re-rooted, the world stays live.
        Refused,
        /// A boot failure past the gate: every service stopped and the record stamped
        /// `WorldStatus::Failed`.
        AfterTheGate,
        /// A teardown failure past the gate: the PID 1 is gone but the record is not stamped
        /// `Failed` yet, so its lineage still reads and only `world_is_live` says it is dead.
        TeardownBeforeStamp,
    }

    struct RecordedRow {
        world: WorldHandle,
        id: String,
        state: ServiceState,
    }

    /// One environment value an `ExecEnv` resolves to (`None` when the name is absent).
    fn env_value(env: &ExecEnv, name: &str) -> Option<String> {
        env.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.into_owned())
    }

    impl RecordingEngine {
        fn new() -> Self {
            Self {
                live: Mutex::new(Vec::new()),
                snapshots: Mutex::new(Vec::new()),
                next_seq: AtomicU64::new(0),
                spawns: AtomicUsize::new(0),
                world_stops: AtomicUsize::new(0),
                destroyed: AtomicUsize::new(0),
                hooks: Mutex::new(Vec::new()),
                greetings: Mutex::new(Vec::new()),
                rows: Mutex::new(Vec::new()),
                reload_fault: Mutex::new(None),
                torndown: Mutex::new(Vec::new()),
                init_gone: Mutex::new(Vec::new()),
            }
        }

        /// `(spawn_service, stop_world_services, hooks run)` — the daemon's supervision footprint.
        fn counts(&self) -> (usize, usize, usize) {
            (
                self.spawns.load(Ordering::SeqCst),
                self.world_stops.load(Ordering::SeqCst),
                self.hooks.lock().expect("hook records").len(),
            )
        }

        /// The double owns `world` (it was minted here and never released).
        fn owns(&self, world: &WorldHandle) -> Result<(), EngineError> {
            let live = self.live.lock().expect("live worlds");
            if live.contains(world) {
                Ok(())
            } else {
                Err(EngineError::WorldGone {
                    world: format!("{world:?}"),
                })
            }
        }

        /// The double owns `world` and its last reload did not fail: a read that needs a live world
        /// fails closed with `WorldNotLive` for a record stamped `Failed`, exactly like the engine.
        fn live_world(&self, world: &WorldHandle) -> Result<(), EngineError> {
            self.owns(world)?;
            let torndown = self
                .torndown
                .lock()
                .expect("torndown worlds")
                .contains(world);
            if torndown {
                return Err(EngineError::WorldNotLive {
                    world: format!("{world:?}"),
                });
            }
            Ok(())
        }

        /// Arm the failure shape the next `reload_world` produces.
        fn fail_next_reload(&self, fault: ReloadFault) {
            *self.reload_fault.lock().expect("reload fault") = Some(fault);
        }
    }

    impl WorldEngine for RecordingEngine {
        fn name(&self) -> &'static str {
            "recording"
        }

        fn create_snapshot(&self, _source_dir: &Path, id: &SnapshotId) -> Result<(), EngineError> {
            self.snapshots.lock().expect("snapshots").push(id.clone());
            Ok(())
        }

        fn snapshot_exists(&self, _id: &SnapshotId) -> Result<bool, EngineError> {
            // Every base is already sealed: the seeding pipeline is never entered by this double.
            Ok(true)
        }

        fn freeze_world(&self, _world: &WorldHandle, _id: &SnapshotId) -> Result<(), EngineError> {
            Err(EngineError::Unsupported {
                engine: self.name(),
                capability: "freeze_world",
            })
        }

        fn clone_world(&self, spec: &WorldSpec) -> Result<WorldHandle, EngineError> {
            spec.validate()?;
            let seq = self.next_seq.fetch_add(1, Ordering::SeqCst) + 1;
            let handle = self.mint_world_handle(seq);
            self.live.lock().expect("live worlds").push(handle.clone());
            Ok(handle)
        }

        fn spawn_service(
            &self,
            world: &WorldHandle,
            spec: &ServiceSpec,
        ) -> Result<ServiceHandle, EngineError> {
            self.live_world(world)?;
            self.spawns.fetch_add(1, Ordering::SeqCst);
            self.greetings
                .lock()
                .expect("spawn envs")
                .push(env_value(&spec.env, "GREETING"));
            {
                let mut rows = self.rows.lock().expect("rows");
                rows.retain(|row| !(row.world == *world && row.id == spec.id));
                rows.push(RecordedRow {
                    world: world.clone(),
                    id: spec.id.clone(),
                    state: ServiceState::Running,
                });
            }
            Ok(self.mint_service_handle(
                world,
                &spec.id,
                Arc::new(std::sync::atomic::AtomicU8::new(
                    ServiceState::Running.to_bits(),
                )),
            ))
        }

        fn stop_service(&self, _service: &ServiceHandle) -> Result<(), EngineError> {
            Err(EngineError::Unsupported {
                engine: self.name(),
                capability: "stop_service",
            })
        }

        fn stop_world_services(&self, world: &WorldHandle) -> Result<(), EngineError> {
            self.live_world(world)?;
            self.world_stops.fetch_add(1, Ordering::SeqCst);
            for row in self.rows.lock().expect("rows").iter_mut() {
                if row.world == *world {
                    row.state = ServiceState::Stopped;
                }
            }
            Ok(())
        }

        /// The real engine's reload contract: the handle survives, the services do not — and a
        /// failure either refuses before any mutation or leaves the record non-live.
        fn reload_world(&self, world: &WorldHandle, spec: &WorldSpec) -> Result<(), EngineError> {
            spec.validate()?;
            self.live_world(world)?;
            let fault = self.reload_fault.lock().expect("reload fault").take();
            if let Some(ReloadFault::Refused) = fault {
                return Err(EngineError::SnapshotNotFound("refused".to_owned()));
            }
            self.world_stops.fetch_add(1, Ordering::SeqCst);
            self.rows
                .lock()
                .expect("rows")
                .retain(|row| row.world != *world);
            if let Some(ReloadFault::AfterTheGate) = fault {
                self.torndown
                    .lock()
                    .expect("torndown worlds")
                    .push(world.clone());
                return Err(EngineError::WorldInitFailed {
                    world: format!("{world:?}"),
                    reason: "the world PID 1 never became ready".to_owned(),
                });
            }
            if let Some(ReloadFault::TeardownBeforeStamp) = fault {
                // The recorded messages of a stop that burned its grace: the daemon must reach the
                // same verdict from the engine's liveness answer alone, never by reading this text.
                self.init_gone
                    .lock()
                    .expect("dead init worlds")
                    .push(world.clone());
                return Err(EngineError::Execution(
                    "service 'api' survived SIGTERM and SIGKILL for 70.0s: pid 4242 is still in \
                     /proc state 'D'"
                        .to_owned(),
                ));
            }
            Ok(())
        }

        fn service_logs(
            &self,
            _world: &WorldHandle,
            _id: &str,
        ) -> Result<slipfork_engine::ServiceLogs, EngineError> {
            Err(EngineError::Unsupported {
                engine: self.name(),
                capability: "service_logs",
            })
        }

        fn world_processes(
            &self,
            world: &WorldHandle,
        ) -> Result<Vec<slipfork_engine::WorldServiceInfo>, EngineError> {
            self.live_world(world)?;
            Ok(self
                .rows
                .lock()
                .expect("rows")
                .iter()
                .filter(|row| row.world == *world)
                .map(|row| slipfork_engine::WorldServiceInfo {
                    id: row.id.clone(),
                    kind: ServiceKind::Component,
                    state: row.state,
                    pid: None,
                    port: None,
                    uptime_secs: None,
                })
                .collect())
        }

        fn world_lineage(
            &self,
            world: &WorldHandle,
        ) -> Result<slipfork_engine::WorldLineage, EngineError> {
            self.live_world(world)?;
            Ok(slipfork_engine::WorldLineage {
                rootfs: None,
                services: Vec::new(),
                seed: None,
                base: None,
            })
        }

        fn world_is_live(&self, world: &WorldHandle) -> Result<bool, EngineError> {
            // Ownership decides "gone"; the two teardown shapes decide "not live". The double keeps
            // no PID 1, so it models the engine's answer directly.
            self.owns(world)?;
            let torndown = self
                .torndown
                .lock()
                .expect("torndown worlds")
                .contains(world);
            let init_gone = self
                .init_gone
                .lock()
                .expect("dead init worlds")
                .contains(world);
            Ok(!torndown && !init_gone)
        }

        fn exec_in_service(
            &self,
            _service: &ServiceHandle,
            _argv: &[String],
            _overlay_env: &ExecEnv,
            _on_output: &mut dyn FnMut(StreamChunk),
        ) -> Result<ExecOutput, EngineError> {
            Err(EngineError::Unsupported {
                engine: self.name(),
                capability: "exec_in_service",
            })
        }

        fn service_entry_command(
            &self,
            _service: &ServiceHandle,
        ) -> Result<slipfork_engine::WorldEntryCommand, EngineError> {
            Err(EngineError::Unsupported {
                engine: self.name(),
                capability: "service_entry_command",
            })
        }

        fn exec_in_world(
            &self,
            world: &WorldHandle,
            argv: &[String],
            _cwd: &Path,
            _overlay_env: &ExecEnv,
            _on_output: &mut dyn FnMut(StreamChunk),
        ) -> Result<ExecOutput, EngineError> {
            self.live_world(world)?;
            self.hooks.lock().expect("hook records").push(argv.to_vec());
            Ok(ExecOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }

        fn entry_command(
            &self,
            _world: &WorldHandle,
            _cwd: &Path,
        ) -> Result<slipfork_engine::WorldEntryCommand, EngineError> {
            Err(EngineError::Unsupported {
                engine: self.name(),
                capability: "entry_command",
            })
        }

        fn dial_world_tcp(
            &self,
            world: &WorldHandle,
            port: u16,
        ) -> Result<std::net::TcpStream, EngineError> {
            Err(EngineError::DialFailed {
                world: format!("{world:?}"),
                port,
                errno: 111,
            })
        }

        fn destroy_world(&self, world: WorldHandle) -> Result<(), EngineError> {
            let mut live = self.live.lock().expect("live worlds");
            let index = live
                .iter()
                .position(|candidate| candidate == &world)
                .ok_or_else(|| EngineError::WorldGone {
                    world: format!("{world:?}"),
                })?;
            live.remove(index);
            self.rows
                .lock()
                .expect("rows")
                .retain(|row| row.world != world);
            self.destroyed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// A rootfs builder double: the pipeline never shells out to Docker in a unit test.
    struct StubRootfs;

    impl BaseImageBuilder for StubRootfs {
        fn materialize(
            &self,
            _req: &slipfork_engine::rootfs::WorldRootfsRequest,
            _on_event: &mut dyn FnMut(slipfork_engine::rootfs::RootfsBuildEvent),
        ) -> Result<WorldRootfs, slipfork_engine::rootfs::BaseImageError> {
            Ok(WorldRootfs {
                digest: slipfork_engine::rootfs::RootfsDigest::from_hex("e".repeat(64))
                    .expect("valid digest"),
                app_rootfs: PathBuf::from("/nonexistent/m3-28-17/app"),
                crun_bin: PathBuf::from("/.slipfork/bin/crun"),
                service_images: BTreeMap::new(),
                metadata: slipfork_engine::rootfs::ImageContractMetadata {
                    os_release: slipfork_engine::rootfs::OsRelease {
                        id: "alpine".to_owned(),
                        version_id: "3.21".to_owned(),
                        version_codename: String::new(),
                    },
                    elf_abi: slipfork_engine::rootfs::ElfAbi {
                        arch: "x86_64".to_owned(),
                        libc: slipfork_engine::rootfs::LibcFamily::Musl,
                        libc_version: "1.2".to_owned(),
                    },
                    exposed_ports: BTreeSet::new(),
                },
                host_caches: Vec::new(),
                cache: slipfork_engine::CacheOutcome::Miss,
                millis: None,
            })
        }
    }

    /// One fixed repository: the daemon's `cwd`-resolution seam without shelling out to `git`.
    struct FixedRepo {
        repo: RepoRoot,
    }

    impl RepoRootResolver for FixedRepo {
        fn resolve(&self, _start: &Path) -> Result<RepoRoot, slipfork_vcs::VcsError> {
            Ok(self.repo.clone())
        }

        fn remote(&self, _repo: &RepoRoot) -> Result<Option<String>, slipfork_vcs::VcsError> {
            Ok(None)
        }
    }

    /// Build the real `DaemonService` over doubled engine/rootfs/repository, the real registry,
    /// vault, and workspace factory.
    fn test_service(
        scratch: &Path,
        worktree: &Path,
    ) -> (Arc<DaemonService>, Arc<WorldRegistry>, Arc<RecordingEngine>) {
        let layout = Layout::explicit(scratch.join("state"), scratch.join("run"), scratch, None);
        let engine = Arc::new(RecordingEngine::new());
        let registry = Arc::new(WorldRegistry::new(engine.clone(), 80));
        let service = DaemonService::new(
            DaemonPorts {
                engine: engine.clone(),
                builder: Arc::new(StubRootfs),
                registry: registry.clone(),
                repo_roots: Arc::new(FixedRepo {
                    repo: RepoRoot {
                        vcs_root: worktree.to_path_buf(),
                        worktree_root: worktree.to_path_buf(),
                        project: ProjectSlug::parse("acme").expect("valid project slug"),
                    },
                }),
                workspaces: Arc::new(RepositoryWorkspaces::new()),
                vaults: Arc::new(LayoutProjectVaults::new(layout.clone())),
                ospp: None,
                signer: Arc::new(slipfork_ospp_testkit::signers::software_signer()),
                hostname: "test-machine".to_owned(),
                browser: Browser::none(),
                entropy: Arc::new(OsCsprngEntropy),
            },
            DaemonConfig {
                layout,
                inherited: Vec::new(),
                runtime: RuntimeAssets {
                    crun: PathBuf::from("/nonexistent/crun"),
                    world_init: PathBuf::from("/nonexistent/slipfork-world-init"),
                },
                target_arch: "x86_64".to_owned(),
                uid: 1000,
                gid: 1000,
                host_caches: HostCaches::default(),
                postgres_ready: PostgresReady::production(),
            },
            IngressInfo::new(vec![SocketAddr::from(([127, 0, 0, 1], 80))]).expect("ingress"),
            None,
        );
        (Arc::new(service), registry, engine)
    }

    #[test]
    fn restart_applies_an_edited_snow_file_and_a_second_up_touches_nothing() {
        let scratch = disk_scratch("restart");
        let worktree = scratch.join("worktree");
        fs::create_dir_all(&worktree).expect("worktree");
        fs::write(
            worktree.join("snow.yml"),
            restart_snow(7000, "echo one", "one"),
        )
        .expect("snow.yml");
        let (service, registry, engine) = test_service(&scratch, &worktree);

        let request = UpRequest {
            cwd: worktree.clone(),
            world: None,
            dump: slipfork_wire::control::DumpRequest::Default,
            resources: slipfork_wire::control::ResourceRequest::Default,
            host_caches: false,
        };
        let up = service.up(&request, &mut |_| {}).expect("boot the world");
        assert_eq!(up.world, "acme");
        assert_eq!(up.state, WorldState::Running);
        assert!(up.changed, "a fresh boot created the world");
        assert_eq!(
            up.restarted,
            vec!["api".to_owned()],
            "the boot started `api`, read back from the engine's rows"
        );
        assert_eq!(
            engine.counts(),
            (1, 0, 1),
            "one spawn and one pre-deploy hook, no world stop"
        );
        assert_eq!(route_port(&registry, "acme.localhost"), 7000);

        // A second `up` on the running world reports no change and performs no engine work.
        let again = service.up(&request, &mut |_| {}).expect("idempotent up");
        assert!(!again.changed);
        assert!(again.restarted.is_empty());
        assert_eq!(
            engine.counts(),
            (1, 0, 1),
            "the idempotent `up` must issue no spawn, stop, or hook call"
        );

        // The edit a `restart` must apply: a new ingress port, a new hook body, a new env value.
        fs::write(
            worktree.join("snow.yml"),
            restart_snow(7100, "echo two", "two"),
        )
        .expect("edited snow.yml");
        let restarted = service
            .restart(
                &WorldSelector {
                    cwd: worktree.clone(),
                    world: None,
                },
                &mut |_| {},
            )
            .expect("restart");
        assert_eq!(restarted.state, WorldState::Running);
        assert!(
            restarted.changed,
            "`api` came back and the hook ran: the restart changed the world"
        );
        let running: Vec<String> = restarted
            .services
            .iter()
            .filter(|row| row.info.state == ServiceState::Running)
            .map(|row| row.info.id.clone())
            .collect();
        assert_eq!(
            restarted.restarted, running,
            "`restarted` is exactly the engine's post-reboot `Running` set"
        );
        assert_eq!(restarted.restarted, vec!["api".to_owned()]);
        assert_eq!(
            route_port(&registry, "acme.localhost"),
            7100,
            "restart must re-attach the edited `ingresses:` table"
        );
        assert_eq!(
            engine.counts(),
            (2, 1, 2),
            "one world stop, one respawn, and the hook ran again"
        );
        assert_eq!(
            engine.hooks.lock().expect("hook records")[1].last(),
            Some(&"echo two".to_owned()),
            "the edited hook body is what ran on restart"
        );
        assert_eq!(
            engine.greetings.lock().expect("spawn envs")[1],
            Some("two".to_owned()),
            "the edited env value reached the respawned component"
        );
        assert_eq!(
            engine.destroyed.load(Ordering::SeqCst),
            0,
            "restart never destroys the world handle (the CoW state survives)"
        );
        assert_eq!(
            engine.spawns.load(Ordering::SeqCst),
            2,
            "the daemon never stopped `api` through its own handle"
        );

        service
            .down(&WorldSelector {
                cwd: worktree,
                world: None,
            })
            .expect("down");
        let _ = fs::remove_dir_all(&scratch);
    }

    /// A reload the engine refuses at its pre-flight gate leaves the world exactly as it was: the
    /// daemon record stays `Running` with its services bound, and `ps` still reads them.
    #[test]
    fn a_refused_reload_keeps_the_running_world_and_its_service_rows() {
        let scratch = disk_scratch("reload-refused");
        let worktree = scratch.join("worktree");
        fs::create_dir_all(&worktree).expect("worktree");
        fs::write(
            worktree.join("snow.yml"),
            restart_snow(7000, "echo one", "one"),
        )
        .expect("snow.yml");
        let (service, _registry, engine) = test_service(&scratch, &worktree);
        let selector = WorldSelector {
            cwd: worktree.clone(),
            world: None,
        };
        service
            .up(
                &UpRequest {
                    cwd: worktree.clone(),
                    world: None,
                    dump: slipfork_wire::control::DumpRequest::Default,
                    resources: slipfork_wire::control::ResourceRequest::Default,
                    host_caches: false,
                },
                &mut |_| {},
            )
            .expect("boot the world");

        // The engine's pre-flight refusal (a pruned base, a malformed spec, a sealing world) stops
        // nothing: the world is still live behind the same handle.
        engine.fail_next_reload(ReloadFault::Refused);
        let refused = service
            .restart(&selector, &mut |_| {})
            .expect_err("the engine refused the reload");
        assert!(
            refused.to_string().contains("cannot re-run world 'acme'"),
            "the refusal reaches the user verbatim: {refused}"
        );
        assert_eq!(
            service.list(None).expect("list")[0].state,
            WorldState::Running,
            "a refusal that mutated nothing must never fail a healthy world"
        );
        let processes = service
            .world_processes(&selector)
            .expect("`ps` reads a world this refusal never touched");
        assert_eq!(processes.state, WorldState::Running);
        assert_eq!(
            processes
                .services
                .iter()
                .map(|row| (row.info.id.as_str(), row.info.state))
                .collect::<Vec<_>>(),
            vec![("api", ServiceState::Running)],
            "the refused reload left the component's own row running"
        );

        let _ = fs::remove_dir_all(&scratch);
    }

    /// A reload that dies *past* the gate stopped the services and tore the world down: the record
    /// is `Failed`, and `ps` names the failed boot and its retry instead of a lost handle.
    #[test]
    fn a_reload_failure_past_the_gate_fails_the_world_and_ps_names_the_retry() {
        let scratch = disk_scratch("reload-torndown");
        let worktree = scratch.join("worktree");
        fs::create_dir_all(&worktree).expect("worktree");
        fs::write(
            worktree.join("snow.yml"),
            restart_snow(7000, "echo one", "one"),
        )
        .expect("snow.yml");
        let (service, _registry, engine) = test_service(&scratch, &worktree);
        let selector = WorldSelector {
            cwd: worktree.clone(),
            world: None,
        };
        service
            .up(
                &UpRequest {
                    cwd: worktree.clone(),
                    world: None,
                    dump: slipfork_wire::control::DumpRequest::Default,
                    resources: slipfork_wire::control::ResourceRequest::Default,
                    host_caches: false,
                },
                &mut |_| {},
            )
            .expect("boot the world");

        engine.fail_next_reload(ReloadFault::AfterTheGate);
        let failed = service
            .restart(&selector, &mut |_| {})
            .expect_err("the reload died after its gate");
        assert!(
            failed.to_string().contains("cannot re-run world 'acme'"),
            "the boot failure reaches the user: {failed}"
        );
        assert_eq!(
            service
                .world_facts(
                    &ProjectSlug::parse("acme").expect("project slug"),
                    &WorldTarget::Canonical,
                )
                .expect("the failed world stays in the index")
                .state,
            WorldState::Failed,
            "a reload that stopped every service leaves a failed world record"
        );
        let err = service
            .world_processes(&selector)
            .expect_err("a world with no live PID 1 has no rows to read");
        let error = DaemonError::from(&err);
        assert_eq!(error.kind, DaemonErrorKind::Internal);
        assert!(
            error
                .message
                .contains("last boot failed; run `slipfork up` to retry"),
            "`ps` names the failed boot and the retry: {}",
            error.message
        );
        assert!(
            !error.message.contains("engine handle is gone"),
            "the daemon still holds this handle: {}",
            error.message
        );

        let _ = fs::remove_dir_all(&scratch);
    }

    /// A reload that dies after it reaped the world PID 1 but before the engine stamps `Failed`
    /// leaves a record that still reads `Live` — the exact shape the old lineage-as-liveness check
    /// mistook for a healthy world. The daemon must mark it `Failed`, never `Running`.
    #[test]
    fn a_reload_failure_that_already_reaped_the_world_init_fails_the_world() {
        let scratch = disk_scratch("reload-init-gone");
        let worktree = scratch.join("worktree");
        fs::create_dir_all(&worktree).expect("worktree");
        fs::write(
            worktree.join("snow.yml"),
            restart_snow(7000, "echo one", "one"),
        )
        .expect("snow.yml");
        let (service, _registry, engine) = test_service(&scratch, &worktree);
        let selector = WorldSelector {
            cwd: worktree.clone(),
            world: None,
        };
        service
            .up(
                &UpRequest {
                    cwd: worktree.clone(),
                    world: None,
                    dump: slipfork_wire::control::DumpRequest::Default,
                    resources: slipfork_wire::control::ResourceRequest::Default,
                    host_caches: false,
                },
                &mut |_| {},
            )
            .expect("boot the world");

        engine.fail_next_reload(ReloadFault::TeardownBeforeStamp);
        let failed = service
            .restart(&selector, &mut |_| {})
            .expect_err("the reload died while tearing the world down");
        assert!(
            failed.to_string().contains("cannot re-run world 'acme'"),
            "the teardown failure reaches the user: {failed}"
        );

        let facts = service
            .world_facts(
                &ProjectSlug::parse("acme").expect("project slug"),
                &WorldTarget::Canonical,
            )
            .expect("the failed world stays in the index");
        assert_eq!(
            facts.state,
            WorldState::Failed,
            "a reload whose world PID 1 is gone must never leave the world `Running`"
        );
        // The bug's precondition, pinned: the engine still answers a record read while the world is
        // dead, so only `world_is_live` may decide liveness.
        assert!(
            engine.world_lineage(&facts.handle).is_ok(),
            "this shape must still read as a record, or it would not reproduce the bug"
        );
        assert!(
            !engine
                .world_is_live(&facts.handle)
                .expect("the engine answers its own liveness"),
            "the reload reaped the world PID 1"
        );

        let _ = fs::remove_dir_all(&scratch);
    }

    /// The port the registry serves for one `*.localhost` host at `/`.
    fn route_port(registry: &WorldRegistry, host: &str) -> u16 {
        registry.resolve(host, "/").expect("attached route").port()
    }
}
