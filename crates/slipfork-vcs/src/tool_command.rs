//! Private `git` / `jj` subprocess runners shared by the Git and Jujutsu adapters.
//!
//! Every invocation is an explicit argument vector (never a shell string) with `LC_ALL=C` and a
//! scrubbed `GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE` / `GIT_COMMON_DIR` /
//! `GIT_OBJECT_DIRECTORY` / `GIT_ALTERNATE_OBJECT_DIRECTORIES` environment, so an ambient Git
//! environment can never hijack repository discovery, index selection, or the Git store Jujutsu
//! projects its fork refs into.

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::Command;

use crate::ports::VcsError;

/// Environment variables that redirect a tool at a different repository/store and must never leak in.
const SCRUBBED_ENV: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// One external version-control tool driven by the adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    Git,
    Jj,
}

impl Tool {
    /// Executable name resolved through `PATH`.
    fn binary(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Jj => "jj",
        }
    }

    /// Actionable hint carried by [`VcsError::ToolMissing`] when the executable is absent.
    fn missing_hint(self) -> &'static str {
        match self {
            Self::Git => {
                "install Git (https://git-scm.com) to resolve repositories, create fork worktrees, and synthesize fork snapshots"
            }
            Self::Jj => {
                "install Jujutsu (https://jj-vcs.github.io) to use Jujutsu fork workspaces, or fork from a Git checkout; to name a checkout whose directory basename is not a valid project slug, set `project = \"<slug>\"` in `slipfork.toml`"
            }
        }
    }
}

/// Captured outcome of one tool invocation.
pub(crate) struct ToolOutput {
    pub(crate) command: String,
    pub(crate) success: bool,
    pub(crate) code: Option<i32>,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: String,
}

impl ToolOutput {
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

    /// Turn a non-zero exit into [`VcsError::CommandFailed`], surfacing the tool's own diagnostic.
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

    /// Fail closed unless the tool exited `0`.
    pub(crate) fn ensure_success(self) -> Result<Self, VcsError> {
        if self.success {
            Ok(self)
        } else {
            Err(self.failure())
        }
    }
}

/// Spawn `tool` in `cwd` and capture its output without interpreting the exit status.
///
/// `env` carries the few variables an adapter legitimately adds (Git's scratch `GIT_INDEX_FILE`).
fn run(
    tool: Tool,
    cwd: &Path,
    args: &[&OsStr],
    env: &[(&str, &OsStr)],
) -> Result<ToolOutput, VcsError> {
    let command = describe(tool, cwd, args);
    let mut cmd = Command::new(tool.binary());
    match tool {
        Tool::Git => {
            cmd.arg("-C").arg(cwd);
        }
        Tool::Jj => {
            // `jj` has no `-C`: a missing working directory would be misreported as a missing
            // binary by the spawn error path, so reject it explicitly first.
            if !cwd.is_dir() {
                return Err(VcsError::Io {
                    path: cwd.to_path_buf(),
                    message: "not a directory".to_owned(),
                });
            }
            cmd.current_dir(cwd);
        }
    }
    cmd.env("LC_ALL", "C").env("LANGUAGE", "C").args(args);
    for key in SCRUBBED_ENV {
        cmd.env_remove(key);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    let output = cmd
        .output()
        .map_err(|err| spawn_error(tool, command.clone(), err))?;
    Ok(ToolOutput {
        command,
        success: output.status.success(),
        code: output.status.code(),
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Spawn Git in `cwd` without interpreting the exit status.
///
/// `index_file` sets `GIT_INDEX_FILE`, which is how the fork snapshot is synthesized without ever
/// opening the live repository index.
pub(crate) fn run_git(
    cwd: &Path,
    args: &[&OsStr],
    index_file: Option<&Path>,
) -> Result<ToolOutput, VcsError> {
    match index_file {
        Some(index) => run(
            Tool::Git,
            cwd,
            args,
            &[("GIT_INDEX_FILE", index.as_os_str())],
        ),
        None => run(Tool::Git, cwd, args, &[]),
    }
}

/// Spawn Git and fail closed on any non-zero exit.
pub(crate) fn run_git_checked(
    cwd: &Path,
    args: &[&OsStr],
    index_file: Option<&Path>,
) -> Result<ToolOutput, VcsError> {
    run_git(cwd, args, index_file)?.ensure_success()
}

/// Spawn `jj` in `cwd` without interpreting the exit status.
pub(crate) fn run_jj(cwd: &Path, args: &[&OsStr]) -> Result<ToolOutput, VcsError> {
    run(Tool::Jj, cwd, args, &[])
}

/// Spawn `jj` and fail closed on any non-zero exit.
pub(crate) fn run_jj_checked(cwd: &Path, args: &[&OsStr]) -> Result<ToolOutput, VcsError> {
    run_jj(cwd, args)?.ensure_success()
}

/// Map a failed spawn: a missing executable is a distinct, actionable `ToolMissing`.
fn spawn_error(tool: Tool, command: String, err: io::Error) -> VcsError {
    if err.kind() == io::ErrorKind::NotFound {
        VcsError::ToolMissing {
            tool: tool.binary().to_owned(),
            hint: tool.missing_hint().to_owned(),
        }
    } else {
        VcsError::CommandSpawn {
            command,
            message: err.to_string(),
        }
    }
}

/// Human-readable `<tool> <args> (in <cwd>)` label carried by every [`VcsError`].
fn describe(tool: Tool, cwd: &Path, args: &[&OsStr]) -> String {
    let mut command = tool.binary().to_owned();
    for arg in args {
        command.push(' ');
        command.push_str(&arg.to_string_lossy());
    }
    command.push_str(" (in ");
    command.push_str(&cwd.display().to_string());
    command.push(')');
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fail_closed_spawn_maps_a_missing_binary_to_tool_missing() {
        for (tool, name) in [(Tool::Git, "git"), (Tool::Jj, "jj")] {
            let err = spawn_error(
                tool,
                format!("{name} status (in /repo)"),
                io::Error::from(io::ErrorKind::NotFound),
            );
            match err {
                VcsError::ToolMissing {
                    tool: ref seen,
                    ref hint,
                } => {
                    assert_eq!(seen, name);
                    assert!(
                        !hint.is_empty(),
                        "a missing tool always carries an actionable hint"
                    );
                }
                other => panic!("expected ToolMissing, got {other}"),
            }
        }
    }

    #[test]
    fn missing_jj_hint_names_the_project_override() {
        let err = spawn_error(
            Tool::Jj,
            "jj --version (in /repo)".to_owned(),
            io::Error::from(io::ErrorKind::NotFound),
        );
        let VcsError::ToolMissing { hint, .. } = err else {
            panic!("expected ToolMissing");
        };
        assert!(
            hint.contains("project = \"<slug>\"") && hint.contains("slipfork.toml"),
            "the jj hint must point at the project override, got: {hint}"
        );
    }

    #[test]
    fn spawn_errors_that_are_not_not_found_surface_the_command() {
        let err = spawn_error(
            Tool::Git,
            "git status (in /repo)".to_owned(),
            io::Error::from(io::ErrorKind::PermissionDenied),
        );
        assert!(matches!(
            err,
            VcsError::CommandSpawn { command, .. } if command == "git status (in /repo)"
        ));
    }
}
