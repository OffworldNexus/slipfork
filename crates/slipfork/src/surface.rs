//! The one shipped-surface table: every verb, its flags, and the three completion scripts.
//!
//! `slipfork --help` and `slipfork completions <shell>` both read `VERBS`, so a verb can never
//! ship in the help text while going missing from the completions (or the reverse).

use std::fmt::Write as _;

/// One shipped verb: its command word, its `--help` summary, and the words it accepts.
pub struct VerbSpec {
    /// The command word typed after `slipfork`.
    pub name: &'static str,
    /// The single help line that follows the verb.
    pub summary: &'static str,
    /// Subcommands such as `daemon install`.
    pub subcommands: &'static [&'static str],
    /// Every flag the verb parses, exactly as its help page spells it.
    pub flags: &'static [&'static str],
}

/// Every verb that ships in this binary, in help order.
pub const VERBS: &[VerbSpec] = &[
    VerbSpec {
        name: "status",
        summary: "Show every running world as a tree (the bare `slipfork` verb)",
        subcommands: &[],
        flags: &["--json", "-v", "--verbose", "-h", "--help"],
    },
    VerbSpec {
        name: "ps",
        summary: "Show only the world or fork rooted in this directory",
        subcommands: &[],
        flags: &["--json", "-v", "--verbose", "-h", "--help"],
    },
    VerbSpec {
        name: "doctor",
        summary: "Check Btrfs, namespaces, and the crun + :80 runtime",
        subcommands: &[],
        flags: &["--json", "-h", "--help"],
    },
    VerbSpec {
        name: "up",
        summary: "Boot the world declared by ./Forkfile and return when ready",
        subcommands: &[],
        flags: &["--world", "--json", "-h", "--help"],
    },
    VerbSpec {
        name: "down",
        summary: "Stop the running world in this directory",
        subcommands: &[],
        flags: &["--json", "-h", "--help"],
    },
    VerbSpec {
        name: "restart",
        summary: "Restart one service, or every service",
        subcommands: &[],
        flags: &["--json", "-h", "--help"],
    },
    VerbSpec {
        name: "fork",
        summary: "Snapshot the project and world data, then boot the fork on its own URL",
        subcommands: &[],
        flags: &["--json", "-h", "--help"],
    },
    VerbSpec {
        name: "drop",
        summary: "Stop a fork and delete its project and world-data snapshots",
        subcommands: &[],
        flags: &["-f", "--force", "--json", "-h", "--help"],
    },
    VerbSpec {
        name: "logs",
        summary: "Stream merged stdout + stderr with per-service gutters",
        subcommands: &[],
        flags: &["-f", "--follow", "--json", "-h", "--help"],
    },
    VerbSpec {
        name: "shell",
        summary: "Open a shell inside a service or image pod",
        subcommands: &[],
        flags: &["--json", "-h", "--help"],
    },
    VerbSpec {
        name: "exec",
        summary: "Run a command inside a pod and forward its exit code",
        subcommands: &[],
        flags: &["--json", "-h", "--help"],
    },
    VerbSpec {
        name: "db",
        summary: "Open psql against the world postgres",
        subcommands: &[],
        flags: &["--json", "-h", "--help"],
    },
    VerbSpec {
        name: "redis",
        summary: "Open redis-cli against the world redis",
        subcommands: &[],
        flags: &["--json", "-h", "--help"],
    },
    VerbSpec {
        name: "env",
        summary: "Print resolved runtime variables; secrets stay redacted unless revealed",
        subcommands: &[],
        flags: &["--reveal", "--json", "-h", "--help"],
    },
    VerbSpec {
        name: "secret",
        summary: "List, override, or remove local offline secrets",
        subcommands: &["list", "set", "unset"],
        flags: &["--json", "-h", "--help"],
    },
    VerbSpec {
        name: "open",
        summary: "Open the world or service URL in your browser, headless-aware",
        subcommands: &[],
        flags: &["--json", "-h", "--help"],
    },
    VerbSpec {
        name: "auth",
        summary: "Pair this machine with a remote OSPP v1 endpoint",
        subcommands: &[],
        flags: &["--ospp-endpoint", "--json", "-h", "--help"],
    },
    VerbSpec {
        name: "fmt",
        summary: "Format a Forkfile in place",
        subcommands: &[],
        flags: &["--check", "--json", "-h", "--help"],
    },
    VerbSpec {
        name: "migrate",
        summary: "Convert a legacy snow.yml into a Forkfile",
        subcommands: &[],
        flags: &["--write", "--force", "--json", "-h", "--help"],
    },
    VerbSpec {
        name: "completions",
        summary: "Print a shell completion script for bash, zsh, or fish",
        subcommands: &[],
        flags: &["-h", "--help"],
    },
    VerbSpec {
        name: "daemon",
        summary: "Install the per-user unit (and with sudo the system :80 socket)",
        subcommands: &["install"],
        flags: &["--ingress", "--json", "-h", "--help"],
    },
    VerbSpec {
        name: "help",
        summary: "Print this overview",
        subcommands: &[],
        flags: &[],
    },
    VerbSpec {
        name: "version",
        summary: "Print the version",
        subcommands: &[],
        flags: &[],
    },
];

/// The three shells that ship completion scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// POSIX bash.
    Bash,
    /// zsh with `#compdef`.
    Zsh,
    /// fish.
    Fish,
}

impl Shell {
    /// The shell named by `word`, or `None` for anything else.
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }

    /// The completion script for this shell.
    pub fn script(self) -> String {
        match self {
            Self::Bash => bash(),
            Self::Zsh => zsh(),
            Self::Fish => fish(),
        }
    }
}

/// The `Commands:` block of the global help, aligned on the widest verb name.
pub fn commands_block() -> String {
    let width = VERBS.iter().map(|verb| verb.name.len()).max().unwrap_or(0) + 2;
    let mut block = String::new();
    for verb in VERBS {
        let _ = writeln!(
            block,
            "  {:<width$}{}",
            verb.name,
            verb.summary,
            width = width
        );
    }
    block
}

fn words<'a>(items: impl Iterator<Item = &'a str>) -> String {
    items.collect::<Vec<_>>().join(" ")
}

fn accepted(verb: &VerbSpec) -> String {
    words(verb.flags.iter().chain(verb.subcommands).copied())
}

fn bash() -> String {
    let mut script = String::from(
        "_slipfork() {\n    local cur\n    cur=\"${COMP_WORDS[COMP_CWORD]}\"\n    if [ \"$COMP_CWORD\" -eq 1 ]; then\n        COMPREPLY=( $(compgen -W \"",
    );
    script.push_str(&words(VERBS.iter().map(|verb| verb.name)));
    script
        .push_str("\" -- \"$cur\") )\n        return\n    fi\n    case \"${COMP_WORDS[1]}\" in\n");
    for verb in VERBS {
        let items = accepted(verb);
        if items.is_empty() {
            continue;
        }
        let _ = writeln!(
            script,
            "        {} ) COMPREPLY=( $(compgen -W \"{}\" -- \"$cur\") ) ;;",
            verb.name, items
        );
    }
    script.push_str("    esac\n}\ncomplete -F _slipfork slipfork\n");
    script
}

fn zsh() -> String {
    let mut script =
        String::from("#compdef slipfork\n\n_slipfork() {\n  local -a verbs\n  verbs=(");
    script.push_str(&words(VERBS.iter().map(|verb| verb.name)));
    script.push_str(
        ")\n  if (( CURRENT == 2 )); then\n    _describe 'command' verbs\n    return\n  fi\n  case \"$words[2]\" in\n",
    );
    for verb in VERBS {
        let items = accepted(verb);
        if items.is_empty() {
            continue;
        }
        let _ = writeln!(script, "    {} ) compadd -- {} ;;", verb.name, items);
    }
    script.push_str("  esac\n}\n\n_slipfork \"$@\"\n");
    script
}

fn fish() -> String {
    let mut script = String::from("# fish completions for slipfork\n");
    for verb in VERBS {
        let _ = writeln!(
            script,
            "complete -c slipfork -n '__fish_use_subcommand' -a '{}' -d '{}'",
            verb.name, verb.summary
        );
    }
    for verb in VERBS {
        for flag in verb.flags {
            let line = if let Some(long) = flag.strip_prefix("--") {
                format!(
                    "complete -c slipfork -n '__fish_seen_subcommand_from {}' -l {long}",
                    verb.name
                )
            } else if let Some(short) = flag.strip_prefix('-') {
                format!(
                    "complete -c slipfork -n '__fish_seen_subcommand_from {}' -s {short}",
                    verb.name
                )
            } else {
                continue;
            };
            let _ = writeln!(script, "{line}");
        }
        for subcommand in verb.subcommands {
            let _ = writeln!(
                script,
                "complete -c slipfork -n '__fish_seen_subcommand_from {}' -a '{subcommand}'",
                verb.name
            );
        }
    }
    script
}
