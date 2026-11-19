//! Plain data the daemon index, boot pipeline, and protocol share.

use slipfork_dag::{DumpPolicy, ResourcePlan};
use slipfork_edge::{EdgeError, IngressRoute};
use slipfork_forkfile::{EvaluatedFork, HookPhase, Resolvers, SecretProvider, TaintedString};
use slipfork_ospp::OsppSession;
use slipfork_vcs::{RepoRoot, VcsWorkspace};
use slipfork_wire::control::{FeatureSelection, ResourceSummary, WorldServiceSummary, WorldState};
use slipfork_world::rootfs::WorldRootfs;
use slipfork_world::{
    BootLineage, ExecEnv, LogChunk, ProjectSlug, ServiceHandle, SnapshotId, StreamChunk,
    WorldHandle, WorldTarget, epoch_millis_now,
};
use std::collections::{BTreeMap, VecDeque};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// Idle poll interval for a follower blocked on a hook window or the restart bus.
const FOLLOW_POLL: Duration = Duration::from_millis(50);

/// One live world owned by this daemon.
///
/// Two groups of fields, and only the first is frozen while the handle lives:
///
/// - **Frozen until a reload re-roots the world:** `worktree`, `handle` (and the CoW state behind
///   it), `rootfs`, `policy`, `workspace`, `hook_logs`, and `project`/`target`/`repo`. A `restart`
///   keeps the handle and the subvolume (so backing-service data survives) and only re-clones that
///   subvolume when the resolved base snapshot changed; it never discards the worktree.
/// - **Re-derived by every pipeline run** (`resolve_world`): `fork`, `resources`, `plan`,
///   `base_env`, `components`, `hooks`, and the registry's ingress/domain table, so an edited
///   `Forkfile` is what the world actually runs after a `restart`.
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
    /// The one enabled-component list: id, argv, readiness port, and environment
    /// (re-derived by `resolve_world`; `start_world` and the hook paths never re-resolve it).
    pub(super) components: Vec<ResolvedComponent>,
    /// Every declared lifecycle hook, resolved once through the canonical evaluator.
    pub(super) hooks: Vec<ResolvedHook>,
    /// The policy this world was booted (or last re-run) with; a later `up` may not silently ask
    /// for a different dump policy or rootfs build on top of the retained CoW state.
    pub(super) policy: BootPolicy,
    /// The `-F/--no-default-features` selection this world last evaluated its `Forkfile` with.
    pub(super) feature_selection: FeatureSelection,
    /// The feature-evaluated manifest this world runs (re-derived with the plan).
    pub(super) fork: EvaluatedFork,
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

/// Size-capped retained output plus its on-disk log for one hook run.
///
/// Mirrors the engine's `ServiceLogs` retention so `slipfork logs <hook_id> --follow` behaves like
/// a service log, while [`HookLog::path`] gives the rescue block a real file to point at. The
/// `target` is the hook id the record was minted for, so `slipfork logs` attributes every chunk to
/// its own hook record instead of the caller's request string.
pub(crate) struct HookLog {
    pub(super) target: String,
    pub(super) path: PathBuf,
    /// Identity of this window, so a hook log reused across a restart resumes at the follower's
    /// cursor instead of re-emitting its whole retained history.
    pub(super) token: Arc<()>,
    pub(super) state: Mutex<HookLogState>,
    pub(super) changed: Condvar,
}

#[derive(Default)]
pub(super) struct HookLogState {
    pub(super) chunks: Vec<LogChunk>,
    pub(super) closed: bool,
}

impl HookLog {
    pub(crate) fn new(target: String, path: PathBuf) -> Self {
        Self {
            target,
            path,
            token: Arc::new(()),
            state: Mutex::new(HookLogState::default()),
            changed: Condvar::new(),
        }
    }

    pub(super) fn token(&self) -> Arc<()> {
        Arc::clone(&self.token)
    }

    /// How many chunks this window has ever retained (hook logs are never evicted).
    pub(super) fn cursor(&self) -> u64 {
        self.lock().chunks.len() as u64
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, HookLogState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn push(&self, chunk: StreamChunk) {
        if chunk.bytes.is_empty() {
            return;
        }
        let captured = LogChunk::output(self.target.clone(), epoch_millis_now(), chunk);
        self.lock().chunks.push(captured);
        self.changed.notify_all();
    }

    pub(crate) fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }

    pub(super) fn snapshot(&self) -> Vec<LogChunk> {
        self.lock().chunks.clone()
    }

    /// Replay from `start` (a [`HookLog::cursor`]), then stream until closed or cancelled.
    ///
    /// `keep_going` is polled through a bounded `Condvar::wait_timeout`, so a disconnected follower
    /// releases its reader thread instead of pinning it for the hook's lifetime.
    pub(super) fn follow_from(
        &self,
        start: u64,
        keep_going: &dyn Fn() -> bool,
        on_chunk: &mut dyn FnMut(LogChunk) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        let mut state = self.lock();
        let mut cursor = start;
        loop {
            if !keep_going() {
                return ControlFlow::Break(());
            }
            while (cursor as usize) < state.chunks.len() {
                let chunk = state.chunks[cursor as usize].clone();
                cursor += 1;
                drop(state);
                let flow = on_chunk(chunk);
                if flow.is_break() {
                    return flow;
                }
                state = self.lock();
            }
            if state.closed {
                return ControlFlow::Continue(());
            }
            state = self
                .changed
                .wait_timeout(state, FOLLOW_POLL)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
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
    /// Restart epochs per world, keyed by world identity and **outliving** the world record so a
    /// follower that attached before a `down` still learns about the next `up`.
    pub(super) log_buses: Vec<(ProjectSlug, WorldTarget, Arc<LogBus>)>,
}

/// Broadcast of a world's restart epochs, shared by every attached `slipfork logs -f` follower.
///
/// The daemon owns exactly one bus per world identity. Each successful boot or in-place reload
/// increments `boots`; every boot after the first is a restart epoch and appends its capture time,
/// which wakes every follower with a typed [`LogChunk`] divider. A `down` removes the world record
/// but not the bus, so `down` + `up` reports one epoch instead of EOF-ing the follower.
#[derive(Default)]
pub(super) struct LogBus {
    state: Mutex<LogBusState>,
    changed: Condvar,
}

/// How many recent restart epochs one bus retains. A world may restart for months; a follower that
/// re-attaches keeps only this tail of dividers instead of pinning every epoch for the bus's life.
const DIVIDERS_RETAINED: usize = 8;

#[derive(Default)]
struct LogBusState {
    boots: u32,
    /// Total restart epochs ever published. This monotonic counter outlives the bounded ring, so a
    /// follower's `seen` mark never rewinds when the oldest dividers are trimmed.
    epochs: u64,
    /// The most recent [`DIVIDERS_RETAINED`] epoch capture times.
    dividers: VecDeque<u64>,
    closed: bool,
}

impl LogBus {
    /// Record one successful boot/reload of this world, publishing a divider for every boot after
    /// the first (the first boot has nothing to divide from).
    pub(super) fn boot(&self, ts_ms: u64) {
        let mut state = self.lock();
        state.boots += 1;
        if state.boots > 1 {
            state.epochs += 1;
            state.dividers.push_back(ts_ms);
            while state.dividers.len() > DIVIDERS_RETAINED {
                state.dividers.pop_front();
            }
        }
        drop(state);
        self.changed.notify_all();
    }

    /// How many restart epochs a fresh follower has already missed.
    pub(super) fn mark(&self) -> u64 {
        self.lock().epochs
    }

    /// Block until an epoch past `seen` exists; `None` once the daemon/request is shutting down.
    ///
    /// Returns the bus's new epoch mark beside every divider the follower has not seen, so a fast
    /// restart sequence is never collapsed into a silent jump. A follower that fell behind by more
    /// than the retained ring sees the ring's tail; the returned mark still advances it past them.
    pub(super) fn wait_past(
        &self,
        seen: u64,
        keep_going: &dyn Fn() -> bool,
    ) -> Option<(u64, Vec<u64>)> {
        let mut state = self.lock();
        loop {
            if state.epochs > seen {
                let oldest = state.epochs - state.dividers.len() as u64;
                let skip = seen.saturating_sub(oldest) as usize;
                let dividers = state.dividers.iter().skip(skip).copied().collect();
                return Some((state.epochs, dividers));
            }
            if state.closed || !keep_going() {
                return None;
            }
            state = self
                .changed
                .wait_timeout(state, FOLLOW_POLL)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// End every follower (daemon shutdown, or a permanently removed world).
    pub(super) fn shutdown(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }

    fn lock(&self) -> MutexGuard<'_, LogBusState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl DaemonState {
    /// The bus for `(project, target)`, minted on first use and never removed by `down`.
    pub(super) fn log_bus(&mut self, project: &ProjectSlug, target: &WorldTarget) -> Arc<LogBus> {
        if let Some((_, _, bus)) = self
            .log_buses
            .iter()
            .find(|(bus_project, bus_target, _)| bus_project == project && bus_target == target)
        {
            return Arc::clone(bus);
        }
        let bus = Arc::new(LogBus::default());
        self.log_buses
            .push((project.clone(), target.clone(), Arc::clone(&bus)));
        bus
    }

    /// Permanently forget a world identity: close every attached follower and drop the bus.
    ///
    /// `down` keeps the bus (so `down` + `up` still reports one divider); `drop`/`prune` remove the
    /// identity for good, which is the one place a bus may be reaped.
    pub(super) fn forget_log_bus(
        &mut self,
        project: &ProjectSlug,
        target: &WorldTarget,
    ) -> Option<Arc<LogBus>> {
        let index = self
            .log_buses
            .iter()
            .position(|(bus_project, bus_target, _)| {
                bus_project == project && bus_target == target
            })?;
        let (_, _, bus) = self.log_buses.remove(index);
        bus.shutdown();
        Some(bus)
    }

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

/// A hook resolved once through the canonical evaluator for entry and streaming alike.
#[derive(Debug, Clone)]
pub(super) struct ResolvedHook {
    /// `<service>:<phase>`, the id `slipfork hook run` names.
    pub(super) id: String,
    /// The service that declared the hook (the canonical prompt component).
    pub(super) service: String,
    pub(super) phase: HookPhase,
    pub(super) argv: Vec<String>,
    pub(super) overlay: ExecEnv,
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

/// One `L0` rootfs materialization: the checkout, the evaluated manifest, and its resolvers.
pub(super) struct RootfsSource<'a> {
    pub(super) worktree: &'a Path,
    pub(super) fork: &'a EvaluatedFork,
    pub(super) resolvers: &'a Resolvers<'a>,
    pub(super) host_caches: bool,
}

pub(super) struct ResolvedWorld {
    pub(super) fork: EvaluatedFork,
    pub(super) vault: Arc<dyn SecretProvider>,
    pub(super) plan: ResourcePlan,
    pub(super) resources: Vec<ResourceSummary>,
    pub(super) base_env: ExecEnv,
    /// The one enabled-component list this run resolved; `start_world` iterates exactly this.
    pub(super) components: Vec<ResolvedComponent>,
    /// Every declared hook, resolved once for the entry and streaming verbs.
    pub(super) hooks: Vec<ResolvedHook>,
    pub(super) ingresses: Vec<IngressRoute>,
    pub(super) domains: Vec<String>,
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
    /// The service's `run` argv, already word-split-safe and resolved.
    pub(super) argv: Vec<String>,
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
    /// The feature selection this pipeline run evaluates the manifest with.
    pub(super) feature_selection: FeatureSelection,
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
