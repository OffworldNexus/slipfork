//! Worktrees, branch topology, and dirty snapshots for Git and Jujutsu backends.

/// Entry marker for the workspace resolver owned by this crate.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceResolver;

impl WorkspaceResolver {
    /// Builds the resolver marker.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}
