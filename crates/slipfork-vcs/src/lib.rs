//! Worktrees, branch topology, and dirty snapshots for Git and Jujutsu backends.
//!
//! `VcsWorkspace` is the canonical `specs/03-under-the-hood.md` §5 port: the daemon asks this crate
//! which branch a checkout is on, switches it, and asks whether it carries uncommitted work. The
//! adapter never reads the environment and never shells out through `/bin/sh`; the composition root
//! injects the executable path, so `GIT_DIR` and friends cannot redirect a call.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Whether a checkout carries uncommitted or untracked work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dirty {
    /// Safe to discard or snapshot as-is.
    Clean,
    /// Refused without an explicit `--force`.
    Dirty,
}

/// One typed VCS refusal: a single line that never dumps an argv or a full stderr transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VcsError {
    /// The injected `git` executable could not be started at all.
    Unavailable {
        /// The operating-system error, already one line.
        reason: String,
    },
    /// The checkout is detached or is not a repository, so it has no branch name.
    NoBranch {
        /// The checkout directory.
        path: PathBuf,
    },
    /// Switching to a branch failed.
    Switch {
        /// The checkout directory.
        path: PathBuf,
        /// The requested branch.
        branch: String,
        /// The first line of `git`'s own refusal.
        reason: String,
    },
    /// Any other `git` invocation failed.
    Command {
        /// What the call was trying to read, for the one-line message.
        action: &'static str,
        /// The first line of `git`'s own refusal.
        reason: String,
    },
}

impl fmt::Display for VcsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable { reason } => write!(formatter, "cannot run git: {reason}"),
            Self::NoBranch { path } => write!(
                formatter,
                "`{}` has no git branch (detached HEAD)",
                path.display()
            ),
            Self::Switch {
                path,
                branch,
                reason,
            } => write!(
                formatter,
                "cannot switch `{}` to {branch}: {reason}",
                path.display()
            ),
            Self::Command { action, reason } => write!(formatter, "git {action} failed: {reason}"),
        }
    }
}

impl std::error::Error for VcsError {}

/// Branch topology and dirty-snapshot detection for one checkout.
///
/// Every method is absolute-path in and typed error out: the adapter never reads the environment,
/// never caches a repository handle, and never makes a caller pre-parse `git` output.
pub trait VcsWorkspace: Send + Sync {
    /// The checkout's current branch, refusing a detached `HEAD`.
    fn branch(&self, project: &Path) -> Result<String, VcsError>;

    /// Switches `project` to `branch`, creating it when the parent never had it.
    fn switch(&self, project: &Path, branch: &str) -> Result<(), VcsError>;

    /// Whether `project` carries uncommitted or untracked work.
    ///
    /// A path that is not a directory is `Clean`: a fork whose checkout never materialised must not
    /// block its own removal.
    fn dirtiness(&self, project: &Path) -> Result<Dirty, VcsError>;
}

/// The `git` adapter behind [`VcsWorkspace`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorkspace {
    git: PathBuf,
}

impl GitWorkspace {
    /// Builds the adapter around the injected `git` executable.
    #[must_use]
    pub fn new(git: impl Into<PathBuf>) -> Self {
        Self { git: git.into() }
    }

    /// Runs one plumbing call with a clean environment, so an inherited `GIT_DIR` cannot point it
    /// at another repository.
    fn run(&self, project: &Path, args: &[&str]) -> Result<Output, VcsError> {
        Command::new(&self.git)
            .env_clear()
            .arg("-C")
            .arg(project)
            .args(args)
            .output()
            .map_err(|error| VcsError::Unavailable {
                reason: error.to_string(),
            })
    }

    /// The first non-empty line of `git`'s own stderr, or a bare fallback.
    fn refusal(output: &Output) -> String {
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("git refused the operation")
            .to_owned()
    }
}

impl VcsWorkspace for GitWorkspace {
    fn branch(&self, project: &Path) -> Result<String, VcsError> {
        let output = self.run(project, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !output.status.success() || branch.is_empty() || branch == "HEAD" {
            return Err(VcsError::NoBranch {
                path: project.to_path_buf(),
            });
        }
        Ok(branch)
    }

    fn switch(&self, project: &Path, branch: &str) -> Result<(), VcsError> {
        let reference = format!("refs/heads/{branch}");
        let existing = self.run(project, &["rev-parse", "--verify", &reference])?;
        let output = if existing.status.success() {
            self.run(project, &["switch", branch])?
        } else {
            self.run(project, &["switch", "-c", branch])?
        };
        if !output.status.success() {
            return Err(VcsError::Switch {
                path: project.to_path_buf(),
                branch: branch.to_owned(),
                reason: Self::refusal(&output),
            });
        }
        Ok(())
    }

    fn dirtiness(&self, project: &Path) -> Result<Dirty, VcsError> {
        if !project.is_dir() {
            return Ok(Dirty::Clean);
        }
        let output = self.run(project, &["status", "--porcelain"])?;
        if !output.status.success() {
            return Err(VcsError::Command {
                action: "status",
                reason: Self::refusal(&output),
            });
        }
        if output.stdout.is_empty() {
            return Ok(Dirty::Clean);
        }
        Ok(Dirty::Dirty)
    }
}
