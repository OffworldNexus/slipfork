//! Fork refusals mapped to the daemon's typed codes and bare one-line messages.

use std::path::Path;

use slipfork_vcs::VcsError;
use slipfork_world::Error as WorldError;

use crate::daemon::Failure;

/// Maps a Btrfs refusal to a typed code and a bare one-line message (the CLI owns the `next:`).
pub(super) fn btrfs_failure(error: &WorldError, project: &Path) -> Failure {
    match error {
        WorldError::NotBtrfs { .. } => Failure::new(
            "not-btrfs",
            format!("`{}` is not on a Btrfs filesystem", project.display()),
        ),
        WorldError::NestedMount { .. } => {
            Failure::new("nested-mount", first_line(&error.to_string()))
        }
        WorldError::UnsupportedEntry { .. } => {
            Failure::new("unsupported-entry", first_line(&error.to_string()))
        }
        WorldError::NotASubvolume { path } => Failure::new(
            "not-subvolume",
            format!("`{}` is not a Btrfs subvolume", path.display()),
        ),
        WorldError::RefusedDirtyCheckout { target } => {
            Failure::new("dirty-fork", format!("{target} has uncommitted changes"))
        }
        other => Failure::new("fork-failed", first_line(&other.to_string())),
    }
}

/// Maps a `VcsWorkspace` refusal to the daemon's typed frame codes.
pub(super) fn vcs_failure(error: &VcsError) -> Failure {
    match error {
        VcsError::Unavailable { .. } => Failure::new("git-missing", "`git` is not on PATH"),
        VcsError::NoBranch { .. } => Failure::new("git-branch", first_line(&error.to_string())),
        VcsError::Switch { .. } | VcsError::Command { .. } => {
            Failure::new("git-switch", first_line(&error.to_string()))
        }
    }
}

pub(super) fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("fork failed").to_owned()
}
