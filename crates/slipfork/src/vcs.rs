//! The composition-root [`VcsWorkspaceFactory`] routing between Git and Jujutsu (`ADR-0005` §3).
//!
//! `slipforkd` never guesses a backend from a CLI flag: a repository whose `<vcs_root>/.jj` is a
//! directory is a Jujutsu repository and is delegated strictly to [`JjWorkspaceAdapter`], which
//! fails closed with [`VcsError::ToolMissing`] when `jj` is absent. There is **no** fallback to
//! Git for a `.jj`-bearing checkout; every other repository uses [`GitWorktreeAdapter`].

use std::path::Path;

use slipfork_vcs::{
    GitWorktreeAdapter, JjWorkspaceAdapter, VcsError, VcsWorkspace, VcsWorkspaceFactory,
    WorkspaceRequest,
};

/// The single backend-selecting factory `slipforkd` injects.
#[derive(Debug, Default, Clone, Copy)]
pub struct RepositoryWorkspaces;

impl RepositoryWorkspaces {
    /// Build the factory over both concrete adapters.
    pub const fn new() -> Self {
        Self
    }
}

impl VcsWorkspaceFactory for RepositoryWorkspaces {
    fn create_workspace(
        &self,
        request: &WorkspaceRequest,
    ) -> Result<Box<dyn VcsWorkspace>, VcsError> {
        if is_jj_root(&request.repo.vcs_root) {
            JjWorkspaceAdapter::new().create_workspace(request)
        } else {
            GitWorktreeAdapter::new().create_workspace(request)
        }
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
}
