//! Machine-checked ANSI purity over the one shipped verb table.
//!
//! Every verb in `slipfork::surface::VERBS` runs against the real binary with `NO_COLOR=1`,
//! `TERM=dumb`, and a piped stdout; every `--json` verb must also emit one parseable object.
//! A real PTY (`script -qec`) proves colour appears only on an interactive terminal.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use slipfork::surface::VERBS;

const ANSI: &str = "\u{1b}[";

fn scratch() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock reads after the epoch")
        .as_nanos();
    let root =
        PathBuf::from("/var/tmp").join(format!("slipfork-ansi-{}-{nanos}", std::process::id()));
    for dir in ["run", "home", "state", "config"] {
        std::fs::create_dir_all(root.join(dir)).expect("create scratch directory");
    }
    root
}

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_slipfork"));
    command
        .current_dir(root)
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env("HOME", root.join("home"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env_remove("SLIPFORK_TEST_ROOT")
        .stdin(Stdio::null());
    command
}

fn run(root: &Path, args: &[&str]) -> Output {
    command(root)
        .args(args)
        .output()
        .expect("the slipfork binary must be runnable")
}

fn assert_plain(output: &Output, args: &[&str]) {
    for (stream, bytes) in [
        ("stdout", output.stdout.as_slice()),
        ("stderr", output.stderr.as_slice()),
    ] {
        let text = String::from_utf8_lossy(bytes);
        assert!(
            !text.contains(ANSI),
            "{args:?} emitted ANSI on {stream}: {text:?}"
        );
    }
}

/// A `--json` invocation that reaches the handler and yields exactly one JSON object.
fn json_probe(verb: &str) -> Option<Vec<&'static str>> {
    Some(match verb {
        "status" => vec!["status", "--json"],
        "ps" => vec!["ps", "--json"],
        "doctor" => vec!["doctor", "--json"],
        "up" => vec!["up", "--json"],
        "down" => vec!["down", "--json"],
        "restart" => vec!["restart", "--json"],
        "fork" => vec!["fork", "--json"],
        "drop" => vec!["drop", "feat-auth", "--json"],
        "logs" => vec!["logs", "--json"],
        "shell" => vec!["shell", "--json"],
        "exec" => vec!["exec", "--json", "api", "--", "true"],
        "db" => vec!["db", "--json"],
        "redis" => vec!["redis", "--json"],
        "env" => vec!["env", "--json"],
        "secret" => vec!["secret", "list", "--json"],
        "open" => vec!["open", "--json"],
        "auth" => vec!["auth", "--json"],
        "fmt" => vec!["fmt", "--check", "--json"],
        "migrate" => vec!["migrate", "--json"],
        "daemon" => vec!["daemon", "install", "--json"],
        _ => return None,
    })
}

/// Sends the daemon owned by this scratch runtime a typed shutdown, then waits for its socket.
fn shutdown_daemon(root: &Path) {
    let socket = root.join("run/daemon.sock");
    if let Ok(mut stream) = UnixStream::connect(&socket) {
        let _ = stream.write_all(b"{\"v\":1,\"id\":\"ansi-bye\",\"method\":\"shutdown\"}\n");
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut reply = [0_u8; 128];
        let _ = stream.read(&mut reply);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn every_shipped_verb_is_plain_and_json_is_parseable() {
    let root = scratch();
    for verb in VERBS {
        let help = [verb.name, "--help"];
        assert_plain(&run(&root, &help), &help);

        let mut bare = vec![verb.name];
        if verb.name == "drop" {
            bare.push("feat-auth");
        }
        assert_plain(&run(&root, &bare), &bare);

        if verb.flags.contains(&"--json") {
            let probe = json_probe(verb.name)
                .unwrap_or_else(|| panic!("`{}` needs a JSON probe", verb.name));
            let output = run(&root, &probe);
            assert_plain(&output, &probe);
            let text = String::from_utf8(output.stdout).expect("stdout is UTF-8");
            let value: Value = serde_json::from_str(text.trim()).unwrap_or_else(|error| {
                panic!(
                    "{probe:?} must emit one JSON object ({error}): {text:?}; stderr={:?}",
                    String::from_utf8_lossy(&output.stderr)
                )
            });
            assert!(value.is_object(), "{probe:?} must emit an object: {value}");
            if value.get("error").is_some() {
                let next = value["error"]["next"]
                    .as_str()
                    .or_else(|| value["next"].as_str());
                assert!(
                    next.is_some_and(|next| !next.is_empty()),
                    "{probe:?} error object needs a `next`: {value}"
                );
                assert_eq!(
                    next.map_or(0, |next| next.matches("next:").count()),
                    0,
                    "{probe:?} embeds a stray `next:` inside JSON: {value}"
                );
            }
        }
    }
    shutdown_daemon(&root);
    let _ = std::fs::remove_dir_all(&root);
}

fn pty(root: &Path, args: &[&str], envs: &[(&str, &str)]) -> Option<String> {
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
    command
        .current_dir(root)
        .env("HOME", root.join("home"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env_remove("NO_COLOR")
        .env("TERM", "xterm-256color");
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command.output().ok()?;
    assert!(output.status.success(), "pty run of {args:?} must succeed");
    Some(String::from_utf8_lossy(&output.stdout).replace('\r', ""))
}

#[test]
fn colour_appears_only_on_a_terminal_that_allows_it() {
    let root = scratch();
    for args in [&["--help"][..], &["status"][..]] {
        if let Some(text) = pty(&root, args, &[]) {
            assert!(
                text.contains(ANSI),
                "a TTY must get colour for {args:?}: {text:?}"
            );
        }
        if let Some(text) = pty(&root, args, &[("NO_COLOR", "1")]) {
            assert!(!text.contains(ANSI), "NO_COLOR must win for {args:?}");
        }
        if let Some(text) = pty(&root, args, &[("TERM", "dumb")]) {
            assert!(
                !text.contains(ANSI),
                "TERM=dumb must stay plain for {args:?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}
