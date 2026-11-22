//! Protected end-to-end check of the shipped binary surface.
//!
//! Real subprocesses only: plain pipes for framing, `script` for a real PTY when
//! asserting that colour appears on a terminal and disappears under `NO_COLOR` or
//! `TERM=dumb`. The PTY assertion reports a skip message when `script` is absent;
//! it never substitutes a mock.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[path = "support/daemon_reap.rs"]
mod reap;

const ANSI: &str = "\u{1b}[";

fn scratch(prefix: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock reads after the epoch")
        .as_nanos();
    let root = PathBuf::from("/var/tmp").join(format!(
        "slipfork-cli-{prefix}-{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).expect("create /var/tmp scratch directory");
    root
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("the slipfork binary must be runnable")
}

fn run_isolated(args: &[&str], root: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(args)
        .env("NO_COLOR", "1")
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("XDG_STATE_HOME", root.join("state"))
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

/// A PTY capture that tolerates a failing exit code (`doctor` exits 1 on an unprepared host).
fn pty_text(args: &[&str], envs: &[(&str, &str)]) -> Option<String> {
    run_in_pty(args, envs).map(|output| {
        let mut text = String::from_utf8_lossy(&output.stdout).replace('\r', "");
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        text
    })
}

#[test]
fn help_and_version_are_the_shipped_surface() {
    let help = help_text();
    assert!(help.contains("Usage: slipfork <command>"), "{help}");
    assert_eq!(
        shipped_verbs(&help),
        [
            "status",
            "ps",
            "doctor",
            "up",
            "down",
            "restart",
            "fork",
            "drop",
            "logs",
            "shell",
            "exec",
            "env",
            "secret",
            "open",
            "auth",
            "fmt",
            "migrate",
            "completions",
            "daemon",
            "help",
            "version"
        ],
        "{help}"
    );
    assert!(!help.contains("  lsp "), "no lsp row may ship: {help}");
    assert!(help.contains("Examples:\n  slipfork\n"), "{help}");
    assert!(!help.contains(ANSI), "piped output must stay plain");

    // One table drives the help rows and all three scripts, so every verb appears in each.
    for shell in ["bash", "zsh", "fish"] {
        let script = run(&["completions", shell]);
        assert!(
            script.status.success(),
            "`completions {shell}` must succeed"
        );
        let text = String::from_utf8(script.stdout).expect("completions are UTF-8");
        assert!(!text.contains(ANSI), "{shell} must stay plain: {text}");
        for verb in shipped_verbs(&help) {
            assert!(
                text.contains(&verb),
                "{shell} script is missing {verb}: {text}"
            );
        }
    }
    let unknown = run(&["completions", "pwsh"]);
    assert_eq!(unknown.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&unknown.stderr);
    assert!(stderr.contains("is not a shell"), "{stderr}");
    assert_eq!(stderr.matches("next:").count(), 1, "{stderr}");

    let version = run(&["--version"]);
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout)
            .expect("version is UTF-8")
            .trim(),
        format!("slipfork {}", env!("CARGO_PKG_VERSION"))
    );

    // Bare `slipfork` is the status tree; with no daemon it is the empty state, exit 0.
    let root = scratch("bare");
    let bare = run_isolated(&[], &root);
    assert!(bare.status.success());
    assert_eq!(
        String::from_utf8(bare.stdout).expect("status is UTF-8"),
        "○ No worlds running\n"
    );
    let _ = std::fs::remove_dir_all(&root);

    // The language server is its own binary; the CLI verb is gone for good.
    let retired = run(&["lsp"]);
    assert_eq!(retired.status.code(), Some(2));
    let stderr = String::from_utf8(retired.stderr).expect("errors are UTF-8");
    assert!(stderr.contains("unknown argument `lsp`"), "{stderr}");
    assert!(
        stderr.contains("standalone slipfork-lsp binary"),
        "{stderr}"
    );
    assert!(stderr.contains("next:"), "{stderr}");
    let retired_help = run(&["lsp", "--help"]);
    assert_eq!(retired_help.status.code(), Some(2));
    let stderr = String::from_utf8(retired_help.stderr).expect("errors are UTF-8");
    assert!(stderr.contains("unknown argument `lsp`"), "{stderr}");

    let unknown = run(&["frobnicate"]);
    assert_eq!(unknown.status.code(), Some(2));
    let stderr = String::from_utf8(unknown.stderr).expect("errors are UTF-8");
    assert!(stderr.contains("error:"), "{stderr}");
    assert!(stderr.contains("next:"), "{stderr}");
}

#[test]
fn host_verbs_ship_help_and_keep_json_clean() {
    for (verb, flags) in [
        ("env", &["--reveal", "--json"][..]),
        ("secret", &["list", "set", "unset", "--json"][..]),
        ("open", &["--json"][..]),
        ("auth", &["--ospp-endpoint", "--json"][..]),
    ] {
        let help = run(&[verb, "--help"]);
        assert!(help.status.success(), "`{verb} --help` must succeed");
        let text = String::from_utf8_lossy(&help.stdout);
        assert!(text.contains(&format!("slipfork {verb}")), "{text}");
        for flag in flags {
            assert!(
                text.contains(flag),
                "`{verb} --help` is missing {flag}: {text}"
            );
        }
        assert_eq!(text.matches("--help").count(), 1, "{text}");
        assert!(!text.contains(ANSI), "piped help must stay plain: {text}");
    }
}

#[test]
fn streaming_verbs_ship_help_and_fail_with_next() {
    for verb in ["logs", "shell", "exec"] {
        let help = run(&[verb, "--help"]);
        assert!(help.status.success(), "`{verb} --help` must succeed");
        let text = String::from_utf8_lossy(&help.stdout);
        assert!(text.contains(&format!("slipfork {verb}")), "{text}");
        assert!(!text.contains(ANSI), "piped help must stay plain: {text}");
    }
    let shell_help = String::from_utf8_lossy(&run(&["shell", "--help"]).stdout).into_owned();
    assert!(
        shell_help.contains("<TARGET>"),
        "`shell` must document its required target: {shell_help}"
    );
    // A missing target is a clap usage error: exit 2 with exactly one remedy.
    let missing = run(&["shell"]);
    assert_eq!(missing.status.code(), Some(2), "{missing:?}");
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(stderr.contains("required arguments"), "{stderr}");
    assert_eq!(stderr.matches("next:").count(), 1, "{stderr}");

    // `db` and `redis` are gone for good: no aliases, no hidden shims.
    for retired in ["db", "redis"] {
        let output = run(&[retired]);
        assert_eq!(output.status.code(), Some(2), "`{retired}` must not ship");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("unrecognized subcommand"), "{stderr}");
    }

    let exec = run(&["exec", "api"]);
    assert_eq!(exec.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&exec.stderr);
    assert!(stderr.contains("needs `--`"), "{stderr}");

    let root = scratch("streams");
    for args in [
        &["logs", "--json"][..],
        &["shell", "api"][..],
        &["exec", "api", "--", "true"][..],
    ] {
        let output = run_isolated(args, &root);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        if args.contains(&"--json") {
            let value: serde_json::Value = serde_json::from_str(stdout.trim())
                .unwrap_or_else(|error| panic!("{args:?} must emit JSON ({error}): {stdout:?}"));
            assert!(
                value["error"]["next"].is_string(),
                "{args:?} JSON failure needs one `next`: {value}"
            );
        } else {
            assert!(stderr.contains("error:"), "{stderr}");
            assert!(stderr.contains("next:"), "{stderr}");
        }
        assert!(!stderr.contains(ANSI), "{stderr}");
        assert!(!stdout.contains(ANSI), "{args:?} must stay plain");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn fork_and_drop_ship_help_and_fail_with_one_next() {
    for verb in ["fork", "drop"] {
        let help = run(&[verb, "--help"]);
        assert!(help.status.success(), "`{verb} --help` must succeed");
        let text = String::from_utf8_lossy(&help.stdout);
        assert!(text.contains(&format!("slipfork {verb}")), "{text}");
        assert!(text.contains("-h, --help"), "{text}");
        assert!(!text.contains(ANSI), "piped help must stay plain: {text}");
    }
    let root = scratch("forkdrop");
    let run_in = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_slipfork"))
            .args(args)
            .current_dir(&root)
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("XDG_STATE_HOME", root.join("state"))
            .output()
            .expect("the slipfork binary must be runnable")
    };
    for args in [&["fork", "feat-auth"][..], &["drop", "feat-auth"][..]] {
        let output = run_in(args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(stderr.matches("next:").count(), 1, "{args:?}: {stderr}");
        assert!(!stderr.contains(ANSI), "{args:?} must stay plain: {stderr}");
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains(ANSI),
            "{args:?} must stay plain"
        );
    }
    // `drop` still spawns the per-user daemon, so reap it before deleting the scratch root.
    reap::reap(&root);
    let missing = run(&["drop"]);
    assert_eq!(missing.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&missing.stderr)
            .matches("next:")
            .count(),
        1
    );
    let _ = std::fs::remove_dir_all(&root);
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

#[test]
fn doctor_and_status_own_a_real_terminal() {
    let Some(doctor) = pty_text(&["doctor"], &[]) else {
        eprintln!("skipping PTY verdict assertion: `script` (util-linux) is not available");
        return;
    };
    assert!(
        doctor.starts_with("\u{1b}[3"),
        "the verdict must lead and be styled: {doctor:?}"
    );
    assert!(
        doctor.contains('✓') || doctor.contains('✗'),
        "every doctor row keeps its mark: {doctor:?}"
    );
    let status = pty_text(&["status"], &[]).expect("script must still be available");
    assert!(
        status.contains(ANSI),
        "`status` must style a real terminal: {status:?}"
    );

    for envs in [vec![("NO_COLOR", "1")], vec![("TERM", "dumb")]] {
        let doctor = pty_text(&["doctor"], &envs).expect("script must still be available");
        assert!(
            !doctor.contains(ANSI),
            "{envs:?} must stay plain: {doctor:?}"
        );
        assert!(
            doctor.starts_with("✓ Ready") || doctor.starts_with("✗ "),
            "the verdict leads even when plain: {doctor:?}"
        );
        let status = pty_text(&["status"], &envs).expect("script must still be available");
        assert!(
            !status.contains(ANSI),
            "{envs:?} must stay plain: {status:?}"
        );
    }
}

#[test]
fn piped_lifecycle_verbs_replay_plain_rows_and_fail_closed() {
    let root = scratch("replay");
    for args in [&["up"][..], &["restart"][..], &["down"][..]] {
        let output = Command::new(env!("CARGO_BIN_EXE_slipfork"))
            .args(args)
            .current_dir(&root)
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("XDG_STATE_HOME", root.join("state"))
            .stdin(std::process::Stdio::null())
            .output()
            .expect("the slipfork binary must be runnable");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        for text in [&stdout, &stderr] {
            assert!(
                !text.contains(ANSI),
                "{args:?} emitted ANSI under a pipe: {text:?}"
            );
            assert!(
                !text.contains('\r'),
                "{args:?} emitted a carriage return: {text:?}"
            );
        }
        assert!(
            stderr.lines().any(|line| line.starts_with("error: ")),
            "{args:?} must name the failure: {stderr}"
        );
        assert_eq!(
            output.status.code(),
            Some(1),
            "no daemon must fail closed, never fake a boot: {args:?}"
        );
        assert_eq!(
            stderr.matches("next:").count(),
            1,
            "{args:?} needs exactly one remedy: {stderr}"
        );
    }
    // `restart` and `down` spawn the per-user daemon, so reap it before deleting the scratch root.
    reap::reap(&root);
    let _ = std::fs::remove_dir_all(&root);
}
