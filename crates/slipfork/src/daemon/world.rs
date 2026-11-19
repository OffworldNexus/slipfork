use super::backing::service_specs;
use super::base::{base_digest, reused_base_lineage, selected_seed};
use super::boot::{check_rerun_policy, restarted_service_ids};
use super::service::DaemonService;
use super::types::PreparedWorld;
use super::types::SealedBase;
use super::types::{BootContext, BootFailure, BootPolicy, BootedWorld, LiveWorld};
use super::{rejected, runtime_error};
use crate::layout::ensure_private_dir;
use slipfork_dag::ResourcePlan;
use slipfork_edge::{EdgeError, world_id};
use slipfork_engine::elapsed_millis;
use slipfork_forkfile::{EvaluatedFork, HookPhase};
use slipfork_vcs::VcsWorkspace;
use slipfork_wire::control::{BootEvent, WorldState, WorldUpSummary};
use slipfork_world::BaseLineage;
use slipfork_world::SeedLineage;
use slipfork_world::ServiceSpec;
use slipfork_world::WorldSpec;
use slipfork_world::rootfs::WorldRootfs;
use slipfork_world::{
    BootLineage, EngineError, ExecEnv, ProjectSlug, ServiceKind, SnapshotId, WorldTarget,
};
use std::collections::BTreeMap;
use std::sync::Arc;

impl DaemonService {
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
    pub(super) fn ensure_base_snapshot(
        &self,
        project: &ProjectSlug,
        fork: &EvaluatedFork,
        plan: &ResourcePlan,
        rootfs: &WorldRootfs,
    ) -> Result<(SnapshotId, BootLineage), EdgeError> {
        let digest = base_digest(project, fork, plan);
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
            let (has_seed, digest_hex) = selected_seed(fork, plan);
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
        let restored = self.restore_base(project, &handle, fork, plan, rootfs);
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
        let (has_seed, digest_hex) = selected_seed(fork, plan);
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
}

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
    /// Start one world's backing services, its components, and its deploy hooks — the single
    /// pipeline tail `up`, `fork`, and `restart` all run.
    ///
    /// `live` already carries the world's handle (and therefore its CoW state), worktree, resolved
    /// environments, and `Forkfile`; this call owns only what happens *inside* that world. The
    /// services it starts are recorded on `live`, so `restarted` is read back from that record
    /// instead of being counted a second time. Returns the number of hooks that actually ran.
    pub(super) fn start_world(
        &self,
        live: &mut LiveWorld,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<usize, EdgeError> {
        let world = world_id(&live.project, &live.target);

        let service_specs = service_specs(&live.fork, &live.rootfs)?;
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
            if component.argv.is_empty() {
                return Err(rejected(format!(
                    "component '{id}' declares no `run` command in the Forkfile\n  next: add `run <command>` to `service {id}`"
                )));
            }
            let spec = ServiceSpec {
                kind: ServiceKind::Component,
                id: id.clone(),
                runtime: slipfork_world::ServiceRuntime::WorldProcess {
                    argv: component.argv.clone(),
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
