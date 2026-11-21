//! The `clap` 4 derive tree: the single argv parser, help renderer, and automation source.
//!
//! Every verb, positional, and flag that ships is declared exactly once here. `--help`,
//! `slipfork completions <shell>`, and the `slipfork(1)` manpage are derived from this tree.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use clap_complete::Shell;

/// The overview footer: the four copy-pasteable invocations worth memorising.
pub const EXAMPLES: &str = "\
Examples:
  slipfork
  slipfork up
  slipfork restart worker
  slipfork logs -f api
  slipfork exec api -- pytest -q
  slipfork daemon install";

/// `slipfork`: copy-on-write Btrfs environments for polyglot apps and AI coding agents.
#[derive(Debug, Parser)]
#[command(
    name = "slipfork",
    version,
    about = "Copy-on-write Btrfs environments for polyglot apps and AI coding agents.",
    override_usage = "slipfork <command> [options]",
    disable_help_subcommand = true,
    after_help = EXAMPLES,
    max_term_width = 100,
)]
pub struct Cli {
    /// Add supervisor lineage and resource detail to status rows.
    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,

    /// Emit machine-readable JSON with zero ANSI.
    #[arg(long, global = true)]
    pub json: bool,

    /// Print the generated `slipfork(1)` manpage; used only by packaging.
    #[arg(long, hide = true)]
    pub manpage: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    /// Reads the hidden `__supervise --project <PATH>` spelling before clap parses, so the
    /// composition root resolves the store root without walking raw argv itself.
    #[must_use]
    pub fn preparse_supervise_project(args: &[OsString]) -> Option<PathBuf> {
        if args.first()? != "__supervise" {
            return None;
        }
        let mut index = 1;
        while index < args.len() {
            let argument = &args[index];
            if argument == "--project" {
                return args.get(index + 1).map(PathBuf::from);
            }
            if let Some(value) = argument
                .to_str()
                .and_then(|argument| argument.strip_prefix("--project="))
            {
                return Some(PathBuf::from(value));
            }
            index += 1;
        }
        None
    }
}

/// Every shipped verb, in help order.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show every running world as one tree; bare `slipfork` is this verb.
    Status,

    /// Show only the world or fork rooted in this directory.
    Ps,

    /// Check Btrfs, namespaces, and the crun + :80 runtime.
    Doctor,

    /// Boot images, resources, setup, hooks, and services from ./Forkfile.
    Up {
        /// World name (default: the project directory name).
        #[arg(long, value_name = "NAME")]
        world: Option<String>,
    },

    /// Stop this directory's running world and every pod in it.
    Down,

    /// Restart one service from its existing bundle, or every service.
    Restart {
        /// Service id to restart (default: every service).
        service: Option<String>,
    },

    /// Snapshot the project and world data, then boot the fork on its own URL.
    ///
    /// `--json` emits `{"fork","world","project","url","elapsed_ms","snapshot_ms"}`.
    Fork {
        /// Fork and branch name (default: the checkout's git branch).
        name: Option<String>,
    },

    /// Stop a fork and delete its project and world-data snapshots.
    Drop {
        /// Fork to stop and delete.
        name: String,
        /// Discard uncommitted changes in the fork checkout.
        #[arg(short = 'f', long)]
        force: bool,
    },

    /// Stream merged stdout and stderr with a colored gutter per service.
    Logs {
        /// Service or resource id; omit to merge application services.
        #[arg(conflicts_with = "all")]
        service: Option<String>,
        /// Stay attached across restarts (Ctrl-C exits 0).
        #[arg(short = 'f', long)]
        follow: bool,
        /// Include postgres, redis, and s3 resource pods.
        #[arg(short = 'a', long)]
        all: bool,
    },

    /// Open `sh -i` inside a service or image pod (no PTY is allocated).
    Shell {
        /// Service or image id.
        target: Option<String>,
    },

    /// Run a command inside a pod and forward its exit code.
    Exec {
        /// Service or image id.
        target: Option<String>,
        /// Command and arguments after `--`.
        #[arg(last = true, value_name = "CMD")]
        argv: Vec<String>,
    },

    /// Open psql against the world's postgres; piped stdin executes SQL.
    Db {
        /// Database id; auto-selected when unambiguous.
        database: Option<String>,
    },

    /// Open redis-cli against the world's redis; piped stdin runs commands.
    Redis {
        /// Redis resource id; auto-selected when unambiguous.
        id: Option<String>,
    },

    /// Print resolved runtime variables; secrets stay redacted by default.
    Env {
        /// Only this component's variables (default: every component).
        component: Option<String>,
        /// Show real secret values; requires an interactive terminal.
        #[arg(long)]
        reveal: bool,
    },

    /// List, override, or remove local offline secrets for this world.
    Secret {
        #[command(subcommand)]
        command: SecretCommand,
    },

    /// Open the world or service URL in your browser, printing it when headless.
    Open {
        /// Open this service's route (default: the primary route).
        service: Option<String>,
    },

    /// Pair this machine with an OSPP v1 endpoint, or reuse the saved record.
    Auth {
        /// Endpoint to pair with (default: the saved record).
        url: Option<String>,
        /// Endpoint that wins over both the positional URL and the saved record.
        #[arg(long, value_name = "URL")]
        ospp_endpoint: Option<String>,
    },

    /// Format a Forkfile in place, or report with `--check`.
    Fmt {
        /// Forkfile to format (default: Forkfile).
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
        /// Report whether PATH is formatted; exit 1 when it is not.
        #[arg(long)]
        check: bool,
    },

    /// Convert a legacy snow.yml manifest into a Forkfile.
    Migrate {
        /// snow.yml manifest to convert (default: snow.yml).
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
        /// Save the result as Forkfile next to PATH.
        #[arg(long)]
        write: bool,
        /// Overwrite an existing Forkfile.
        #[arg(long)]
        force: bool,
    },

    /// Print a completion script for bash, zsh, or fish.
    Completions {
        /// Shell to generate for: bash, zsh, or fish.
        #[arg(value_name = "SHELL", value_parser = parse_shell)]
        shell: Shell,
    },

    /// Install the per-user unit (and with sudo the system :80 socket).
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },

    /// Print this overview, or one verb's help.
    Help {
        /// Verb to describe (default: this overview).
        command: Option<String>,
    },

    /// Print the version.
    Version,

    /// Hidden detached-supervisor mode spawned by the daemon.
    #[command(name = "__supervise", hide = true)]
    Supervise {
        #[arg(long, value_name = "PATH")]
        project: PathBuf,
        #[arg(long, value_name = "NAME")]
        world: String,
    },
}

/// `slipfork daemon` subcommands.
#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Write the per-user daemon unit; `--ingress` adds the system :80 socket.
    Install {
        /// Install the system :80 socket, its template unit, and AppArmor (needs root).
        #[arg(long)]
        ingress: bool,
    },
}

/// `slipfork secret` subcommands.
#[derive(Debug, Subcommand)]
pub enum SecretCommand {
    /// Show declared secrets, always redacted.
    List,

    /// Store one value read from stdin (never echoed).
    Set {
        /// `COMPONENT.KEY` to store.
        target: String,
    },

    /// Remove exactly one stored key.
    Unset {
        /// `COMPONENT.KEY` to remove.
        target: String,
    },
}

/// Parses `bash`, `zsh`, or `fish`, refusing anything else with the remedy in the message.
fn parse_shell(value: &str) -> Result<Shell, String> {
    match value {
        "bash" => Ok(Shell::Bash),
        "zsh" => Ok(Shell::Zsh),
        "fish" => Ok(Shell::Fish),
        other => Err(format!(
            "`{other}` is not a shell; expected bash, zsh, or fish"
        )),
    }
}
