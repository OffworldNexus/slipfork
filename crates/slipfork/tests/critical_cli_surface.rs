//! Protected end-to-end check of the shipped binary surface.
//!
//! Real subprocesses only: plain pipes for framing, `script` for a real PTY when
//! asserting that colour appears on a terminal and disappears under `NO_COLOR` or
//! `TERM=dumb`. The PTY assertion reports a skip message when `script` is absent;
//! it never substitutes a mock.

use std::process::{Command, Output};

const ANSI: &str = "\u{1b}[";

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("the slipfork binary must be runnable")
}

fn help_text() -> String {
    let help = run(&["--help"]);
    assert!(help.status.success());
    String::from_utf8(help.stdout).expect("help is UTF-8")
}

/// Verbs listed under `Commands:`, in order, as the help output declares them.
fn shipped_verbs(help: &str) -> Vec<String> {
    help.lines()
        .skip_while(|line| line.trim() != "Commands:")
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .map(|line| {
            line.split_whitespace()
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

fn run_in_pty(args: &[&str], envs: &[(&str, &str)]) -> Option<Output> {
    if !Command::new("script")
        .arg("--version")
        .output()
        .ok()?
        .status
        .success()
    {
        return None;
    }

    let bin = env!("CARGO_BIN_EXE_slipfork");
    let line = format!("'{}' {}", bin.replace('\'', r"'\''"), args.join(" "));
    let mut command = Command::new("script");
    command.args(["-qec", &line, "/dev/null"]);
    command.env_remove("NO_COLOR");
    command.env("TERM", "xterm-256color");
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().ok()
}

fn pty_stdout(binary: &[&str], envs: &[(&str, &str)]) -> String {
    match run_in_pty(binary, envs) {
        Some(output) => {
            assert!(output.status.success(), "pty run should succeed");
            String::from_utf8_lossy(&output.stdout).replace('\r', "")
        }
        None => {
            eprintln!("skipping PTY colour assertion: `script` (util-linux) is not available");
            String::new()
        }
    }
}

#[test]
fn help_and_version_are_the_whole_shipped_surface() {
    let help = help_text();
    assert!(help.contains("Usage: slipfork <command>"), "{help}");
    assert_eq!(
        shipped_verbs(&help),
        ["fmt", "migrate", "lsp", "help", "version"],
        "{help}"
    );
    assert!(
        help.contains("Examples:\n  slipfork fmt --check Forkfile\n"),
        "{help}"
    );
    assert!(!help.contains(ANSI), "piped output must stay plain");

    let version = run(&["--version"]);
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout)
            .expect("version is UTF-8")
            .trim(),
        format!("slipfork {}", env!("CARGO_PKG_VERSION"))
    );

    let bare = run(&[]);
    assert!(bare.status.success());
    assert_eq!(bare.stdout, help.as_bytes());

    let unknown = run(&["up"]);
    assert_eq!(unknown.status.code(), Some(2));
    let stderr = String::from_utf8(unknown.stderr).expect("errors are UTF-8");
    assert!(stderr.contains("error:"), "{stderr}");
    assert!(stderr.contains("next:"), "{stderr}");
}

#[test]
fn colour_only_appears_on_a_terminal_that_allows_it() {
    let colored = pty_stdout(&["--help"], &[]);
    if !colored.is_empty() {
        assert!(colored.contains(ANSI), "a TTY must get colour: {colored}");
    }

    let no_color = pty_stdout(&["--help"], &[("NO_COLOR", "1")]);
    if !no_color.is_empty() {
        assert!(!no_color.contains(ANSI), "NO_COLOR must win: {no_color}");
    }

    let dumb = pty_stdout(&["--help"], &[("TERM", "dumb")]);
    if !dumb.is_empty() {
        assert!(!dumb.contains(ANSI), "TERM=dumb must stay plain: {dumb}");
    }
}
