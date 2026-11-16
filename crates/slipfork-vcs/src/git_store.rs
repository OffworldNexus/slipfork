//! Git ref-store primitives shared by the [`GitWorktreeAdapter`](crate::git::GitWorktreeAdapter)
//! and [`JjWorkspaceAdapter`](crate::jj::JjWorkspaceAdapter).
//!
//! Both adapters project their fork refs into a Git store — Jujutsu workspaces onto the repository's
//! colocated Git store, so `git fetch slipfork` can move the commits (`ADR-0005` §3) — so ref
//! existence, ref deletion, commit resolution, and the workspace filesystem probes live here once.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::Path;

use crate::ports::VcsError;
use crate::tool_command::{run_git, run_git_checked};

/// `true` when `reference` exists in `root`'s ref store.
pub(crate) fn ref_exists(root: &Path, reference: &str) -> Result<bool, VcsError> {
    let out = run_git(
        root,
        &[
            OsStr::new("show-ref"),
            OsStr::new("--verify"),
            OsStr::new("--quiet"),
            OsStr::new(reference),
        ],
        None,
    )?;
    match out.code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(out.failure()),
    }
}

/// Delete `reference` (callers check [`ref_exists`] first, keeping teardown idempotent).
pub(crate) fn delete_ref(root: &Path, reference: &str) -> Result<(), VcsError> {
    run_git_checked(
        root,
        &[
            OsStr::new("update-ref"),
            OsStr::new("-d"),
            OsStr::new(reference),
        ],
        None,
    )?;
    Ok(())
}

/// Resolve `reference` to the commit object id the remote helper advertises and verifies.
///
/// The `^{commit}` peel makes a tag or tree ref fail closed instead of being served as a commit.
pub(crate) fn resolve_commit(root: &Path, reference: &str) -> Result<String, VcsError> {
    let spec = format!("{reference}^{{commit}}");
    let out = run_git_checked(
        root,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--verify"),
            OsStr::new("--end-of-options"),
            OsStr::new(&spec),
        ],
        None,
    )?;
    let oid = out.stdout_trimmed()?;
    if oid.is_empty() {
        return Err(out.unexpected("rev-parse returned an empty commit id"));
    }
    Ok(oid.to_owned())
}

/// `true` when `path` exists (including broken symlinks).
pub(crate) fn path_exists(path: &Path) -> Result<bool, VcsError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(VcsError::io(path, &err)),
    }
}

/// Remove `path`, tolerating an already-absent file.
pub(crate) fn remove_if_exists(path: &Path) -> Result<(), VcsError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(VcsError::io(path, &err)),
    }
}
