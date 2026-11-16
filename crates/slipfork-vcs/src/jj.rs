//! `JjWorkspaceAdapter`: Jujutsu workspaces mapping `@-` -> `refs/heads/<fork>` and `@` ->
//! `refs/slipfork/<fork>/wip` (collapsing to `@-`'s commit when `@` is empty/clean).
//!
//! Jujutsu has no branches: the working copy is the `@` commit and its parent is `@-` (`ADR-0005`
//! §3). A fork is created with `jj workspace add --revision <requesting @>`, so the new workspace
//! starts from the developer's uncommitted state exactly like a Git worktree fork does, while the
//! fork refs are projected from the *requesting* checkout: `@-` becomes `refs/heads/<fork>` and `@`
//! becomes `refs/slipfork/<fork>/wip`, collapsing to `@-` when the working copy is tree-identical
//! to its parent. Both refs are written into the repository's colocated Git store so
//! `git fetch slipfork` can move them without ever creating `refs/heads/<fork>/wip` (Git's D/F
//! refname lock).
//!
//! Jujutsu snapshots every file into `@`, so there is no tracked/untracked split to observe:
//! [`VcsWorkspace::changes`] diffs the fork working copy against `refs/heads/<fork>` and reports
//! paths that are absent from that branch (`A`) as [`WorkspaceChanges::untracked`], which is what an
//! untracked file means for a branch-scoped view.
//!
//! `jj` may be absent from a host: every entry point fails closed with
//! [`VcsError::ToolMissing`] (never falling back to Git), and
//! [`JjWorkspaceAdapter::detect`] exposes the same probe to callers that must skip loudly.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use slipfork_world::validate_fork_name;

use crate::git_store::{delete_ref, path_exists, ref_exists, resolve_commit};
use crate::ports::{
    MAX_WORKSPACE_CHANGES, VcsError, VcsWorkspace, VcsWorkspaceFactory, WORKTREES_DIRECTORY,
    WorkspaceChanges, WorkspaceRequest, fork_branch_ref, fork_snapshot_ref,
};
use crate::tool_command::{ToolOutput, run_git_checked, run_jj_checked};

/// Jujutsu revset of the working-copy commit.
const WORKING_COPY: &str = "@";
/// Jujutsu revset of the working-copy commit's parent.
const WORKING_COPY_PARENT: &str = "@-";
/// Single-value `jj log` template printing the full commit id.
const COMMIT_ID_TEMPLATE: &str = "commit_id";
/// `jj workspace list` template printing one workspace name per line.
const WORKSPACE_NAME_TEMPLATE: &str = "name ++ \"\\n\"";
/// `jj diff` template printing `<status> <path>` per changed entry.
///
/// `path` is the target of a copy or rename, so one line always names the path the fork holds.
const DIFF_TEMPLATE: &str = "status_char ++ \" \" ++ path ++ \"\\n\"";

/// Jujutsu [`VcsWorkspaceFactory`]: creates `.worktrees/<fork>` Jujutsu workspaces and projects
/// their `@-` / `@` pair into the repository's colocated Git store.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JjWorkspaceAdapter;

impl JjWorkspaceAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Probe `jj` on `PATH`, failing closed with an actionable hint when it is absent.
    ///
    /// Callers that must tolerate a host without Jujutsu (the hermetic conformance matrix) use this
    /// to skip **loudly**; [`VcsWorkspaceFactory::create_workspace`] performs the same probe and
    /// never falls back to Git.
    pub fn detect(&self, start: &Path) -> Result<(), VcsError> {
        probe_jj(start)
    }
}

impl VcsWorkspaceFactory for JjWorkspaceAdapter {
    fn create_workspace(
        &self,
        request: &WorkspaceRequest,
    ) -> Result<Box<dyn VcsWorkspace>, VcsError> {
        let repo = &request.repo;
        let fork = request.fork.as_str();
        // Fork names become a `.worktrees/<fork>` path component, a Jujutsu workspace name, and a
        // refname, so they use the same single-DNS-label contract as `.localhost` user forks.
        validate_fork_name(fork, &[])?;
        probe_jj(&repo.vcs_root)?;
        require_colocated_git_store(&repo.vcs_root)?;

        let worktree = repo.vcs_root.join(WORKTREES_DIRECTORY).join(fork);
        let base_ref = fork_branch_ref(fork);
        let snapshot_ref = fork_snapshot_ref(fork);
        if path_exists(&worktree)?
            || ref_exists(&repo.vcs_root, &base_ref)?
            || ref_exists(&repo.vcs_root, &snapshot_ref)?
            || workspace_registered(&repo.vcs_root, fork)?
        {
            return Err(VcsError::WorkspaceExists {
                fork: fork.to_owned(),
            });
        }

        // Projection happens before anything is created: every `jj` command snapshots the working
        // copy it runs in, so these ids already include the developer's uncommitted edits, and a
        // failure here leaves the repository untouched.
        let base = commit_id(&repo.worktree_root, WORKING_COPY_PARENT)?;
        let working_copy = commit_id(&repo.worktree_root, WORKING_COPY)?;
        let snapshot = if working_copy_changed(&repo.worktree_root, &base, &working_copy)? {
            working_copy.as_str()
        } else {
            base.as_str()
        };

        let worktrees = repo.vcs_root.join(WORKTREES_DIRECTORY);
        fs::create_dir_all(&worktrees).map_err(|err| VcsError::io(&worktrees, &err))?;
        if let Err(error) = materialize(
            &repo.worktree_root,
            &repo.vcs_root,
            fork,
            &worktree,
            &working_copy,
            &base,
            snapshot,
        ) {
            rollback(&repo.vcs_root, fork, &worktree);
            return Err(error);
        }

        Ok(Box::new(JjWorkspace {
            vcs_root: repo.vcs_root.clone(),
            path: worktree,
            fork: fork.to_owned(),
        }))
    }
}

/// One materialized `.worktrees/<fork>` Jujutsu workspace.
struct JjWorkspace {
    vcs_root: PathBuf,
    path: PathBuf,
    fork: String,
}

impl VcsWorkspace for JjWorkspace {
    fn fork_name(&self) -> &str {
        &self.fork
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn resolve(&self, reference: &str) -> Result<String, VcsError> {
        resolve_commit(&self.vcs_root, reference)
    }

    fn changes(&self) -> Result<WorkspaceChanges, VcsError> {
        let base = resolve_commit(&self.vcs_root, &fork_branch_ref(&self.fork))?;
        let diff = run_jj_checked(
            &self.path,
            &[
                OsStr::new("diff"),
                OsStr::new("--from"),
                OsStr::new(&base),
                OsStr::new("--to"),
                OsStr::new(WORKING_COPY),
                OsStr::new("--template"),
                OsStr::new(DIFF_TEMPLATE),
            ],
        )?;
        parse_workspace_changes(&diff)
    }

    fn forget(&self) -> Result<(), VcsError> {
        teardown(&self.vcs_root, &self.fork, &self.path)
    }
}

/// Create the Jujutsu workspace on top of the requesting checkout's `@` and publish both fork refs
/// into the colocated Git store.
fn materialize(
    source: &Path,
    vcs_root: &Path,
    fork: &str,
    worktree: &Path,
    revision: &str,
    base: &str,
    snapshot: &str,
) -> Result<(), VcsError> {
    run_jj_checked(
        source,
        &[
            OsStr::new("workspace"),
            OsStr::new("add"),
            worktree.as_os_str(),
            OsStr::new("--name"),
            OsStr::new(fork),
            OsStr::new("--revision"),
            OsStr::new(revision),
        ],
    )?;
    let branch_ref = fork_branch_ref(fork);
    let snapshot_ref = fork_snapshot_ref(fork);
    run_git_checked(
        vcs_root,
        &[
            OsStr::new("update-ref"),
            OsStr::new(&branch_ref),
            OsStr::new(base),
        ],
        None,
    )?;
    run_git_checked(
        vcs_root,
        &[
            OsStr::new("update-ref"),
            OsStr::new(&snapshot_ref),
            OsStr::new(snapshot),
        ],
        None,
    )?;
    Ok(())
}

/// Remove every artifact one fork owns: Jujutsu workspace, checkout, fork branch, and snapshot ref.
///
/// Shared by [`VcsWorkspace::forget`], which propagates failures, and [`rollback`], which discards
/// them because it runs while an earlier error is already being reported.
fn teardown(vcs_root: &Path, fork: &str, worktree: &Path) -> Result<(), VcsError> {
    if workspace_registered(vcs_root, fork)? {
        run_jj_checked(
            vcs_root,
            &[
                OsStr::new("workspace"),
                OsStr::new("forget"),
                OsStr::new(fork),
            ],
        )?;
    }
    remove_checkout(vcs_root, worktree)?;
    let snapshot_ref = fork_snapshot_ref(fork);
    if ref_exists(vcs_root, &snapshot_ref)? {
        delete_ref(vcs_root, &snapshot_ref)?;
    }
    let branch_ref = fork_branch_ref(fork);
    if ref_exists(vcs_root, &branch_ref)? {
        delete_ref(vcs_root, &branch_ref)?;
    }
    Ok(())
}

/// Remove artifacts created by a failed [`VcsWorkspaceFactory::create_workspace`] call.
///
/// Cleanup is best-effort on purpose: the caller receives the original, actionable failure.
fn rollback(vcs_root: &Path, fork: &str, worktree: &Path) {
    let _ = teardown(vcs_root, fork, worktree);
}

/// Remove the fork checkout, whether Jujutsu registered a Git worktree for it or not.
fn remove_checkout(vcs_root: &Path, worktree: &Path) -> Result<(), VcsError> {
    if !path_exists(worktree)? {
        return Ok(());
    }
    if fs::symlink_metadata(worktree.join(".git")).is_ok() {
        // A colocated repository (or `git.colocate = true`) registers the workspace as a Git
        // worktree; deleting the directory behind Git's back would leave stale worktree metadata.
        run_git_checked(
            vcs_root,
            &[
                OsStr::new("worktree"),
                OsStr::new("remove"),
                OsStr::new("--force"),
                worktree.as_os_str(),
            ],
            None,
        )?;
    } else {
        fs::remove_dir_all(worktree).map_err(|err| VcsError::io(worktree, &err))?;
    }
    Ok(())
}

/// `true` when `fork` is a workspace of `vcs_root`'s Jujutsu repository.
fn workspace_registered(vcs_root: &Path, fork: &str) -> Result<bool, VcsError> {
    let out = run_jj_checked(
        vcs_root,
        &[
            OsStr::new("workspace"),
            OsStr::new("list"),
            OsStr::new("--template"),
            OsStr::new(WORKSPACE_NAME_TEMPLATE),
        ],
    )?;
    Ok(out.stdout_str()?.lines().any(|name| name == fork))
}

/// Resolve `revset` to a full commit id in `cwd`, snapshotting the working copy on the way.
fn commit_id(cwd: &Path, revset: &str) -> Result<String, VcsError> {
    let out = run_jj_checked(
        cwd,
        &[
            OsStr::new("log"),
            OsStr::new("--no-graph"),
            OsStr::new("--revision"),
            OsStr::new(revset),
            OsStr::new("--template"),
            OsStr::new(COMMIT_ID_TEMPLATE),
        ],
    )?;
    let id = out.stdout_trimmed()?;
    if id.is_empty() {
        return Err(out.unexpected(format!("`jj log -r {revset}` returned no commit id")));
    }
    Ok(id.to_owned())
}

/// `true` when the checkout's working copy differs from its parent.
///
/// A clean Jujutsu working copy is still a commit of its own, so the decision has to compare trees
/// (`jj diff`), never commit ids.
fn working_copy_changed(cwd: &Path, base: &str, working_copy: &str) -> Result<bool, VcsError> {
    let out = run_jj_checked(
        cwd,
        &[
            OsStr::new("diff"),
            OsStr::new("--from"),
            OsStr::new(base),
            OsStr::new("--to"),
            OsStr::new(working_copy),
            OsStr::new("--summary"),
        ],
    )?;
    Ok(!out.stdout_trimmed()?.is_empty())
}

/// Fork refs are only Git-fetchable when Jujutsu stores its commits in a colocated `.git`.
fn require_colocated_git_store(vcs_root: &Path) -> Result<(), VcsError> {
    if fs::symlink_metadata(vcs_root.join(".git")).is_ok() {
        return Ok(());
    }
    Err(VcsError::UnsupportedTopology {
        topology: "jujutsu repository without a colocated git store",
        root: vcs_root.to_path_buf(),
        hint: format!(
            "run `jj git init --colocate` in {} so fork refs are Git-fetchable, then retry",
            vcs_root.display()
        ),
    })
}

/// Probe `jj` on `PATH` (a missing binary maps to [`VcsError::ToolMissing`]).
fn probe_jj(start: &Path) -> Result<(), VcsError> {
    run_jj_checked(start, &[OsStr::new("--version")]).map(|_| ())
}

/// Parse `jj diff --template 'status_char ++ " " ++ path'` into tracked/untracked buckets.
fn parse_workspace_changes(diff: &ToolOutput) -> Result<WorkspaceChanges, VcsError> {
    let mut changes = WorkspaceChanges::default();
    for line in diff.stdout_str()?.lines() {
        if line.is_empty() {
            continue;
        }
        let Some((status, path)) = line.split_once(' ') else {
            return Err(diff.unexpected("diff entry misses its status separator"));
        };
        if changes.tracked.len() + changes.untracked.len() >= MAX_WORKSPACE_CHANGES {
            changes.truncated = true;
            break;
        }
        let path = PathBuf::from(path);
        match status {
            // Jujutsu tracks every file, so a path that the fork branch does not have is the only
            // thing "untracked" can mean for the branch-scoped view this port reports.
            "A" => changes.untracked.push(path),
            "M" | "D" | "R" | "C" => changes.tracked.push(path),
            other => return Err(diff.unexpected(format!("unknown diff status '{other}'"))),
        }
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::WorkspaceRefs;

    fn output(stdout: &str) -> ToolOutput {
        ToolOutput {
            command: "jj diff (in /repo/.worktrees/feat)".to_owned(),
            success: true,
            code: Some(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: String::new(),
        }
    }

    #[test]
    fn diff_entries_map_added_paths_to_untracked_and_others_to_tracked() {
        let changes = parse_workspace_changes(&output(
            "M tracked.txt\nD deleted.txt\nA untracked.txt\nR old.txt\n",
        ))
        .expect("parse diff");
        assert_eq!(
            changes.tracked,
            vec![
                PathBuf::from("tracked.txt"),
                PathBuf::from("deleted.txt"),
                PathBuf::from("old.txt"),
            ]
        );
        assert_eq!(changes.untracked, vec![PathBuf::from("untracked.txt")]);
        assert!(!changes.truncated);
    }

    #[test]
    fn diff_parsing_truncates_at_the_port_limit() {
        let mut stdout = String::new();
        for index in 0..=MAX_WORKSPACE_CHANGES {
            stdout.push_str(&format!("M path-{index:04}.txt\n"));
        }
        let changes = parse_workspace_changes(&output(&stdout)).expect("parse large diff");
        assert!(changes.truncated);
        assert_eq!(changes.tracked.len(), MAX_WORKSPACE_CHANGES);
    }

    #[test]
    fn unknown_diff_status_fails_closed() {
        let error = parse_workspace_changes(&output("Z strange.txt\n"))
            .expect_err("unknown statuses are never guessed");
        assert!(
            matches!(error, VcsError::UnexpectedOutput { ref message, .. } if message.contains("unknown diff status")),
            "{error}"
        );
    }

    #[test]
    fn refs_project_the_fork_branch_and_the_snapshot_ref() {
        let workspace = JjWorkspace {
            vcs_root: PathBuf::from("/repo"),
            path: PathBuf::from("/repo/.worktrees/feat-a"),
            fork: "feat-a".to_owned(),
        };
        assert_eq!(
            workspace.refs(),
            WorkspaceRefs {
                base: "refs/heads/feat-a".to_owned(),
                wip: "refs/slipfork/feat-a/wip".to_owned(),
            }
        );
    }
}
