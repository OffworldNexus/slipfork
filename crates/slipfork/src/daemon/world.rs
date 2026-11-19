use super::boot::{check_rerun_policy, restarted_service_ids};
use super::runtime_error;
use super::service::DaemonService;
use super::types::PreparedWorld;
use super::types::{BootContext, BootFailure, BootPolicy, BootedWorld, LiveWorld};
use slipfork_edge::{EdgeError, world_id};
use slipfork_vcs::VcsWorkspace;
use slipfork_wire::control::{BootEvent, WorldState, WorldUpSummary};
use slipfork_world::{ProjectSlug, WorldTarget};
use std::collections::BTreeMap;
use std::sync::Arc;

impl DaemonService {
    /// Materialize, attach, and start one world.
    pub(super) fn boot(
        &self,
        context: BootContext,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<BootedWorld, BootFailure> {
        let BootContext {
            repo,
            target,
            worktree,
            policy,
            feature_selection,
            mut workspace,
        } = context;
        let project = repo.project.clone();
        let fail = |error: EdgeError, workspace: Option<Box<dyn VcsWorkspace>>| BootFailure {
            error,
            world: None,
            workspace,
        };

        let prepared = match self.prepare_world(
            &repo,
            &target,
            &worktree,
            policy,
            &feature_selection,
            on_event,
        ) {
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
            hooks: resolved.hooks,
            policy,
            feature_selection,
            fork: resolved.fork,
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
        // The restart epoch is published by `record_world`, after this world is inserted, so a
        // woken follower always re-resolves a world that already exists.
        Ok(BootedWorld {
            live,
            rows: rows_after,
            changed: true,
            restarted,
        })
    }
}

impl DaemonService {
    /// The restart-epoch bus for one world identity (never removed by `down`).
    pub(super) fn log_bus(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
    ) -> Arc<super::types::LogBus> {
        self.lock().log_bus(project, target)
    }
}

impl DaemonService {
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
    pub(super) fn reboot(
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
        let prepared = match self.prepare_world(
            &live.repo,
            &live.target,
            &live.worktree,
            policy,
            &live.feature_selection,
            on_event,
        ) {
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

        // The engine owns the world's lifecycle: it stops every service pod (group-signalling the
        // `crun` runtime and its direct `execve`d children), reaps the PID 1, keeps or re-clones the subvolume
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

        live.fork = resolved.fork;
        live.resources = resolved.resources;
        live.base_env = resolved.base_env;
        live.components = resolved.components;
        live.hooks = resolved.hooks;
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
        // `record_world` publishes the epoch after inserting the reloaded world; one divider per
        // reload reaches every attached follower.
        self.record_world(live, rows_after, changed, restarted)
    }
}
