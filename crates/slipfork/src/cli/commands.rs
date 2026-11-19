//! The top-level `slipfork` verb table and its per-verb argument structs.
//!
//! One cohesive seam: every command shape `clap` renders lives here, while [`super::Cli`] keeps the
//! global flags and the parse entry point. Re-exported from `cli` so `slipfork::cli::Commands`
//! stays the one public path.

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use slipfork_wire::control::{DumpRequest, ResourceRequest};

use super::features::FeatureArgs;

/// Shared `--world` help text for every world-scoped command. Carries no trailing period because
/// `clap` strips the one it derives from a doc comment; the rendered help stays byte-identical.
const WORLD_HELP: &str =
    "World id (`<fork>`, `<project>:<fork>`, or canonical `<project>`); omit for `cwd`'s world";

/// Top-level `slipfork` commands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Show every running world's services and forks.
    Status,

    /// Probe the Btrfs substrate, the live subvolume ioctl round-trip, and unprivileged namespaces.
    Doctor,

    /// Convert an upstream `snow.yml` into a `Forkfile` (one-way, statically verified).
    Migrate {
        /// Upstream manifest (default: `./snow.yml`, then `./snow.yaml`).
        #[arg(value_name = "FILE")]
        file: Option<PathBuf>,
        /// Where to write the converted `Forkfile` (default: `./Forkfile`).
        #[arg(short = 'o', long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// Print the converted text without writing anything (`--json` prints it as one JSON string).
        #[arg(long, conflicts_with = "output")]
        stdout: bool,
        /// Replace an existing `Forkfile`.
        #[arg(short = 'f', long)]
        force: bool,
    },

    /// Boot the world selected by `--world` (or `cwd`) through `slipforkd`.
    Up {
        #[arg(short = 'w', long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
        #[command(flatten)]
        dump: DumpArgs,
        #[command(flatten)]
        resources: ResourceArgs,
        #[command(flatten)]
        features: FeatureArgs,
        /// Mount the developer's host toolchain caches read-only after the ELF/libc ABI probe
        /// passes (`ADR-0009` §2.3).
        #[arg(long)]
        host_caches: bool,
    },

    /// Stop a world's services while keeping its worktree (idempotent).
    Down {
        #[arg(short = 'w', long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
    },

    /// Re-run a held world in place: stop its components + services, then run the same boot
    /// pipeline as `up` (`0.1.0` has no `--watch`; re-run this command after an edit).
    Restart {
        #[arg(short = 'w', long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
        #[command(flatten)]
        features: FeatureArgs,
    },

    /// Show one world's state, lineage, and every supervised component / `crun` service row.
    Ps {
        #[arg(short = 'w', long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
    },

    /// Open the selected world's origin in the desktop browser, or print the SSH recipe.
    Open {
        #[arg(short = 'w', long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
        /// Print the SSH local-forward recipe for a laptop instead of opening a browser.
        #[arg(short = 'r', long)]
        remote: bool,
        /// Laptop-side local port for the SSH forward.
        #[arg(short = 'p', long, value_name = "PORT", default_value_t = DEFAULT_REMOTE_PORT)]
        port: u16,
    },

    /// Create `.worktrees/<fork>` plus its CoW world through `slipforkd`.
    Fork {
        /// Fork name (default: the current branch).
        #[arg(value_name = "FORK")]
        fork: Option<String>,
        /// Commit-ish to fork from (defaults to the requesting checkout's `HEAD`).
        #[arg(long)]
        base: Option<String>,
        #[command(flatten)]
        dump: DumpArgs,
        #[command(flatten)]
        resources: ResourceArgs,
        #[command(flatten)]
        features: FeatureArgs,
        /// Mount the developer's host toolchain caches read-only after the ELF/libc ABI probe
        /// passes (`ADR-0009` §2.3).
        #[arg(long)]
        host_caches: bool,
    },

    /// Destroy a fork world and remove its `.worktrees/<fork>` checkout.
    Drop {
        /// Fork name to destroy (default: the fork whose `.worktrees` checkout contains `cwd`).
        #[arg(value_name = "FORK")]
        fork: Option<String>,
        /// Destroy the worktree even when it has uncommitted or untracked changes.
        #[arg(short = 'f', long)]
        force: bool,
    },

    /// Preview (default) or reap worlds whose checkout directory is gone.
    ///
    /// Sealed snapshot bases and the rootfs/OCI caches are retained, not collected.
    Prune {
        /// Preview only; delete nothing (the default when `--yes` is absent).
        #[arg(long, conflicts_with = "yes")]
        dry_run: bool,
        /// Reap what the preview reports.
        #[arg(short = 'y', long, conflicts_with = "dry_run")]
        yes: bool,
    },

    /// Manage the project's local `0600` vault secrets through `slipforkd`.
    Secret {
        #[command(subcommand)]
        action: SecretAction,
    },

    /// Daemon administration.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },

    /// Run a command non-interactively inside a world (daemon-resolved environment).
    Exec {
        #[arg(short = 'w', long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
        /// Service whose container runs the command (auto-selected for a single-service world or
        /// when `cwd` is inside that service's directory).
        #[arg(short = 's', long, value_name = "SERVICE")]
        service: Option<String>,
        /// Working directory used to resolve the world and to enter it.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Command and arguments to execute (`--` is optional).
        #[arg(required = true, trailing_var_arg = true)]
        argv: Vec<String>,
    },

    /// Run a `Forkfile` `task <name> in <service>` inside that service, streaming its output.
    Run {
        /// Task name declared in the `Forkfile`.
        #[arg(value_name = "TASK")]
        task: String,
        /// Extra arguments appended to a single-command task (rejected for a block task). The
        /// `-w/--world` and `--json` flags belong to the CLI, so they must precede `<task>`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        #[arg(short = 'w', long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
    },

    /// Print one service's resolved environment (`[REDACTED]` unless `--reveal`).
    Env {
        #[arg(short = 's', long, value_name = "SERVICE")]
        service: Option<String>,
        /// Show secret values (requires an interactive terminal).
        #[arg(long)]
        reveal: bool,
        #[command(flatten)]
        features: FeatureArgs,
        #[arg(short = 'w', long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
    },

    /// Enter a service's container interactively with native terminal stdio (requires a TTY).
    Shell {
        /// Service to enter (auto-selected for a single-service world or from `cwd`).
        #[arg(value_name = "SERVICE")]
        service: Option<String>,
        #[arg(short = 'w', long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
    },

    /// Read or follow retained component and hook logs from `slipforkd`.
    Logs {
        #[arg(short = 'w', long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
        /// Component id, hook id (`api:pre-deploy`), or omit for every retained log.
        target: Option<String>,
        /// Keep streaming new output.
        #[arg(short = 'f', long)]
        follow: bool,
        /// Read the stderr stream instead of stdout.
        #[arg(long)]
        stderr: bool,
        /// Number of trailing lines to print.
        #[arg(short = 'n', long)]
        lines: Option<usize>,
    },

    /// Hook administration.
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },

    /// Open `psql` in the world's PostgreSQL container, or print the database URI on a pipe.
    Db {
        /// Declared `pg_databases.<id>` entry; inferred when exactly one is declared.
        db_id: Option<String>,
        #[arg(short = 'w', long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
    },

    /// Open `redis-cli` in the world's Redis container, or print the cluster URI on a pipe.
    Redis {
        /// Declared `redis_clusters.<id>` entry; inferred when exactly one is declared.
        cluster_id: Option<String>,
        #[arg(short = 'w', long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
    },

    /// Bind daemon-owned `127.0.0.1:0` listeners bridged into a world (held until Ctrl-C).
    Forward {
        #[arg(short = 'w', long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
        /// Target ids (`pg_databases.main`, `components.api`); empty forwards every target.
        targets: Vec<String>,
    },

    /// Pair this machine with the daemon's OSPP endpoint.
    Auth {
        /// Optional endpoint URL; the daemon refuses one that differs from its injected endpoint.
        endpoint: Option<String>,
    },
}

/// `slipfork hook` actions.
#[derive(Debug, Subcommand)]
pub enum HookAction {
    /// Re-run one `<component>:<phase>` hook (direct native TTY unless `--no-tty`).
    Run {
        /// `<component>:<phase>` hook id.
        hook: String,
        #[arg(short = 'w', long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
        /// Run non-interactively through `DaemonControl::hook_run`.
        #[arg(long)]
        no_tty: bool,
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
    #[command(name = "list")]
    Keys,
}

/// `slipfork daemon` actions.
#[derive(Debug, Subcommand)]
pub enum DaemonAction {
    /// Render the non-template `slipforkd.socket` / `slipforkd.service` units and the
    /// `/etc/apparmor.d/slipfork` `userns` profile on stdout.
    Unit {
        /// Render only one unit/profile instead of all three.
        #[arg(long, value_enum)]
        only: Option<UnitKind>,
        /// Planned static forward port emitted as the trailing `ListenStream=`.
        #[arg(long, default_value_t = DEFAULT_FORWARD_PORT)]
        forward_port: u16,
        /// Account the rendered `User=` names (default: the invoking account); previews exactly
        /// what `sudo slipfork daemon install --user <ACCOUNT>` would write.
        #[arg(long, value_name = "ACCOUNT")]
        user: Option<String>,
        /// Directory holding the `slipfork{,d,-world-init}` executables; repeat for several
        /// install locations (default: this executable's directory).
        #[arg(long, value_name = "DIR", action = clap::ArgAction::Append)]
        bin_dir: Vec<PathBuf>,
    },

    /// Install the systemd units plus the AppArmor profile and enable `:80` socket activation
    /// (writes `/etc`; requires root).
    Install {
        /// Account `slipforkd.service` runs as (default: the invoking `sudo` account).
        #[arg(long, value_name = "ACCOUNT")]
        user: Option<String>,
        /// Directory holding the `slipfork{,d,-world-init}` executables; repeat for several
        /// install locations (default: this executable's directory).
        #[arg(long, value_name = "DIR", action = clap::ArgAction::Append)]
        bin_dir: Vec<PathBuf>,
        /// Planned static forward port emitted as the trailing `ListenStream=`.
        #[arg(long, default_value_t = DEFAULT_FORWARD_PORT)]
        forward_port: u16,
    },
}

/// Which unit/profile `daemon unit --only` renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum UnitKind {
    /// `slipforkd.socket`.
    Socket,
    /// `slipforkd.service`.
    Service,
    /// `/etc/apparmor.d/slipfork` unprivileged `userns` profile (`ADR-0010` §7).
    Apparmor,
}

/// Default planned static forward port rendered into the systemd socket unit (`ADR-0008` §5).
pub const DEFAULT_FORWARD_PORT: u16 = 7080;

/// Default laptop-side local port of the `open --remote` SSH forward.
pub const DEFAULT_REMOTE_PORT: u16 = 8080;

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
