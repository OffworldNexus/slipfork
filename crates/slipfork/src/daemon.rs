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
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use slipfork_dag::{
    DumpPolicy, ResourceMode, ResourcePlan, ResourceStrategy, S3Resolution, SlipforkConfig,
};
use slipfork_edge::{
    BootEvent, DaemonControl, DaemonError, DaemonErrorKind, DaemonStatus, DropForkRequest,
    EdgeError, ExecTargetRequest, ForkRequest, ForwardRequest, ForwardSummary, HookRunRequest,
    IngressRoute, LogsRequest, PruneReport, ResourceSummary, SharedWorldInfo, SnowResolveRequest,
    SnowResolveSummary, UpRequest, WorldRegistry, WorldSelector, WorldState, WorldSummary,
    WorldUpSummary, world_id,
};
use slipfork_engine::{
    ExecEnv, InheritedVar, LocalIngressBridge, MountMode, ServiceHandle, ServiceSpec, SnapshotId,
    SnapshotMode, StreamChunk, WorldEngine, WorldHandle, WorldMount, WorldSpec,
};
use slipfork_snow::{
    HookPhase, Namespace, OsCsprngEntropy, ResolvedEnvMap, ResourceCoordinates, S3Coordinates,
    SecretProvider, SnowFile,
};
use slipfork_vcs::{
    RepoRoot, RepoRootResolver, VcsWorkspace, VcsWorkspaceFactory, WORKTREES_DIRECTORY,
    WorkspaceRequest,
};
use slipfork_world::{LocalHostRoute, ProjectSlug, WorldOrigin, WorldTarget, validate_fork_name};

use crate::layout::{Layout, ensure_private_dir};
use crate::project::load_slipfork_config;
use crate::vaults::ProjectVaults;

/// The world mode this daemon supervises (`snow.yml` `modes.local`, `ADR-0001` §1).
const WORLD_MODE: &str = "local";

/// Everything the daemon needs that the Composition Root resolved once.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Explicit `--state-dir` / `--runtime-dir` roots and the project-vault data home.
    pub layout: Layout,
    /// `PATH`/`TERM`/`LANG` values the engine hands to world processes (never the whole env).
    pub inherited: Vec<(InheritedVar, String)>,
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
    workspace: Option<Box<dyn VcsWorkspace>>,
}

/// Mutable daemon index, mutated only in short critical sections (never across engine I/O).
#[derive(Default)]
struct DaemonState {
    worlds: Vec<LiveWorld>,
    /// Base state snapshot created on first use per project in this daemon's lifetime.
    bases: BTreeMap<ProjectSlug, SnapshotId>,
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
    /// HTTP ingress registry and origin source.
    pub registry: std::sync::Arc<WorldRegistry>,
    /// Canonical repository resolver.
    pub repo_roots: std::sync::Arc<dyn RepoRootResolver>,
    /// Fork workspace factory.
    pub workspaces: std::sync::Arc<dyn VcsWorkspaceFactory>,
    /// Project-scoped `0600` vault factory.
    pub vaults: std::sync::Arc<dyn ProjectVaults>,
}

/// The daemon-owned [`DaemonControl`] implementation.
pub struct DaemonService {
    engine: std::sync::Arc<dyn WorldEngine>,
    registry: std::sync::Arc<WorldRegistry>,
    repo_roots: std::sync::Arc<dyn RepoRootResolver>,
    workspaces: std::sync::Arc<dyn VcsWorkspaceFactory>,
    vaults: std::sync::Arc<dyn ProjectVaults>,
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
            registry: ports.registry,
            repo_roots: ports.repo_roots,
            workspaces: ports.workspaces,
            vaults: ports.vaults,
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
        let path = vcs_root.join("slipfork.toml");
        if !path.is_file() {
            return Err(runtime_error(format!(
                "cannot read {}: every world boot needs the project's slipfork.toml (the DAG/resources manifest); create it with `project = \"<slug>\"`",
                path.display()
            )));
        }
        load_slipfork_config(&path).map_err(runtime_error)
    }

    /// The single base-state snapshot per project for this daemon's lifetime.
    fn ensure_base_snapshot(&self, project: &ProjectSlug) -> Result<SnapshotId, EdgeError> {
        if let Some(snapshot) = self.lock().bases.get(project).cloned() {
            return Ok(snapshot);
        }
        let seed = self.config.layout.base_seed_dir(project);
        ensure_private_dir(&seed).map_err(runtime_error)?;
        let snapshot = SnapshotId(format!("{project}-base"));
        self.engine
            .create_snapshot(&seed, &snapshot, SnapshotMode::DiskCow)
            .map_err(|err| {
                runtime_error(format!("cannot create the base world snapshot: {err}"))
            })?;
        self.lock().bases.insert(project.clone(), snapshot.clone());
        Ok(snapshot)
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
        let plan = match resolve_plan(&config, &snow, &target, dump, force_local_resources) {
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
        let vault = match self.vaults.vault(&project) {
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

        let snapshot = match self.ensure_base_snapshot(&project) {
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
            workspace: workspace.take(),
        };

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
                argv: vec![
                    "sh".to_owned(),
                    "-eu".to_owned(),
                    "-c".to_owned(),
                    resolved.run_command.clone(),
                ],
                cwd: worktree.clone(),
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

        if let Err(error) = self.run_hooks(&live, HookPhase::PreDeploy, on_event) {
            live.state = WorldState::Failed;
            return Err(BootFailure {
                error,
                world: Some(Box::new(live)),
                workspace: None,
            });
        }
        if let Err(error) = self.run_hooks(&live, HookPhase::PostDeploy, on_event) {
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
        live: &LiveWorld,
        phase: HookPhase,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<(), EdgeError> {
        for hook in &live.snow.hooks {
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
            let output = self
                .engine
                .exec_in_world(
                    &live.handle,
                    &argv,
                    &live.worktree,
                    &overlay,
                    &mut |chunk| {
                        on_event(BootEvent::HookOutput {
                            id: id.clone(),
                            chunk,
                        });
                    },
                )
                .map_err(|err| {
                    runtime_error(format!(
                        "cannot run hook '{id}' in world '{}': {err}",
                        world_id(&live.project, &live.target)
                    ))
                })?;
            if output.exit_code != 0 {
                return Err(runtime_error(format!(
                    "hook '{id}' exited with code {} in world '{}' (the world is kept for inspection)",
                    output.exit_code,
                    world_id(&live.project, &live.target)
                )));
            }
        }
        Ok(())
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
        let vault = self.vaults.vault(&project)?;
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
        let world = {
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
            )
        };
        let (handle, service_ids) = world;
        let targets: Vec<String> = match &req.target {
            Some(target) if service_ids.iter().any(|id| id == target) => vec![target.clone()],
            Some(target) => {
                return Err(rejected(format!(
                    "world '{}' has no service '{target}' (known services: {})",
                    world_id(&selection.repo.project, &selection.target),
                    service_ids.join(", ")
                )));
            }
            None => service_ids,
        };
        for target in targets {
            let log = self
                .engine
                .service_logs(&handle, &target)
                .map_err(|err| runtime_error(err.to_string()))?;
            let wants = |chunk: &StreamChunk| {
                req.stderr == matches!(chunk.source, slipfork_engine::StreamSource::Stderr)
            };
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
        Ok(())
    }

    fn exec_target(
        &self,
        req: &ExecTargetRequest,
    ) -> Result<slipfork_edge::WorldExecTarget, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        let (handle, base_env) = {
            let state = self.lock();
            let Some(world) = state.world(&selection.repo.project, &selection.target) else {
                return Err(Self::unknown_world(
                    &selection.repo.project,
                    &selection.target,
                ));
            };
            (world.handle.clone(), world.base_env.clone())
        };
        let entry = self
            .engine
            .entry_command(&handle, &req.cwd)
            .map_err(|err| runtime_error(err.to_string()))?;
        let origin = self
            .registry
            .origin(&selection.repo.project, &selection.target)?;
        Ok(slipfork_edge::WorldExecTarget {
            world: world_id(&selection.repo.project, &selection.target),
            origin,
            entry,
            env: base_env,
            target_argv: req.argv.clone(),
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
        let (component, phase) = parse_hook_id(&req.hook).ok_or_else(|| {
            rejected(format!(
                "invalid hook id '{}' (expected <component>:<phase>)",
                req.hook
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
            .ok_or_else(|| rejected(format!("world has no hook '{}'", req.hook)))?;
        if hook.run_command.trim().is_empty() {
            return Err(rejected(format!(
                "hook '{}' declares an empty run_command",
                req.hook
            )));
        }
        let argv = vec![
            "sh".to_owned(),
            "-eu".to_owned(),
            "-c".to_owned(),
            hook.run_command.clone(),
        ];
        let overlay = component_envs.get(component).cloned().unwrap_or_default();
        let output = self
            .engine
            .exec_in_world(&handle, &argv, &worktree, &overlay, on_chunk)
            .map_err(|err| runtime_error(err.to_string()))?;
        Ok(output.exit_code)
    }

    fn forward(
        &self,
        _req: &ForwardRequest,
        _wait_disconnect: &mut dyn FnMut(ForwardSummary),
    ) -> Result<(), EdgeError> {
        Err(rejected(
            "`slipfork forward` ships with CLI part 2 (M2.17); the daemon already owns the \
             registry bridge, but the CLI surface is not wired yet",
        ))
    }

    fn auth_login(&self, _pairing_url: &str) -> Result<slipfork_edge::AuthSummary, EdgeError> {
        Err(rejected(
            "`slipfork auth login` ships with CLI part 2 (M2.17); no OSPP endpoint is configured \
             for this daemon yet",
        ))
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

/// Map one invalid request to a typed rejection (the message is the whole diagnostic).
fn rejected(message: impl Into<String>) -> EdgeError {
    EdgeError::Control(DaemonError::new(DaemonErrorKind::Rejected, message))
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
    ResourcePlan::resolve(&spec, ResourceMode::from(target), dump, None)
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
