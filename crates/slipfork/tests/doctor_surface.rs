//! Real-PTY acceptance for the four-row `slipfork doctor` and clap help styling.
//!
//! Every assertion drives the shipped binary through `script -qec` on a real terminal, or through
//! a real pipe. No mock terminal, no captured formatter: colour must appear only where the
//! composition root allows it, and the user copy must stay free of kernel and socket jargon.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const ANSI: &str = "\u{1b}[";
const JARGON: [&str; 4] = ["netns", "ingress", "unprivileged user", ":80"];

fn scratch() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock reads after the epoch")
        .as_nanos();
    let root = PathBuf::from("/var/tmp").join(format!(
        "slipfork-doctor-surface-{}-{nanos}",
        std::process::id()
    ));
    for dir in ["home/demo", "run", "state", "config"] {
        std::fs::create_dir_all(root.join(dir)).expect("create scratch directory");
    }
    root
}

/// One disk-backed lab with a `~/demo` project and an isolated runtime directory.
struct Lab {
    root: PathBuf,
}

impl Lab {
    fn new() -> Self {
        Self { root: scratch() }
    }

    fn project(&self) -> PathBuf {
        self.root.join("home/demo")
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(self.project())
            .env("HOME", self.home())
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env_remove("NO_COLOR")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("SSH_CONNECTION")
            .env_remove("SSH_TTY")
            .stdin(Stdio::null());
        command
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Removes SGR sequences so row and jargon assertions read the visible text.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            out.push(character);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

/// Runs the shipped binary under a real PTY; `script` is mandatory, never skipped.
fn pty(lab: &Lab, args: &[&str], envs: &[(&str, &str)]) -> String {
    let available = Command::new("script")
        .arg("--version")
        .output()
        .expect("the doctor surface needs `script`(1) for its PTY probe")
        .status
        .success();
    assert!(available, "`script --version` must succeed");
    let bin = env!("CARGO_BIN_EXE_slipfork");
    let line = format!("'{}' {}", bin.replace('\'', r"'\''"), args.join(" "));
    let mut command = lab.command("script");
    command.args(["-qec", &line, "/dev/null"]);
    command.env("TERM", "xterm-256color");
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command.output().expect("run the PTY probe");
    String::from_utf8_lossy(&output.stdout).replace('\r', "")
}

fn piped(lab: &Lab, args: &[&str]) -> Output {
    let mut command = lab.command(env!("CARGO_BIN_EXE_slipfork"));
    command.args(args).env("TERM", "xterm-256color");
    command.output().expect("run the piped probe")
}

fn visible_rows(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.starts_with("  "))
        .filter(|line| {
            let row = line.trim_start();
            row.starts_with('✓') || row.starts_with('✗')
        })
        .map(|line| line.trim_start_matches([' ', '✓', '✗']).trim().to_owned())
        .collect()
}

#[test]
fn a_packaged_socket_reads_installed_but_inactive() {
    let lab = Lab::new();
    let install_root = lab.root.join("root");
    let unit_dir = install_root.join("usr/lib/systemd/system");
    std::fs::create_dir_all(&unit_dir).expect("create the packaged unit tree");
    std::fs::write(unit_dir.join("slipfork-ingress.socket"), "[Socket]\n")
        .expect("write the packaged socket unit");
    let output = lab
        .command(env!("CARGO_BIN_EXE_slipfork"))
        .arg("doctor")
        .env("SLIPFORK_INSTALL_ROOT", &install_root)
        .output()
        .expect("run doctor against the packaged unit tree");
    let plain = String::from_utf8_lossy(&output.stdout);
    assert!(
        plain.contains("local domain routing inactive"),
        "a packaged socket must read installed-but-inactive: {plain}"
    );
    assert!(
        !plain.contains("local domain routing not installed"),
        "a packaged socket is not missing: {plain}"
    );
    assert!(
        plain.contains("sudo systemctl enable --now slipfork-ingress.socket"),
        "the packaged remedy must enable the socket: {plain}"
    );
}

#[test]
fn doctor_has_four_plain_rows_and_styled_roles_on_a_terminal() {
    let lab = Lab::new();
    let raw = pty(&lab, &["doctor"], &[]);
    let plain = strip_ansi(&raw);

    let rows = visible_rows(&plain);
    assert_eq!(rows.len(), 4, "doctor renders exactly four rows: {plain:?}");
    for (index, name) in ["Storage", "Containers", "Daemon", "Domains"]
        .iter()
        .enumerate()
    {
        assert!(
            rows[index].starts_with(name),
            "row {index} must be {name}: {rows:?}"
        );
    }

    for (needle, styled) in [
        ("~/demo", "\u{1b}[1;36m~/demo"),
        ("Btrfs", "\u{1b}[1mBtrfs"),
        ("crun ", "\u{1b}[1mcrun "),
        ("http://*.localhost", "\u{1b}[1;4;36mhttp://*.localhost"),
    ] {
        assert!(plain.contains(needle), "missing {needle}: {plain:?}");
        assert!(
            raw.contains(styled),
            "{needle} must carry its SGR role: {raw:?}"
        );
    }
    // The package name and unit name are strings a user must copy verbatim; the jargon guard
    // still covers every other occurrence of `netns` and `ingress`.
    let readable = plain
        .replace("slirp4netns", "")
        .replace("slipfork-ingress", "");
    for word in JARGON {
        assert!(
            !readable.contains(word),
            "doctor leaked `{word}`: {plain:?}"
        );
    }

    let no_color = pty(&lab, &["doctor"], &[("NO_COLOR", "1")]);
    assert!(
        !no_color.contains(ANSI),
        "NO_COLOR must emit zero escapes: {no_color:?}"
    );
    let piped_run = piped(&lab, &["doctor"]);
    for (stream, bytes) in [
        ("stdout", piped_run.stdout.as_slice()),
        ("stderr", piped_run.stderr.as_slice()),
    ] {
        let text = String::from_utf8_lossy(bytes);
        assert!(
            !text.contains(ANSI),
            "a pipe must emit zero escapes on {stream}: {text:?}"
        );
    }

    let json = piped(&lab, &["doctor", "--json"]);
    assert!(!String::from_utf8_lossy(&json.stdout).contains(ANSI));
    let value: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("doctor --json is one object");
    let names: Vec<&str> = value["checks"]
        .as_array()
        .expect("checks is an array")
        .iter()
        .map(|check| check["name"].as_str().expect("check name"))
        .collect();
    assert_eq!(names, ["Storage", "Containers", "Daemon", "Domains"]);
}

#[test]
fn help_is_styled_on_a_terminal_and_plain_on_a_pipe() {
    let lab = Lab::new();
    for args in [&["--help"][..], &["shell", "--help"][..]] {
        let raw = pty(&lab, args, &[]);
        assert!(
            raw.contains(ANSI),
            "{args:?} must be styled on a terminal: {raw:?}"
        );
        let piped = piped(&lab, args);
        let text = String::from_utf8_lossy(&piped.stdout);
        assert!(
            !text.contains(ANSI),
            "{args:?} must stay plain on a pipe: {text:?}"
        );
    }
}
