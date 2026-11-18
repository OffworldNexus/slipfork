//! The single `clap` command surface shared by `slipfork` and `slipforkd`.
//!
//! Every world-scoped command selects a non-default world exclusively through `--world <WORLD>`,
//! which accepts exactly the ids `slipfork list` prints: the canonical `<project>`,
//! `<project>:<fork>`, or the repo-local bare `<fork>` (`ADR-0008` §4). `drop` names its fork
//! positionally because it destroys it; `fork` takes one positionally when given and otherwise
//! derives it from the current branch inside `slipforkd` (`docs/design/dx-0.1.0-ux.md` §7).
//! `/M2.16` deleted M1's `snap` subcommand: worlds and forks are the only snapshot producers.

use std::collections::BTreeSet;
use std::path::PathBuf;

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use slipfork_edge::{DumpRequest, ResourceRequest};

/// Shared `--world` help text for every world-scoped command. Carries no trailing period because
/// `clap` strips the one it derives from a doc comment; the rendered help stays byte-identical.
const WORLD_HELP: &str =
    "World id (`<fork>`, `<project>:<fork>`, or canonical `<project>`); omit for `cwd`'s world";

/// `slipfork` thin client.
///
// The subcommand is optional so bare `slipfork` is the status dashboard: it runs the exact same
// handler as `slipfork status` (`docs/design/dx-0.1.0-ux.md` §3), with no alias and no second help
// mode — `--help` still lists every command.
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

    /// Add per-node lineage and `[resources]` detail to human output (ignored under `--json`).
    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,

    /// Explicit CoW state directory (defaults to `$XDG_CACHE_HOME/slipfork` or `~/.cache/slipfork`).
    ///
    /// The client's control socket is always `<state_dir>/run/daemon.sock`; there is no
    /// `--runtime-dir` override outside `slipforkd` itself (`ADR-0011` §2).
    #[arg(long, global = true)]
    pub state_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// Every top-level verb this binary really implements, taken from the `clap` table itself.
///
/// This is the same table `slipfork --help` prints and `tools/check_docs_honesty.py` parses, so a
/// renderer that suggests a command can filter it against this set instead of duplicating (and
/// drifting from) the command list. A verb nobody implemented can therefore never be hinted.
pub fn implemented_commands() -> BTreeSet<String> {
    Cli::command()
        .get_subcommands()
        .map(|command| command.get_name().to_owned())
        .collect()
}

/// Top-level `slipfork` commands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Show the daemon ingress plus every world's tree, lineage, and next steps.
    Status,

    /// Probe the Btrfs substrate, the live subvolume ioctl round-trip, and unprivileged namespaces.
    Doctor,

    /// Validate and resolve `snow.yml` modes & `!!` auto-generated secrets.
    Snow {
        #[command(subcommand)]
        action: SnowAction,
    },

    /// Boot the world selected by `--world` (or `cwd`) through `slipforkd`.
    Up {
        #[arg(long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
        #[command(flatten)]
        dump: DumpArgs,
        #[command(flatten)]
        resources: ResourceArgs,
        /// Mount the developer's host toolchain caches read-only after the ELF/libc ABI probe
        /// passes (`ADR-0009` §2.3; never a `slipfork.toml` key).
        #[arg(long)]
        host_caches: bool,
    },

    /// Stop a world's services while keeping its worktree (idempotent).
    Down {
        #[arg(long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
    },

    /// Re-run a held world in place: stop its components + services, then run the same boot
    /// pipeline as `up` (`0.1.0` has no `--watch`; re-run this command after an edit).
    Restart {
        #[arg(long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
    },

    /// Show one world's state, lineage, and every supervised component / `crun` service row.
    Ps {
        #[arg(long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
    },

    /// Open the selected world's origin in the desktop browser, or print the SSH recipe.
    Open {
        #[arg(long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
        /// Print the SSH local-forward recipe for a laptop instead of opening a browser.
        #[arg(long)]
        remote: bool,
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
        /// Mount the developer's host toolchain caches read-only after the ELF/libc ABI probe
        /// passes (`ADR-0009` §2.3; never a `slipfork.toml` key).
        #[arg(long)]
        host_caches: bool,
    },

    /// Destroy a fork world and remove its `.worktrees/<fork>` checkout.
    Drop {
        /// Fork name to destroy.
        fork: String,
    },

    /// List worlds with state, worktree path, and exactly one origin URL.
    List,

    /// Preview (default) or reap worlds whose checkout directory is gone.
    ///
    /// Sealed snapshot bases and the rootfs/OCI caches are retained, not collected.
    Prune {
        /// Preview only; delete nothing (the default when `--yes` is absent).
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

    /// Daemon administration.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },

    /// Run a command non-interactively inside a world (daemon-resolved environment).
    Exec {
        #[arg(long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
        /// Working directory used to resolve the world and to enter it.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Command and arguments to execute.
        #[arg(required = true, trailing_var_arg = true)]
        argv: Vec<String>,
    },

    /// Enter a world interactively with native terminal stdio (requires a TTY).
    Shell {
        #[arg(long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
    },

    /// Read or follow retained component and hook logs from `slipforkd`.
    Logs {
        #[arg(long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
        /// Component id, hook id (`api:pre-deploy`), or omit for every retained log.
        target: Option<String>,
        /// Keep streaming new output.
        #[arg(long)]
        follow: bool,
        /// Read the stderr stream instead of stdout.
        #[arg(long)]
        stderr: bool,
        /// Number of trailing lines to print.
        #[arg(long)]
        lines: Option<usize>,
    },

    /// Hook administration.
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },

    /// Open `psql` inside the world's running PostgreSQL service container (requires a TTY).
    Db {
        /// Declared `pg_databases.<id>` entry; inferred when exactly one is declared.
        db_id: Option<String>,
        #[arg(long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
    },

    /// Open `redis-cli` inside the world's running Redis service container (requires a TTY).
    Redis {
        /// Declared `redis_clusters.<id>` entry; inferred when exactly one is declared.
        cluster_id: Option<String>,
        #[arg(long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
    },

    /// Bind daemon-owned `127.0.0.1:0` listeners bridged into a world (held until Ctrl-C).
    Forward {
        #[arg(long, value_name = "WORLD", help = WORLD_HELP)]
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
        #[arg(long, value_name = "WORLD", help = WORLD_HELP)]
        world: Option<String>,
        /// Run non-interactively through `DaemonControl::hook_run`.
        #[arg(long)]
        no_tty: bool,
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
    fn bare_invocation_and_status_share_one_command() {
        let bare = Cli::try_parse_from(["slipfork"]).expect("bare slipfork parses");
        assert!(
            bare.command.is_none(),
            "a missing subcommand must reach the status handler, not a clap error"
        );
        let explicit = Cli::try_parse_from(["slipfork", "status"]).expect("slipfork status parses");
        assert!(matches!(explicit.command, Some(Commands::Status)));
        assert!(
            Cli::try_parse_from(["slipfork", "--json"]).is_ok(),
            "the bare invocation accepts the global flags"
        );
    }

    #[test]
    fn restart_has_a_world_selector_and_no_watcher() {
        let restart = Cli::try_parse_from(["slipfork", "restart", "--world", "demo:feat"])
            .expect("restart parses");
        let Some(Commands::Restart { world }) = restart.command else {
            panic!("expected `restart`");
        };
        assert_eq!(world.as_deref(), Some("demo:feat"));

        // `0.1.0` ships zero file watchers (`M3.28.1`, §4): `--watch` is not a flag of any verb.
        let err = Cli::try_parse_from(["slipfork", "restart", "--watch"])
            .expect_err("restart --watch was locked out of 0.1.0");
        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
        assert!(Cli::try_parse_from(["slipfork", "--verbose", "restart"]).is_ok());
        assert!(Cli::try_parse_from(["slipfork", "restart", "-v"]).is_ok());
    }

    #[test]
    fn ps_and_open_select_a_world_and_open_has_a_remote_recipe() {
        let ps =
            Cli::try_parse_from(["slipfork", "ps", "--world", "demo:feat"]).expect("ps parses");
        let Some(Commands::Ps { world }) = ps.command else {
            panic!("expected `ps`");
        };
        assert_eq!(world.as_deref(), Some("demo:feat"));
        assert!(Cli::try_parse_from(["slipfork", "ps"]).is_ok());

        let open = Cli::try_parse_from(["slipfork", "open", "--remote"]).expect("open parses");
        let Some(Commands::Open { world, remote }) = open.command else {
            panic!("expected `open`");
        };
        assert!(world.is_none());
        assert!(
            remote,
            "`--remote` prints the SSH recipe instead of opening a browser"
        );
        // The laptop-side forward port is the fixed constant `8080`; there is no extra flag.
        let err = Cli::try_parse_from(["slipfork", "open", "--remote", "--local-port", "9090"])
            .expect_err("`--local-port` was locked out of 0.1.0");
        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn fork_takes_an_optional_name_and_drop_still_requires_one() {
        let derived = Cli::try_parse_from(["slipfork", "fork"]).expect("zero-argument fork parses");
        let Some(Commands::Fork { fork, .. }) = derived.command else {
            panic!("expected `fork`");
        };
        assert!(
            fork.is_none(),
            "an absent fork name is derived by `slipforkd` from the current branch"
        );
        let named = Cli::try_parse_from(["slipfork", "fork", "feat-ux"]).expect("named fork");
        let Some(Commands::Fork { fork, .. }) = named.command else {
            panic!("expected `fork`");
        };
        assert_eq!(fork.as_deref(), Some("feat-ux"));

        // Destructive teardown always names the fork explicitly: `drop` has no derived form.
        let err = Cli::try_parse_from(["slipfork", "drop"])
            .expect_err("`slipfork drop` was locked out of 0.1.0");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn fork_help_shows_the_fork_slot_as_optional() {
        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("fork")
            .expect("`fork` is a command")
            .render_long_help()
            .to_string();
        assert!(help.contains("[FORK]"), "the slot is optional: {help}");
        assert!(
            !help.contains("<FORK>"),
            "a required-name usage line is gone: {help}"
        );
        assert!(
            help.contains("Fork name (default: the current branch)"),
            "the help names the derivation: {help}"
        );
    }

    #[test]
    fn dump_flags_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["slipfork", "up", "--no-dump", "--refresh-dump"]).is_err());
        assert!(Cli::try_parse_from(["slipfork", "up", "--no-dump"]).is_ok());
        assert!(Cli::try_parse_from(["slipfork", "up", "--refresh-dump"]).is_ok());
    }

    #[test]
    fn prune_flags_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["slipfork", "prune"]).is_ok());
        assert!(Cli::try_parse_from(["slipfork", "prune", "--yes"]).is_ok());
        assert!(Cli::try_parse_from(["slipfork", "prune", "--dry-run"]).is_ok());
        assert!(Cli::try_parse_from(["slipfork", "prune", "--dry-run", "--yes"]).is_err());
    }

    #[test]
    fn the_deleted_snap_subcommand_is_gone() {
        let err = Cli::try_parse_from(["slipfork", "snap", "--source", ".", "base"])
            .expect_err("snap was deleted in M2.16");
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn the_deleted_dag_subcommand_is_gone() {
        let err = Cli::try_parse_from(["slipfork", "dag", "plan"])
            .expect_err("dag was deleted in M3.28.11");
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn world_selectors_are_flags_never_positional_forks() {
        assert!(Cli::try_parse_from(["slipfork", "up", "feat"]).is_err());
        assert!(Cli::try_parse_from(["slipfork", "up", "--world", "feat"]).is_ok());
    }

    #[test]
    fn the_client_has_no_runtime_dir_override() {
        let err = Cli::try_parse_from(["slipfork", "--runtime-dir", "/run/user/1000", "list"])
            .expect_err("--runtime-dir is a slipforkd-only flag");
        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
        assert!(
            Cli::try_parse_from(["slipfork", "--state-dir", "/state", "list"]).is_ok(),
            "--state-dir stays global"
        );
    }

    #[test]
    fn daemon_bin_dir_is_repeatable_for_unit_and_install() {
        let unit = Cli::try_parse_from([
            "slipfork",
            "daemon",
            "unit",
            "--only",
            "apparmor",
            "--bin-dir",
            "/usr/local/bin",
            "--bin-dir",
            "/tmp/ux/bin",
        ])
        .expect("repeatable --bin-dir");
        let Some(Commands::Daemon {
            action: DaemonAction::Unit { bin_dir, .. },
        }) = unit.command
        else {
            panic!("expected `daemon unit`");
        };
        assert_eq!(
            bin_dir,
            vec![
                PathBuf::from("/usr/local/bin"),
                PathBuf::from("/tmp/ux/bin")
            ]
        );

        let install = Cli::try_parse_from([
            "slipfork",
            "daemon",
            "install",
            "--user",
            "dev",
            "--bin-dir",
            "/usr/local/bin",
        ])
        .expect("daemon install");
        let Some(Commands::Daemon {
            action:
                DaemonAction::Install {
                    user,
                    bin_dir,
                    forward_port,
                },
        }) = install.command
        else {
            panic!("expected `daemon install`");
        };
        assert_eq!(user.as_deref(), Some("dev"));
        assert_eq!(bin_dir, vec![PathBuf::from("/usr/local/bin")]);
        assert_eq!(forward_port, DEFAULT_FORWARD_PORT);
    }
}
