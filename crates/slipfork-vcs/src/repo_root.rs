//! `GitRepoRootResolver`: canonical `<vcs_root>` / `<worktree_root>` / [`ProjectSlug`] discovery
//! through `git rev-parse`, failing closed on bare repositories, submodules, and
//! `--separate-git-dir` layouts (`ADR-0005` §3).
//!
//! The `project = "<slug>"` override is injected as a [`ProjectSlugOverride`] capability, so this
//! crate honors `<vcs_root>/slipfork.toml` without owning a TOML parser or depending on
//! `slipfork-dag`.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use slipfork_world::ProjectSlug;

use crate::ports::{ProjectSlugOverride, RepoRoot, RepoRootResolver, VcsError};
use crate::tool_command::{ToolOutput, run_git, run_git_checked};

/// First line Git emits when `start` is outside any repository (`LC_ALL=C`).
const NOT_A_REPOSITORY_PREFIX: &str = "fatal: not a git repository (or any ";

/// Git [`RepoRootResolver`]: resolves a start directory with `git rev-parse` only.
pub struct GitRepoRootResolver {
    project_override: Arc<dyn ProjectSlugOverride>,
}

impl GitRepoRootResolver {
    /// Build a resolver whose `project` precedence is: injected override, else `<vcs_root>`
    /// directory basename.
    pub fn new(project_override: Arc<dyn ProjectSlugOverride>) -> Self {
        Self { project_override }
    }

    /// Resolve an otherwise-unsupported topology through the explicit `project` override.
    ///
    /// Bare repositories have no working tree and therefore no `<vcs_root>/slipfork.toml`, so they
    /// are rejected before this point. Submodules and separate-git-dir checkouts do have one, and
    /// their slug may never be derived from the directory basename (it would name the admin store
    /// or `.git/modules/<id>`); an explicit override is the only honest way to name them.
    fn resolve_unsupported(
        &self,
        topology: &'static str,
        vcs_root: PathBuf,
        worktree_root: PathBuf,
        hint: String,
    ) -> Result<RepoRoot, VcsError> {
        match self.project_override.project_override(&vcs_root)? {
            Some(project) => Ok(RepoRoot {
                vcs_root,
                worktree_root,
                project,
            }),
            None => Err(VcsError::UnsupportedTopology {
                topology,
                root: vcs_root,
                hint,
            }),
        }
    }
}

impl RepoRootResolver for GitRepoRootResolver {
    fn resolve(&self, start: &Path) -> Result<RepoRoot, VcsError> {
        let start = fs::canonicalize(start).map_err(|err| VcsError::io(start, &err))?;
        if !start.is_dir() {
            return Err(VcsError::Io {
                path: start,
                message: "not a directory".to_owned(),
            });
        }

        // Bare repositories have no worktree, so `--show-toplevel` would fail first with a
        // misleading message; probe the topology before anything else.
        let bare = run_git(
            &start,
            &[OsStr::new("rev-parse"), OsStr::new("--is-bare-repository")],
            None,
        )?;
        if !bare.success {
            if is_not_a_repository(&bare) {
                return Err(VcsError::NotARepository { start });
            }
            return Err(bare.failure());
        }
        if bare.stdout_trimmed()? == "true" {
            return Err(bare_repository_error(&start)?);
        }

        let worktree_root = probe_path(
            &start,
            &[OsStr::new("rev-parse"), OsStr::new("--show-toplevel")],
        )?;

        let superproject = run_git_checked(
            &worktree_root,
            &[
                OsStr::new("rev-parse"),
                OsStr::new("--show-superproject-working-tree"),
            ],
            None,
        )?
        .stdout_trimmed()?
        .to_owned();
        if !superproject.is_empty() {
            let hint = format!(
                "this checkout is a git submodule of {superproject}; set `project = \"<slug>\"` in {} to name it explicitly",
                config_hint(&worktree_root)
            );
            return self.resolve_unsupported(
                "submodule",
                worktree_root.clone(),
                worktree_root,
                hint,
            );
        }

        let git_dir = probe_path(
            &worktree_root,
            &[
                OsStr::new("rev-parse"),
                OsStr::new("--path-format=absolute"),
                OsStr::new("--git-dir"),
            ],
        )?;
        let common_dir = probe_path(
            &worktree_root,
            &[
                OsStr::new("rev-parse"),
                OsStr::new("--path-format=absolute"),
                OsStr::new("--git-common-dir"),
            ],
        )?;
        let dot_git = worktree_root.join(".git");

        let vcs_root = if is_plain_checkout(&dot_git, &git_dir) {
            worktree_root.clone()
        } else if let Some(main_root) = linked_worktree_root(&dot_git, &git_dir, &common_dir) {
            main_root
        } else {
            let hint = format!(
                "this checkout uses a git directory outside its working tree or an unrecognized gitdir indirection; set `project = \"<slug>\"` in {} to name it explicitly",
                config_hint(&worktree_root)
            );
            return self.resolve_unsupported(
                "separate git directory",
                worktree_root.clone(),
                worktree_root,
                hint,
            );
        };

        let project = match self.project_override.project_override(&vcs_root)? {
            Some(project) => project,
            None => slug_from_vcs_root(&vcs_root)?,
        };
        Ok(RepoRoot {
            vcs_root,
            worktree_root,
            project,
        })
    }
}

/// `true` when Git reported that `start` is outside any repository.
fn is_not_a_repository(output: &ToolOutput) -> bool {
    output
        .stderr
        .trim_start()
        .starts_with(NOT_A_REPOSITORY_PREFIX)
}

/// Actionable refusal for a bare repository (`ADR-0005` §3: point at `git clone`).
fn bare_repository_error(start: &Path) -> Result<VcsError, VcsError> {
    let root = probe_path(
        start,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--git-dir"),
        ],
    )?;
    Ok(VcsError::UnsupportedTopology {
        topology: "bare repository",
        root: root.clone(),
        hint: format!(
            "bare repositories have no working tree or `slipfork.toml`; clone it into a working tree (`git clone {} <dir>`) and set `project = \"<slug>\"` in `<dir>/slipfork.toml` if the clone directory name is not a valid project slug",
            root.display()
        ),
    })
}

/// `true` when `<worktree_root>/.git` resolves to the checkout's own git directory (an ordinary
/// checkout, including a `.git` symlink to its git directory).
fn is_plain_checkout(dot_git: &Path, git_dir: &Path) -> bool {
    fs::canonicalize(dot_git).is_ok_and(|canonical| canonical == git_dir)
}

/// Resolve the main checkout root of a linked `git worktree` checkout.
///
/// A linked worktree has `<toplevel>/.git` as a *file*, its git directory inside
/// `<main_root>/.git/worktrees/<id>`, and `--git-dir != --git-common-dir`. The main root is only
/// accepted when it is itself a worktree root, which also rejects `--separate-git-dir` layouts
/// (where `--git-dir == --git-common-dir`).
fn linked_worktree_root(dot_git: &Path, git_dir: &Path, common_dir: &Path) -> Option<PathBuf> {
    let dot_git_is_file = fs::metadata(dot_git).is_ok_and(|metadata| metadata.is_file());
    if !dot_git_is_file || git_dir == common_dir {
        return None;
    }
    if common_dir.file_name() != Some(OsStr::new(".git")) {
        return None;
    }
    let main_root = common_dir.parent()?.to_path_buf();
    let main_toplevel = probe_path(
        &main_root,
        &[OsStr::new("rev-parse"), OsStr::new("--show-toplevel")],
    )
    .ok()?;
    (main_toplevel == main_root).then_some(main_root)
}

/// Derive the canonical slug from `<vcs_root>`'s directory basename.
fn slug_from_vcs_root(vcs_root: &Path) -> Result<ProjectSlug, VcsError> {
    let hint = config_hint(vcs_root);
    let Some(name) = vcs_root.file_name().and_then(OsStr::to_str) else {
        return Err(VcsError::UnnamedProjectRoot {
            vcs_root: vcs_root.to_path_buf(),
            hint,
        });
    };
    ProjectSlug::parse(name).map_err(|_| VcsError::InvalidProjectSlug {
        vcs_root: vcs_root.to_path_buf(),
        slug: name.to_owned(),
        hint,
    })
}

/// `<vcs_root>/slipfork.toml`, where the `project = "<slug>"` override lives.
fn config_hint(vcs_root: &Path) -> String {
    vcs_root.join("slipfork.toml").display().to_string()
}

/// Run a `git rev-parse` path probe and canonicalize its result.
fn probe_path(cwd: &Path, args: &[&OsStr]) -> Result<PathBuf, VcsError> {
    let out = run_git_checked(cwd, args, None)?;
    let raw = out.stdout_trimmed()?.to_owned();
    fs::canonicalize(&raw).map_err(|err| VcsError::io(Path::new(&raw), &err))
}
