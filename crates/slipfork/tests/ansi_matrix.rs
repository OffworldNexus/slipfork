//! Machine-checked ANSI purity over the one shipped verb table.
//!
//! Every verb in `slipfork::surface::VERBS` runs against the real binary with `NO_COLOR=1`,
//! `TERM=dumb`, and a piped stdout, then again with `NO_COLOR` unset and `TERM=xterm-256color`
//! still piped; a pipe must never carry an escape in either permutation. Every `--json` verb must
//! also emit one parseable object. A real PTY (`script -qec`) proves colour appears only on an
//! interactive terminal — and that `--json` stays one plain object even there.

#[path = "support/pty.rs"]
mod pty;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use slipfork::surface::VERBS;
use slipfork::ui::report::{Failure, Report, ReportRenderer, WallZone};
use slipfork::ui::{Terminal, UiMode};

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
        "features" => vec!["features", "--json"],
        "secret" => vec!["secret", "list", "--json"],
        "docker" => vec!["docker", "logout", "docker.io", "--json"],
        "open" => vec!["open", "--json"],
        "fmt" => vec!["fmt", "--check", "--json"],
        "migrate" => vec!["migrate", "--json"],
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
fn script_pty(root: &Path, args: &[&str], envs: &[(&str, &str)]) -> String {
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
        let text = script_pty(root, args, &[]);
        assert!(
            text.contains(ESCAPE),
            "a TTY must get colour for {args:?}: {text:?}"
        );
        let plain = script_pty(root, args, &[("NO_COLOR", "1")]);
        assert!(!plain.contains(ESCAPE), "NO_COLOR must win for {args:?}");
        let dumb = script_pty(root, args, &[("TERM", "dumb")]);
        assert!(
            !dumb.contains(ESCAPE),
            "TERM=dumb must stay plain for {args:?}"
        );
    }

    // `--json` on a real terminal is still one plain, parseable object.
    let json = script_pty(root, &["status", "--json"], &[]);
    assert!(
        !json.contains(ESCAPE),
        "`--json` must stay plain on a PTY: {json:?}"
    );
    let value: Value = serde_json::from_str(json.trim())
        .unwrap_or_else(|error| panic!("`status --json` must emit one object ({error}): {json:?}"));
    assert!(value.is_object(), "{value}");
}

/// The seeded shell prompt every grid assertion protects.
const PROMPT: &str = "slipfork@slipfork-dev-vm:~/demo$ slipfork up";
/// The same line after the user pressed Enter: the cursor starts on the row below the prompt.
const SEED: &str = "slipfork@slipfork-dev-vm:~/demo$ slipfork up\r\n";

/// `NO_COLOR` is removed so the shipped append-only boot pane (completed rows plus one
/// `\r`-rewritten status line) really runs.
fn tty_command(root: &Path) -> Command {
    let mut command = command(root);
    command.env_remove("NO_COLOR").env("TERM", "xterm-256color");
    command
}

#[test]
fn vt_grid_scrolls_completed_lines_without_cursor_addressing() {
    let mut screen = pty::Screen::new(6, 20);
    screen.place(0, 0, "prompt");
    screen.feed(b"\r\nline one\r\nline two\r\n");
    assert_eq!(screen.find_row("prompt"), Some(0), "{}", screen.text());
    assert_eq!(screen.find_row("line one"), Some(1), "{}", screen.text());
    assert_eq!(screen.row_text(2), "line two");
    assert_eq!(
        screen.dsr_requests(),
        0,
        "append-only output never asks the terminal for its cursor"
    );
}

/// Scans raw PTY bytes for the absolutely forbidden cursor/region/screen-control sequences.
fn assert_append_only(output: &str, cols: usize) {
    for forbidden in [
        "\x1b[r",
        "\x1b[2J",
        "\x1b[J",
        "\x1bc",
        "\x1b[?1049",
        "\x1b[?47",
        "\x1b[?1047",
        "\x1b[6n",
    ] {
        assert!(
            !output.contains(forbidden),
            "append-only boot output carried {forbidden:?}: {output:?}"
        );
    }
    let bytes = output.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\r' {
            if output[index + 1..].starts_with('\n') {
                index += 2;
                continue;
            }
            if output[index + 1..].starts_with("\x1b[2K") {
                let start = index + 5;
                let rest = &output[start..];
                let end = rest.find(['\r', '\n']).unwrap_or(rest.len());
                let line = &rest[..end];
                if !rest[end..].starts_with("\r\x1b[2K") && !rest[end..].is_empty() {
                    // A completed row reuses the clear prefix; only true status frames are sized.
                    index = start;
                    continue;
                }
                let width = console::measure_text_width(&strip_ansi(line));
                assert!(
                    width <= cols,
                    "a status frame must fit the {cols}-column terminal ({width}): {line:?}"
                );
                index = start + end;
                continue;
            }
            panic!(
                "a bare carriage return that is not a `\\r\\x1b[2K` status frame: {:?}",
                &output[index.saturating_sub(24)..]
            );
        }
        if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'[') {
            let mut end = index + 2;
            while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b';') {
                end += 1;
            }
            if end < bytes.len() && matches!(bytes[end], b'H' | b'f' | b'r' | b'J') {
                panic!("append-only boot output addressed a screen cell: {output:?}");
            }
            index = end;
            continue;
        }
        index += 1;
    }
}

/// Visible width after dropping every CSI/SGR sequence.
fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for ch in chars.by_ref() {
                if ('@'..='~').contains(&ch) {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[test]
fn vt_grid_prompt_survives_up_restart_down_and_fork() {
    let lab = Lab::new();
    let root = lab.root.as_path();
    std::fs::write(
        root.join("Forkfile"),
        "# forkfile: 0.1\nimage box from python:3.13-slim {\n  service api {\n    run: sleep 3600\n  }\n}\n",
    )
    .expect("write the grid Forkfile");

    for (args, seed_row) in [
        (&["up"][..], 39usize),
        (&["restart"][..], 2),
        (&["down"][..], 39),
        (&["fork", "prompt-fork!"][..], 2),
    ] {
        let mut command = tty_command(root);
        command.args(args);
        let run = pty::run(command, Some((seed_row, SEED)), None);
        let row = run.screen.find_row(PROMPT).unwrap_or_else(|| {
            panic!(
                "`slipfork {args:?}` lost the seeded prompt\nscreen:\n{}\nbytes: {:?}",
                run.screen.text(),
                run.output
            )
        });
        assert_eq!(
            run.screen.row_text(row),
            PROMPT,
            "`slipfork {args:?}` mutated the prompt cells: {:?}",
            run.output
        );
        assert!(
            row <= 38,
            "`slipfork {args:?}` left the prompt inside the status line: {}",
            run.screen.text()
        );
        if seed_row == 2 {
            assert_eq!(
                row,
                2,
                "a prompt above the flow cursor must not move: {}",
                run.screen.text()
            );
        }
        // The shipped renderer must be append-only: no region, cursor probe, clear, or park.
        assert_append_only(&run.output, usize::from(pty::COLS));
        assert!(
            run.output.ends_with('\n'),
            "exit writes nothing after the final newline-terminated row: {:?}",
            run.output
        );
        if args[0] == "up" {
            let plain = strip_ansi(&run.output).replace('\r', "");
            assert!(
                plain.contains("error:"),
                "`slipfork up` must reach a typed error: {plain:?}"
            );
            assert_eq!(
                plain
                    .lines()
                    .filter(|line| line.starts_with("next:"))
                    .count(),
                1,
                "the typed error carries exactly one next: remedy: {plain:?}"
            );
        }
        // Off-Btrfs with no live world every verb must fail; a success here means the lab stopped
        // exercising the pane and the whole journey passed vacuously.
        assert!(
            !run.status.success(),
            "`slipfork {args:?}` unexpectedly succeeded; expected a typed failure (off-Btrfs or no \
             running world):\nscreen:\n{}\nbytes: {:?}",
            run.screen.text(),
            run.output
        );
    }
}

/// M3.149: the real `up` boot pane must reach a typed failure, never `capacity overflow` from a
/// full `[3/3]` bar. Reuses the seeded prompt PTY harness so the append-only status line really
/// runs.
#[test]
fn up_never_panics_the_progress_bar_and_ends_with_a_typed_failure() {
    let lab = Lab::new();
    let root = lab.root.as_path();
    std::fs::write(
        root.join("Forkfile"),
        "# forkfile: 0.1\nimage box from python:3.13-slim {\n  service api {\n    run: sleep 3600\n  }\n}\n",
    )
    .expect("write the progress Forkfile");

    let mut command = tty_command(root);
    command.args(["up"]);
    let run = pty::run(command, Some((39, SEED)), None);
    assert!(
        !run.output.contains("panicked at"),
        "`slipfork up` panicked instead of failing typed: {:?}",
        run.output
    );
    assert!(
        !run.status.success(),
        "`slipfork up` must fail typed off-Btrfs:\n{}\nbytes: {:?}",
        run.screen.text(),
        run.output
    );
    let text = run.output.replace('\r', "");
    assert!(
        text.contains("error:"),
        "the run ends with a typed error line: {text:?}"
    );
    assert!(
        text.contains("next:"),
        "the typed error carries its next: remedy: {text:?}"
    );
}

/// `__ingress --listen` on an already-bound port emits one plain `error:` + `next:` pair whose
/// strings are exactly the `--json` `{"error":{code,message,next}}` projection.
#[test]
fn an_ingress_bind_failure_is_plain_and_matches_its_json_projection() {
    let root = scratch();
    let holder = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a throwaway port");
    let port = holder.local_addr().expect("read the throwaway port").port();
    let address = format!("127.0.0.1:{port}");
    let mut command = command(root.as_path());
    command.args(["__ingress", "--listen", &address]);
    let run = pty::run(command, None, None);
    let text = run.output.replace('\r', "");
    assert!(
        !text.contains(ESCAPE),
        "a NO_COLOR PTY must stay pure: {text:?}"
    );
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(lines.len(), 2, "one error line and one remedy: {text:?}");
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("next:"))
            .count(),
        1,
        "exactly one next: line: {text:?}"
    );
    let error_line = lines[0];
    assert!(
        error_line.starts_with(&format!("error: cannot bind {address}: ")),
        "the bind error names the blocked address: {text:?}"
    );
    let next = "next: choose a free address or stop the process holding this port";
    assert_eq!(lines[1], next, "{text:?}");
    let message = error_line
        .strip_prefix("error: ")
        .expect("the human line carries the error prefix");
    let remedy = next.strip_prefix("next: ").expect("the remedy prefix");
    let terminal = Terminal {
        cols: 80,
        rows: 0,
        mode: UiMode::Json,
        stdin_tty: false,
    };
    let renderer = ReportRenderer::new(terminal, WallZone::fixed(0).expect("a utc fixture zone"));
    let mut json = Vec::new();
    let mut json_err = Vec::new();
    renderer
        .emit_to(
            &Report::failure(Failure::new("ingress-bind", message, remedy)),
            &mut json,
            &mut json_err,
        )
        .expect("render the error projection");
    assert!(json_err.is_empty(), "a json failure stays on stdout");
    let projection: Value = serde_json::from_slice(&json).expect("one json object");
    assert_eq!(projection["error"]["code"], "ingress-bind");
    assert_eq!(projection["error"]["message"], message);
    assert_eq!(projection["error"]["next"], remedy);
    assert!(
        !run.status.success(),
        "a bind failure exits non-zero: {text:?}"
    );
    drop(holder);
    std::fs::remove_dir_all(&root).expect("remove the scratch lab tree");
}
