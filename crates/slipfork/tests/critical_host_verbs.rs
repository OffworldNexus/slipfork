//! Protected lab journey for the host verbs: `env`, `secret`, `open`, and `auth`.
//!
//! Real `0600` vault under an injected state directory, a real `slipfork __daemon` socket, real
//! `/var/tmp` executables, and the real `127.0.0.1:0` OSPP reference server — no mocks anywhere.

#[path = "support/reference_ospp.rs"]
#[allow(dead_code)]
mod reference_ospp;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reference_ospp::{ReferenceOsppServer, ReferenceServerConfig};
use serde_json::Value;

const FORKFILE: &str = "# forkfile: 0.1\n\nenv app {\n    LOG_LEVEL = \"info\"\n    SECRET_KEY = secret(SECRET_KEY)\n}\n";
const SECRET: &str = "s3cr3t-value";
const ANSI: &str = "\u{1b}[";

fn scratch(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_nanos();
    let root = PathBuf::from("/var/tmp").join(format!(
        "slipfork-host-{prefix}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("create /var/tmp scratch");
    root
}

struct Lab {
    root: PathBuf,
    project: PathBuf,
}

impl Lab {
    fn new(prefix: &str) -> Self {
        let root = scratch(prefix);
        let project = root.join("project");
        fs::create_dir_all(&project).expect("create project");
        fs::write(project.join("Forkfile"), FORKFILE).expect("write Forkfile");
        fs::create_dir_all(root.join("state")).expect("state root");
        fs::create_dir_all(root.join("run")).expect("run root");
        Self { root, project }
    }

    fn state(&self) -> PathBuf {
        self.root.join("state/slipfork")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_slipfork"));
        command
            .args(args)
            .current_dir(&self.project)
            .env("NO_COLOR", "1")
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("HOME", self.root.join("home"))
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("SSH_CONNECTION")
            .env_remove("SSH_TTY");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run slipfork")
    }

    fn run_with_stdin(&self, args: &[&str], stdin: &str) -> Output {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn slipfork");
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(stdin.as_bytes())
            .expect("write secret to stdin");
        child.wait_with_output().expect("wait for slipfork")
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        shutdown(&self.root);
        kill_daemons(&self.root);
    }
}

fn shutdown(root: &Path) {
    let socket = root.join("run/daemon.sock");
    if let Ok(mut stream) = UnixStream::connect(&socket) {
        let _ =
            stream.write_all(b"{\"v\":1,\"id\":\"test\",\"method\":\"shutdown\",\"params\":{}}\n");
        let _ = stream.flush();
        let mut line = String::new();
        let _ = BufReader::new(stream).read_line(&mut line);
        let deadline = Instant::now() + Duration::from_secs(5);
        while socket.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Kills any `__daemon` still carrying this lab's runtime directory, so no survivor leaks.
fn kill_daemons(root: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let scope = root.as_os_str().as_bytes();
    for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        if !cmdline.windows(b"__daemon".len()).any(|w| w == b"__daemon") {
            continue;
        }
        let Ok(environ) = fs::read(format!("/proc/{pid}/environ")) else {
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

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn next_lines(output: &Output) -> usize {
    text(output)
        .lines()
        .filter(|line| line.starts_with("next: "))
        .count()
}

fn assert_clean(output: &Output) {
    assert!(
        !text(output).contains(ANSI),
        "ANSI leaked: {}",
        text(output)
    );
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

#[test]
fn secrets_and_env_round_trip_through_the_0600_vault() {
    let lab = Lab::new("secret");
    let listed = lab.run(&["secret", "list", "--json"]);
    assert!(listed.status.success(), "{}", text(&listed));
    assert_clean(&listed);
    let value: Value = serde_json::from_slice(&listed.stdout).expect("json list");
    assert_eq!(value["app"]["SECRET_KEY"], "<unset>");

    let set = lab.run_with_stdin(&["secret", "set", "app.SECRET_KEY"], &format!("{SECRET}\n"));
    assert!(set.status.success(), "{}", text(&set));
    assert!(text(&set).contains("Set app.SECRET_KEY"), "{}", text(&set));
    assert!(!text(&set).contains(SECRET), "`set` echoed the value");

    let redacted = lab.run(&["secret", "list"]);
    assert!(redacted.status.success(), "{}", text(&redacted));
    assert!(
        text(&redacted).contains("[REDACTED]"),
        "{}",
        text(&redacted)
    );
    assert!(!text(&redacted).contains(SECRET), "list leaked the value");
    assert_clean(&redacted);

    let vault = lab.state().join("secrets.json");
    assert_eq!(mode_of(&vault), 0o600);
    assert!(fs::read_to_string(&vault).expect("vault").contains(SECRET));

    let json = lab.run(&["env", "--json"]);
    assert!(json.status.success(), "{}", text(&json));
    assert_clean(&json);
    let value: Value = serde_json::from_slice(&json.stdout).expect("json env");
    assert_eq!(value["app"]["SECRET_KEY"], "[REDACTED]");
    assert_eq!(value["app"]["LOG_LEVEL"], "info");

    let refused = lab.run(&["env", "--reveal"]);
    assert!(!refused.status.success(), "`--reveal` must fail on a pipe");
    assert!(!text(&refused).contains(SECRET));
    assert_eq!(next_lines(&refused), 1, "{}", text(&refused));
    assert_clean(&refused);

    let human = lab.run(&["env"]);
    assert!(human.status.success(), "{}", text(&human));
    assert!(text(&human).contains("[REDACTED]"), "{}", text(&human));
    assert!(!text(&human).contains(SECRET));

    let unset = lab.run(&["secret", "unset", "app.SECRET_KEY"]);
    assert!(unset.status.success(), "{}", text(&unset));
    assert!(text(&unset).contains("Unset app.SECRET_KEY"));

    let again = lab.run(&["secret", "unset", "app.SECRET_KEY"]);
    assert!(!again.status.success());
    assert!(text(&again).contains("is not set"), "{}", text(&again));
    assert_eq!(next_lines(&again), 1, "{}", text(&again));

    let empty = lab.run(&["secret", "list", "--json"]);
    let value: Value = serde_json::from_slice(&empty.stdout).expect("json list");
    assert_eq!(value["app"]["SECRET_KEY"], "<unset>");
}

#[test]
fn open_prints_the_url_when_headless_and_runs_a_real_opener_when_displayed() {
    let lab = Lab::new("open");
    let headless = lab
        .command(&["open"])
        .env("SSH_TTY", "/dev/pts/0")
        .output()
        .expect("headless open");
    assert!(headless.status.success(), "{}", text(&headless));
    assert!(
        text(&headless).contains("http://project.localhost"),
        "{}",
        text(&headless)
    );
    assert_eq!(next_lines(&headless), 1, "{}", text(&headless));
    assert_clean(&headless);

    let bin = lab.root.join("bin");
    fs::create_dir_all(&bin).expect("create bin");
    let log = lab.root.join("opened.txt");
    let opener = bin.join("xdg-open");
    fs::write(
        &opener,
        format!("#!/bin/sh\nprintf '%s' \"$1\" > '{}'\n", log.display()),
    )
    .expect("write opener");
    fs::set_permissions(&opener, fs::Permissions::from_mode(0o755)).expect("chmod opener");

    let displayed = lab
        .command(&["open"])
        .env("DISPLAY", ":0")
        .env("PATH", &bin)
        .output()
        .expect("displayed open");
    assert!(displayed.status.success(), "{}", text(&displayed));
    assert_eq!(
        fs::read_to_string(&log).expect("opener ran"),
        "http://project.localhost"
    );
}

#[test]
fn auth_pairs_through_the_loopback_reference_server() {
    let lab = Lab::new("auth");
    let server = ReferenceOsppServer::start(ReferenceServerConfig::default());
    let url = server.endpoint().as_str().to_owned();
    let mut child = lab
        .command(&["auth", &url])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn auth");

    let deadline = Instant::now() + Duration::from_secs(10);
    let handle = loop {
        if let Some(record) = server.state().pairings.first() {
            break record.handle.clone();
        }
        assert!(
            child.try_wait().expect("poll auth").is_none(),
            "auth exited before pairing"
        );
        assert!(Instant::now() < deadline, "the pairing never started");
        thread::sleep(Duration::from_millis(20));
    };
    server.approve_pairing(&handle);
    let output = child.wait_with_output().expect("wait for auth");
    assert!(output.status.success(), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("alice"), "{stdout}");
    assert!(stdout.contains("dev-laptop"), "{stdout}");
    assert!(stdout.contains(&url), "{stdout}");
    assert!(stdout.contains("next: slipfork up"), "{stdout}");
    assert_clean(&output);

    let state = lab.state();
    let device = state.join("device-key.json");
    let record = state.join("ospp.json");
    assert_eq!(mode_of(&device), 0o600);
    assert_eq!(mode_of(&record), 0o600);
    let key = fs::read_to_string(&device).expect("device key");
    let record_body = fs::read_to_string(&record).expect("pairing record");
    assert!(record_body.contains(&url), "{record_body}");
    assert!(
        !record_body.contains(key.trim()),
        "ospp.json leaked key material"
    );
    server.shutdown();
}

#[test]
fn auth_without_an_endpoint_fails_with_one_next_line() {
    let lab = Lab::new("auth-missing");
    let output = lab.run(&["auth"]);
    assert!(!output.status.success());
    assert!(
        text(&output).contains("this daemon has no OSPP endpoint"),
        "{}",
        text(&output)
    );
    assert_eq!(next_lines(&output), 1, "{}", text(&output));
    assert_clean(&output);
}
