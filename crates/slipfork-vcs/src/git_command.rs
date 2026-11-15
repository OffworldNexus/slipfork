//! Private `git` subprocess runner shared by the Git adapters.
//!
//! Every invocation is an explicit argument vector (never a shell string) with `LC_ALL=C` and a
//! scrubbed `GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE` / `GIT_COMMON_DIR` environment, so an
//! ambient Git environment can never hijack repository discovery or index selection.

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::Command;

use crate::ports::VcsError;

/// The Git executable invoked by every adapter step.
const GIT: &str = "git";

/// Environment variables that redirect Git at a different repository and must never leak in.
const SCRUBBED_ENV: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// Captured outcome of one Git invocation.
pub(crate) struct GitOutput {
    command: String,
    pub(crate) success: bool,
    pub(crate) code: Option<i32>,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: String,
}

impl GitOutput {
    /// UTF-8 view of stdout.
    pub(crate) fn stdout_str(&self) -> Result<&str, VcsError> {
        std::str::from_utf8(&self.stdout).map_err(|err| VcsError::UnexpectedOutput {
            command: self.command.clone(),
            message: format!("stdout is not valid UTF-8: {err}"),
        })
    }

    /// Trimmed stdout, as every ref/OID/path probe expects.
    pub(crate) fn stdout_trimmed(&self) -> Result<&str, VcsError> {
        Ok(self.stdout_str()?.trim())
    }

    /// Build [`VcsError::UnexpectedOutput`] for this invocation (non-UTF-8 or malformed output).
    pub(crate) fn unexpected(&self, message: impl Into<String>) -> VcsError {
        VcsError::UnexpectedOutput {
            command: self.command.clone(),
            message: message.into(),
        }
    }

    /// Turn a non-zero exit into [`VcsError::CommandFailed`], surfacing Git's own diagnostic.
    pub(crate) fn failure(self) -> VcsError {
        VcsError::CommandFailed {
            status: self.code.map_or_else(
                || "terminated by signal".to_owned(),
                |code| code.to_string(),
            ),
            command: self.command,
            stderr: self.stderr.trim().to_owned(),
        }
    }

    /// Fail closed unless Git exited `0`.
    pub(crate) fn ensure_success(self) -> Result<Self, VcsError> {
        if self.success {
            Ok(self)
        } else {
            Err(self.failure())
        }
    }
}

/// Spawn Git in `cwd` and capture its output without interpreting the exit status.
///
/// `index_file` sets `GIT_INDEX_FILE`, which is how the fork snapshot is synthesized without ever
/// opening the live repository index.
pub(crate) fn run_git(
    cwd: &Path,
    args: &[&OsStr],
    index_file: Option<&Path>,
) -> Result<GitOutput, VcsError> {
    let command = describe(cwd, args);
    let mut cmd = Command::new(GIT);
    cmd.arg("-C")
        .arg(cwd)
        .env("LC_ALL", "C")
        .env("LANGUAGE", "C")
        .args(args);
    for key in SCRUBBED_ENV {
        cmd.env_remove(key);
    }
    if let Some(index_file) = index_file {
        cmd.env("GIT_INDEX_FILE", index_file);
    }
    let output = cmd
        .output()
        .map_err(|err| spawn_error(command.clone(), err))?;
    Ok(GitOutput {
        command,
        success: output.status.success(),
        code: output.status.code(),
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Spawn Git and fail closed on any non-zero exit.
pub(crate) fn run_git_checked(
    cwd: &Path,
    args: &[&OsStr],
    index_file: Option<&Path>,
) -> Result<GitOutput, VcsError> {
    run_git(cwd, args, index_file)?.ensure_success()
}

/// Map a failed spawn: a missing executable is a distinct, actionable `ToolMissing`.
fn spawn_error(command: String, err: io::Error) -> VcsError {
    if err.kind() == io::ErrorKind::NotFound {
        VcsError::ToolMissing {
            tool: GIT.to_owned(),
        }
    } else {
        VcsError::CommandSpawn {
            command,
            message: err.to_string(),
        }
    }
}

/// Human-readable `git -C <cwd> <args>` label carried by every [`VcsError`].
fn describe(cwd: &Path, args: &[&OsStr]) -> String {
    let mut command = format!("git -C {}", cwd.display());
    for arg in args {
        command.push(' ');
        command.push_str(&arg.to_string_lossy());
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fail_closed_spawn_maps_a_missing_binary_to_tool_missing() {
        let err = spawn_error(
            "git -C /repo status".to_owned(),
            io::Error::from(io::ErrorKind::NotFound),
        );
        assert_eq!(
            err,
            VcsError::ToolMissing {
                tool: "git".to_owned()
            }
        );
    }

    #[test]
    fn spawn_errors_that_are_not_not_found_surface_the_command() {
        let err = spawn_error(
            "git -C /repo status".to_owned(),
            io::Error::from(io::ErrorKind::PermissionDenied),
        );
        assert!(matches!(
            err,
            VcsError::CommandSpawn { command, .. } if command == "git -C /repo status"
        ));
    }
}
