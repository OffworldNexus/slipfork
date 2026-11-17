//! `GitWorktreeAdapter`: `.worktrees/<fork>` creation plus the synthesized
//! `refs/slipfork/<fork>/wip` dirty view computed through a scratch `GIT_INDEX_FILE` under
//! `<vcs_root>/.slipfork/git/`.
//!
//! Every Git invocation is an explicit `Command` argument vector (never a shell string) and the live
//! `.git/index` is never opened, renamed, or locked: the snapshot is built in a scratch index, and
//! the checkout is materialized through the linked worktree's own index.
//!
//! The ref layout is dictated by Git's D/F refname lock (`ADR-0005` §3): the fork branch stays
//! `refs/heads/<fork>` while the dirty view lives at `refs/slipfork/<fork>/wip`, pointing at the
//! base commit when the tree is clean so `wip == base` holds without an empty synthetic commit.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use slipfork_world::validate_fork_name;

use crate::git_store::{delete_ref, path_exists, ref_exists, remove_if_exists, resolve_commit};
use crate::ports::{
    MAX_WORKSPACE_CHANGES, RepoRoot, VcsError, VcsWorkspace, VcsWorkspaceFactory,
    WORKTREES_DIRECTORY, WorkspaceChanges, WorkspaceRequest, fork_branch_ref, fork_snapshot_ref,
};
use crate::tool_command::{ToolOutput, run_git, run_git_checked, run_git_with_pinned_identity};

/// Repository-relative Slipfork state directory (never `/tmp`: it sits beside the checkout).
const STATE_DIRECTORY: &str = ".slipfork";
/// Directory under [`STATE_DIRECTORY`] holding per-fork scratch indexes.
const SCRATCH_INDEX_DIRECTORY: &str = "git";
/// Message of every synthesized dirty-view commit.
const SNAPSHOT_MESSAGE: &str = "slipfork: fork snapshot";
/// Commit-ish used when [`WorkspaceRequest::base`] is `None`.
const DEFAULT_BASE: &str = "HEAD";

/// Git [`VcsWorkspaceFactory`]: creates `.worktrees/<fork>` checkouts on `refs/heads/<fork>` and
/// projects the dirty working tree into `refs/slipfork/<fork>/wip`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GitWorktreeAdapter;

impl GitWorktreeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl VcsWorkspaceFactory for GitWorktreeAdapter {
    fn create_workspace(
        &self,
        request: &WorkspaceRequest,
    ) -> Result<Box<dyn VcsWorkspace>, VcsError> {
        let repo = &request.repo;
        let fork = request.fork.as_str();
        // Fork names become a `.worktrees/<fork>` path component and a refname, so they use the
        // same single-DNS-label contract as `.localhost` user forks (no slashes, dots, or `test-*`).
        validate_fork_name(fork, &[])?;

        let worktree = repo.vcs_root.join(WORKTREES_DIRECTORY).join(fork);
        let branch = fork_branch_ref(fork);
        let snapshot_ref = fork_snapshot_ref(fork);
        if path_exists(&worktree)?
            || ref_exists(&repo.vcs_root, &branch)?
            || ref_exists(&repo.vcs_root, &snapshot_ref)?
        {
            return Err(VcsError::WorkspaceExists {
                fork: fork.to_owned(),
            });
        }

        // Read-only and scratch-index work happens before anything is created, so a failure here
        // leaves the repository untouched (the `.worktrees/<fork>` checkout would otherwise appear
        // as an untracked path in its own snapshot).
        let base = resolve_base(&repo.worktree_root, request.base.as_deref())?;
        let snapshot = synthesize_snapshot(repo, fork, &base)?;

        let worktrees = repo.vcs_root.join(WORKTREES_DIRECTORY);
        fs::create_dir_all(&worktrees).map_err(|err| VcsError::io(&worktrees, &err))?;
        if let Err(err) = materialize(repo, fork, &worktree, &base, &snapshot) {
            rollback(repo, fork, &worktree);
            return Err(err);
        }

        Ok(Box::new(GitWorkspace {
            vcs_root: repo.vcs_root.clone(),
            path: worktree,
            fork: fork.to_owned(),
        }))
    }

    fn current_branch(&self, repo: &RepoRoot) -> Result<Option<String>, VcsError> {
        // The requesting checkout's own `HEAD`, so a linked `.worktrees/<fork>` reports its branch.
        let out = run_git(
            &repo.worktree_root,
            &[
                OsStr::new("symbolic-ref"),
                OsStr::new("--short"),
                OsStr::new("--quiet"),
                OsStr::new("HEAD"),
            ],
            None,
        )?;
        match out.code {
            // A branch checkout: `HEAD` is a symref to `refs/heads/<branch>`.
            Some(0) => {
                let branch = out.stdout_trimmed()?;
                if branch.is_empty() {
                    return Err(out.unexpected("symbolic-ref returned an empty branch name"));
                }
                Ok(Some(branch.to_owned()))
            }
            // `symbolic-ref --quiet HEAD` exits 1 exactly when `HEAD` is not a symbolic ref, i.e. a
            // detached `HEAD`, and prints nothing. Any other exit 1 — a diagnostic on stderr — is a
            // real failure and must never be read as "detached".
            Some(1) if out.stderr.trim().is_empty() => Ok(None),
            _ => Err(out.failure()),
        }
    }
}

/// One materialized `.worktrees/<fork>` checkout.
struct GitWorkspace {
    vcs_root: PathBuf,
    path: PathBuf,
    fork: String,
}

impl VcsWorkspace for GitWorkspace {
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
        let status = run_git_checked(
            &self.path,
            &[
                OsStr::new("status"),
                OsStr::new("--porcelain=v1"),
                OsStr::new("-z"),
                OsStr::new("--untracked-files=all"),
            ],
            None,
        )?;
        parse_workspace_changes(&status)
    }

    fn forget(&self) -> Result<(), VcsError> {
        teardown(&self.vcs_root, &self.fork, &self.path)
    }
}

/// Digest of one fork snapshot: the commit `refs/slipfork/<fork>/wip` should point at.
struct Snapshot {
    commit: String,
}

/// Resolve a requested base commit-ish to a commit OID (`HEAD` by default).
fn resolve_base(worktree_root: &Path, base: Option<&str>) -> Result<String, VcsError> {
    resolve_commit(worktree_root, base.unwrap_or(DEFAULT_BASE))
}

/// Build the dirty view of `repo.worktree_root` against `base` in a scratch index.
///
/// The snapshot commit is authored by Slipfork: `-c user.name=slipfork -c user.email=slipfork@localhost`
/// are passed as Git global options before `commit-tree`, so a host without a configured
/// `user.email` never blocks a fork and the developer's Git config is never read, written, or
/// required. Returns the base commit itself when the staged tree equals the base tree, so a clean
/// fork's `refs/slipfork/<fork>/wip` is byte-identical to `refs/heads/<fork>`.
fn synthesize_snapshot(repo: &RepoRoot, fork: &str, base: &str) -> Result<Snapshot, VcsError> {
    let scratch_directory = repo
        .vcs_root
        .join(STATE_DIRECTORY)
        .join(SCRATCH_INDEX_DIRECTORY);
    fs::create_dir_all(&scratch_directory).map_err(|err| VcsError::io(&scratch_directory, &err))?;
    let index = scratch_index_path(&repo.vcs_root, fork);
    remove_if_exists(&index)?;

    let worktree = repo.worktree_root.as_path();
    run_git_checked(
        worktree,
        &[OsStr::new("read-tree"), OsStr::new(base)],
        Some(&index),
    )?;
    stage_snapshot_index(worktree, &index)?;
    let tree = run_git_checked(worktree, &[OsStr::new("write-tree")], Some(&index))?
        .stdout_trimmed()?
        .to_owned();

    let base_tree_spec = format!("{base}^{{tree}}");
    let base_tree = run_git_checked(
        worktree,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--verify"),
            OsStr::new(&base_tree_spec),
        ],
        None,
    )?
    .stdout_trimmed()?
    .to_owned();
    if tree == base_tree {
        return Ok(Snapshot {
            commit: base.to_owned(),
        });
    }

    let commit = run_git_with_pinned_identity(
        worktree,
        &[
            OsStr::new("commit-tree"),
            OsStr::new(&tree),
            OsStr::new("-p"),
            OsStr::new(base),
            OsStr::new("-m"),
            OsStr::new(SNAPSHOT_MESSAGE),
        ],
        None,
    )?
    .ensure_success()?
    .stdout_trimmed()?
    .to_owned();
    Ok(Snapshot { commit })
}

/// Stage the whole requesting worktree into the scratch index, minus Slipfork's own directories.
///
/// The `git add` pathspec is a bare `.` on purpose: naming `.slipfork` or `.worktrees` explicitly
/// makes Git fail closed (`fatal: Pathspec '.worktrees' is in .gitignore`) in the recommended setup
/// where a repository ignores its own fork checkouts, so Git's own ignore rules decide what is
/// staged. The two Slipfork-owned directories are dropped from the scratch index afterwards
/// instead: a sibling `.worktrees/<other-fork>` must never become an embedded-repository gitlink,
/// and `.slipfork` scratch state must never enter the snapshot. `--cached` never touches the
/// working tree, `--ignore-unmatch` keeps the drop a no-op when nothing was staged there, and `-f`
/// is required because the scratch index file itself lives under `.slipfork/git/` and its bytes
/// change while it is staged.
fn stage_snapshot_index(worktree: &Path, index: &Path) -> Result<(), VcsError> {
    run_git_checked(
        worktree,
        &[
            OsStr::new("add"),
            OsStr::new("-A"),
            OsStr::new("--"),
            OsStr::new("."),
        ],
        Some(index),
    )?;
    run_git_checked(
        worktree,
        &[
            OsStr::new("rm"),
            OsStr::new("-r"),
            OsStr::new("-f"),
            OsStr::new("--cached"),
            OsStr::new("--ignore-unmatch"),
            OsStr::new("-q"),
            OsStr::new("--"),
            OsStr::new(STATE_DIRECTORY),
            OsStr::new(WORKTREES_DIRECTORY),
        ],
        Some(index),
    )?;
    Ok(())
}

/// Create `.worktrees/<fork>` on `refs/heads/<fork>`, publish `refs/slipfork/<fork>/wip`, and
/// materialize the dirty view without touching `<vcs_root>/.git/index`.
fn materialize(
    repo: &RepoRoot,
    fork: &str,
    worktree: &Path,
    base: &str,
    snapshot: &Snapshot,
) -> Result<(), VcsError> {
    run_git_checked(
        &repo.vcs_root,
        &[
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--no-checkout"),
            worktree.as_os_str(),
            OsStr::new("-b"),
            OsStr::new(fork),
            OsStr::new(base),
        ],
        None,
    )?;
    let snapshot_ref = fork_snapshot_ref(fork);
    run_git_checked(
        &repo.vcs_root,
        &[
            OsStr::new("update-ref"),
            OsStr::new(&snapshot_ref),
            OsStr::new(&snapshot.commit),
        ],
        None,
    )?;
    // Check out the snapshot, then rewind only the linked worktree's index to the base tree: the
    // files stay dirty, so `git status` reproduces the developer's M/D/?? view exactly.
    run_git_checked(
        worktree,
        &[
            OsStr::new("read-tree"),
            OsStr::new("-u"),
            OsStr::new("--reset"),
            OsStr::new(&snapshot.commit),
        ],
        None,
    )?;
    run_git_checked(worktree, &[OsStr::new("read-tree"), OsStr::new(base)], None)?;
    Ok(())
}

/// Remove every artifact one fork owns: checkout, worktree metadata, branch, dirty-view ref, and
/// scratch index.
///
/// Shared by [`VcsWorkspace::forget`], which propagates failures, and [`rollback`], which discards
/// them because it runs while an earlier error is already being reported.
fn teardown(vcs_root: &Path, fork: &str, worktree: &Path) -> Result<(), VcsError> {
    if path_exists(worktree)? {
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
        // Idempotent re-run: the checkout is already gone, so only stale worktree metadata
        // (if any) remains to be pruned.
        run_git_checked(
            vcs_root,
            &[OsStr::new("worktree"), OsStr::new("prune")],
            None,
        )?;
    }
    let snapshot_ref = fork_snapshot_ref(fork);
    if ref_exists(vcs_root, &snapshot_ref)? {
        delete_ref(vcs_root, &snapshot_ref)?;
    }
    let branch = fork_branch_ref(fork);
    if ref_exists(vcs_root, &branch)? {
        run_git_checked(
            vcs_root,
            &[OsStr::new("branch"), OsStr::new("-D"), OsStr::new(fork)],
            None,
        )?;
    }
    remove_if_exists(&scratch_index_path(vcs_root, fork))
}

/// Remove artifacts created by a failed [`VcsWorkspaceFactory::create_workspace`] call.
///
/// Cleanup is best-effort on purpose: the caller receives the original, actionable failure. Only
/// the branch, snapshot ref, scratch index, and checkout this call created are touched.
fn rollback(repo: &RepoRoot, fork: &str, worktree: &Path) {
    let _ = teardown(&repo.vcs_root, fork, worktree);
}

/// Parse `git status --porcelain=v1 -z` into tracked/untracked buckets.
fn parse_workspace_changes(status: &ToolOutput) -> Result<WorkspaceChanges, VcsError> {
    let mut changes = WorkspaceChanges::default();
    let mut fields = status.stdout.split(|byte| *byte == 0);
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        let Some((code, path)) = record.split_at_checked(2) else {
            return Err(status.unexpected("porcelain status record is shorter than two bytes"));
        };
        let Some(path) = path.strip_prefix(b" ") else {
            return Err(status.unexpected("porcelain status record misses its separator"));
        };
        let rename_or_copy = code.contains(&b'R') || code.contains(&b'C');
        if rename_or_copy {
            // `-z` emits the original path as a second NUL-terminated field for R/C records.
            let _ = fields.next();
        }
        if changes.tracked.len() + changes.untracked.len() >= MAX_WORKSPACE_CHANGES {
            changes.truncated = true;
            break;
        }
        let path = PathBuf::from(OsStr::from_bytes(path));
        if code == b"??".as_slice() {
            changes.untracked.push(path);
        } else {
            changes.tracked.push(path);
        }
    }
    Ok(changes)
}

/// Path of the scratch index used to synthesize `fork`'s dirty view.
///
/// The fork snapshot is built at `<vcs_root>/.slipfork/git/index-<fork>` so a fork never opens,
/// renames, or locks the live `<vcs_root>/.git/index`.
pub fn scratch_index_path(vcs_root: &Path, fork: &str) -> PathBuf {
    vcs_root
        .join(STATE_DIRECTORY)
        .join(SCRATCH_INDEX_DIRECTORY)
        .join(format!("index-{fork}"))
}
