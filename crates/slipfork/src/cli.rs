//! The single `clap` command surface shared by `slipfork` and `slipforkd`.
//!
//! Every world-scoped command selects a non-default world exclusively through `--world <fork>`
//! (`ADR-0008` §4); `fork` and `drop` name their fork positionally because they create/destroy it.
//! `/M2.16` deleted M1's `snap` subcommand: worlds and forks are the only snapshot producers.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use slipfork_edge::{DumpRequest, ResourceRequest};

/// `slipfork` thin client.
#[derive(Debug, Parser)]
#[command(
    name = "slipfork",
    version,
    about = "Instant CoW dev worlds & parallel test DAG orchestrator"
)]
pub struct Cli {
    /// Emit machine-readable JSON on stdout.
    #[arg(long, global = true)]
    pub json: bool,

    /// Explicit CoW state directory (defaults to `$XDG_CACHE_HOME/slipfork` or `~/.cache/slipfork`).
    #[arg(long, global = true)]
    pub state_dir: Option<PathBuf>,

    /// Explicit runtime directory holding `daemon.sock`
    /// (defaults to `$XDG_RUNTIME_DIR/slipfork` or `<state_dir>/run`).
    #[arg(long, global = true)]
    pub runtime_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

/// Top-level `slipfork` commands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Probe host CoW reflink latency, Incus socket availability, and unprivileged netns support.
    Doctor,

    /// Validate and resolve `snow.yml` modes & `!!` auto-generated secrets.
    Snow {
        #[command(subcommand)]
        action: SnowAction,
    },

    /// Compute the orthogonal Artifact + Fixture Snapshot DAG execution plan.
    Dag {
        #[command(subcommand)]
        action: DagAction,
    },

    /// Boot the world selected by `--world` (or `cwd`) through `slipforkd`.
    Up {
        /// World fork to boot; omit for the canonical world of `cwd`'s repository.
        #[arg(long)]
        world: Option<String>,
        #[command(flatten)]
        dump: DumpArgs,
        #[command(flatten)]
        resources: ResourceArgs,
    },

    /// Stop a world's services while keeping its worktree (idempotent).
    Down {
        /// World fork to stop; omit for the canonical world of `cwd`'s repository.
        #[arg(long)]
        world: Option<String>,
    },

    /// Create `.worktrees/<fork>` plus its CoW world through `slipforkd`.
    Fork {
        /// Fork name (a single lowercase DNS label).
        fork: String,
        /// Commit-ish to fork from (defaults to the requesting checkout's `HEAD`).
        #[arg(long)]
        base: Option<String>,
        #[command(flatten)]
        dump: DumpArgs,
        #[command(flatten)]
        resources: ResourceArgs,
    },

    /// Destroy a fork world and remove its `.worktrees/<fork>` checkout.
    Drop {
        /// Fork name to destroy.
        fork: String,
    },

    /// List worlds with state, worktree path, and exactly one origin URL.
    List,

    /// Preview or reap orphaned worlds and snapshots.
    Prune {
        /// Preview only; delete nothing.
        #[arg(long, conflicts_with = "yes")]
        dry_run: bool,
        /// Reap what the preview reports.
        #[arg(long, conflicts_with = "dry_run")]
        yes: bool,
    },

    /// Manage the project's local `0600` vault secrets through `slipforkd`.
    Secret {
        #[command(subcommand)]
        action: SecretAction,
    },

    /// Share a world through the tunnel registrar, or stop sharing it.
    Share {
        /// Drop the world's tunnel lease instead of minting one.
        #[arg(long)]
        stop: bool,
        /// World fork to share; omit for the canonical world of `cwd`'s repository.
        #[arg(long)]
        world: Option<String>,
    },

    /// Daemon administration.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },

    /// Run a command non-interactively inside a world (daemon-resolved environment).
    Exec {
        /// World fork to enter; omit for the canonical world of `cwd`'s repository.
        #[arg(long)]
        world: Option<String>,
        /// Working directory used to resolve the world and to enter it.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Command and arguments to execute.
        #[arg(required = true, trailing_var_arg = true)]
        argv: Vec<String>,
    },
}

/// `slipfork snow` actions.
#[derive(Debug, Subcommand)]
pub enum SnowAction {
    /// Validate `snow.yml`, persist missing `!!` secrets, and output resolved/redacted variables.
    Resolve {
        /// Path to `snow.yml`.
        #[arg(long, default_value = "snow.yml")]
        file: PathBuf,
        /// Active mode (`local`, `cloud`, etc.).
        #[arg(long, default_value = "local")]
        mode: String,
        /// Optional component ID filter.
        #[arg(long)]
        component: Option<String>,
        /// Expose plaintext secrets (only for internal child-process wiring; redacted by default).
        #[arg(long)]
        expose_secrets: bool,
    },
}

/// `slipfork dag` actions.
#[derive(Debug, Subcommand)]
pub enum DagAction {
    /// Hash repo inputs and print cached vs rebuild waves.
    Plan {
        /// Path to `slipfork.toml` (defaults to `<vcs_root>/slipfork.toml`).
        #[arg(long)]
        file: Option<PathBuf>,
        /// Repository root directory walked for input globs (defaults to `<vcs_root>`).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

/// `slipfork secret` actions.
#[derive(Debug, Subcommand)]
pub enum SecretAction {
    /// Store a secret read from stdin, refusing to replace an existing key without `--force`.
    Set {
        /// Secret key name (`STRIPE_SECRET_KEY`).
        key: String,
        /// Rotate an existing value.
        #[arg(long)]
        force: bool,
    },
    /// Remove a stored secret key.
    Unset {
        /// Secret key name.
        key: String,
    },
    /// List the project's stored secret keys (never values).
    List,
}

/// `slipfork daemon` actions.
#[derive(Debug, Subcommand)]
pub enum DaemonAction {
    /// Render the non-template `slipforkd.socket` / `slipforkd.service` units on stdout.
    Unit {
        /// Render only one unit instead of both.
        #[arg(long, value_enum)]
        only: Option<UnitKind>,
        /// Planned static forward port emitted as the trailing `ListenStream=`.
        #[arg(long, default_value_t = DEFAULT_FORWARD_PORT)]
        forward_port: u16,
    },
}

/// Which systemd unit `daemon unit --only` renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum UnitKind {
    /// `slipforkd.socket`.
    Socket,
    /// `slipforkd.service`.
    Service,
}

/// Default planned static forward port rendered into the systemd socket unit (`ADR-0008` §5).
pub const DEFAULT_FORWARD_PORT: u16 = 7080;

/// Mutually exclusive `--no-dump | --refresh-dump` (`ADR-0008` §3).
#[derive(Debug, Default, Args)]
pub struct DumpArgs {
    /// Skip seed dumps and run a clean `initdb`.
    #[arg(long, conflicts_with = "refresh_dump")]
    pub no_dump: bool,
    /// Require a live OSPP dump check (fails closed when offline).
    #[arg(long, conflicts_with = "no_dump")]
    pub refresh_dump: bool,
}

impl DumpArgs {
    /// The wire request carried by `UpRequest` / `ForkRequest`.
    pub fn request(&self) -> DumpRequest {
        if self.no_dump {
            DumpRequest::NoDump
        } else if self.refresh_dump {
            DumpRequest::RefreshDump
        } else {
            DumpRequest::Default
        }
    }
}

/// `--resources local` per-invocation override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ResourceScope {
    /// Force hermetic world-local resources.
    Local,
}

/// Optional per-invocation `[resources]` override.
#[derive(Debug, Default, Args)]
pub struct ResourceArgs {
    /// Force world-local resources for this invocation.
    #[arg(long, value_enum)]
    pub resources: Option<ResourceScope>,
}

impl ResourceArgs {
    /// The wire request carried by `UpRequest` / `ForkRequest`.
    pub fn request(&self) -> ResourceRequest {
        match self.resources {
            Some(ResourceScope::Local) => ResourceRequest::Local,
            None => ResourceRequest::Default,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_flags_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["slipfork", "up", "--no-dump", "--refresh-dump"]).is_err());
        assert!(Cli::try_parse_from(["slipfork", "up", "--no-dump"]).is_ok());
        assert!(Cli::try_parse_from(["slipfork", "up", "--refresh-dump"]).is_ok());
    }

    #[test]
    fn prune_flags_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["slipfork", "prune", "--dry-run", "--yes"]).is_err());
    }

    #[test]
    fn the_deleted_snap_subcommand_is_gone() {
        let err = Cli::try_parse_from(["slipfork", "snap", "--source", ".", "base"])
            .expect_err("snap was deleted in M2.16");
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn world_selectors_are_flags_never_positional_forks() {
        assert!(Cli::try_parse_from(["slipfork", "up", "feat"]).is_err());
        assert!(Cli::try_parse_from(["slipfork", "up", "--world", "feat"]).is_ok());
    }
}
