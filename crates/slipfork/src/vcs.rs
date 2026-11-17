//! The composition-root [`VcsWorkspaceFactory`] routing between Git and Jujutsu (`ADR-0005` §3).
//!
//! `slipforkd` never guesses a backend from a CLI flag: a repository whose `<vcs_root>/.jj` is a
//! directory is a Jujutsu repository and is delegated strictly to [`JjWorkspaceAdapter`], which
//! fails closed with [`VcsError::ToolMissing`] when `jj` is absent. There is **no** fallback to
//! Git for a `.jj`-bearing checkout; every other repository uses [`GitWorktreeAdapter`].
//!
//! Both adapters are held as fields and every port method goes through the one `backend` rule, so
//! adding a third backend means extending that rule once — never a second ladder per method.

use std::path::Path;

use slipfork_vcs::{
    GitWorktreeAdapter, JjWorkspaceAdapter, RepoRoot, VcsError, VcsWorkspace, VcsWorkspaceFactory,
    WorkspaceRequest,
};

/// The single backend-selecting factory `slipforkd` injects.
#[derive(Debug, Default, Clone, Copy)]
pub struct RepositoryWorkspaces {
    git: GitWorktreeAdapter,
    jj: JjWorkspaceAdapter,
}

impl RepositoryWorkspaces {
    /// Build the factory over both concrete adapters.
    pub fn new() -> Self {
        Self::default()
    }

    /// The one backend-selection rule: a `.jj` directory is a Jujutsu repository (`ADR-0005` §3).
    fn backend(&self, root: &RepoRoot) -> &dyn VcsWorkspaceFactory {
        if is_jj_root(&root.vcs_root) {
            &self.jj
        } else {
            &self.git
        }
    }
}

impl VcsWorkspaceFactory for RepositoryWorkspaces {
    fn create_workspace(
        &self,
        request: &WorkspaceRequest,
    ) -> Result<Box<dyn VcsWorkspace>, VcsError> {
        self.backend(&request.repo).create_workspace(request)
    }

    fn current_branch(&self, repo: &RepoRoot) -> Result<Option<String>, VcsError> {
        self.backend(repo).current_branch(repo)
    }
}

/// A Jujutsu repository is exactly a checkout carrying a `.jj` directory (`ADR-0005` §3).
fn is_jj_root(vcs_root: &Path) -> bool {
    vcs_root.join(".jj").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_dot_jj_directory_selects_the_jujutsu_backend() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!is_jj_root(dir.path()));
        std::fs::create_dir(dir.path().join(".jj")).expect("create .jj");
        assert!(is_jj_root(dir.path()));
        std::fs::remove_dir(dir.path().join(".jj")).expect("remove .jj");
        std::fs::write(dir.path().join(".jj"), "not a directory").expect("write file");
        assert!(!is_jj_root(dir.path()), "a .jj file is not a jj root");
    }

    #[test]
    fn the_branch_probe_routes_exactly_like_workspace_creation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = RepoRoot {
            vcs_root: dir.path().to_path_buf(),
            worktree_root: dir.path().to_path_buf(),
            project: slipfork_world::ProjectSlug::parse("routing").expect("valid test slug"),
        };
        let factory = RepositoryWorkspaces::new();

        // A plain directory selects Git: `symbolic-ref` fails because it is not a repository.
        let git_error = factory
            .current_branch(&repo)
            .expect_err("outside a repository the probe fails closed");
        assert!(
            matches!(git_error, VcsError::CommandFailed { ref command, .. } if command.starts_with("git ")),
            "a non-Jujutsu checkout is probed by git, got: {git_error}"
        );

        std::fs::create_dir(dir.path().join(".jj")).expect("create .jj");
        let jj_error = factory
            .current_branch(&repo)
            .expect_err("a .jj root is probed by Jujutsu");
        let answered_by_jj = matches!(&jj_error, VcsError::ToolMissing { tool, .. } if tool == "jj")
            || matches!(&jj_error, VcsError::CommandFailed { command, .. } if command.starts_with("jj "));
        assert!(
            answered_by_jj,
            "a .jj root must be probed by jj, never by the Git adapter, got: {jj_error}"
        );
    }
}
