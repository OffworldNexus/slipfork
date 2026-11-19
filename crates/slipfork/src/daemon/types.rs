//! Plain data the daemon index, boot pipeline, and protocol share.

use slipfork_dag::{DumpPolicy, ResourcePlan};
use slipfork_edge::{EdgeError, IngressRoute};
use slipfork_forkfile::TaintedString;
use slipfork_ospp::{OsppClaims, OsppSession};
use slipfork_snow::{ResourceCoordinates, SecretProvider, SnowFile};
use slipfork_vcs::{RepoRoot, VcsWorkspace};
use slipfork_wire::control::{ResourceSummary, WorldServiceSummary, WorldState};
use slipfork_world::rootfs::WorldRootfs;
use slipfork_world::{
    BootLineage, ExecEnv, ProjectSlug, ServiceHandle, SnapshotId, StreamChunk, WorldHandle,
    WorldTarget,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

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
pub(super) struct LiveWorld {
    pub(super) project: ProjectSlug,
    pub(super) target: WorldTarget,
    pub(super) repo: RepoRoot,
    pub(super) worktree: PathBuf,
    pub(super) handle: WorldHandle,
    pub(super) state: WorldState,
    /// Resolved world environment (re-derived by `resolve_world`).
    pub(super) base_env: ExecEnv,
    /// The one enabled-component list: id, run command, readiness port, and environment
    /// (re-derived by `resolve_world`; `start_world` and the hook paths never re-resolve it).
    pub(super) components: Vec<ResolvedComponent>,
    /// The policy this world was booted (or last re-run) with; a later `up` may not silently ask
    /// for a different dump policy or rootfs build on top of the retained CoW state.
    pub(super) policy: BootPolicy,
    /// Resources coordinates for `resources::*`/`deployment::*` values, re-derived with the plan.
    pub(super) coords: ResourceCoordinates,
    pub(super) snow: SnowFile,
    /// The resource plan this run resolved (kept so `env_updated` can re-derive ID-only claims).
    pub(super) plan: ResourcePlan,
    /// The last decrypted server environment applied to this world (never persisted).
    pub(super) ospp_env: Option<BTreeMap<String, BTreeMap<String, TaintedString>>>,
    /// The `L0` rootfs this world is currently rooted in: `restart` replaces it (and its
    /// subvolume, when the base changed) only when the resolver's digest changed.
    pub(super) rootfs: WorldRootfs,
    pub(super) resources: Vec<ResourceSummary>,
    pub(super) services: Vec<ServiceHandle>,
    /// Retained hook logs by `<component>:<phase>` (`slipfork logs <hook_id>`, `ADR-0008` §4).
    pub(super) hook_logs: BTreeMap<String, Arc<HookLog>>,
    pub(super) workspace: Option<Arc<dyn VcsWorkspace>>,
}

/// The one daemon-held `ospp-v1` session and its stable per-project subscriptions.
#[derive(Default)]
pub(super) struct OsppRuntime {
    pub(super) session: Option<Arc<dyn OsppSession>>,
    /// Subscription id -> project. One subscription per project while its worlds run.
    pub(super) subscriptions: BTreeMap<String, ProjectSlug>,
}

/// The daemon's own record of one held world, copied out of the index so engine reads never run
/// under the state mutex.
pub(super) struct WorldFacts {
    pub(super) state: WorldState,
    pub(super) worktree: PathBuf,
    pub(super) resources: Vec<ResourceSummary>,
    pub(super) handle: WorldHandle,
}

impl WorldFacts {
    pub(super) fn of(world: &LiveWorld) -> Self {
        Self {
            state: world.state,
            worktree: world.worktree.clone(),
            resources: world.resources.clone(),
            handle: world.handle.clone(),
        }
    }
}

/// One `slipfork logs` target: an engine-retained service log or a daemon-retained hook log.
pub(super) enum RetainedTarget {
    Service(String),
    Hook(Arc<HookLog>),
}

/// Size-capped retained output plus its on-disk log for one hook run.
///
/// Mirrors the engine's `ServiceLogs` retention so `slipfork logs <hook_id> --follow` behaves like
/// a service log, while [`HookLog::path`] gives the rescue block a real file to point at. The
/// `target` is the hook id the record was minted for, so `slipfork logs` attributes every chunk to
/// its own hook record instead of the caller's request string.
pub(super) struct HookLog {
    pub(super) target: String,
    pub(super) path: PathBuf,
    pub(super) state: Mutex<HookLogState>,
    pub(super) changed: Condvar,
}

#[derive(Default)]
pub(super) struct HookLogState {
    pub(super) chunks: Vec<StreamChunk>,
    pub(super) closed: bool,
}

impl HookLog {
    pub(super) fn new(target: String, path: PathBuf) -> Self {
        Self {
            target,
            path,
            state: Mutex::new(HookLogState::default()),
            changed: Condvar::new(),
        }
    }

    pub(super) fn target(&self) -> &str {
        &self.target
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, HookLogState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn push(&self, chunk: StreamChunk) {
        if chunk.bytes.is_empty() {
            return;
        }
        self.lock().chunks.push(chunk);
        self.changed.notify_all();
    }

    pub(super) fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }

    pub(super) fn snapshot(&self) -> Vec<StreamChunk> {
        self.lock().chunks.clone()
    }

    pub(super) fn follow(&self, on_chunk: &mut dyn FnMut(StreamChunk)) {
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
pub(super) struct SealedBase {
    pub(super) snapshot: SnapshotId,
    pub(super) lineage: BootLineage,
}

/// Mutable daemon index, mutated only in short critical sections (never across engine I/O).
#[derive(Default)]
pub(super) struct DaemonState {
    pub(super) worlds: Vec<LiveWorld>,
    /// Base state snapshot created on first use per `(project, base digest)` in this daemon.s
    /// lifetime, with the `BootLineage` a later world reuses (base `reused`, seed `cached`). The
    /// digest covers backing-service versions and every resolved seed-dump digest, so `--no-dump`
    /// and a restored dump never share a base.
    pub(super) bases: BTreeMap<(ProjectSlug, String), SealedBase>,
}

impl DaemonState {
    pub(super) fn world(&self, project: &ProjectSlug, target: &WorldTarget) -> Option<&LiveWorld> {
        self.worlds
            .iter()
            .find(|world| world.project == *project && world.target == *target)
    }

    pub(super) fn world_index(&self, project: &ProjectSlug, target: &WorldTarget) -> Option<usize> {
        self.worlds
            .iter()
            .position(|world| world.project == *project && world.target == *target)
    }

    pub(super) fn world_mut(
        &mut self,
        project: &ProjectSlug,
        target: &WorldTarget,
    ) -> Option<&mut LiveWorld> {
        self.worlds
            .iter_mut()
            .find(|world| world.project == *project && world.target == *target)
    }
}

/// A hook resolved once for both interactive entry and non-interactive streaming.
pub(super) struct ResolvedHook {
    pub(super) argv: Vec<String>,
    pub(super) overlay: ExecEnv,
    pub(super) phase_label: &'static str,
}

/// A resolved world selection: repository identity, target, and the checkout its world mounts.
pub(super) struct WorldSelection {
    pub(super) repo: RepoRoot,
    pub(super) target: WorldTarget,
    pub(super) worktree: PathBuf,
}

/// The per-invocation boot policy (`--no-dump | --refresh-dump`, `--resources local`,
/// `--host-caches`), threaded into whichever verb runs the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BootPolicy {
    pub(super) dump: DumpPolicy,
    pub(super) force_local_resources: bool,
    pub(super) host_caches: bool,
}

/// Inputs to one `L0` rootfs materialization.
pub(super) struct RootfsInputs<'a> {
    pub(super) worktree: &'a Path,
    pub(super) snow: &'a SnowFile,
    pub(super) vault: &'a dyn SecretProvider,
    pub(super) coords: &'a ResourceCoordinates,
    pub(super) claims: &'a OsppClaims,
    pub(super) host_caches: bool,
}

/// Inputs to one world-level environment resolution.
pub(super) struct WorldEnvInputs<'a> {
    pub(super) snow: &'a SnowFile,
    pub(super) vault: &'a dyn SecretProvider,
    pub(super) coords: &'a ResourceCoordinates,
    pub(super) project: &'a ProjectSlug,
    pub(super) target: &'a WorldTarget,
    pub(super) claims: &'a OsppClaims,
    /// Env keys whose expression is locally claimed: the literal expression never reaches a world.
    pub(super) claimed_env: &'a std::collections::BTreeSet<String>,
    pub(super) ospp_env: Option<&'a BTreeMap<String, BTreeMap<String, TaintedString>>>,
}

pub(super) struct ResolvedWorld {
    pub(super) snow: SnowFile,
    pub(super) vault: Arc<dyn SecretProvider>,
    pub(super) plan: ResourcePlan,
    pub(super) resources: Vec<ResourceSummary>,
    pub(super) coords: ResourceCoordinates,
    pub(super) base_env: ExecEnv,
    /// The one enabled-component list this run resolved; `start_world` iterates exactly this.
    pub(super) components: Vec<ResolvedComponent>,
    pub(super) ingresses: Vec<IngressRoute>,
    pub(super) domains: Vec<String>,
    /// The ID-only claims this boot sent (local vault keys, local resources, local domains).
    pub(super) claims: OsppClaims,
    /// The server-resolved per-component environment (already decrypted in memory), kept so a
    /// pushed `env_updated` can be diffed against what the world runs.
    pub(super) ospp_env: Option<BTreeMap<String, BTreeMap<String, TaintedString>>>,
}

/// Everything one `up`/`restart` pipeline run resolves **before** the world exists.
///
/// Both verbs call [`DaemonService::prepare_world`], so the `L0` cache verdict (hit vs rebuild),
/// the base-snapshot seal, and every environment are decided by one code path; only what the
/// engine does with the result differs (`clone_world` vs `reload_world`).
pub(super) struct PreparedWorld {
    pub(super) resolved: ResolvedWorld,
    pub(super) rootfs: WorldRootfs,
    pub(super) snapshot: SnapshotId,
    pub(super) boot: BootLineage,
}

/// One enabled component of the world: everything `start_world` needs, resolved once.
#[derive(Debug, Clone)]
pub(super) struct ResolvedComponent {
    pub(super) id: String,
    pub(super) run_command: String,
    pub(super) readiness_port: Option<u16>,
    pub(super) env: ExecEnv,
}

/// A world a fresh `boot` produced, plus what that boot observably did.
pub(super) struct BootedWorld {
    pub(super) live: LiveWorld,
    /// The engine's rows read after the pipeline ran; the summary reports these, not a second read.
    pub(super) rows: Vec<WorldServiceSummary>,
    pub(super) changed: bool,
    pub(super) restarted: Vec<String>,
}

/// Inputs to one world materialization.
pub(super) struct BootContext {
    pub(super) repo: RepoRoot,
    pub(super) target: WorldTarget,
    pub(super) worktree: PathBuf,
    pub(super) policy: BootPolicy,
    pub(super) workspace: Option<Box<dyn VcsWorkspace>>,
}

/// Failure of one boot; a world that already exists stays attached for inspection.
pub(super) struct BootFailure {
    pub(super) error: EdgeError,
    /// A world that already exists keeps its handle so it stays inspectable; boxed because the
    /// success value is only returned out-of-band from `boot`'s `Result`.
    pub(super) world: Option<Box<LiveWorld>>,
    pub(super) workspace: Option<Box<dyn VcsWorkspace>>,
}

/// The serde `snake_case` label of a domain enum (the single wire spelling).
pub(super) fn json_label<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Parent directory of an explicit manifest path (`"."` for a bare file name).
pub(super) fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}
