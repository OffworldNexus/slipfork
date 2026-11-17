//! Private `git` / `jj` subprocess runners shared by the Git and Jujutsu adapters.
//!
//! Every invocation is an explicit argument vector (never a shell string) with `LC_ALL=C` and a
//! scrubbed `GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE` / `GIT_COMMON_DIR` /
//! `GIT_OBJECT_DIRECTORY` / `GIT_ALTERNATE_OBJECT_DIRECTORIES` environment, so an ambient Git
//! environment can never hijack repository discovery, index selection, or the Git store Jujutsu
//! projects its fork refs into.
//!
//! The author of every Slipfork-authored snapshot commit is pinned here, as tool *global* options
//! before the subcommand (`git -c …`, `jj --config …`), so a host without a `user.name` /
//! `user.email` never blocks a fork and the developer's own configuration is never read, written,
//! or required (`docs/design/dx-0.1.0-ux.md` §7.3).

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

/// Author name every Slipfork-authored snapshot commit carries.
const SNAPSHOT_AUTHOR_NAME: &str = "slipfork";
/// Author email every Slipfork-authored snapshot commit carries.
const SNAPSHOT_AUTHOR_EMAIL: &str = "slipfork@localhost";

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
    /// Full `<tool> <args…> (in <cwd>)` label, carried by spawn and unexpected-output diagnostics.
    pub(crate) command: String,
    /// `<tool> <subcommand>` label carried by [`VcsError::CommandFailed`].
    pub(crate) subcommand: String,
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

    /// Turn a non-zero exit into one line: the failing subcommand plus the tool's last stderr line.
    ///
    /// `git` prints multi-line advice blocks after a fatal error; the owner-facing contract is a
    /// single `slipfork:` line, so only the final non-blank stderr line is kept
    /// (`docs/design/dx-0.1.0-ux.md` §7.4).
    pub(crate) fn failure(self) -> VcsError {
        VcsError::CommandFailed {
            status: self.code.map_or_else(
                || "terminated by signal".to_owned(),
                |code| code.to_string(),
            ),
            command: self.subcommand,
            stderr: last_stderr_line(&self.stderr),
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
/// `globals` are tool *global* options, placed before the subcommand (`git -c key=value …`,
/// `jj --config key=value …`); `env` carries the few variables an adapter legitimately adds (Git's
/// scratch `GIT_INDEX_FILE`).
fn run(
    tool: Tool,
    cwd: &Path,
    globals: &[String],
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
    cmd.args(globals);
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
        subcommand: subcommand_label(tool, args),
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
    run_git_with(cwd, &[], args, index_file)
}

/// Spawn Git and fail closed on any non-zero exit.
pub(crate) fn run_git_checked(
    cwd: &Path,
    args: &[&OsStr],
    index_file: Option<&Path>,
) -> Result<ToolOutput, VcsError> {
    run_git(cwd, args, index_file)?.ensure_success()
}

/// Spawn Git with Slipfork's pinned snapshot author as global `-c` options before the subcommand.
///
/// Only the snapshot commit uses this: every other Git call is a read, a checkout, or a ref update
/// that must neither depend on nor override the developer's `user.name` / `user.email`.
pub(crate) fn run_git_with_pinned_identity(
    cwd: &Path,
    args: &[&OsStr],
    index_file: Option<&Path>,
) -> Result<ToolOutput, VcsError> {
    run_git_with(cwd, &identity_globals(Tool::Git), args, index_file)
}

/// Combined [`run_git`] body: one place owns the scratch-index environment.
fn run_git_with(
    cwd: &Path,
    globals: &[String],
    args: &[&OsStr],
    index_file: Option<&Path>,
) -> Result<ToolOutput, VcsError> {
    match index_file {
        Some(index) => run(
            Tool::Git,
            cwd,
            globals,
            args,
            &[("GIT_INDEX_FILE", index.as_os_str())],
        ),
        None => run(Tool::Git, cwd, globals, args, &[]),
    }
}

/// Spawn `jj` with Slipfork's pinned identity and fail closed on any non-zero exit.
///
/// Every Jujutsu command snapshots the working copy it runs in, so the author is pinned for all of
/// them; `jj`'s equivalent of `git -c` is the `--config` global option.
pub(crate) fn run_jj_checked(cwd: &Path, args: &[&OsStr]) -> Result<ToolOutput, VcsError> {
    run(Tool::Jj, cwd, &identity_globals(Tool::Jj), args, &[])?.ensure_success()
}

/// Probe `jj` on `PATH`; a missing binary maps to [`VcsError::ToolMissing`].
///
/// The version probe carries no configuration overrides: it must answer "is `jj` installed?" even
/// before any repository or identity is known.
pub(crate) fn probe_jj(cwd: &Path) -> Result<(), VcsError> {
    run(Tool::Jj, cwd, &[], &[OsStr::new("--version")], &[])?.ensure_success()?;
    Ok(())
}

/// Identity global options for `tool`, single-sourced from the snapshot author constants.
pub(crate) fn identity_globals(tool: Tool) -> [String; 4] {
    match tool {
        Tool::Git => [
            "-c".to_owned(),
            format!("user.name={SNAPSHOT_AUTHOR_NAME}"),
            "-c".to_owned(),
            format!("user.email={SNAPSHOT_AUTHOR_EMAIL}"),
        ],
        Tool::Jj => [
            "--config".to_owned(),
            format!("user.name={SNAPSHOT_AUTHOR_NAME}"),
            "--config".to_owned(),
            format!("user.email={SNAPSHOT_AUTHOR_EMAIL}"),
        ],
    }
}

/// Last non-blank stderr line, or an explicit placeholder when the tool said nothing.
fn last_stderr_line(stderr: &str) -> String {
    stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map_or_else(
            || "no stderr output".to_owned(),
            |line| line.trim().to_owned(),
        )
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

/// Human-readable `<tool> <args> (in <cwd>)` label carried by every spawn / output diagnostic.
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

/// `<tool> <subcommand>` label carried by [`VcsError::CommandFailed`].
///
/// The subcommand is the first non-flag argument (`git commit-tree`; Jujutsu reports its two-word
/// commands as `jj workspace`), so the owner-facing line names what failed without dumping argv.
fn subcommand_label(tool: Tool, args: &[&OsStr]) -> String {
    match args
        .iter()
        .find(|arg| !arg.to_string_lossy().starts_with('-'))
    {
        Some(subcommand) => format!("{} {}", tool.binary(), subcommand.to_string_lossy()),
        None => tool.binary().to_owned(),
    }
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

    #[test]
    fn failures_are_one_line_naming_the_subcommand_and_the_last_stderr_line() {
        let failed = ToolOutput {
            command: "git -C /repo commit-tree abc -p def -m msg (in /repo)".to_owned(),
            subcommand: "git commit-tree".to_owned(),
            success: false,
            code: Some(128),
            stdout: Vec::new(),
            stderr: "Author identity unknown\n\n*** Please tell me who you are.\n\nRun\n\n  git config --global user.email \"you@example.com\"\nfatal: unable to auto-detect email address\n\n".to_owned(),
        };
        let message = failed.failure().to_string();
        assert_eq!(
            message,
            "git commit-tree failed (exit 128): fatal: unable to auto-detect email address"
        );
        assert!(
            !message.contains('\n'),
            "a VCS failure is exactly one line, got: {message}"
        );
    }

    #[test]
    fn identity_globals_use_each_tool_own_syntax() {
        assert_eq!(
            identity_globals(Tool::Git),
            [
                "-c",
                "user.name=slipfork",
                "-c",
                "user.email=slipfork@localhost"
            ]
        );
        assert_eq!(
            identity_globals(Tool::Jj),
            [
                "--config",
                "user.name=slipfork",
                "--config",
                "user.email=slipfork@localhost"
            ]
        );
    }

    #[test]
    fn subcommand_labels_skip_global_flags_and_operands() {
        let jj_args = [
            OsStr::new("--ignore-working-copy"),
            OsStr::new("log"),
            OsStr::new("--no-graph"),
        ];
        assert_eq!(subcommand_label(Tool::Jj, &jj_args), "jj log");
        let git_args = [OsStr::new("commit-tree"), OsStr::new("abc123")];
        assert_eq!(subcommand_label(Tool::Git, &git_args), "git commit-tree");
        assert_eq!(subcommand_label(Tool::Git, &[]), "git");
    }
}
