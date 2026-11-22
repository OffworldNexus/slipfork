//! Machine-checked ANSI purity over the one shipped verb table.
//!
//! Every verb in `slipfork::surface::VERBS` runs against the real binary with `NO_COLOR=1`,
//! `TERM=dumb`, and a piped stdout, then again with `NO_COLOR` unset and `TERM=xterm-256color`
//! still piped; a pipe must never carry an escape in either permutation. Every `--json` verb must
//! also emit one parseable object. A real PTY (`script -qec`) proves colour appears only on an
//! interactive terminal — and that `--json` stays one plain object even there.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use slipfork::surface::VERBS;

const ESCAPE: char = '\u{1b}';

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
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("SSH_CONNECTION")
        .env_remove("SSH_TTY")
        .stdin(Stdio::null());
    command
}

/// The pipe-only permutation: a colour-capable terminal name, but stdout is still a pipe.
fn command_pipe(root: &Path) -> Command {
    let mut command = command(root);
    command.env_remove("NO_COLOR").env("TERM", "xterm-256color");
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
            !text.contains(ESCAPE),
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

/// Kills any `__daemon` still carrying this lab's runtime directory, so no survivor leaks.
fn kill_daemons(root: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let scope = root.as_os_str().as_bytes();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        if !cmdline.windows(b"__daemon".len()).any(|w| w == b"__daemon") {
            continue;
        }
        let Ok(environ) = std::fs::read(format!("/proc/{pid}/environ")) else {
            continue;
        };
        if environ.windows(scope.len()).any(|window| window == scope) {
            if let Ok(pid) = i32::try_from(pid) {
                // SAFETY: `kill(2)` never dereferences memory.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }
}

/// One scratch root that reaps the daemon it may have started and then removes the disk tree.
struct Lab {
    root: PathBuf,
}

impl Lab {
    fn new() -> Self {
        Self { root: scratch() }
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        shutdown_daemon(&self.root);
        kill_daemons(&self.root);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn verb_table_matches_the_real_help_surface() {
    let lab = Lab::new();
    let root = lab.root.as_path();
    let help = run(root, &["--help"]);
    assert!(help.status.success(), "`slipfork --help` must succeed");
    let text = String::from_utf8(help.stdout).expect("help is UTF-8");
    let listed: Vec<&str> = text
        .lines()
        .skip_while(|line| line.trim() != "Commands:")
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .map(|line| line.split_whitespace().next().unwrap_or_default())
        .collect();
    let table: Vec<&str> = VERBS.iter().map(|verb| verb.name).collect();
    assert_eq!(
        listed, table,
        "`slipfork::surface::VERBS` drifted from `slipfork --help`; a new verb must join the matrix"
    );
    for verb in VERBS {
        let help = run(root, &[verb.name, "--help"]);
        assert!(help.status.success(), "`{} --help` must succeed", verb.name);
        let text = String::from_utf8(help.stdout).expect("help is UTF-8");
        for flag in verb.flags {
            assert!(
                text.contains(*flag),
                "`{} --help` is missing {flag}: {text}",
                verb.name
            );
        }
    }
}

#[test]
fn every_shipped_verb_is_plain_and_json_is_parseable() {
    let lab = Lab::new();
    let root = lab.root.as_path();
    for verb in VERBS {
        let help = [verb.name, "--help"];
        assert_plain(&run(root, &help), &help);
        assert_plain(
            &command_pipe(root).args(help).output().expect("pipe"),
            &help,
        );

        let mut bare = vec![verb.name];
        if verb.name == "drop" {
            bare.push("feat-auth");
        }
        assert_plain(&run(root, &bare), &bare);
        assert_plain(
            &command_pipe(root).args(&bare).output().expect("pipe"),
            &bare,
        );

        if verb.flags.contains(&"--json") {
            let probe = json_probe(verb.name)
                .unwrap_or_else(|| panic!("`{}` needs a JSON probe", verb.name));
            let output = run(root, &probe);
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
}

/// Runs `args` under a real PTY; fails (never passes vacuously) when `script` is unavailable.
fn pty(root: &Path, args: &[&str], envs: &[(&str, &str)]) -> String {
    let available = Command::new("script")
        .arg("--version")
        .output()
        .expect("the ANSI matrix needs `script`(1) for its PTY probe")
        .status
        .success();
    assert!(
        available,
        "`script --version` must succeed for the PTY probe"
    );
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
    let output = command.output().expect("run the PTY probe");
    assert!(output.status.success(), "pty run of {args:?} must succeed");
    String::from_utf8_lossy(&output.stdout).replace('\r', "")
}

#[test]
fn colour_appears_only_on_a_terminal_that_allows_it() {
    let lab = Lab::new();
    let root = lab.root.as_path();
    for args in [&["--help"][..], &["status"][..], &["ps"][..]] {
        let text = pty(root, args, &[]);
        assert!(
            text.contains(ESCAPE),
            "a TTY must get colour for {args:?}: {text:?}"
        );
        let plain = pty(root, args, &[("NO_COLOR", "1")]);
        assert!(!plain.contains(ESCAPE), "NO_COLOR must win for {args:?}");
        let dumb = pty(root, args, &[("TERM", "dumb")]);
        assert!(
            !dumb.contains(ESCAPE),
            "TERM=dumb must stay plain for {args:?}"
        );
    }

    // `--json` on a real terminal is still one plain, parseable object.
    let json = pty(root, &["status", "--json"], &[]);
    assert!(
        !json.contains(ESCAPE),
        "`--json` must stay plain on a PTY: {json:?}"
    );
    let value: Value = serde_json::from_str(json.trim())
        .unwrap_or_else(|error| panic!("`status --json` must emit one object ({error}): {json:?}"));
    assert!(value.is_object(), "{value}");
}
