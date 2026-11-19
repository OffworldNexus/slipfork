use super::resolve::dump_policy;
use super::service::DaemonService;
use super::types::BootContext;
use super::types::BootPolicy;
use super::{rejected, runtime_error};
use slipfork_edge::{EdgeError, world_id};
use slipfork_vcs::WorkspaceRequest;
use slipfork_vcs::{RepoRoot, VcsWorkspaceFactory, WORKTREES_DIRECTORY, sanitize_fork_name};
use slipfork_wire::control::{
    BootEvent, DaemonError, DaemonErrorKind, DropForkRequest, ForkRequest, UpRequest,
    WorldSelector, WorldServiceSummary, WorldState, WorldUpSummary,
};
use slipfork_world::{ProjectSlug, ServiceState, WorldTarget, validate_fork_name};

/// The two verbs that run the one boot pipeline (`docs/design/dx-0.1.0-ux.md` §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BootCommand {
    /// `slipfork up`: boot an absent world, confirm a `Running` one, rescue a held one in place.
    Up,
    /// `slipfork restart`: re-run a world this daemon holds; never boot what was not running.
    Restart,
}

/// What one verb must do to the selected world (`docs/design/dx-0.1.0-ux.md` §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BootVerb {
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
pub(super) fn boot_verb(command: BootCommand, state: Option<WorldState>) -> Option<BootVerb> {
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
pub(super) fn branch_fork_name(
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
pub(super) fn not_running(project: &ProjectSlug, target: &WorldTarget) -> EdgeError {
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
pub(super) fn restarted_service_ids(
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

/// Refuse a rerun that asks for a fact only a fresh boot can produce (`docs/design/dx-0.1.0-ux.md`
/// §4): dump policy and `--host-caches` are inputs of the sealed base and the built rootfs, which a
/// `restart`/in-place `up` deliberately keeps.
pub(super) fn check_rerun_policy(
    world: &str,
    held: BootPolicy,
    policy: BootPolicy,
) -> Result<(), EdgeError> {
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
pub(super) fn request_policy(
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
pub(super) fn rerun_policy(
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

impl DaemonService {
    pub(super) fn up(
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
}

impl DaemonService {
    pub(super) fn restart(
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
}

impl DaemonService {
    pub(super) fn down(&self, req: &WorldSelector) -> Result<bool, EdgeError> {
        let selection = self.select(&req.cwd, req.world.as_deref())?;
        self.release_only(&selection.repo.project, &selection.target)
    }
}

impl DaemonService {
    pub(super) fn fork(
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
}

impl DaemonService {
    pub(super) fn drop_fork(&self, req: &DropForkRequest) -> Result<bool, EdgeError> {
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
        // A dropped fork never returns: reap its restart bus and every attached follower.
        self.forget_log_bus(&project, &target);
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
}
