//! Compile-time descriptor of the shipped verb matrix, consumed by the ANSI matrix test.
//!
//! The shipped parser, `--help`, `--version`, completion scripts, and the `slipfork(1)` manpage
//! are generated from the `clap` derive tree in [`crate::cli`]. Nothing here feeds the binary; the
//! matrix test iterates this list so every verb is exercised on a pipe, a PTY, and with `--json`.

/// One shipped verb: its command word, its one-line summary, and the flags it accepts.
pub struct VerbSpec {
    /// The command word typed after `slipfork`.
    pub name: &'static str,
    /// The single help line that follows the verb.
    pub summary: &'static str,
    /// Every flag the verb documents, exactly as clap spells it.
    pub flags: &'static [&'static str],
}

/// Every verb the matrix exercises, in shipped help order.
pub const VERBS: &[VerbSpec] = &[
    VerbSpec {
        name: "status",
        summary: "Show every running world as a tree.",
        flags: &["--json", "-v", "--verbose"],
    },
    VerbSpec {
        name: "ps",
        summary: "Show only the world or fork rooted in this directory.",
        flags: &["--json", "-v", "--verbose"],
    },
    VerbSpec {
        name: "doctor",
        summary: "Check Btrfs, namespaces, and the crun + :80 runtime.",
        flags: &["--json"],
    },
    VerbSpec {
        name: "up",
        summary: "Boot the world declared by ./Forkfile.",
        flags: &["--world", "--json"],
    },
    VerbSpec {
        name: "down",
        summary: "Stop the running world in this directory.",
        flags: &["--json"],
    },
    VerbSpec {
        name: "restart",
        summary: "Restart one service, or every service.",
        flags: &["--json"],
    },
    VerbSpec {
        name: "fork",
        summary: "Snapshot the world and boot the fork on its own URL.",
        flags: &["--json"],
    },
    VerbSpec {
        name: "drop",
        summary: "Stop a fork and delete its snapshots.",
        flags: &["-f", "--force", "--json"],
    },
    VerbSpec {
        name: "logs",
        summary: "Stream merged service stdout and stderr.",
        flags: &["-f", "--follow", "-a", "--all", "--json"],
    },
    VerbSpec {
        name: "shell",
        summary: "Open a shell inside a service or image pod.",
        flags: &["--json"],
    },
    VerbSpec {
        name: "exec",
        summary: "Run a command inside a pod and forward its exit code.",
        flags: &["--json"],
    },
    VerbSpec {
        name: "db",
        summary: "Open psql against the world postgres.",
        flags: &["--json"],
    },
    VerbSpec {
        name: "redis",
        summary: "Open redis-cli against the world redis.",
        flags: &["--json"],
    },
    VerbSpec {
        name: "env",
        summary: "Print resolved runtime variables.",
        flags: &["--reveal", "--json"],
    },
    VerbSpec {
        name: "secret",
        summary: "List, override, or remove local offline secrets.",
        flags: &["--json"],
    },
    VerbSpec {
        name: "open",
        summary: "Open the world or service URL in your browser.",
        flags: &["--json"],
    },
    VerbSpec {
        name: "auth",
        summary: "Pair this machine with an OSPP v1 endpoint.",
        flags: &["--ospp-endpoint", "--json"],
    },
    VerbSpec {
        name: "fmt",
        summary: "Format a Forkfile in place.",
        flags: &["--check", "--json"],
    },
    VerbSpec {
        name: "migrate",
        summary: "Convert a legacy snow.yml into a Forkfile.",
        flags: &["--write", "--force", "--json"],
    },
    VerbSpec {
        name: "completions",
        summary: "Print a shell completion script.",
        flags: &[],
    },
    VerbSpec {
        name: "daemon",
        summary: "Install the per-user unit (and with sudo the :80 socket).",
        flags: &["--ingress", "--json"],
    },
    VerbSpec {
        name: "help",
        summary: "Print this overview.",
        flags: &[],
    },
    VerbSpec {
        name: "version",
        summary: "Print the version.",
        flags: &[],
    },
];
