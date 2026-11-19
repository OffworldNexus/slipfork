//! The single `clap` command surface shared by `slipfork` and `slipforkd`.
//!
//! Every world-scoped command selects a non-default world exclusively through `-w/--world
//! <WORLD>`, which accepts exactly the ids `slipfork status` prints: the canonical `<project>`,
//! `<project>:<fork>`, or the repo-local bare `<fork>` (`ADR-0008` §4). `drop` takes an optional
//! fork name, defaulting to the `.worktrees/<fork>` checkout containing `cwd`; `fork` takes one
//! positionally when given and otherwise derives it from the current branch inside `slipforkd`
//! (`docs/design/dx-cli-0.1.0.md` §1).
//! `slipfork list` was deleted: bare `slipfork` (or `ps` for one world) is the only fleet view.

use std::collections::BTreeSet;
use std::path::PathBuf;

use clap::builder::Styles;
use clap::builder::styling::{AnsiColor, Effects};
use clap::{CommandFactory, FromArgMatches, Parser};

mod commands;
mod features;

pub use commands::{
    Commands, DEFAULT_FORWARD_PORT, DEFAULT_REMOTE_PORT, DaemonAction, DumpArgs, HookAction,
    ResourceArgs, ResourceScope, SecretAction, UnitKind,
};
pub use features::FeatureArgs;

/// `slipfork` thin client.
///
// The subcommand is optional so bare `slipfork` is the status dashboard: it runs the exact same
// handler as `slipfork status` (`docs/design/dx-0.1.0-ux.md` §3), with no alias and no second help
// mode — `--help` still lists every command.
#[derive(Debug, Parser)]
#[command(
    name = "slipfork",
    version,
    about = "Instant CoW Btrfs dev worlds & worktree forks from a Forkfile"
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

impl Cli {
    /// Parse argv once, painting `clap`'s help/errors with the same palette as [`crate::ui`].
    ///
    /// The composition root decides `color` (TTY + `NO_COLOR` + `--json`); this constructor never
    /// probes the terminal itself.
    #[must_use]
    pub fn parse_styled(args: &[std::ffi::OsString], color: bool) -> Self {
        let command = Cli::command().styles(styles(color));
        let matches = command.get_matches_from(args);
        Self::from_arg_matches(&matches).unwrap_or_else(|err| err.exit())
    }
}

/// The `clap` help/error `Styles` for one capability snapshot.
///
/// Color off is `Styles::plain()`, so a piped call, `NO_COLOR=1`, and `--json` share the same
/// byte-identical help text.
#[must_use]
pub fn styles(color: bool) -> Styles {
    if !color {
        return Styles::plain();
    }
    Styles::styled()
        .header(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
        .usage(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
        .literal(AnsiColor::Cyan.on_default())
        .placeholder(AnsiColor::Green.on_default())
        .error(AnsiColor::Red.on_default().effects(Effects::BOLD))
        .valid(AnsiColor::Green.on_default())
        .invalid(AnsiColor::Yellow.on_default())
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
        let Some(Commands::Restart { world, .. }) = restart.command else {
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
        let Some(Commands::Open { world, remote, .. }) = open.command else {
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
    fn fork_and_drop_both_take_optional_names() {
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

        let unnamed = Cli::try_parse_from(["slipfork", "drop"]).expect("zero-argument drop parses");
        let Some(Commands::Drop { fork, .. }) = unnamed.command else {
            panic!("expected `drop`");
        };
        assert!(
            fork.is_none(),
            "an absent fork name defaults to `cwd`'s `.worktrees` checkout"
        );
        let dropped = Cli::try_parse_from(["slipfork", "drop", "feat-ux"]).expect("named drop");
        let Some(Commands::Drop { fork, .. }) = dropped.command else {
            panic!("expected `drop`");
        };
        assert_eq!(fork.as_deref(), Some("feat-ux"));
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
        let err = Cli::try_parse_from(["slipfork", "--runtime-dir", "/run/user/1000", "status"])
            .expect_err("--runtime-dir is a slipforkd-only flag");
        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
        assert!(
            Cli::try_parse_from(["slipfork", "--state-dir", "/state", "status"]).is_ok(),
            "--state-dir stays global"
        );
    }

    #[test]
    fn the_deleted_list_verb_is_gone_and_the_short_flags_parse() {
        let err = Cli::try_parse_from(["slipfork", "list"])
            .expect_err("`slipfork list` was deleted in M3.32.4");
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidSubcommand);

        // §2's short flags land on the verbs that own the behaviour today.
        assert!(Cli::try_parse_from(["slipfork", "up", "-w", "demo", "-F", "api,worker"]).is_ok());
        assert!(
            Cli::try_parse_from(["slipfork", "up", "--no-default-features"]).is_ok(),
            "the default feature set can be switched off"
        );
        assert!(Cli::try_parse_from(["slipfork", "ps", "-w", "demo"]).is_ok());
        assert!(Cli::try_parse_from(["slipfork", "logs", "-f", "-n", "20"]).is_ok());
        assert!(Cli::try_parse_from(["slipfork", "prune", "-y"]).is_ok());
        assert!(Cli::try_parse_from(["slipfork", "open", "-r", "-p", "9090"]).is_ok());
        assert!(Cli::try_parse_from(["slipfork", "drop", "feat", "-f"]).is_ok());
        assert!(Cli::try_parse_from(["slipfork", "exec", "-s", "api", "true"]).is_ok());

        // `secret list` keeps its name while no variant is spelled `List`.
        let secret =
            Cli::try_parse_from(["slipfork", "secret", "list"]).expect("secret list parses");
        assert!(matches!(
            secret.command,
            Some(Commands::Secret {
                action: SecretAction::Keys
            })
        ));
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
