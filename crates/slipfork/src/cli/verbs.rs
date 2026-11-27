//! The `clap` 4 derive tree: the single argv parser, help renderer, and automation source.
//!
//! Every verb, positional, and flag that ships is declared exactly once here. `--help`,
//! `slipfork completions <shell>`, and the `slipfork(1)` manpage are derived from this tree.

use std::path::PathBuf;

use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{Parser, Subcommand};
use clap_complete::Shell;
use slipfork_forkfile::FeatureEdit;

/// The overview footer: the four copy-pasteable invocations worth memorising.
pub const EXAMPLES: &str = "\
Examples:
  slipfork
  slipfork up
  slipfork up feat-auth
  slipfork restart -s worker
  slipfork logs -f api
  slipfork exec api -- pytest -q";

/// The one help palette: every verb inherits it from the root `Cli`.
const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Yellow.on_default())
    .valid(AnsiColor::Cyan.on_default());

/// `slipfork`: copy-on-write Btrfs environments for polyglot apps and AI coding agents.
#[derive(Debug, Parser)]
#[command(
    name = "slipfork",
    version,
    about = "Copy-on-write Btrfs environments for polyglot apps and AI coding agents.",
    override_usage = "slipfork <command> [options]",
    disable_help_subcommand = true,
    after_help = EXAMPLES,
    styles = STYLES,
    max_term_width = 100,
)]
pub struct Cli {
    /// Add supervisor pid and resource detail to status rows.
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

impl Cli {}

/// `-F, --features` plus `--no-default-features`, shared by `up`, `fork`, and `env`.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct FeatureArgs {
    /// Feature edits: `mail` or `+mail` enables, `-mail` / `!mail` disables.
    ///
    /// Accepts comma lists: `-F dev,mail`.
    #[arg(
        short = 'F',
        long,
        value_name = "FEATURES",
        value_delimiter = ',',
        allow_hyphen_values = true,
        value_parser = parse_feature_edit
    )]
    pub features: Vec<FeatureEdit>,

    /// Start from an empty feature base instead of the built-in `dev` default.
    #[arg(long)]
    pub no_default_features: bool,
}

/// Parses one `-F` value through the `Forkfile` compiler's canonical edit grammar.
fn parse_feature_edit(text: &str) -> Result<FeatureEdit, String> {
    text.parse()
}

/// Every shipped verb, in help order.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show every running world as one tree; bare slipfork is this verb.
    Status,

    /// Show only the world or fork rooted in this directory.
    Ps,

    /// Check Btrfs, namespaces, and the crun + :80 runtime.
    Doctor,

    /// Boot from ./Forkfile, or the world/fork named by TARGET.
    ///
    /// Feature flags persist to this world's `.slipfork/features.json` for the next boot; `env`
    /// evaluates them only. `dev` mounts the live checkout at /project; off runs the image's own
    /// ENTRYPOINT/CMD.
    Up {
        /// World, fork, or .forks/<name> to boot (default: this directory).
        #[arg(value_name = "TARGET")]
        target: Option<String>,
        #[command(flatten)]
        features: FeatureArgs,
        /// Ignore sealed image and step caches for this run.
        #[arg(long)]
        no_cache: bool,
    },

    /// Stop this directory's world, or the named world/fork, and every pod in it.
    Down {
        /// World or fork to stop (default: this directory's world).
        #[arg(value_name = "TARGET")]
        target: Option<String>,
    },

    /// Restart a service, world, or fork; -s always means a service.
    Restart {
        /// Service, world, or fork to restart (default: every service here).
        #[arg(value_name = "TARGET")]
        target: Option<String>,
        /// Service id to restart; always a service, never a world.
        #[arg(short = 's', long, value_name = "SERVICE")]
        service: Option<String>,
    },

    /// Snapshot the project and world data, then boot the fork on its own URL.
    ///
    /// Feature flags persist for the fork's own world, merged over the file inherited from the
    /// snapshot. --json emits {"fork","world","project","url","elapsed_ms","snapshot_ms"}.
    Fork {
        /// Fork and branch name (default: the checkout's git branch).
        name: Option<String>,
        #[command(flatten)]
        features: FeatureArgs,
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
        /// Service or resource ids; omit to merge application services.
        #[arg(value_name = "TARGETS")]
        targets: Vec<String>,
        /// Stay attached across restarts (Ctrl-C exits 0).
        #[arg(short = 'f', long)]
        follow: bool,
        /// Include postgres, redis, and s3 resource pods.
        #[arg(short = 'a', long)]
        all: bool,
        /// Only lines at or after this time (30s|5m|2h|1d or RFC 3339).
        #[arg(long, value_name = "TIME", value_parser = crate::commands::parse_since)]
        since: Option<u64>,
        /// Only lines at or before this time (30s|5m|2h|1d or RFC 3339).
        #[arg(long, value_name = "TIME", value_parser = crate::commands::parse_until)]
        until: Option<u64>,
        /// Prefix every line with a human timestamp gutter.
        #[arg(short = 't', long)]
        timestamps: bool,
        /// Replay a recorded run: failed (default when bare), last, or <id>; use -r=<id> next to a target.
        #[arg(
            short = 'r',
            long = "run",
            value_name = "RUN",
            num_args = 0..=1,
            default_missing_value = "failed",
            conflicts_with = "runs"
        )]
        run: Option<String>,
        /// Tail the last N rendered lines (with -f defaults to 20).
        #[arg(long, value_name = "N", conflicts_with = "runs")]
        last: Option<String>,
        /// List every recorded step run for this world.
        #[arg(long, conflicts_with_all = ["targets", "run", "follow"])]
        runs: bool,
    },

    /// Attach a real PTY: bash/sh, psql, or redis-cli for one declared target.
    Shell {
        /// Service, image, postgres, database, or redis id.
        #[arg(value_name = "TARGET")]
        target: String,
    },

    /// Run a command inside a pod and forward its exit code.
    Exec {
        /// Service or image id.
        target: Option<String>,
        /// Demand a real PTY; fails closed without an interactive terminal.
        #[arg(short = 'i', long, conflicts_with_all = ["no_interactive", "json"])]
        interactive: bool,
        /// Keep plain piped stdio even when stdin is a terminal.
        #[arg(long, conflicts_with = "interactive")]
        no_interactive: bool,
        /// Command and arguments after --.
        #[arg(last = true, value_name = "CMD")]
        argv: Vec<String>,
    },

    /// Print resolved runtime variables; secrets stay redacted by default.
    ///
    /// Feature flags evaluate only and never write `.slipfork/features.json`.
    Env {
        /// Only this component's variables (default: every component).
        component: Option<String>,
        /// Show real secret values; requires an interactive terminal.
        #[arg(long)]
        reveal: bool,
        #[command(flatten)]
        features: FeatureArgs,
    },

    /// Show this world's features, or persist +mail/-dev/reset edits.
    ///
    /// Flags merge with CHANGES; `features reset` restores the built-in default.
    Features {
        /// `reset`, `enable <f>...`, `disable <f>...`, or `+mail`/`-dev`/bare `mail`.
        #[arg(value_name = "CHANGES", allow_hyphen_values = true)]
        changes: Vec<String>,
        #[command(flatten)]
        features: FeatureArgs,
    },

    /// List, override, or remove local offline secrets for this world.
    Secret {
        #[command(subcommand)]
        command: SecretCommand,
    },

    /// Docker-compatible registry and image verbs; never a Docker daemon.
    ///
    /// `login`/`logout` manage the owner-only credential store, `list`/`images` inspect it and the
    /// OCI cache, and `pull`/`build` reuse the native engine that `up` boots.
    Docker {
        #[command(subcommand)]
        command: DockerCommand,
    },

    /// Open the world or service URL in your browser, printing it when headless.
    Open {
        /// Open this service's route (default: the primary route).
        service: Option<String>,
    },

    // `slipfork auth` is deferred past 0.1.0: OSPP pairing ships in a later release.
    // Auth {
    //     /// Endpoint to pair with (default: the saved record).
    //     url: Option<String>,
    //     /// Endpoint that wins over both the positional URL and the saved record.
    //     #[arg(long, value_name = "URL")]
    //     ospp_endpoint: Option<String>,
    // },
    /// Format a Forkfile in place, or report with --check.
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
        #[arg(long = "name", value_name = "NAME")]
        world: String,
        #[arg(long, value_name = "PATH")]
        heads: PathBuf,
        #[arg(long)]
        no_cache: bool,
    },
}

/// `slipfork secret` subcommands.
#[derive(Debug, Subcommand)]
pub enum SecretCommand {
    /// Show declared secrets, always redacted.
    List,

    /// Store one value read from stdin (never echoed).
    Set {
        /// COMPONENT.KEY to store.
        target: String,
    },

    /// Remove exactly one stored key.
    Unset {
        /// COMPONENT.KEY to remove.
        target: String,
    },
}

/// `slipfork docker` subcommands.
#[derive(Debug, Subcommand)]
pub enum DockerCommand {
    /// Verify credentials against a registry, then save them to the 0600 store.
    Login {
        /// Registry host (default: Docker Hub).
        #[arg(value_name = "HOST", default_value = "docker.io")]
        host: String,
        /// Registry username.
        #[arg(long, value_name = "USERNAME")]
        username: String,
        /// Read the password from one stdin line instead of the terminal prompt.
        #[arg(long)]
        password_stdin: bool,
    },

    /// Forget one host's stored credentials.
    Logout {
        /// Registry host (default: Docker Hub).
        #[arg(value_name = "HOST", default_value = "docker.io")]
        host: String,
    },

    /// List the hosts in the Slipfork credential store.
    List,

    /// List cached OCI references and their digests.
    Images,

    /// Pull one OCI reference into the sealed image cache.
    Pull {
        /// Full reference, e.g. `ghcr.io/owner/image:tag`.
        #[arg(value_name = "REFERENCE")]
        reference: String,
    },

    /// Build one `Forkfile` image component, streaming its Dockerfile steps.
    Build {
        /// Image component id declared in this directory's Forkfile.
        #[arg(value_name = "COMPONENT")]
        component: String,
        /// Build only up to this Dockerfile stage.
        #[arg(long, value_name = "STAGE")]
        target: Option<String>,
        /// Ignore the sealed step cache and execute every instruction again.
        #[arg(long)]
        no_cache: bool,
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, Command};

    fn logs(argv: &[&str]) -> (Option<String>, Vec<String>) {
        let cli = Cli::try_parse_from(argv).expect("the logs argv parses");
        match cli.command.expect("logs is a command") {
            Command::Logs { run, targets, .. } => (run, targets),
            other => panic!("expected logs, parsed {other:?}"),
        }
    }

    /// Bare `-r` defaults to `failed`; explicit values and `-r=<id> <target>` stay unambiguous.
    #[test]
    fn bare_run_defaults_to_failed_and_explicit_forms_parse_cleanly() {
        assert_eq!(
            logs(&["slipfork", "logs", "-r"]),
            (Some("failed".to_owned()), Vec::new())
        );
        assert_eq!(
            logs(&["slipfork", "logs", "-r", "last"]),
            (Some("last".to_owned()), Vec::new())
        );
        assert_eq!(
            logs(&["slipfork", "logs", "-r", "5be421"]),
            (Some("5be421".to_owned()), Vec::new())
        );
        assert_eq!(
            logs(&["slipfork", "logs", "-r=5be421", "api"]),
            (Some("5be421".to_owned()), vec!["api".to_owned()])
        );
        assert_eq!(
            logs(&["slipfork", "logs", "api", "-r"]),
            (Some("failed".to_owned()), vec!["api".to_owned()])
        );
    }
}
