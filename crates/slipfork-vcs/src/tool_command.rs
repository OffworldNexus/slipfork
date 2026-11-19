//! Private `git` / `jj` subprocess runner shared by the Git and Jujutsu adapters.
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
use std::path::{Path, PathBuf};
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
                "install Jujutsu (https://jj-vcs.github.io) to use Jujutsu fork workspaces, or fork from a Git checkout; to name a checkout whose directory basename is not a valid project slug, set `project <slug>` in the `Forkfile`"
            }
        }
    }

    /// Identity global options for `tool`, single-sourced from the snapshot author constants.
    fn identity_globals(self) -> [String; 4] {
        match self {
            Self::Git => [
                "-c".to_owned(),
                format!("user.name={SNAPSHOT_AUTHOR_NAME}"),
                "-c".to_owned(),
                format!("user.email={SNAPSHOT_AUTHOR_EMAIL}"),
            ],
            Self::Jj => [
                "--config".to_owned(),
                format!("user.name={SNAPSHOT_AUTHOR_NAME}"),
                "--config".to_owned(),
                format!("user.email={SNAPSHOT_AUTHOR_EMAIL}"),
            ],
        }
    }

    /// Map a failed spawn: a missing executable is a distinct, actionable `ToolMissing`.
    fn spawn_error(self, command: String, err: io::Error) -> VcsError {
        if err.kind() == io::ErrorKind::NotFound {
            VcsError::ToolMissing {
                tool: self.binary().to_owned(),
                hint: self.missing_hint().to_owned(),
            }
        } else {
            VcsError::CommandSpawn {
                command,
                message: err.to_string(),
            }
        }
    }

    /// Human-readable `<tool> <args> (in <cwd>)` label carried by every spawn / output diagnostic.
    fn describe(self, cwd: &Path, args: &[&OsStr]) -> String {
        let mut command = self.binary().to_owned();
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
    /// The subcommand is the first non-flag argument (`git commit-tree`; Jujutsu reports its
    /// two-word commands as `jj workspace`), so the owner-facing line names what failed without
    /// dumping argv.
    fn subcommand_label(self, args: &[&OsStr]) -> String {
        match args
            .iter()
            .find(|arg| !arg.to_string_lossy().starts_with('-'))
        {
            Some(subcommand) => format!("{} {}", self.binary(), subcommand.to_string_lossy()),
            None => self.binary().to_owned(),
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
            stderr: Self::last_stderr_line(&self.stderr),
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
}

/// One tool invocation bound to its working directory, global options, and scratch index.
///
/// Constructors encode the three legitimate shapes — a plain Git read/checkout, a Git snapshot
/// commit carrying Slipfork's pinned author, and a Jujutsu command (which always snapshots the
/// working copy it runs in) — so no caller can accidentally pin an identity on a read or drop it
/// from a snapshot.
pub(crate) struct VcsCli {
    tool: Tool,
    cwd: PathBuf,
    globals: Vec<String>,
    index_file: Option<PathBuf>,
}

impl VcsCli {
    /// Plain `git` in `cwd`: reads, checkouts, and ref updates (never a pinned identity).
    pub(crate) fn git(cwd: &Path) -> Self {
        Self {
            tool: Tool::Git,
            cwd: cwd.to_path_buf(),
            globals: Vec::new(),
            index_file: None,
        }
    }

    /// `git` with Slipfork's pinned snapshot author as global `-c` options before the subcommand.
    ///
    /// Only the snapshot commit uses this: every other Git call is a read, a checkout, or a ref
    /// update that must neither depend on nor override the developer's `user.name` / `user.email`.
    pub(crate) fn snapshot(cwd: &Path) -> Self {
        Self {
            tool: Tool::Git,
            cwd: cwd.to_path_buf(),
            globals: Tool::Git.identity_globals().to_vec(),
            index_file: None,
        }
    }

    /// `jj` in `cwd` with the pinned identity (`--config`), the equivalent of `git -c`.
    ///
    /// Every Jujutsu command snapshots the working copy it runs in, so the author is pinned for all
    /// of them.
    pub(crate) fn jj(cwd: &Path) -> Self {
        Self {
            tool: Tool::Jj,
            cwd: cwd.to_path_buf(),
            globals: Tool::Jj.identity_globals().to_vec(),
            index_file: None,
        }
    }

    /// Point the invocation at a scratch index (`GIT_INDEX_FILE`), never the live repository index.
    pub(crate) fn with_index(mut self, index_file: &Path) -> Self {
        self.index_file = Some(index_file.to_path_buf());
        self
    }

    /// Spawn and capture its output without interpreting the exit status.
    pub(crate) fn run(&self, args: &[&OsStr]) -> Result<ToolOutput, VcsError> {
        self.spawn(&self.globals, args)
    }

    /// Spawn and fail closed on any non-zero exit.
    pub(crate) fn checked(&self, args: &[&OsStr]) -> Result<ToolOutput, VcsError> {
        self.run(args)?.ensure_success()
    }

    /// Probe the executable on `PATH`; a missing binary maps to [`VcsError::ToolMissing`].
    ///
    /// The version probe carries no configuration overrides: it must answer "is `jj` installed?"
    /// even before any repository or identity is known.
    pub(crate) fn probe(&self) -> Result<(), VcsError> {
        self.spawn(&[], &[OsStr::new("--version")])?
            .ensure_success()?;
        Ok(())
    }

    fn spawn(&self, globals: &[String], args: &[&OsStr]) -> Result<ToolOutput, VcsError> {
        let command = self.tool.describe(&self.cwd, args);
        let mut cmd = Command::new(self.tool.binary());
        match self.tool {
            Tool::Git => {
                cmd.arg("-C").arg(&self.cwd);
            }
            Tool::Jj => {
                // `jj` has no `-C`: a missing working directory would be misreported as a missing
                // binary by the spawn error path, so reject it explicitly first.
                if !self.cwd.is_dir() {
                    return Err(VcsError::Io {
                        path: self.cwd.clone(),
                        message: "not a directory".to_owned(),
                    });
                }
                cmd.current_dir(&self.cwd);
            }
        }
        cmd.args(globals);
        cmd.env("LC_ALL", "C").env("LANGUAGE", "C").args(args);
        for key in SCRUBBED_ENV {
            cmd.env_remove(key);
        }
        if let Some(index_file) = &self.index_file {
            cmd.env("GIT_INDEX_FILE", index_file);
        }
        let output = cmd
            .output()
            .map_err(|err| self.tool.spawn_error(command.clone(), err))?;
        Ok(ToolOutput {
            command,
            subcommand: self.tool.subcommand_label(args),
            success: output.status.success(),
            code: output.status.code(),
            stdout: output.stdout,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fail_closed_spawn_maps_a_missing_binary_to_tool_missing() {
        for (tool, name) in [(Tool::Git, "git"), (Tool::Jj, "jj")] {
            let err = tool.spawn_error(
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
        let err = Tool::Jj.spawn_error(
            "jj --version (in /repo)".to_owned(),
            io::Error::from(io::ErrorKind::NotFound),
        );
        let VcsError::ToolMissing { hint, .. } = err else {
            panic!("expected ToolMissing");
        };
        assert!(
            hint.contains("project <slug>") && hint.contains("Forkfile"),
            "the jj hint must point at the project override, got: {hint}"
        );
    }

    #[test]
    fn spawn_errors_that_are_not_not_found_surface_the_command() {
        let err = Tool::Git.spawn_error(
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
            Tool::Git.identity_globals(),
            [
                "-c",
                "user.name=slipfork",
                "-c",
                "user.email=slipfork@localhost"
            ]
        );
        assert_eq!(
            Tool::Jj.identity_globals(),
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
        assert_eq!(Tool::Jj.subcommand_label(&jj_args), "jj log");
        let git_args = [OsStr::new("commit-tree"), OsStr::new("abc123")];
        assert_eq!(Tool::Git.subcommand_label(&git_args), "git commit-tree");
        assert_eq!(Tool::Git.subcommand_label(&[]), "git");
    }
}
