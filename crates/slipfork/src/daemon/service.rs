//! The daemon service: index, lifecycle orchestration entry points, and world release.

use super::ospp_supervisor::OsppSupervisor;
use super::types::{DaemonState, LiveWorld, WorldFacts};
use super::{DaemonConfig, DaemonPorts, engine_read_error, runtime_error};
use crate::vaults::ProjectVaults;
use slipfork_edge::{EdgeError, WorldRegistry, parent_world, world_id};
use slipfork_engine::rootfs::BaseImageBuilder;
use slipfork_snow::EntropySource;
use slipfork_vcs::{RepoRootResolver, VcsWorkspaceFactory};
use slipfork_wire::control::{
    DaemonError, DaemonErrorKind, DaemonStatus, IngressInfo, PruneReport, WorldProcessReport,
    WorldSelector, WorldServiceSummary, WorldState, WorldSummary, WorldUpSummary,
};
use slipfork_world::{ProjectSlug, WorldEngine, WorldHandle, WorldTarget};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The daemon-owned `DaemonControl` implementation.
pub struct DaemonService {
    pub(super) engine: std::sync::Arc<dyn WorldEngine>,
    pub(super) builder: std::sync::Arc<dyn BaseImageBuilder>,
    pub(super) registry: std::sync::Arc<WorldRegistry>,
    pub(super) repo_roots: std::sync::Arc<dyn RepoRootResolver>,
    pub(super) workspaces: std::sync::Arc<dyn VcsWorkspaceFactory>,
    pub(super) vaults: std::sync::Arc<dyn ProjectVaults>,
    /// The daemon's one OSPP client, live session, and public pairing record.
    pub(super) ospp: OsppSupervisor,
    pub(super) entropy: Arc<dyn EntropySource>,
    pub(super) config: DaemonConfig,
    pub(super) ingress: IngressInfo,
    pub(super) forward_port: Option<u16>,
    pub(super) state: Mutex<DaemonState>,
    /// Serializes world materialization so one project's base snapshot is minted exactly once.
    pub(super) boot_lock: Mutex<()>,
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
    pub(super) fn new(
        ports: DaemonPorts,
        config: DaemonConfig,
        ingress: IngressInfo,
        forward_port: Option<u16>,
    ) -> Self {
        let ospp = OsppSupervisor::new(
            ports.ospp,
            ports.signer,
            ports.hostname,
            ports.browser,
            ports.pairing,
        );
        Self {
            engine: ports.engine,
            builder: ports.builder,
            registry: ports.registry,
            repo_roots: ports.repo_roots,
            workspaces: ports.workspaces,
            vaults: ports.vaults,
            ospp,
            entropy: ports.entropy,
            config,
            ingress,
            forward_port,
            state: Mutex::new(DaemonState::default()),
            boot_lock: Mutex::new(()),
        }
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, DaemonState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn boot_guard(&self) -> MutexGuard<'_, ()> {
        self.boot_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl DaemonService {
    pub(super) fn insert_world(&self, live: LiveWorld) {
        let mut state = self.lock();
        if let Some(index) = state.world_index(&live.project, &live.target) {
            state.worlds[index] = live;
        } else {
            state.worlds.push(live);
        }
    }

    pub(super) fn take_world(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
    ) -> Option<LiveWorld> {
        let mut state = self.lock();
        state
            .world_index(project, target)
            .map(|index| state.worlds.remove(index))
    }
}

impl DaemonService {
    pub(super) fn status(&self) -> Result<DaemonStatus, EdgeError> {
        let worlds = self.list(None)?;
        Ok(DaemonStatus {
            pid: std::process::id(),
            ingress: self.ingress.clone(),
            forward_port: self.forward_port,
            worlds,
        })
    }
}

impl DaemonService {
    pub(super) fn list(&self, repo_root: Option<&Path>) -> Result<Vec<WorldSummary>, EdgeError> {
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
}

impl DaemonService {
    pub(super) fn world_processes(
        &self,
        req: &WorldSelector,
    ) -> Result<WorldProcessReport, EdgeError> {
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
}

impl DaemonService {
    pub(super) fn prune(&self, dry_run: bool) -> Result<PruneReport, EdgeError> {
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
                // The checkout is gone for good: close and drop this identity's restart bus.
                self.forget_log_bus(project, target);
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
}

impl DaemonService {
    pub(super) fn shutdown(&self) -> Result<(), EdgeError> {
        // Wake every attached `logs -f` follower so its bus wait reaches the `None` arm and the
        // reader threads are reaped before the process exits. The transport server stops
        // accepting and releases `daemon.sock` after this ack.
        let buses: Vec<Arc<super::types::LogBus>> = {
            let state = self.lock();
            state
                .log_buses
                .iter()
                .map(|(_, _, bus)| Arc::clone(bus))
                .collect()
        };
        for bus in buses {
            bus.shutdown();
        }
        Ok(())
    }
}

impl DaemonService {
    /// Record a world a pipeline run just produced and return its truthful summary.
    ///
    /// The world is inserted **before** the summary is read, so an engine-read failure can never
    /// drop a live world and make the next `restart` report `not_running`.
    pub(super) fn record_world(
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
        // Publish the epoch **after** the record exists: a follower woken by `boot` re-resolves the
        // world's sources and finds them immediately, with no re-resolve bridge race.
        self.log_bus(&project, &target)
            .boot(slipfork_world::epoch_millis_now());
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
    pub(super) fn engine_holds_live_world(&self, handle: &WorldHandle) -> bool {
        self.engine.world_is_live(handle).unwrap_or(false)
    }
}

impl DaemonService {
    /// Release one world from the registry and the daemon index (idempotent).
    ///
    /// Returns the registry's `released` flag plus the removed in-process record, so `drop` can
    /// forget the fork workspace with the same critical section ordering.
    pub(super) fn release_world(
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

    pub(super) fn release_only(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
    ) -> Result<bool, EdgeError> {
        self.release_world(project, target)
            .map(|(released, _)| released)
    }

    /// Close every follower of a world identity that will never come back (`drop`/`prune`).
    ///
    /// `down` deliberately keeps the bus so `down` + `up` still reports one divider; this is the
    /// only path that removes the identity from `log_buses`.
    pub(super) fn forget_log_bus(&self, project: &ProjectSlug, target: &WorldTarget) {
        self.lock().forget_log_bus(project, target);
    }

    /// Reject a request for a world this daemon does not hold (`code: unknown_world`).
    pub(super) fn unknown_world(project: &ProjectSlug, target: &WorldTarget) -> EdgeError {
        EdgeError::Control(DaemonError::new(
            DaemonErrorKind::UnknownWorld,
            format!(
                "no world '{}' here; boot it with `slipfork up` or list worlds with `slipfork list`",
                world_id(project, target)
            ),
        ))
    }
}
