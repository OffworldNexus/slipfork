//! One owner for world ids: selector parsing, repository selection, and summaries.

use super::service::DaemonService;
use super::types::{WorldFacts, WorldSelection};
use super::{rejected, runtime_error};
use crate::status::world_next_commands;
use slipfork_edge::{EdgeError, world_id};
use slipfork_vcs::{RepoRoot, WORKTREES_DIRECTORY};
use slipfork_wire::control::{
    WorldSelector, WorldSelectorRepo, WorldServiceSummary, WorldState, WorldUpSummary,
};
use slipfork_world::{ProjectSlug, WorldHandle, WorldTarget, validate_fork_name};
use std::path::Path;

/// The minimal world-id round trip (`ADR-0005` §3, 0.1.0 subset; the typed `WorldId` lands in
/// 0.2.0): accept exactly the forms `slipfork list` prints through the one grammar owner in
/// [`slipfork_wire::control::WorldSelector::parse`].
pub(super) fn resolve_world_selector(raw: &str, repo: &RepoRoot) -> Result<WorldTarget, EdgeError> {
    WorldSelector::parse(
        raw,
        WorldSelectorRepo {
            project: &repo.project,
            vcs_root: &repo.vcs_root,
            worktree_root: &repo.worktree_root,
        },
    )
    .map_err(rejected)
}

impl DaemonService {
    /// Resolve the repository, target, and checkout for one `cwd` + `--world` request.
    pub(super) fn select(
        &self,
        cwd: &Path,
        world: Option<&str>,
    ) -> Result<WorldSelection, EdgeError> {
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
}

impl DaemonService {
    pub(super) fn world_state(&self, selection: &WorldSelection) -> Option<WorldState> {
        self.lock()
            .world(&selection.repo.project, &selection.target)
            .map(|world| world.state)
    }
}

impl DaemonService {
    /// The daemon's own record of one held world, copied out of the index so no engine I/O runs
    /// under the state mutex.
    pub(super) fn world_facts(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
    ) -> Option<WorldFacts> {
        self.lock().world(project, target).map(WorldFacts::of)
    }
}

impl DaemonService {
    /// The one `WorldUpSummary` builder: every field is read back from the world's own record, its
    /// engine handle, and the engine's service rows, so only `changed` and `restarted` (both
    /// observed by the pipeline run) are caller-supplied facts.
    pub(super) fn summary(
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
}

impl DaemonService {
    /// The engine's own service rows for one world (`world_processes` is the supervision record).
    pub(super) fn service_rows(
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
}
