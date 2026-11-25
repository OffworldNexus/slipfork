//! Live lab journey: a real world's services, ports, and routes through `status` and `ps`, plus the
//! append-only PTY boot narrative, orphaned-pod reconcile, and zero-escape plain paths.
//!
//! Builds a real Btrfs subvolume world from a `Forkfile`, boots it with `slipfork up` on a real
//! `openpty` PTY, and asserts the shipped append-only contract: completed rows are ordinary
//! newline-terminated scrollback lines, only the one status line uses `\r` + `ESC[2K`, and no
//! `DECSTBM`/`ESC[r`/`ESC[{row};1H`/DSR/alt-screen bytes exist anywhere. A second journey edits the
//! `Forkfile`, proves the undeclared pod is yellow `orphaned`, then reaps it with `slipfork up`.
//! The lab is opt-in: without `SLIPFORK_TEST_ROOT` the test fails closed with the exact remedy.

use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[path = "support/pty.rs"]
mod pty;

use slipfork_world::{
    BtrfsEngine, Dirtiness, STORE_DIR, StoreLayout, WorldEngine, WorldName, is_btrfs_path,
};

const SUITE_FORKFILE: &str = "# forkfile: 0.1\n\
postgres pg {\n  version 18\n}\n\
redis cache\n\
s3 uploads {\n  policy public-read\n  cdn assets-cdn\n}\n\
image box from python:3.13-slim {\n\
\x20 service alpha {\n    port 8101\n    route /alpha\n    run: python3 -m http.server 8101\n  }\n\
\x20 service beta {\n    port 8102\n    route /beta\n    run: python3 -m http.server 8102\n  }\n\
}\n";

const HOOK_FORKFILE: &str = "# forkfile: 0.1\n\
postgres pg {\n  version 18\n}\n\
redis cache\n\
s3 uploads {\n  policy public-read\n  cdn assets-cdn\n}\n\
image box from python:3.13-slim {\n  pre-deploy: sh -c 'echo rolling-tail-1; sleep 1.4; echo rolling-tail-2; : 01234567890123456789012345678901234567890123456789'\n  service worker {\n    run: sleep 3600\n  }\n}\n";

const FAIL_FORKFILE: &str = "# forkfile: 0.1\n\
image box from python:3.13-slim {\n  pre-deploy: sh -c 'exit 3'\n  service worker {\n    run: sleep 3600\n  }\n}\n";

/// A portless service must survive the 650ms boot window before dying. Its first launch creates
/// `/tmp/once` and exits 7 after 0.8s, so the first `up` boots; the restart reuses the pod rootfs,
/// sees the marker, and dies in milliseconds, keeping the strict failure verdict deterministic.
const DEAD_SERVICE_FORKFILE: &str = "# forkfile: 0.1\n\
image box from python:3.13-slim {\n\
\x20 service web {\n    run: sh -c 'if [ -e /tmp/once ]; then exit 7; fi; touch /tmp/once; sleep 0.8; exit 7'\n  }\n\
}\n";

const ONE_SERVICE: &str = "# forkfile: 0.1\n\
postgres pg {\n  version 18\n}\n\
redis cache\n\
s3 uploads {\n  policy public-read\n  cdn assets-cdn\n}\n\
image box from python:3.13-slim {\n\
\x20 service alpha {\n    port 8101\n    route /alpha\n    run: python3 -m http.server 8101\n  }\n\
}\n";

const BUILD_FORKFILE: &str = "# forkfile: 0.1\n\
image web . {\n  dockerfile Dockerfile\n\
\x20 service api {\n    port 8103\n    run: python3 -m http.server 8103\n  }\n}\n";

const FAIL_SERVICE_FORKFILE: &str = "# forkfile: 0.1\n\
image box from python:3.13-slim {\n\
\x20 service web {\n    port 8104\n    run: sh -c 'echo starting web; echo boom: missing DATABASE_URL >&2; exit 7'\n  }\n}\n";

const BRAILLE: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

/// Isolated daemon runtime (`tmpfs`, short `< 107`-byte `AF_UNIX` path) and state directory
/// (`SLIPFORK_TEST_ROOT` disk) so the suite never shares `~/.local/state/slipfork/worlds.json`
/// with a host user daemon.
#[derive(Clone)]
struct LabEnv {
    runtime: PathBuf,
    state: PathBuf,
}

impl LabEnv {
    fn isolated(root: &Path) -> Self {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/dev/shm/slipfork-live-runtime"));
        fs::create_dir_all(&base).expect("create the live runtime base");
        let base = fs::canonicalize(&base).unwrap_or(base);
        let pid = std::process::id();
        let runtime = base.join(format!("r{pid}"));
        let state = root.join(format!(".state-{pid}"));
        fs::create_dir_all(&runtime).expect("create the isolated runtime directory");
        fs::create_dir_all(&state).expect("create the isolated state directory");
        Self { runtime, state }
    }
}

fn test_root() -> PathBuf {
    let Some(root) = std::env::var_os("SLIPFORK_TEST_ROOT").filter(|root| !root.is_empty()) else {
        panic!(
            "SLIPFORK_TEST_ROOT is unset or empty\nnext: SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test"
        )
    };
    let root = PathBuf::from(root);
    if let Err(source) = fs::create_dir_all(&root) {
        panic!(
            "fail closed: cannot create `{}`: {source}\nnext: sudo chown -R \"$(id -u):$(id -g)\" {}",
            root.display(),
            root.display()
        );
    }
    let root = fs::canonicalize(&root).unwrap_or(root);
    assert!(
        is_btrfs_path(&root).unwrap_or(false),
        "fail closed: `{}` is not on Btrfs.\nnext: SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test",
        root.display()
    );
    root
}

/// A `{pid}-{nanos:x}` suffix so two live tests never share a project or world while keeping
/// `AF_UNIX` `control.sock` paths well under 107 bytes.
fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock reads after the epoch")
        .as_nanos();
    format!("{}-{:08x}", std::process::id(), nanos as u32)
}

/// Fails closed when a fork world's `control.sock` would overflow `sun_path`.
fn assert_socket_fits(env: &LabEnv, world: &str) {
    let path = env
        .runtime
        .join("slipfork/pods")
        .join(world)
        .join("control.sock");
    let len = path.as_os_str().len();
    assert!(
        len < 108,
        "control.sock path is {len} bytes (AF_UNIX limit is 108): {}\nnext: shorten XDG_RUNTIME_DIR so the path fits",
        path.display()
    );
}

fn mount_store_for(path: &Path) -> PathBuf {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").expect("read /proc/self/mountinfo");
    let mut best: Option<(PathBuf, usize)> = None;
    for line in mountinfo.lines() {
        let Some((head, _)) = line.split_once(" - ") else {
            continue;
        };
        let Some(raw) = head.split_ascii_whitespace().nth(4) else {
            continue;
        };
        let bytes = raw.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\'
                && i + 3 < bytes.len()
                && bytes[i + 1..=i + 3]
                    .iter()
                    .all(|b| (b'0'..=b'7').contains(b))
            {
                out.push(
                    ((bytes[i + 1] - b'0') << 6)
                        | ((bytes[i + 2] - b'0') << 3)
                        | (bytes[i + 3] - b'0'),
                );
                i += 4;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        let mount = PathBuf::from(OsString::from_vec(out));
        if canonical.starts_with(&mount) {
            let depth = mount.components().count();
            if best.as_ref().is_none_or(|(_, prev)| depth >= *prev) {
                best = Some((mount, depth));
            }
        }
    }
    best.map_or_else(|| PathBuf::from("/"), |(mount, _)| mount)
        .join(STORE_DIR)
}

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_slipfork")
}

/// Longest any CLI invocation may take before the harness fails with a remedy.
const RUN_TIMEOUT: Duration = Duration::from_secs(240);

fn run(project: &Path, env: &LabEnv, args: &[&str]) -> Output {
    let mut child = Command::new(binary())
        .args(args)
        .current_dir(project)
        .env("XDG_RUNTIME_DIR", &env.runtime)
        .env("XDG_STATE_HOME", &env.state)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the slipfork binary must run");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut stream = stdout;
        let _ = stream.read_to_end(&mut bytes);
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut stream = stderr;
        let _ = stream.read_to_end(&mut bytes);
        bytes
    });
    let status = wait_bounded(&mut child, args);
    Output {
        status,
        stdout: out.join().expect("stdout reader"),
        stderr: err.join().expect("stderr reader"),
    }
}

/// Waits for `child`, killing it at [`RUN_TIMEOUT`] and failing with the log remedy.
fn wait_bounded(child: &mut std::process::Child, args: &[&str]) -> std::process::ExitStatus {
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {}
            Err(error) => panic!("cannot wait for `slipfork {args:?}`: {error}"),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "`slipfork {args:?}` did not finish within {}s\nnext: inspect `.slipfork/supervisor.log` and daemon.log",
                RUN_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Polls `check` until it holds or `budget` elapses; a timeout names the exact remedy.
fn wait_until(what: &str, budget: Duration, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if check() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out after {}s waiting for {what}", budget.as_secs());
}

struct PtyRun {
    output: String,
    status: std::process::ExitStatus,
    screen: pty::Screen,
}

/// Runs `args` under a real `openpty` PTY (120x40 unless resized); `script` is not needed.
fn run_pty(
    project: &Path,
    env: &LabEnv,
    args: &[&str],
    envs: &[(&str, &str)],
    resize: Option<(Duration, u16, u16)>,
) -> PtyRun {
    run_pty_seeded(project, env, args, envs, resize, None)
}

/// Like [`run_pty`], but pre-seeds a shell prompt row; the harness still answers a `DSR` query so
/// the suite can prove the shipped pane never asks for one.
fn run_pty_seeded(
    project: &Path,
    env: &LabEnv,
    args: &[&str],
    envs: &[(&str, &str)],
    resize: Option<(Duration, u16, u16)>,
    seed: Option<(usize, &'static str)>,
) -> PtyRun {
    let mut command = Command::new(binary());
    command
        .args(args)
        .current_dir(project)
        .env("XDG_RUNTIME_DIR", &env.runtime)
        .env("XDG_STATE_HOME", &env.state)
        .env_remove("NO_COLOR")
        .env("TERM", "xterm-256color");
    for (key, value) in envs {
        command.env(key, value);
    }
    let run = pty::run(command, seed, resize);
    PtyRun {
        output: run.output,
        status: run.status,
        screen: run.screen,
    }
}
/// Every maximal run of panel cells, so a sheared or multi-width bar cannot pass unnoticed.
fn bar_runs(text: &str) -> Vec<usize> {
    let mut runs = Vec::new();
    let mut run = 0;
    for character in text.chars() {
        if matches!(character, '█' | '░') {
            run += 1;
        } else if run > 0 {
            runs.push(run);
            run = 0;
        }
    }
    if run > 0 {
        runs.push(run);
    }
    runs
}

/// Every complete `[<24 cells>]  <done>/<total>` draw: `(filled, done, total)`.
fn panel_fills(text: &str) -> Vec<(usize, usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut draws = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '[' {
            let start = index + 1;
            let end = start + BAR_CELLS;
            if end < chars.len()
                && chars[start..end]
                    .iter()
                    .all(|cell| matches!(cell, '█' | '░'))
                && chars[end] == ']'
            {
                let filled = chars[start..end]
                    .iter()
                    .filter(|cell| **cell == '█')
                    .count();
                let rest: String = chars[end + 1..].iter().take(16).collect();
                if let Some((done, total)) = counter(&rest)
                    && total > 0
                {
                    draws.push((filled, done, total));
                }
            }
        }
        index += 1;
    }
    draws
}

/// Parses `  <done>/<total>` after a panel bar.
fn counter(rest: &str) -> Option<(usize, usize)> {
    let (counts, _) = rest.trim_start().split_once(' ')?;
    let (done, total) = counts.split_once('/')?;
    Some((done.parse().ok()?, total.parse().ok()?))
}

/// The one width every panel draw must occupy (`specs/02` §3).
const BAR_CELLS: usize = 24;

/// Removes every escape sequence so rows can be asserted exactly as a user reads them.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            out.push(character);
            continue;
        }
        if chars.clone().next() == Some('[') {
            let _ = chars.next();
            for byte in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&byte) {
                    break;
                }
            }
        } else {
            let _ = chars.next();
        }
    }
    out
}

/// Asserts the shipped append-only boot contract over one raw colour-PTY capture.
///
/// One-for-one replacement for the removed widget pins (`DECSTBM`, `ESC[r`, `ESC[{row};1H`, DSR,
/// and the park): zero cursor addressing (`ESC[r`, `ESC[<row>;<col>H|f`, `ESC[J`, `ESC c`,
/// alt-screen, DSR); a bare `\r` is only ever the `\r\x1b[2K` status-line prefix (or a PTY `\r\n`);
/// and the capture ends on a newline, so every completed row is a real scrollback line.
fn assert_append_only(output: &str, surface: &str) {
    for forbidden in [
        "\u{1b}[r",
        "\u{1b}[2J",
        "\u{1b}[J",
        "\u{1b}[K",
        "\u{1b}c",
        "\u{1b}[?1049",
        "\u{1b}[?1047",
        "\u{1b}[?47",
        "\u{1b}[6n",
    ] {
        assert!(
            !output.contains(forbidden),
            "{surface} carried the removed widget escape {forbidden:?}: {output:?}"
        );
    }
    // The only legal clear-line sequence is the status prefix; a standalone `ESC[2K` is not.
    let mut search = 0;
    while let Some(position) = output[search..].find("\u{1b}[2K") {
        let absolute = search + position;
        assert!(
            absolute > 0 && output.as_bytes()[absolute - 1] == b'\r',
            "{surface} wrote `ESC[2K` outside the one `\\r\\x1b[2K` status prefix: {output:?}"
        );
        search = absolute + 4;
    }
    let bytes = output.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\r' {
            assert!(
                output[index + 1..].starts_with('\n')
                    || output[index + 1..].starts_with("\u{1b}[2K"),
                "{surface} wrote a bare `\\r` outside the one status line: {:?}",
                &output[index..]
            );
            // Skip the verified status prefix so its `K` is never read as an addressed cell.
            index += if output[index + 1..].starts_with('\n') {
                1
            } else {
                5
            };
            continue;
        }
        if bytes[index] == 0x1b && matches!(bytes.get(index + 1), Some(b'7') | Some(b'8')) {
            panic!("{surface} saved/restored the cursor: {output:?}");
        }
        if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'[') {
            let mut end = index + 2;
            while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b';') {
                end += 1;
            }
            if end < bytes.len()
                && matches!(
                    bytes[end],
                    b'A' | b'B'
                        | b'C'
                        | b'D'
                        | b'E'
                        | b'F'
                        | b'G'
                        | b'H'
                        | b'f'
                        | b'r'
                        | b'J'
                        | b'K'
                        | b'S'
                        | b'T'
                        | b'L'
                        | b'M'
                )
            {
                panic!("{surface} addressed a screen cell: {output:?}");
            }
            index = end;
            continue;
        }
        index += 1;
    }
    assert!(
        output.ends_with('\n'),
        "{surface} must finish every completed row with a newline: {output:?}"
    );
}

/// The plain visible text of every status frame in `text` (the `\r\x1b[2K`-owned lines).
fn status_frames(text: &str) -> Vec<String> {
    text.split("\r\u{1b}[2K")
        .skip(1)
        .map(|frame| plain(frame.lines().next().unwrap_or_default()))
        .collect()
}

/// The capture with its status frames and SGR removed: exactly the completed scrollback rows.
fn completed_rows(output: &str) -> String {
    let mut out = String::new();
    let mut rest = output;
    while let Some(position) = rest.find("\r\u{1b}[2K") {
        out.push_str(&rest[..position]);
        let after = &rest[position + 5..];
        match (after.find('\r'), after.find('\n')) {
            (Some(cr), _) if after[cr..].starts_with("\r\n") => {
                // A completed row shares the same clear prefix before its `\r\n`.
                out.push_str(&after[..cr]);
                out.push('\n');
                rest = &after[cr + 2..];
            }
            (Some(cr), _) => rest = &after[cr..],
            (_, Some(nl)) => {
                out.push_str(&after[..nl]);
                out.push('\n');
                rest = &after[nl + 1..];
            }
            (None, None) => rest = "",
        }
    }
    out.push_str(rest);
    plain(&out).replace("\r\n", "\n").replace('\r', "\n")
}

/// Each needle appears exactly once, in the given order, in the completed rows.
fn assert_rows_once_in_order(text: &str, needles: &[String], surface: &str) {
    let mut cursor = 0;
    for needle in needles {
        assert_eq!(
            text.matches(needle).count(),
            1,
            "{surface}: {needle:?} must appear exactly once:\n{text}"
        );
        let found = text[cursor..]
            .find(needle)
            .unwrap_or_else(|| panic!("{surface}: {needle:?} is out of order:\n{text}"));
        cursor += found + needle.len();
    }
}

/// A stopped header renders no domain or support sub-rows beneath it.
fn assert_stopped_rows_have_no_sub_rows(human: &str) {
    let lines: Vec<&str> = human.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        if line.starts_with("○ ")
            && let Some(next) = lines.get(index + 1)
        {
            assert!(
                next.trim().is_empty() || next.starts_with('○') || next.starts_with('●'),
                "a stopped row must render no sub-rows:\n{human}"
            );
        }
    }
}

/// Stops **this world**, shuts down **this isolated test daemon**, and deletes **this project** on
/// every exit path without ever touching the host user's `~/.local/state/slipfork` worlds.
struct WorldGuard {
    project: PathBuf,
    env: LabEnv,
    store_root: PathBuf,
    world: String,
    /// Set when step 1 fails: leave the project on disk for the lab diagnosis.
    keep: bool,
}

impl Drop for WorldGuard {
    fn drop(&mut self) {
        let socket = self.env.runtime.join("daemon.sock");
        if socket.exists() {
            let _ = run_quiet(&self.project, &self.env, &["down"]);
            shutdown_isolated_daemon(&socket);
        }
        if self.keep {
            return;
        }
        let engine = BtrfsEngine::new(StoreLayout::at(&self.store_root));
        if let Ok(world) = WorldName::parse(&self.world) {
            let _ = engine.drop_world(&world);
        }
        let _ = engine.destroy(&self.project, Dirtiness::Clean, true);
        let _ = fs::remove_dir_all(&self.project);
        let _ = fs::remove_dir_all(&self.env.runtime);
        let _ = fs::remove_dir_all(&self.env.state);
    }
}

fn shutdown_isolated_daemon(socket: &Path) {
    if let Ok(mut stream) = UnixStream::connect(socket) {
        let _ = stream
            .write_all(b"{\"v\":1,\"id\":\"guard-bye\",\"method\":\"shutdown\",\"params\":{}}\n");
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut reply = [0_u8; 128];
        let _ = stream.read(&mut reply);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// One best-effort CLI run with a short budget; the guard must never hang or panic.
fn run_quiet(project: &Path, env: &LabEnv, args: &[&str]) -> bool {
    let Ok(mut child) = Command::new(binary())
        .args(args)
        .current_dir(project)
        .env("XDG_RUNTIME_DIR", &env.runtime)
        .env("XDG_STATE_HOME", &env.state)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => return false,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn prepare(project: &Path, store_root: &Path, world: &str, forkfile: &str) {
    let engine = BtrfsEngine::new(StoreLayout::at(store_root));
    if let Ok(world) = WorldName::parse(world) {
        let _ = engine.drop_world(&world);
    }
    let _ = engine.destroy(project, Dirtiness::Clean, true);
    let _ = fs::remove_dir_all(project);
    fs::create_dir_all(project).expect("create the live project");
    fs::write(project.join("Forkfile"), forkfile).expect("write the live Forkfile");
}

/// Diagnostic for a failed lifecycle run: both pipes plus the tail of the isolated daemon log.
fn failure_detail(env: &LabEnv, output: &Output) -> String {
    let log = fs::read_to_string(env.state.join("slipfork/daemon.log"))
        .unwrap_or_else(|error| format!("<no daemon.log: {error}>"));
    let mut tail: Vec<&str> = log.lines().rev().take(20).collect();
    tail.reverse();
    format!(
        "stdout: {:?}\nstderr: {:?}\ndaemon.log tail:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        tail.join("\n")
    )
}

fn services(world: &serde_json::Value) -> Vec<serde_json::Value> {
    world["services"].as_array().cloned().unwrap_or_default()
}

/// Depth-first lookup of a `ps` world/fork row by world name or fork name.
fn find_row<'a>(worlds: &'a [serde_json::Value], name: &str) -> Option<&'a serde_json::Value> {
    for world in worlds {
        if world["world"] == name || world["fork"] == name {
            return Some(world);
        }
        if let Some(children) = world["forks"].as_array()
            && let Some(found) = find_row(children, name)
        {
            return Some(found);
        }
    }
    None
}

#[test]
fn ansi_split_bar_halves_measure_as_one_24_cell_run() {
    let styled = "\u{1b}[1;36m██████████████\u{1b}[0m\u{1b}[38;5;244m░░░░░░░░░░\u{1b}[0m";
    let panel =
        format!("\u{1b}[40;1H\u{1b}[2K⠋ [{styled}]  6/10  60% · Starting api… · 1.2s\u{1b}[K");
    let readable = plain(&panel);
    assert_eq!(bar_runs(&readable), vec![24]);
    assert_eq!(panel_fills(&readable), vec![(14, 6, 10)]);

    // The very first draw precedes the first `started` frame: `0/0` has no fill ratio to check.
    let empty = format!(
        "\u{1b}[40;1H\u{1b}[2K⠋ [{}]  0/0  0% · Booting demo · 0.0s",
        "░".repeat(24)
    );
    assert_eq!(panel_fills(&plain(&empty)), Vec::new());
}

#[test]
fn pty_helper_closes_the_slave_and_never_hangs() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock reads after the epoch")
        .as_nanos();
    let dir = PathBuf::from("/var/tmp").join(format!(
        "slipfork-pty-helper-{}-{nanos}",
        std::process::id()
    ));
    let env = LabEnv {
        runtime: dir.join("run"),
        state: dir.join("state"),
    };
    fs::create_dir_all(&env.runtime).expect("create the PTY scratch runtime");
    fs::create_dir_all(&env.state).expect("create the PTY scratch state");
    let began = Instant::now();
    let out = run_pty(&dir, &env, &["--help"], &[], None);
    let elapsed = began.elapsed();
    assert!(
        out.status.success(),
        "`slipfork --help` on a PTY must exit cleanly: {:?}",
        out.output
    );
    assert!(
        out.output.contains("slipfork"),
        "the PTY output must be captured: {:?}",
        out.output
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "the PTY reader must stop as soon as the child exits, took {elapsed:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn live_boots_once_paints_the_panel_reconciles_orphans_and_replays_logs() {
    let root = test_root();
    let store_root = mount_store_for(&root);
    let env = LabEnv::isolated(&root);
    let suffix = unique_suffix();
    let project_name = format!("live-{suffix}");
    let world_id = project_name.clone();
    let project = root.join(&project_name);
    prepare(&project, &store_root, &world_id, SUITE_FORKFILE);
    let mut guard = WorldGuard {
        project: project.clone(),
        env: env.clone(),
        store_root,
        world: world_id.clone(),
        keep: false,
    };
    assert_socket_fits(&env, &format!("feat-stopped--{world_id}"));

    // 1. One rich colour-PTY boot: append-only rows in scrollback, one in-place status line,
    //    24-cell panel maths, one row per task. The DECSTBM/`ESC[{row};1H` widget is gone, so the
    //    substitute invariants are "zero cursor addressing", "only `\r\x1b[2K` owns a line", and
    //    "every completed row is newline-terminated", all asserted by `assert_append_only`.
    let boot = run_pty(&project, &env, &["up"], &[], None);
    if !boot.status.success() {
        guard.keep = true;
        let supervisor = fs::read_to_string(project.join(".slipfork/supervisor.log"))
            .unwrap_or_else(|error| format!("<no supervisor.log: {error}>"));
        panic!(
            "PTY `slipfork up` failed\nstdout: {:?}\nstderr: {:?}\nsupervisor.log:\n{supervisor}",
            boot.output, boot.output
        );
    }
    assert_append_only(&boot.output, "colour-PTY `up`");
    assert!(
        boot.output.matches("\r\u{1b}[2K").count() > 1,
        "a colour PTY owns the one repainted status line: {:?}",
        boot.output
    );
    assert!(
        BRAILLE.chars().any(|glyph| boot.output.contains(glyph)),
        "a colour PTY must show the Braille spinner: {:?}",
        boot.output
    );
    let readable = plain(&boot.output);
    let runs = bar_runs(&readable);
    assert!(
        !runs.is_empty(),
        "a colour PTY must show the panel bar: {:?}",
        boot.output
    );
    assert!(
        runs.iter().all(|cells| *cells == BAR_CELLS),
        "every panel draw is exactly {BAR_CELLS} cells: {runs:?}"
    );
    let fills = panel_fills(&readable);
    assert!(
        !fills.is_empty(),
        "at least one complete panel draw must be measured: {readable}"
    );
    for (filled, done, total) in fills {
        let low = (done * BAR_CELLS + total / 2) / total;
        let high = ((done + 1).min(total) * BAR_CELLS + total / 2) / total;
        assert!(
            done <= total && filled >= low && filled <= high,
            "filled cells {filled} must lie within outer slot [{done}/{total}..={}/{total}] = [{low}..={high}]: {readable}",
            (done + 1).min(total)
        );
    }
    let boot_rows: Vec<&str> = readable
        .split(['\n', '\r'])
        .filter(|row| row.contains('✓'))
        .collect();
    for resource in ["pg", "cache", "uploads"] {
        assert_eq!(
            boot_rows
                .iter()
                .filter(|row| {
                    row.contains("resource")
                        && row.contains("ready")
                        && row.split_whitespace().any(|cell| cell == resource)
                })
                .count(),
            1,
            "the boot must print exactly one finished `{resource}` row: {readable}"
        );
    }
    assert!(
        readable.contains("alpha") && readable.contains("beta"),
        "both services must render a finished row: {readable}"
    );
    assert!(
        readable
            .split(['\n', '\r'])
            .any(|row| row.contains("python:3.13-slim") && row.contains("from"))
            && !readable.contains("registry-1.docker.io"),
        "`from` row must carry the reference as written, never normalized: {readable}"
    );
    for forbidden in [
        "sha256",
        "sealed",
        "reusing cache",
        "crun",
        "Failed to",
        "dockerfile",
    ] {
        assert!(
            !readable.contains(forbidden),
            "engine bookkeeping `{forbidden}` must never reach the terminal without `-v`: {readable}"
        );
    }
    // Completed rows are ordinary scrollback lines, printed exactly once each. The old pane
    // asserted `ESC[2K` + `ESC[r` before the verdict; the substitute proves the one status line is
    // erased in place (`\r\x1b[2K`) and every finished row, including the verdict, ends in `\n`.
    let rows = completed_rows(&boot.output);
    let verdict_rows: Vec<&str> = rows
        .lines()
        .filter(|line| line.starts_with("✓ ["))
        .collect();
    assert!(
        !verdict_rows.is_empty(),
        "the boot must settle at least one completed outer row: {rows}"
    );
    let unique: std::collections::BTreeSet<&str> = verdict_rows.iter().copied().collect();
    assert_eq!(
        unique.len(),
        verdict_rows.len(),
        "every completed boot row prints exactly once: {rows}"
    );
    let mut slots: Vec<usize> = verdict_rows
        .iter()
        .map(|row| {
            let open = row.find('[').unwrap_or_else(|| panic!("no counter: {row}"));
            let close = row[open..]
                .find('/')
                .unwrap_or_else(|| panic!("no counter: {row}"));
            row[open + 1..open + close]
                .trim()
                .parse()
                .unwrap_or_else(|_| panic!("no counter: {row}"))
        })
        .collect();
    slots.sort_unstable();
    assert_eq!(
        slots,
        (1..=verdict_rows.len()).collect::<Vec<_>>(),
        "the completed rows cover every plan slot exactly once: {rows}"
    );
    let verdict_at = boot
        .output
        .find("✓ Ready")
        .unwrap_or_else(|| panic!("the final verdict must print: {:?}", boot.output));
    let before_verdict = &boot.output[..verdict_at];
    let clear = before_verdict.rfind("\r\u{1b}[2K").unwrap_or_else(|| {
        panic!("the status line must be erased before the verdict: {before_verdict:?}")
    });
    assert!(
        !before_verdict[clear + 5..].contains('\n'),
        "the verdict follows the one in-place status-line erase, not a stray row: {:?}",
        &before_verdict[clear..]
    );
    assert!(
        boot.output[verdict_at..].contains(&format!("{world_id}.localhost")),
        "the verdict must carry the world URL: {:?}",
        boot.output
    );

    // 2. The typed status/ps surfaces: rows, resources, colours, and `--json` fields.
    let status = run(&project, &env, &["status", "--json"]);
    assert!(status.status.success(), "{status:?}");
    let status_json: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("status --json parses");
    let world = &status_json["worlds"][0];
    assert_eq!(world["url"], format!("http://{world_id}.localhost"));
    let rows = services(world);
    let ids: Vec<&str> = rows
        .iter()
        .filter_map(|service| service["id"].as_str())
        .collect();
    for id in ["alpha", "beta", "pg", "cache", "uploads"] {
        assert!(ids.contains(&id), "missing `{id}`: {ids:?}");
    }
    let uploads = rows
        .iter()
        .find(|service| service["id"] == "uploads")
        .expect("uploads row");
    assert_eq!(uploads["cdn"], "assets-cdn");
    assert_eq!(uploads["policy"], "public-read");
    assert_eq!(uploads["running"], true);
    assert_eq!(uploads["orphaned"], false);

    let human = run(&project, &env, &["status"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).expect("status is UTF-8");
    assert!(human.contains(&format!("● {world_id}")), "{human}");
    assert!(human.contains("postgres 18"), "{human}");
    assert!(human.contains("redis 7"), "{human}");
    assert!(human.contains("s3 (public-read)"), "{human}");
    assert!(!human.contains('\u{1b}'), "{human}");

    let ps = run(&project, &env, &["ps", "--json"]);
    assert!(ps.status.success(), "{ps:?}");
    let reply: serde_json::Value = serde_json::from_slice(&ps.stdout).expect("ps --json");
    let rows = services(&reply["worlds"][0]);
    for id in ["alpha", "beta"] {
        let row = rows
            .iter()
            .find(|row| row["id"] == id)
            .unwrap_or_else(|| panic!("missing `{id}`: {rows:?}"));
        assert_eq!(row["running"], true, "{row}");
        assert_eq!(row["orphaned"], false, "{row}");
    }
    let pg = rows.iter().find(|row| row["id"] == "pg").expect("pg row");
    assert_eq!(pg["kind"], "postgres");
    let scoped = run(&project, &env, &["ps"]);
    assert!(scoped.status.success());
    assert!(String::from_utf8_lossy(&scoped.stdout).contains("postgres 18"));

    // 3. Delete `beta`: the live pod stays visible yellow `orphaned`, then `up` reaps it.
    fs::write(project.join("Forkfile"), ONE_SERVICE).expect("write the reduced Forkfile");
    assert!(project.join("Forkfile").is_file());
    let ps = run(&project, &env, &["ps", "--json"]);
    let reply: serde_json::Value = serde_json::from_slice(&ps.stdout).expect("ps --json");
    let rows = services(&reply["worlds"][0]);
    let beta = rows
        .iter()
        .find(|row| row["id"] == "beta")
        .unwrap_or_else(|| panic!("orphaned `beta` must stay visible: {rows:?}"));
    assert_eq!(beta["orphaned"], true, "{beta}");
    assert_eq!(beta["running"], true, "{beta}");
    let human = run(&project, &env, &["ps"]);
    let human = String::from_utf8(human.stdout).expect("ps is UTF-8");
    assert!(human.contains("orphaned"), "{human}");
    let coloured = run_pty(&project, &env, &["ps"], &[], None);
    assert!(
        coloured.output.contains("\u{1b}[33morphaned\u{1b}[0m"),
        "`orphaned` must be bold yellow on a colour TTY: {:?}",
        coloured.output
    );

    let up = run(&project, &env, &["up"]);
    assert!(
        up.status.success(),
        "reconcile `up` failed: {}",
        String::from_utf8_lossy(&up.stderr)
    );
    assert!(!String::from_utf8_lossy(&up.stdout).contains('\u{1b}'));
    let ps = run(&project, &env, &["ps", "--json"]);
    let reply: serde_json::Value = serde_json::from_slice(&ps.stdout).expect("ps --json");
    let rows = services(&reply["worlds"][0]);
    assert!(
        rows.iter().all(|row| row["id"] != "beta"),
        "the reaped orphan must leave the tree: {rows:?}"
    );
    let alpha = rows
        .iter()
        .find(|row| row["id"] == "alpha")
        .expect("alpha survives");
    assert_eq!(alpha["running"], true, "{alpha}");
    let http = run(
        &project,
        &env,
        &[
            "exec",
            "alpha",
            "--",
            "python3",
            "-c",
            "import urllib.request; print(urllib.request.urlopen('http://127.0.0.1:8101/').status)",
        ],
    );
    assert!(
        http.status.success(),
        "the remaining service must answer HTTP: {}",
        String::from_utf8_lossy(&http.stderr)
    );
    assert!(
        String::from_utf8_lossy(&http.stdout).contains("200"),
        "the remaining service must answer HTTP: {:?}",
        String::from_utf8_lossy(&http.stdout)
    );

    // 3b. A real fork that is stopped stays visible as a dim `○ ... stopped` row from the parent.
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(&project)
            .env("GIT_AUTHOR_NAME", "slipfork-lab")
            .env("GIT_AUTHOR_EMAIL", "lab@slipfork.invalid")
            .env("GIT_COMMITTER_NAME", "slipfork-lab")
            .env("GIT_COMMITTER_EMAIL", "lab@slipfork.invalid")
            .output()
            .expect("git must run for the live fork journey")
    };
    assert!(git(&["init", "-q"]).status.success(), "git init");
    let added = git(&["add", "-A"]);
    assert!(added.status.success(), "git add");
    let committed = git(&["commit", "-qm", "live baseline"]);
    assert!(committed.status.success(), "git commit");
    let fork = run(&project, &env, &["fork", "feat-stopped", "--json"]);
    assert!(
        fork.status.success(),
        "fork failed:\n{}",
        failure_detail(&env, &fork)
    );
    let stopped = run(&project, &env, &["down", "feat-stopped"]);
    assert!(
        stopped.status.success(),
        "down feat-stopped failed:\n{}",
        failure_detail(&env, &stopped)
    );
    let ps = run(&project, &env, &["ps", "--json"]);
    let reply: serde_json::Value = serde_json::from_slice(&ps.stdout).expect("ps --json");
    let forks = reply["worlds"][0]["forks"]
        .as_array()
        .unwrap_or_else(|| panic!("forks must be an array: {reply}"));
    let stopped = forks
        .iter()
        .find(|fork| fork["fork"] == "feat-stopped")
        .unwrap_or_else(|| panic!("a stopped fork must stay discoverable: {reply}"));
    assert_eq!(stopped["state"], "stopped", "{stopped}");
    assert_eq!(
        stopped["lineage"],
        serde_json::json!([world_id]),
        "a stopped fork still names its root lineage: {stopped}"
    );
    assert!(
        stopped["forks"]
            .as_array()
            .is_some_and(|rows| rows.is_empty()),
        "a stopped row nests zero forks: {stopped}"
    );
    assert!(
        stopped["services"]
            .as_array()
            .is_some_and(|rows| rows.iter().all(|row| row["running"] == false)),
        "a stopped row renders zero running sub-rows: {stopped}"
    );
    let human = run(&project, &env, &["ps"]);
    let human = String::from_utf8(human.stdout).expect("ps is UTF-8");
    assert!(
        human.contains("○ feat-stopped") && human.contains("stopped"),
        "the stopped fork row must render dim with its name: {human}"
    );
    assert!(!human.contains('\u{1b}'), "{human}");
    assert_stopped_rows_have_no_sub_rows(&human);
    let coloured = run_pty(&project, &env, &["ps"], &[], None);
    assert!(
        coloured
            .output
            .contains(&format!("\u{1b}[38;5;244m← {world_id}")),
        "the dim `← parent` lineage suffix must render on a colour TTY: {:?}",
        coloured.output
    );

    // A plain `up <fork>` from the parent must publish the fork flag so `ps` nests it live.
    let reup = run(&project, &env, &["up", "feat-stopped", "--json"]);
    assert!(
        reup.status.success(),
        "up feat-stopped failed: {}",
        String::from_utf8_lossy(&reup.stderr)
    );
    let ps = run(&project, &env, &["ps", "--json"]);
    let reply: serde_json::Value = serde_json::from_slice(&ps.stdout).expect("ps --json");
    let live_fork = reply["worlds"][0]["forks"]
        .as_array()
        .and_then(|forks| forks.iter().find(|fork| fork["fork"] == "feat-stopped"))
        .unwrap_or_else(|| panic!("the live fork must nest under its parent: {reply}"));
    assert_eq!(live_fork["alive"], true, "{live_fork}");
    assert_eq!(
        live_fork["world"],
        format!("feat-stopped--{world_id}"),
        "{live_fork}"
    );
    let human = run(&project, &env, &["ps"]);
    let human = String::from_utf8(human.stdout).expect("ps is UTF-8");
    assert!(
        human.contains("● feat-stopped") && !human.contains("○ feat-stopped"),
        "a live fork renders as a `●` world, never a stopped row: {human}"
    );
    let stopped = run(&project, &env, &["down", "feat-stopped"]);
    assert!(
        stopped.status.success(),
        "down feat-stopped failed: {}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    let ps = run(&project, &env, &["ps", "--json"]);
    let reply: serde_json::Value = serde_json::from_slice(&ps.stdout).expect("ps --json");
    let stopped = reply["worlds"][0]["forks"]
        .as_array()
        .and_then(|forks| forks.iter().find(|fork| fork["fork"] == "feat-stopped"))
        .unwrap_or_else(|| panic!("after `down` the fork returns to a stopped row: {reply}"));
    assert_eq!(stopped["state"], "stopped", "{reply}");

    // 3c. A world whose checkout vanished is a `✗` broken row with exactly one `!` remedy; it is
    //     never a fake `●` world with invented sub-rows, and `drop` splices the row away.
    //     (Replaces the pre-M3.141 `PsStoppedFork`/`stopped_forks` shape: forks are `forks[]` rows
    //     whose `state` is `broken`, and a broken row owns zero services/children.)
    let missing = run(&project, &env, &["fork", "feat-missing", "--json"]);
    assert!(
        missing.status.success(),
        "fork failed: {}",
        String::from_utf8_lossy(&missing.stderr)
    );
    // Remove only the user's checkout; the fork's world data survives, so converge must settle on
    // the honest `broken` row instead of deleting the ledger entry.
    let missing_checkout = project.join(".forks/feat-missing");
    fs::remove_dir_all(&missing_checkout).expect("remove the live fork checkout");
    let ps = run(&project, &env, &["ps", "--json"]);
    let reply: serde_json::Value = serde_json::from_slice(&ps.stdout).expect("ps --json");
    let forks = reply["worlds"][0]["forks"]
        .as_array()
        .unwrap_or_else(|| panic!("forks must be an array: {reply}"));
    let fork_row = forks
        .iter()
        .find(|fork| fork["fork"] == "feat-missing")
        .unwrap_or_else(|| panic!("the broken fork row must stay: {reply}"));
    assert_eq!(fork_row["state"], "broken", "{fork_row}");
    let problem = fork_row["problem"]
        .as_str()
        .unwrap_or_else(|| panic!("a broken row must name its problem: {fork_row}"));
    assert!(
        problem.contains("checkout") && problem.contains("missing"),
        "a vanished checkout must name its problem: {fork_row}"
    );
    assert_eq!(
        problem.matches("slipfork ").count(),
        1,
        "a broken row carries exactly one remedy: {fork_row}"
    );
    assert_eq!(
        fork_row["lineage"],
        serde_json::json!([world_id]),
        "a broken fork keeps its root lineage: {fork_row}"
    );
    for sub_row in ["services", "forks"] {
        assert!(
            fork_row[sub_row]
                .as_array()
                .is_some_and(|rows| rows.is_empty()),
            "a broken row owns zero `{sub_row}`: {fork_row}"
        );
    }
    let broken_at = forks
        .iter()
        .position(|fork| fork["fork"] == "feat-missing")
        .expect("broken row");
    let stopped_at = forks
        .iter()
        .position(|fork| fork["state"] == "stopped")
        .expect("stopped row");
    assert!(
        broken_at < stopped_at,
        "live/broken rows render before stopped ones: {reply}"
    );
    let human = run(&project, &env, &["ps"]);
    let human = String::from_utf8(human.stdout).expect("ps is UTF-8");
    assert!(
        human.lines().any(|line| line.starts_with("✗ feat-missing")),
        "the broken row renders a `✗`: {human}"
    );
    let remedies: Vec<&str> = human
        .lines()
        .filter(|line| line.trim_start().starts_with("! "))
        .collect();
    assert_eq!(
        remedies.len(),
        1,
        "exactly one `!` remedy line for the broken row: {human}"
    );
    assert!(
        remedies[0].matches("`slipfork ").count() == 1,
        "the one remedy names exactly one command: {human}"
    );
    let coloured = run_pty(&project, &env, &["ps"], &[], None);
    assert!(
        coloured.output.contains("\u{1b}[31m✗")
            && coloured
                .output
                .contains(&format!("\u{1b}[38;5;244m← {world_id}")),
        "the broken row keeps the red `✗` and dim lineage: {:?}",
        coloured.output
    );

    // Post-`drop` splice: the ledger row vanishes, so no stale `✗`/`!` row survives.
    let dropped = run(&project, &env, &["drop", "feat-missing", "-f", "--json"]);
    assert!(
        dropped.status.success(),
        "drop feat-missing failed: {}",
        failure_detail(&env, &dropped)
    );
    let ps = run(&project, &env, &["ps", "--json"]);
    let reply: serde_json::Value = serde_json::from_slice(&ps.stdout).expect("ps --json");
    let human = String::from_utf8(run(&project, &env, &["ps"]).stdout).expect("ps is UTF-8");
    assert!(
        !reply.to_string().contains("feat-missing") && !human.contains("feat-missing"),
        "the dropped fork is spliced out of `ps`: {reply}\n{human}"
    );
    assert!(
        !human
            .lines()
            .any(|line| line.trim_start().starts_with("! ")),
        "the dropped row leaves no dangling remedy: {human}"
    );

    // 4. A hook boot on the colour PTY: no hook bytes, and a `SIGWINCH` resize keeps the same
    //    append-only narrative. (Replaces `ESC[1;39r`/`ESC[1;29r`/`ESC[30;1H`: with no scroll
    //    region or addressed pane row there is nothing to re-apply, so the substitute proves the
    //    hook rows still stream after the resize and the status line re-measures to the new width.)
    fs::write(project.join("Forkfile"), HOOK_FORKFILE).expect("write the hook Forkfile");
    assert!(project.join("Forkfile").is_file());
    let hook_boot = run_pty(
        &project,
        &env,
        &["up"],
        &[],
        Some((Duration::from_millis(700), 30, 100)),
    );
    assert!(
        hook_boot.status.success(),
        "PTY `slipfork up` with hook failed: {:?}",
        hook_boot.output
    );
    assert_append_only(&hook_boot.output, "colour-PTY `up` (SIGWINCH 30×100)");
    let hook_readable = plain(&hook_boot.output);
    // The hook is a phase row: its own command anchors it, and every child line streams under it
    // in the gutter shape instead of being swallowed.
    assert!(
        hook_readable.contains("[1/") && hook_readable.contains("hook"),
        "the hook phase row must name its verb: {hook_readable}"
    );
    assert!(
        hook_readable.contains("│ rolling-tail-1") && hook_readable.contains("│ rolling-tail-2"),
        "hook output must stream under the phase row: {hook_readable}"
    );
    let tail_1 = hook_boot
        .output
        .find(" rolling-tail-1\r\n")
        .expect("the first streamed hook row");
    let tail_2 = hook_boot
        .output
        .find(" rolling-tail-2\r\n")
        .expect("the second streamed hook row");
    assert!(tail_1 < tail_2, "hook lines arrive in order");
    let resized_frames = status_frames(&hook_boot.output[tail_1..tail_2]);
    assert!(
        !resized_frames.is_empty(),
        "the status line must repaint while the hook sleeps: {:?}",
        hook_boot.output
    );
    let before_resize = status_frames(&hook_boot.output[..tail_1]);
    assert!(
        before_resize
            .iter()
            .any(|frame| frame.chars().count() > 100),
        "the 120-column status line must overflow 100 cells before SIGWINCH: {before_resize:?}"
    );
    let width = resized_frames
        .iter()
        .rev()
        .find(|frame| frame.chars().any(|glyph| BRAILLE.contains(glyph)))
        .map(|frame| frame.chars().count())
        .unwrap_or(0);
    assert!(
        (90..=100).contains(&width),
        "the post-SIGWINCH status line is re-measured into 100 columns, got {width}: {resized_frames:?}"
    );

    // 4b. A failing hook keeps the append-only rows and prints one crisp failure line; the old
    //     `ESC[r` restore pin is replaced by the no-region/no-address invariant above.
    let stopped = run(&project, &env, &["down"]);
    assert!(stopped.status.success(), "{stopped:?}");
    fs::write(project.join("Forkfile"), FAIL_FORKFILE).expect("write the failing Forkfile");
    assert!(project.join("Forkfile").is_file());
    let failed = run_pty(&project, &env, &["up"], &[], None);
    assert!(
        !failed.status.success(),
        "a failing hook must fail `up`: {:?}",
        failed.output
    );
    assert_append_only(&failed.output, "colour-PTY `up` (failing hook)");
    let failed_readable = plain(&failed.output);
    assert!(
        failed_readable.contains('✗')
            && failed_readable.contains("hook")
            && failed_readable.contains("failed"),
        "the failed row must render: {failed_readable}"
    );
    assert!(
        failed_readable.contains("✗ hook") && failed_readable.contains("failed (exit 3)"),
        "the verdict names the hook and its exit code: {failed_readable}"
    );
    assert!(
        !failed_readable.contains("Failed to"),
        "the rejected `Failed to …` sentence is gone: {failed_readable}"
    );
    assert_eq!(
        failed_readable.matches("next:").count(),
        1,
        "a failure prints exactly one `next:` line: {failed_readable}"
    );
    assert!(
        failed_readable.contains("next: slipfork logs -r "),
        "the one remedy replays the catalog run: {failed_readable}"
    );
    assert!(
        !failed_readable.contains("check logs with:"),
        "the superseded hint copy is gone: {failed_readable}"
    );
    // A failed boot may have no published live entry; best-effort cleanup before the next boot.
    let _ = run(&project, &env, &["down"]);

    // 4c. `NO_COLOR`/`TERM=dumb` on a real PTY emit zero escape bytes.
    fs::write(project.join("Forkfile"), ONE_SERVICE).expect("restore the service Forkfile");
    assert!(project.join("Forkfile").is_file());
    for (args, envs) in [
        (&["up"][..], &[("NO_COLOR", "1")][..]),
        (&["restart"][..], &[("TERM", "dumb")][..]),
    ] {
        let output = run_pty(&project, &env, args, envs, None);
        assert!(
            output.status.success(),
            "{args:?} on a plain PTY failed: {:?}",
            output.output
        );
        assert!(
            !output.output.contains('\u{1b}'),
            "{args:?} with {envs:?} emitted an escape: {:?}",
            output.output
        );
        assert!(
            !output.output.replace("\r\n", "\n").contains('\r'),
            "{args:?} with {envs:?} emitted a raw carriage return: {:?}",
            output.output
        );
        assert!(
            !BRAILLE.chars().any(|glyph| output.output.contains(glyph)),
            "{args:?} must never paint a spinner: {:?}",
            output.output
        );
        assert!(
            output.output.contains('✓'),
            "{args:?} must keep the append-only verdict shape: {:?}",
            output.output
        );
        if args == ["up"] {
            assert!(
                output.output.contains("→ ["),
                "{args:?} must keep the append-only stream header: {:?}",
                output.output
            );
        }
    }

    // 5. `down` keeps captures: every logs shape replays history, and `-f` follows the next `up`.
    let stopped = run(&project, &env, &["down"]);
    assert!(stopped.status.success(), "{stopped:?}");
    fs::write(project.join("Forkfile"), ONE_SERVICE).expect("restore the service Forkfile");
    let history = run(&project, &env, &["logs", "alpha"]);
    assert!(history.status.success(), "{history:?}");
    let history_text = String::from_utf8_lossy(&history.stdout).into_owned();
    assert!(history_text.contains("alpha"), "{history_text:?}");
    let dividers = history_text.matches("── alpha started ──").count();
    assert!(
        dividers > 0,
        "alpha history must carry a boot divider: {history_text:?}"
    );

    let bare = run(&project, &env, &["logs"]);
    assert!(bare.status.success(), "{bare:?}");
    let bare_text = String::from_utf8_lossy(&bare.stdout).into_owned();
    assert!(
        bare_text.contains("alpha │") || bare_text.contains("alpha ┃"),
        "{bare_text:?}"
    );
    let windowed = run(&project, &env, &["logs", "--since", "5m"]);
    assert!(windowed.status.success(), "{windowed:?}");
    let windowed_text = String::from_utf8_lossy(&windowed.stdout).into_owned();
    assert!(
        windowed_text.contains("alpha │") || windowed_text.contains("alpha ┃"),
        "{windowed_text:?}"
    );
    let all = run(&project, &env, &["logs", "-a"]);
    assert!(all.status.success(), "{all:?}");
    assert!(
        String::from_utf8_lossy(&all.stdout)
            .lines()
            .any(|line| line.contains("alpha") && (line.contains('│') || line.contains('┃'))),
        "{:?}",
        String::from_utf8_lossy(&all.stdout)
    );

    // A follower started while the world is down picks up the next boot's capture divider.
    let mut follower = Command::new(binary())
        .args(["logs", "-f", "alpha"])
        .current_dir(&project)
        .env("XDG_RUNTIME_DIR", &env.runtime)
        .env("XDG_STATE_HOME", &env.state)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("logs -f must start");
    let mut pipe = follower.stdout.take().expect("follower stdout");
    let seen = Arc::new(std::sync::Mutex::new(String::new()));
    let sink = Arc::clone(&seen);
    let reader = std::thread::spawn(move || {
        let mut chunk = [0_u8; 4096];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    if let Ok(mut text) = sink.lock() {
                        text.push_str(&String::from_utf8_lossy(&chunk[..read]));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
    std::thread::sleep(Duration::from_millis(600));
    let up = run(&project, &env, &["up"]);
    assert!(
        up.status.success(),
        "follower `up` failed: {}",
        String::from_utf8_lossy(&up.stderr)
    );
    wait_until(
        "the follower to replay the next boot",
        Duration::from_secs(60),
        || {
            seen.lock()
                .is_ok_and(|text| text.matches("── alpha started ──").count() > dividers)
        },
    );
    if let Ok(pid) = i32::try_from(follower.id()) {
        // SAFETY: `kill(2)` targets the child we spawned.
        unsafe {
            libc::kill(pid, libc::SIGINT);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match follower.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                let _ = follower.kill();
                break follower.wait().expect("kill follower");
            }
            Err(error) => panic!("cannot wait for `logs -f`: {error}"),
        }
    };
    let _ = reader.join();
    assert_eq!(status.code(), Some(0), "Ctrl-C on `logs -f` must exit 0");

    let stopped = run(&project, &env, &["down"]);
    assert!(stopped.status.success(), "{stopped:?}");
    wait_until(
        "every world to report stopped",
        Duration::from_secs(60),
        || {
            let ps = run(&project, &env, &["ps", "--json"]);
            let text = String::from_utf8_lossy(&ps.stdout).into_owned();
            ps.status.success()
                && text.contains("\"state\":\"stopped\"")
                && !text.contains("\"state\":\"live\"")
        },
    );

    // 6. A real multi-step Dockerfile streams one row per instruction as it happens; the failing
    //    `RUN` frame carries typed facts, and its `crun` plumbing never reaches `message`.
    let stamp = unique_suffix();
    fs::write(project.join("Forkfile"), BUILD_FORKFILE).expect("write the build Forkfile");
    fs::write(
        project.join("Dockerfile"),
        format!(
            "FROM python:3.13-slim\nCOPY . /app\nWORKDIR /app\n\
             RUN sh -c 's={stamp}; echo a-$s; sleep 1; echo b-$s >&2'\n\
             RUN sh -c 'echo out-{stamp}; echo err-{stamp} >&2; exit 3'\n"
        ),
    )
    .expect("write the multi-step Dockerfile");
    // Stamp each stdout line as it arrives, so a batched read cannot fake the live gap.
    let arrived = Arc::new(std::sync::Mutex::new(Vec::<(Instant, String)>::new()));
    let mut child = Command::new(binary())
        .args(["up", "--json"])
        .current_dir(&project)
        .env("XDG_RUNTIME_DIR", &env.runtime)
        .env("XDG_STATE_HOME", &env.state)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the slipfork binary must run");
    let pipe = child.stdout.take().expect("piped stdout");
    let sink = Arc::clone(&arrived);
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines() {
            let Ok(line) = line else { break };
            if let Ok(mut lines) = sink.lock() {
                lines.push((Instant::now(), line));
            }
        }
    });
    let mut errors = child.stderr.take().expect("piped stderr");
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = errors.read_to_end(&mut bytes);
        bytes
    });
    let status = wait_bounded(&mut child, &["up", "--json"]);
    let _ = reader.join();
    let stderr =
        String::from_utf8_lossy(&stderr_reader.join().expect("stderr reader")).into_owned();
    let arrived = arrived
        .lock()
        .map(|lines| lines.clone())
        .unwrap_or_default();
    let stdout = arrived
        .iter()
        .map(|(_, line)| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let detail = format!("stdout: {stdout}\nstderr: {stderr}");
    assert!(
        !status.success(),
        "a failing `RUN` must fail `up`: {detail}"
    );
    for text in [&stdout, &stderr] {
        assert!(
            !text.contains("\u{1b}[") && !text.contains('\r'),
            "`up --json` streams stay free of escapes and `\\r`: {text}"
        );
    }
    let frames: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("`{line}` is not NDJSON: {error}\n{detail}"))
        })
        .collect();
    let from_rows: Vec<&serde_json::Value> = frames
        .iter()
        .filter(|frame| {
            frame["event"] == "step" && frame["image"] == "web" && frame["instruction"] == "FROM"
        })
        .collect();
    assert_eq!(
        from_rows
            .iter()
            .filter(|frame| frame["state"] == "started")
            .count(),
        1,
        "exactly one `FROM` row per image: {stdout}\n{detail}"
    );
    assert!(
        from_rows
            .iter()
            .all(|frame| frame["args"] == "python:3.13-slim"),
        "`FROM` args must be emitted as written, never normalized: {from_rows:?}\n{detail}"
    );
    for forbidden in ["sha256", "sealed", "reusing cache", "registry-1.docker.io"] {
        assert!(
            !stdout.contains(forbidden),
            "`up --json` without `-v` must not leak `{forbidden}`: {stdout}"
        );
    }
    assert!(
        from_rows
            .iter()
            .any(|frame| frame["state"] != "started" && frame["elapsed_ms"].as_u64().is_some()),
        "the `FROM` row settles with its elapsed time: {stdout}\n{detail}"
    );
    let started: Vec<&serde_json::Value> = frames
        .iter()
        .filter(|frame| {
            frame["event"] == "step" && frame["image"] == "web" && frame["state"] == "started"
        })
        .collect();
    assert_eq!(
        started
            .iter()
            .map(|frame| frame["instruction"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["FROM", "COPY", "WORKDIR", "RUN", "RUN"],
        "one `started` row per instruction, in order: {stdout}\n{detail}"
    );
    assert!(
        started
            .iter()
            .enumerate()
            .all(|(offset, frame)| frame["index"] == offset + 1),
        "the rows count 1..n in declaration order: {started:?}\n{detail}"
    );
    let position = |index: i64, state: &str| {
        frames
            .iter()
            .position(|frame| {
                frame["event"] == "step" && frame["index"] == index && frame["state"] == state
            })
            .unwrap_or_else(|| {
                panic!("instruction {index} has no `{state}` row: {stdout}\n{detail}")
            })
    };
    let line_position = |marker: &str| {
        frames
            .iter()
            .position(|frame| {
                frame["event"] == "log"
                    && frame["line"]
                        .as_str()
                        .is_some_and(|line| line.starts_with(marker))
            })
            .unwrap_or_else(|| panic!("no log frame for `{marker}`: {stdout}\n{detail}"))
    };
    for frame in frames.iter().filter(|frame| frame["event"] == "log") {
        assert!(
            matches!(frame["stream"].as_str(), Some("stdout" | "stderr"))
                && frame["ts_ms"].as_u64().is_some(),
            "every log frame carries its stream and timestamp: {frame}"
        );
    }
    let a = line_position(&format!("a-{stamp}"));
    let b = line_position(&format!("b-{stamp}"));
    assert_eq!(frames[a]["stream"], "stdout");
    assert_eq!(frames[b]["stream"], "stderr");
    let gap = frames[b]["ts_ms"]
        .as_u64()
        .unwrap_or_default()
        .saturating_sub(frames[a]["ts_ms"].as_u64().unwrap_or_default());
    assert!(
        gap >= 900,
        "the `sleep 1` lines must arrive a second apart, saw {gap}ms: {stdout}"
    );
    // The reader stamped each line as it arrived: the frames really were live, not batched.
    // Match the child `line` field of a `log` frame only: the RUN row's `args` carries both
    // markers, so a substring scan would stamp both arrivals from the same pre-execution row.
    let arrival = |marker: &str| {
        let needle = format!("\"line\":\"{marker}\",");
        arrived
            .iter()
            .find(|(_, line)| line.contains("\"event\":\"log\"") && line.contains(&needle))
            .map(|(at, _)| *at)
            .unwrap_or_else(|| panic!("no `log` frame carried `{marker}`: {stdout}"))
    };
    let live_gap = arrival(&format!("b-{stamp}")).duration_since(arrival(&format!("a-{stamp}")));
    assert!(
        live_gap >= Duration::from_millis(900),
        "the `a` and `b` lines must reach the client a second apart, saw {live_gap:?}: {stdout}"
    );
    assert!(
        position(4, "started") < a && a < b && b < position(4, "done"),
        "step 4 must be live while its output streams: {stdout}"
    );
    // The shipped daemon facts settle a failed instruction on its `step` row and finish the image
    // with one `error` envelope; the removed `event:"failed"` frame's `phase`/`source`/`runtime`
    // fields no longer ship, so the substitute asserts the same facts on their new owners.
    let failed = &frames[position(5, "failed")];
    assert_eq!(
        failed["exit_code"], 3,
        "the exit-3 `RUN` row keeps its code: {stdout}"
    );
    assert_eq!(failed["instruction"], "RUN", "{failed}");
    assert!(
        failed["args"]
            .as_str()
            .is_some_and(|args| args.contains(&format!("out-{stamp}"))),
        "the failure names the instruction as written: {failed}"
    );
    let finished = frames
        .iter()
        .rev()
        .find(|frame| {
            frame["event"] == "finished" && frame["state"] == "failed" && frame["kind"] == "image"
        })
        .unwrap_or_else(|| panic!("no failed image frame: {stdout}\n{detail}"));
    let run_id = finished["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("the failed image frame carries its run id: {finished}"));
    let error = frames
        .iter()
        .find_map(|frame| frame.get("error"))
        .unwrap_or_else(|| panic!("no error envelope: {stdout}\n{detail}"));
    assert_eq!(error["code"], "image-build", "{error}");
    let remedy = error["next"].as_str().unwrap_or_default();
    assert_eq!(
        remedy.matches("slipfork logs -r ").count(),
        1,
        "exactly one copy-pasteable remedy: {error}\nimage frame: {finished}"
    );
    assert!(
        remedy.contains(&format!("slipfork logs -r {run_id}")),
        "the one remedy replays the failed image run: {error}\nimage frame: {finished}"
    );
    assert!(
        !error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("next:"),
        "the message never repeats a remedy: {error}"
    );
    assert!(
        !error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("crun"),
        "no crun text in the primary copy: {error}"
    );

    // 7. The owner-visible two-level narrative under a real colour PTY: outer `→ [1/2] image web`,
    //    indented `n/N` spinner phase row with indent-4 live gutter output + level-2 bar label
    //    while the child sleeps, collapsed settled inner rows, one expanded failure, and a single
    //    verdict + remedy. Fresh stamp so step 4 executes live rather than hitting step cache.
    let pty_stamp = unique_suffix();
    fs::write(
        project.join("Dockerfile"),
        format!(
            "FROM python:3.13-slim\nCOPY . /app\nWORKDIR /app\n\
             RUN sh -c 's={pty_stamp}; echo a-$s; sleep 1; echo b-$s >&2'\n\
             RUN sh -c 'echo out-{pty_stamp}; echo err-{pty_stamp} >&2; exit 3'\n"
        ),
    )
    .expect("write the uncached PTY Dockerfile");
    let narrative = run_pty(&project, &env, &["up"], &[], None);
    assert!(
        !narrative.status.success(),
        "the exit-3 `RUN` must fail `up`: {:?}",
        narrative.output
    );
    assert_append_only(&narrative.output, "colour-PTY failing build");
    // Append-only substitute for the removed DECSC (`ESC7`) park: completed rows advance the
    // cursor, so the live status frame after the streamed `│ a-…` line proves the `sleep 1`
    // window, and the `│ b-…` line arrives as a later completed row.
    let a_row = format!("a-{pty_stamp}\r\n");
    let b_row = format!("b-{pty_stamp}\r\n");
    let a_at = narrative
        .output
        .find(&a_row)
        .unwrap_or_else(|| panic!("no streamed `a` row: {:?}", narrative.output));
    let b_at = narrative
        .output
        .find(&b_row)
        .unwrap_or_else(|| panic!("no streamed `b` row: {:?}", narrative.output));
    assert!(a_at < b_at, "the `sleep 1` lines arrive in order");
    let live_frames = status_frames(&narrative.output[a_at..b_at]);
    assert!(
        live_frames.iter().any(|frame| {
            frame.contains("4/5 run") && frame.chars().any(|glyph| BRAILLE.contains(glyph))
        }),
        "the status frame between `a` and `b` must paint the live `4/5 run` spinner:\n{live_frames:#?}"
    );
    assert!(
        plain(&narrative.output).contains("[1/2] image web · step 4/5 RUN"),
        "the two-level bar label must name `[1/2] image web · step 4/5 RUN`:\n{}",
        plain(&narrative.output)
    );
    let completed = completed_rows(&narrative.output);
    assert_rows_once_in_order(
        &completed,
        &[
            "→ [1/2] image  web".to_owned(),
            "  ✓ 1/5 from".to_owned(),
            "  ✓ 2/5 copy".to_owned(),
            "  ✓ 3/5 workdir".to_owned(),
            "  ✓ 4/5 run".to_owned(),
            "  → 5/5 run".to_owned(),
            "✗ [1/2] image  web  failed at 5/5".to_owned(),
            "error: build `web` failed".to_owned(),
            "next:".to_owned(),
        ],
        "failing build",
    );
    let shown = plain(&narrative.output);
    assert!(
        !shown.contains("3/10"),
        "internal `outer_total * inner_total` counter `3/10` must never leak into the bar: {shown}"
    );
    assert!(
        shown.contains("→ [1/2] image  web"),
        "a fresh build must open with `→ [1/2] image  web`: {shown}"
    );
    assert!(
        shown.contains("  ✓ 1/5 from")
            && shown.contains("python:3.13-slim")
            && !shown.contains("registry-1.docker.io"),
        "`from` row must carry `python:3.13-slim` as written: {shown}"
    );
    for settled in [
        "  ✓ 1/5 from",
        "  ✓ 2/5 copy",
        "  ✓ 3/5 workdir",
        "  ✓ 4/5 run",
    ] {
        assert!(
            shown.contains(settled),
            "a settled inner step must collapse to one indented `{settled}` row: {shown}"
        );
    }
    for bracketed in ["[1/5]", "[2/5]", "[3/5]", "[4/5]", "[5/5]"] {
        assert!(
            !shown.contains(bracketed),
            "inner Dockerfile rows must never carry `{bracketed}` brackets: {shown}"
        );
    }
    assert!(
        !shown.contains("built ") && !shown.contains("✓ image web"),
        "a failed build must never print the build-end success row: {shown}"
    );
    assert!(
        shown.contains("✗ [1/2] image  web  failed at 5/5"),
        "the failed image row names the last started step: {shown}"
    );
    assert!(
        completed
            .lines()
            .any(|line| line.starts_with("  → 5/5 run")),
        "the failing instruction keeps its indented stream header: {completed}"
    );
    assert!(
        !shown.contains("\n\n\n"),
        "the failure block follows at most one blank line: {shown}"
    );
    assert!(shown.contains(&format!("    │ out-{pty_stamp}")), "{shown}");
    assert!(shown.contains(&format!("    ┃ err-{pty_stamp}")), "{shown}");
    assert!(
        shown.contains("  → 5/5 run"),
        "the failing inner step is still named once at indent 2: {shown}"
    );
    assert_eq!(
        shown.matches("error: build `web` failed").count(),
        1,
        "exactly one verdict: {shown}"
    );
    assert_eq!(
        shown.matches("next: ").count(),
        1,
        "exactly one remedy line: {shown}"
    );
    assert_eq!(
        shown.matches("slipfork logs -r ").count(),
        1,
        "the one remedy names exactly one run: {shown}"
    );
    for forbidden in [
        "sha256",
        "sealed",
        "reusing cache",
        "crun",
        "Failed to",
        " out ",
        " err ",
    ] {
        assert!(
            !shown.contains(forbidden),
            "`{forbidden}` must never appear on the default terminal: {shown}"
        );
    }

    // 8. `-v` keeps the same one verdict and one remedy; when the daemon settles a step with a
    //    runtime fact (the shipped `event["runtime"]` field; a failing instruction may omit it),
    //    the dim `runtime:` row sits between the verdict and the remedy.
    let verbose = run_pty(&project, &env, &["up", "-v"], &[], None);
    assert!(!verbose.status.success(), "{:?}", verbose.output);
    assert_append_only(&verbose.output, "colour-PTY `up -v`");
    let verbose = plain(&verbose.output);
    let verdict_at = verbose
        .find("error: build `web` failed")
        .unwrap_or_else(|| panic!("no verdict under -v: {verbose}"));
    let next_at = verbose
        .find("next: ")
        .unwrap_or_else(|| panic!("no remedy under -v: {verbose}"));
    assert_eq!(
        verbose.matches("error: build `web` failed").count(),
        1,
        "-v keeps exactly one verdict: {verbose}"
    );
    assert_eq!(
        verbose.matches("next: ").count(),
        1,
        "-v keeps exactly one remedy: {verbose}"
    );
    assert_eq!(
        verbose.matches("slipfork logs -r ").count(),
        1,
        "-v keeps exactly one run remedy: {verbose}"
    );
    if let Some(runtime_at) = verbose.find("runtime: ") {
        assert!(
            verdict_at < runtime_at && runtime_at < next_at,
            "runtime sits between verdict and next: {verbose}"
        );
    }

    // 9. `NO_COLOR` on a pipe is append-only: two levels, indent-4 gutter, zero escapes.
    let pipe_stamp = unique_suffix();
    fs::write(
        project.join("Dockerfile"),
        format!(
            "FROM python:3.13-slim\nCOPY . /app\nWORKDIR /app\n\
             RUN sh -c 's={pipe_stamp}; echo a-$s; echo b-$s >&2'\n\
             RUN sh -c 'echo out-{pipe_stamp}; echo err-{pipe_stamp} >&2; exit 3'\n"
        ),
    )
    .expect("write the uncached piped Dockerfile");
    let piped = run(&project, &env, &["up"]);
    assert!(!piped.status.success(), "{piped:?}");
    let piped_out = String::from_utf8_lossy(&piped.stdout).into_owned();
    let piped_err = String::from_utf8_lossy(&piped.stderr).into_owned();
    let piped_all = format!("{piped_out}\n{piped_err}");
    assert!(!piped_all.contains('\u{1b}'), "zero escapes: {piped_all}");
    assert!(!piped_all.contains('\r'), "no `\\r` redraws: {piped_all}");
    assert!(
        !BRAILLE.chars().any(|glyph| piped_all.contains(glyph)),
        "no spinner frames: {piped_all}"
    );
    assert!(piped_out.contains("→ [1/2] image  web"), "{piped_out}");
    assert!(piped_out.contains("  ✓ 1/5 from"), "{piped_out}");
    assert!(piped_out.contains("python:3.13-slim"), "{piped_out}");
    assert!(piped_out.contains("  → 4/5 run"), "{piped_out}");
    for bracketed in ["[1/5]", "[2/5]", "[3/5]", "[4/5]", "[5/5]"] {
        assert!(
            !piped_all.contains(bracketed),
            "append-only inner rows must never carry `{bracketed}`: {piped_all}"
        );
    }
    assert!(
        piped_out.contains(&format!("    │ a-{pipe_stamp}")),
        "{piped_out}"
    );
    assert!(
        piped_out.contains(&format!("    ┃ b-{pipe_stamp}")),
        "{piped_out}"
    );
    assert_eq!(
        piped_out
            .matches(&format!("    │ out-{pipe_stamp}"))
            .count(),
        1,
        "append-only must not repeat child output on failure: {piped_out}"
    );
    assert_eq!(
        piped_out
            .matches(&format!("    ┃ err-{pipe_stamp}"))
            .count(),
        1,
        "append-only must not repeat stderr output on failure: {piped_out}"
    );
    assert!(
        !piped_all.contains("built ") && !piped_all.contains("✓ image web"),
        "a failed build must never print a build-end row: {piped_all}"
    );
    assert!(
        piped_out.contains("✗ [1/2] image  web  failed at 5/5"),
        "the piped failed image row names the last started step: {piped_out}"
    );
    assert!(
        piped_out
            .lines()
            .any(|line| line.starts_with("  → 5/5 run")),
        "the piped failing instruction keeps its indented stream header: {piped_out}"
    );
    assert!(
        !piped_all.contains("\n\n\n"),
        "the piped failure block follows at most one blank line: {piped_all}"
    );
    assert_eq!(
        piped_err.matches("error: ").count(),
        1,
        "one plain failure line: {piped_all}"
    );
    assert!(
        !piped_err.contains("\n\n\nerror:"),
        "at most one blank line before `error:`: {piped_all}"
    );
    assert!(
        piped_err.contains("error: build `web` failed"),
        "the one stderr failure names the build: {piped_all}"
    );
    assert_eq!(piped_err.matches("next: ").count(), 1, "{piped_all}");
    assert_eq!(
        piped_err.matches("slipfork logs -r ").count(),
        1,
        "{piped_all}"
    );
    for forbidden in [
        "sha256",
        "sealed",
        "reusing cache",
        "crun",
        "Failed to",
        "registry-1.docker.io",
    ] {
        assert!(
            !piped_all.contains(forbidden),
            "`{forbidden}` must never appear on a non-`-v` pipe: {piped_all}"
        );
    }

    // 9b. An eleven-instruction Dockerfile proves the padded `i/11` counter column (` 1/11` vs
    //     `10/11`), one settled row per instruction in declaration order, and one `→` stream header
    //     per executed instruction. The bottom bar keeps outer `total == 2` and switches its label
    //     from the inner step to `[2/2] start api`.
    let first_stamp = unique_suffix();
    let second_stamp = unique_suffix();
    let dockerfile = |stamp: &str, last_two: &str| -> String {
        let mut lines = vec![
            "FROM python:3.13-slim".to_owned(),
            "WORKDIR /app".to_owned(),
        ];
        for index in 3..=11 {
            let run = if index >= 10 { last_two } else { stamp };
            lines.push(format!("RUN sh -c 'echo step-{index:02}-{run}'"));
        }
        lines.join("\n") + "\n"
    };
    fs::write(
        project.join("Dockerfile"),
        dockerfile(&first_stamp, &first_stamp),
    )
    .expect("write the eleven-instruction Dockerfile");
    let ok_build = run_pty(&project, &env, &["up"], &[], None);
    assert!(
        ok_build.status.success(),
        "the eleven-instruction Dockerfile must boot cleanly: {:?}",
        ok_build.output
    );
    assert_append_only(&ok_build.output, "colour-PTY eleven-instruction build");
    let ok_shown = plain(&ok_build.output);
    let ok_rows = completed_rows(&ok_build.output);
    let ok_screen = ok_build.screen.text();
    for visible in ["  ✓ 10/11 run", "  ✓ 11/11 run", "✓ [1/2] image  web"] {
        assert!(
            ok_screen.contains(visible),
            "the completed row `{visible}` must survive in the screen scrollback:\n{ok_screen}"
        );
    }
    let ok_fills = panel_fills(&ok_shown);
    assert!(
        ok_fills
            .iter()
            .any(|(filled, _, total)| *filled > 0 && *total == 2),
        "the bottom bar must keep outer `total == 2` and advance fractional fill as steps settle: {ok_fills:?}\n{ok_shown}"
    );
    assert!(
        ok_shown.contains("[1/2] image web · step ") && ok_shown.contains("[2/2] start  api"),
        "the bottom bar label must switch from the inner build step to the outer step:\n{ok_shown}"
    );
    let mut first_rows = vec![
        "→ [1/2] image  web".to_owned(),
        "  ✓  1/11 from".to_owned(),
        "  ✓  2/11 workdir".to_owned(),
    ];
    for index in 3..=11 {
        first_rows.push(format!("→ {index:>2}/11 run"));
        first_rows.push(format!("  ✓ {index:>2}/11"));
    }
    assert_rows_once_in_order(&ok_rows, &first_rows, "eleven-instruction build");
    for bracketed in ["[1/11]", "[2/11]", "[10/11]", "[11/11]"] {
        assert!(
            !ok_shown.contains(bracketed),
            "inner rows must never carry `{bracketed}`:\n{ok_shown}"
        );
    }
    assert!(
        ok_rows.lines().any(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("✓ [1/2] image  web") && trimmed.contains("built ")
        }),
        "a fully executed build must settle with a `built` count:\n{ok_rows}"
    );
    assert!(
        ok_rows
            .lines()
            .any(|line| line.trim_start().starts_with("✓ [2/2] start  api")),
        "the outer service row must carry `[2/2]`:\n{ok_rows}"
    );
    for forbidden in [
        "sha256",
        "sealed",
        "reusing cache",
        "crun",
        "Failed to",
        "registry-1.docker.io",
    ] {
        assert!(
            !ok_shown.contains(forbidden),
            "`{forbidden}` must never appear on a succeeding non-`-v` build: {ok_shown}"
        );
    }
    let stopped = run(&project, &env, &["down"]);
    assert!(stopped.status.success(), "{stopped:?}");

    // 9c. Rebuild with exactly the last two instructions changed: nine instruction cache hits and
    //     two real executions must read `cached 9 · built 2`, with one settled row per instruction
    //     and never a full `built 11 steps` claim.
    fs::write(
        project.join("Dockerfile"),
        dockerfile(&first_stamp, &second_stamp),
    )
    .expect("write the mixed cached/built Dockerfile");
    let mixed = run_pty(&project, &env, &["up"], &[], None);
    assert!(
        mixed.status.success(),
        "the mixed cached/built build must boot cleanly: {:?}",
        mixed.output
    );
    assert_append_only(&mixed.output, "colour-PTY mixed cached/built build");
    let mixed_rows = completed_rows(&mixed.output);
    assert!(
        mixed_rows.lines().any(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("✓ [1/2] image  web") && trimmed.contains("cached 9 · built 2")
        }),
        "nine cached plus two executed instructions must settle as `cached 9 · built 2`:\n{mixed_rows}"
    );
    assert!(
        !mixed_rows.contains("built 11 steps"),
        "a partial rebuild must never claim `built 11 steps`:\n{mixed_rows}"
    );
    for index in 1..=11 {
        let settled = format!("  ✓ {index:>2}/11");
        assert_eq!(
            mixed_rows.matches(&settled).count(),
            1,
            "{settled:?} must print exactly once (cache hits settle like executed rows):\n{mixed_rows}"
        );
    }
    let stopped = run(&project, &env, &["down"]);
    assert!(stopped.status.success(), "{stopped:?}");

    // 9d. Warm re-up on the now fully cached image: `✓ [1/2] image  web  cached`, with all eleven
    //     cache hits replayed once and no `built` claim.
    let cached_up = run_pty(&project, &env, &["up"], &[], None);
    assert!(
        cached_up.status.success(),
        "warm cached `up` must boot cleanly: {:?}",
        cached_up.output
    );
    assert_append_only(&cached_up.output, "colour-PTY cached boot");
    let cached_rows = completed_rows(&cached_up.output);
    let cached_screen = cached_up.screen.text();
    assert!(
        cached_screen.contains("✓ [1/2] image  web") && cached_screen.contains("  ✓ 11/11"),
        "the cached verdict and its replayed rows must survive on the screen:\n{cached_screen}"
    );
    assert!(
        cached_rows.lines().any(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("✓ [1/2] image  web")
                && trimmed.contains("cached")
                && !trimmed.contains("cached 9")
        }),
        "a cached image must settle as `✓ [1/2] image  web  cached`:\n{cached_rows}"
    );
    for index in 1..=11 {
        let settled = format!("  ✓ {index:>2}/11");
        assert_eq!(
            cached_rows.matches(&settled).count(),
            1,
            "{settled:?} must print exactly once on a cache replay:\n{cached_rows}"
        );
    }
    assert!(
        !cached_rows.contains("built "),
        "a cached image never claims a build:\n{cached_rows}"
    );
    let stopped = run(&project, &env, &["down"]);
    assert!(stopped.status.success(), "{stopped:?}");

    // 9e. Boot-time service failure on the colour PTY: outer `✗ [2/2] start  web`, indent-2
    //     gutter in arrival order (`│ starting web` above `┃ boom`), one verdict, and one `next:`.
    fs::write(project.join("Forkfile"), FAIL_SERVICE_FORKFILE)
        .expect("write the failing-service Forkfile");
    let svc_failed = run_pty(&project, &env, &["up"], &[], None);
    assert!(
        !svc_failed.status.success(),
        "a failing service must fail `up`: {:?}",
        svc_failed.output
    );
    assert_append_only(&svc_failed.output, "colour-PTY failing service");
    let svc_shown = plain(&svc_failed.output);
    let svc_screen = svc_failed.screen.text();
    assert!(
        svc_screen
            .lines()
            .any(|line| line.starts_with("✗ [2/2] start  web")),
        "the failing service expands its outer `[2/2] start  web` row:\n{svc_screen}"
    );
    let out_pos = svc_screen
        .find("  │ starting web")
        .unwrap_or_else(|| panic!("missing indent-2 stdout gutter line:\n{svc_screen}"));
    let err_pos = svc_screen
        .find("  ┃ boom: missing DATABASE_URL")
        .unwrap_or_else(|| panic!("missing indent-2 stderr gutter line:\n{svc_screen}"));
    assert!(
        out_pos < err_pos,
        "`│ starting web` must render above `┃ boom: missing DATABASE_URL`:\n{svc_screen}"
    );
    assert_eq!(
        svc_shown.matches("✗ start web failed (exit 7)").count(),
        1,
        "service failure prints exactly one verdict:\n{svc_shown}"
    );
    assert_eq!(
        svc_shown.matches("next: slipfork logs -r ").count(),
        1,
        "service failure prints exactly one `next:`:\n{svc_shown}"
    );
    let _ = run(&project, &env, &["down"]);
}

#[test]
fn live_ps_keeps_live_and_stopped_fork_rows_when_the_root_world_is_down() {
    let root = test_root();
    let store_root = mount_store_for(&root);
    let env = LabEnv::isolated(&root);
    let suffix = unique_suffix();
    let project_name = format!("scope-{suffix}");
    let world_id = project_name.clone();
    let project = root.join(&project_name);
    prepare(&project, &store_root, &world_id, ONE_SERVICE);
    let _guard = WorldGuard {
        project: project.clone(),
        env: env.clone(),
        store_root,
        world: world_id.clone(),
        keep: false,
    };
    assert_socket_fits(&env, &format!("feat-auth--{world_id}"));
    assert_socket_fits(&env, &format!("youpi--{world_id}"));
    assert_socket_fits(&env, &format!("salut--{world_id}"));

    let up = run(&project, &env, &["up"]);
    assert!(
        up.status.success(),
        "root `up` failed:\n{}",
        failure_detail(&env, &up)
    );

    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(&project)
            .env("GIT_AUTHOR_NAME", "slipfork-lab")
            .env("GIT_AUTHOR_EMAIL", "lab@slipfork.invalid")
            .env("GIT_COMMITTER_NAME", "slipfork-lab")
            .env("GIT_COMMITTER_EMAIL", "lab@slipfork.invalid")
            .output()
            .expect("git must run for the fork scope journey")
    };
    assert!(git(&["init", "-q"]).status.success(), "git init");
    assert!(git(&["add", "-A"]).status.success(), "git add");
    assert!(
        git(&["commit", "-qm", "fork scope baseline"])
            .status
            .success(),
        "git commit"
    );

    for fork in ["feat-auth", "youpi"] {
        let forked = run(&project, &env, &["fork", fork, "--json"]);
        assert!(
            forked.status.success(),
            "fork `{fork}` failed:\n{}",
            failure_detail(&env, &forked)
        );
    }
    // A fork of a live fork proves the three-deep lineage on the shipped surface.
    let nested = run(
        &project.join(".forks/feat-auth"),
        &env,
        &["fork", "salut", "--json"],
    );
    assert!(
        nested.status.success(),
        "nested fork `salut` failed:\n{}",
        failure_detail(&env, &nested)
    );
    let down = run(&project, &env, &["down", "youpi"]);
    assert!(
        down.status.success(),
        "`down youpi` failed:\n{}",
        failure_detail(&env, &down)
    );
    let down = run(&project, &env, &["down"]);
    assert!(
        down.status.success(),
        "root `down` failed:\n{}",
        failure_detail(&env, &down)
    );

    // 1. The project root still names the stopped root, the stopped sibling, and the live fork.
    let human = String::from_utf8(run(&project, &env, &["ps"]).stdout).expect("ps is UTF-8");
    for row in [
        format!("○ {world_id}"),
        "○ youpi".to_owned(),
        "● feat-auth".to_owned(),
    ] {
        assert!(human.contains(&row), "missing `{row}`:\n{human}");
    }
    for row in [
        format!("○ {world_id}"),
        "○ youpi".to_owned(),
        "● feat-auth".to_owned(),
    ] {
        assert_eq!(
            human.matches(&row).count(),
            1,
            "`{row}` must render exactly once:\n{human}"
        );
    }
    assert!(!human.contains('\u{1b}'), "{human}");
    // The shipped `forks[]` surface: typed `state`, ancestor lineage, live/broken rows before
    // stopped rows, and zero sub-rows on a stopped row. (Replaces the deleted
    // `PsStoppedFork`/`stopped_forks` shape from `M3.141`.)
    let ps = run(&project, &env, &["ps", "--json"]);
    let reply: serde_json::Value = serde_json::from_slice(&ps.stdout).expect("ps --json");
    let worlds = reply["worlds"].as_array().expect("worlds");
    assert_eq!(reply["worlds"][0]["state"], "stopped", "{reply}");
    let feat =
        find_row(worlds, "feat-auth").unwrap_or_else(|| panic!("missing `feat-auth`: {reply}"));
    assert_eq!(feat["state"], "live", "{feat}");
    assert_eq!(
        feat["lineage"],
        serde_json::json!([world_id]),
        "a fork names its root: {feat}"
    );
    let salut = find_row(worlds, "salut").unwrap_or_else(|| panic!("missing `salut`: {reply}"));
    assert_eq!(salut["state"], "live", "{salut}");
    assert_eq!(
        salut["lineage"],
        serde_json::json!(["feat-auth", world_id]),
        "a fork of a fork names its parent then the root: {salut}"
    );
    let youpi = find_row(worlds, "youpi").unwrap_or_else(|| panic!("missing `youpi`: {reply}"));
    assert_eq!(youpi["state"], "stopped", "{youpi}");
    assert!(
        youpi["forks"]
            .as_array()
            .is_some_and(|rows| rows.is_empty()),
        "a stopped row nests zero forks: {youpi}"
    );
    assert!(
        youpi["services"]
            .as_array()
            .is_some_and(|rows| rows.iter().all(|row| row["running"] == false)),
        "a stopped row renders zero running sub-rows: {youpi}"
    );
    let position = |needle: &str| {
        human
            .lines()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("missing `{needle}`:\n{human}"))
    };
    assert!(
        position("● feat-auth") < position("● salut") && position("● salut") < position("○ youpi"),
        "live forks render before the stopped sibling:\n{human}"
    );
    let coloured = run_pty(&project, &env, &["ps"], &[], None);
    assert!(
        coloured
            .output
            .contains(&format!("\u{1b}[38;5;244m← feat-auth ← {world_id}")),
        "a fork of a fork renders the dim `← parent ← root` lineage: {:?}",
        coloured.output
    );
    // A stopped header owns a literal `stopped` column, and the live fork still carries its
    // domain and service rows from the fixture.
    let stopped_header = human
        .lines()
        .find(|line| line.starts_with("○ ") && line.contains(&world_id))
        .unwrap_or_else(|| panic!("no stopped root header:\n{human}"));
    assert!(
        stopped_header.trim_end().ends_with("stopped"),
        "the stopped column must be literal: {stopped_header:?}"
    );
    assert!(
        human.contains(&format!("http://feat-auth--{world_id}.localhost")),
        "the live fork keeps its domain:\n{human}"
    );
    assert!(
        human.contains("/alpha") && human.contains("running"),
        "the live fork keeps its service row:\n{human}"
    );

    // 2. A cwd inside the stopped fork scopes strictly to its one dim row.
    let human = String::from_utf8(run(&project.join(".forks/youpi"), &env, &["ps"]).stdout)
        .expect("ps is UTF-8");
    assert!(human.contains("○ youpi"), "{human}");
    assert!(
        !human.contains("feat-auth") && !human.contains(&format!("○ {world_id}")),
        "a stopped fork scope never leaks its siblings:\n{human}"
    );

    // 3. A cwd inside the running fork scopes strictly to that live row.
    let human = String::from_utf8(run(&project.join(".forks/feat-auth"), &env, &["ps"]).stdout)
        .expect("ps is UTF-8");
    assert!(human.contains("● feat-auth"), "{human}");
    assert!(
        !human.contains("youpi") && !human.contains(&format!("○ {world_id}")),
        "a running fork scope never leaks its parent or siblings:\n{human}"
    );

    // 4. Bare unscoped `status` from an unrelated cwd shows the same set exactly once.
    let elsewhere = root.join(format!("elsewhere-{suffix}"));
    fs::create_dir_all(&elsewhere).expect("create an unrelated directory");
    let human =
        String::from_utf8(run(&elsewhere, &env, &["status"]).stdout).expect("status is UTF-8");
    for row in [
        format!("○ {world_id}"),
        "○ youpi".to_owned(),
        "● feat-auth".to_owned(),
    ] {
        assert_eq!(
            human.matches(&row).count(),
            1,
            "unscoped status must not duplicate `{row}`:\n{human}"
        );
    }

    // 5. Dropping the last live fork leaves only dim `stopped` rows: no world and no `-pgid`
    // process group survives.
    let down = run(&project, &env, &["down", "salut"]);
    assert!(
        down.status.success(),
        "`down salut` failed:\n{}",
        failure_detail(&env, &down)
    );
    let down = run(&project, &env, &["down", "feat-auth"]);
    assert!(
        down.status.success(),
        "`down feat-auth` failed:\n{}",
        failure_detail(&env, &down)
    );
    // The root world already reported `stopped` above; `down` is a stop verb, not a
    // registry delete, so the journey proves the stopped state through `ps` below.
    wait_until(
        "every world to report stopped",
        Duration::from_secs(60),
        || {
            let ps = run(&project, &env, &["ps", "--json"]);
            let text = String::from_utf8_lossy(&ps.stdout).into_owned();
            ps.status.success()
                && text.contains("\"state\":\"stopped\"")
                && !text.contains("\"state\":\"live\"")
        },
    );
    let human = String::from_utf8(run(&project, &env, &["ps"]).stdout).expect("ps is UTF-8");
    assert!(
        !human.lines().any(|line| line.starts_with('●')),
        "no world may stay live:\n{human}"
    );
    for row in [
        format!("○ {world_id}"),
        "○ feat-auth".to_owned(),
        "○ salut".to_owned(),
        "○ youpi".to_owned(),
    ] {
        assert!(human.contains(&row), "missing `{row}`:\n{human}");
    }
    assert!(
        human
            .lines()
            .filter(|line| line.starts_with('○'))
            .all(|line| line.contains(" stopped")),
        "every remaining header is a stopped row:\n{human}"
    );
    assert_stopped_rows_have_no_sub_rows(&human);
    assert!(
        !human.contains("\"state\""),
        "the human tree never prints wire keys:\n{human}"
    );
    assert!(!human.contains('\u{1b}'), "{human}");
    fs::remove_dir_all(&elsewhere).expect("remove the unrelated directory");
}

/// A user's shell prompt is data: every lifecycle verb must leave every prompt cell untouched.
const PROMPT: &str = "slipfork@slipfork-dev-vm:~/demo$ slipfork up";
/// The same line after Enter: the cursor starts on the row below the prompt, as in a real shell.
const SEED: &str = "slipfork@slipfork-dev-vm:~/demo$ slipfork up\r\n";

fn assert_prompt(run: &PtyRun, args: &[&str], seed_row: usize) {
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
        "`slipfork {args:?}` mutated the prompt cells\nscreen:\n{}",
        run.screen.text()
    );
    if seed_row == 2 {
        assert_eq!(
            row,
            2,
            "a prompt above the flow cursor must not move\nscreen:\n{}",
            run.screen.text()
        );
    }
    // The append-only narrative never owns a scroll region or parks the cursor: assert zero cursor
    // addressing over the whole capture and that the flow cursor stays below the untouched prompt.
    // This one-for-one replaces the removed `DECSTBM`/`ESC[r`/row-40 park check: a (1,1) full-grid
    // regression cannot survive either the no-address invariant or the "below the prompt" check.
    assert_append_only(
        &run.output,
        &format!("`slipfork {}` (seeded prompt)", args[0]),
    );
    let (cursor_row, _) = run.screen.cursor();
    let cursor_line = cursor_row + 1; // 1-based, as a user reads it
    let prompt_line = row + 1;
    assert_ne!(
        cursor_line,
        1,
        "`slipfork {args:?}` (seed row {seed_row}) parked on the (1,1) top row\nscreen:\n{}",
        run.screen.text()
    );
    assert!(
        cursor_line > prompt_line,
        "`slipfork {args:?}` (seed row {seed_row}) left the flow cursor on 1-based row \
         {cursor_line}, not below the prompt on row {prompt_line}\nscreen:\n{}",
        run.screen.text()
    );
}

#[test]
fn live_prompt_is_byte_identical_across_lifecycle_verbs() {
    let root = test_root();
    let store_root = mount_store_for(&root);
    let env = LabEnv::isolated(&root);
    let suffix = unique_suffix();
    let project_name = format!("prompt-{suffix}");
    let world_id = project_name.clone();
    let project = root.join(&project_name);
    prepare(&project, &store_root, &world_id, SUITE_FORKFILE);
    let mut guard = WorldGuard {
        project: project.clone(),
        env: env.clone(),
        store_root,
        world: world_id.clone(),
        keep: false,
    };

    // One daemon, one boot, four verbs: the prompt is seeded on the bottom row and on row 3.
    for (args, seed_row) in [
        (&["up"][..], 39usize),
        (&["restart"][..], 2usize),
        (&["up"][..], 39),
        // No fork name: this project is not a git checkout, so `fork` fails after its pane is
        // live — still real fork bytes, and no half-fork to reap.
        (&["fork"][..], 2),
        (&["down"][..], 39),
    ] {
        let run = run_pty_seeded(&project, &env, args, &[], None, Some((seed_row, SEED)));
        if !run.status.success() && args[0] != "fork" {
            guard.keep = true;
            panic!(
                "PTY `slipfork {args:?}` failed\nscreen:\n{}\nbytes: {:?}",
                run.screen.text(),
                run.output
            );
        }
        assert_prompt(&run, args, seed_row);
    }
    drop(guard);
}

/// A crashed long-lived service must never yield `✓ Ready`.
///
/// `world_answers` only proves the control socket connects, so the canonical `ps` rows decide
/// health: the restart always fails with the exact `next:` remedy, and `up` must never report
/// Ready while a subsequent `ps` still shows the service down. `--json` must agree.
#[test]
fn live_up_never_reports_ready_for_a_dead_service() {
    let root = test_root();
    let store_root = mount_store_for(&root);
    let env = LabEnv::isolated(&root);
    let suffix = unique_suffix();
    let project_name = format!("dead-{suffix}");
    let world_id = project_name.clone();
    let project = root.join(&project_name);
    prepare(&project, &store_root, &world_id, DEAD_SERVICE_FORKFILE);
    let _guard = WorldGuard {
        project: project.clone(),
        env: env.clone(),
        store_root,
        world: world_id.clone(),
        keep: false,
    };

    let boot = run(&project, &env, &["up"]);
    assert!(
        boot.status.success(),
        "the world must boot before its service dies:\n{}",
        failure_detail(&env, &boot)
    );

    // Reads the canonical `ps` row so `up`'s verdict is judged against the same health model.
    let web_running = |label: &str| -> bool {
        let ps = run(&project, &env, &["ps", "--json"]);
        let detail = failure_detail(&env, &ps);
        let reply: serde_json::Value = serde_json::from_slice(&ps.stdout)
            .unwrap_or_else(|error| panic!("{label}: ps --json does not parse: {error}\n{detail}"));
        let rows = reply["worlds"]
            .as_array()
            .and_then(|worlds| worlds.first())
            .map(services)
            .unwrap_or_default();
        let row = rows
            .iter()
            .find(|row| row["id"] == "web")
            .unwrap_or_else(|| panic!("{label}: the `web` row is missing: {reply}\n{detail}"));
        row["running"].as_bool().unwrap_or_else(|| {
            panic!("{label}: the `web` row has no boolean `running`: {row}\n{detail}")
        })
    };
    wait_until("web to exit", Duration::from_secs(20), || {
        !web_running("waiting for web")
    });

    // The service dies immediately on every relaunch: `up` must fail, never print Ready.
    let again = run(&project, &env, &["up"]);
    let detail = failure_detail(&env, &again);
    let stdout = String::from_utf8_lossy(&again.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&again.stderr).into_owned();
    assert!(
        !again.status.success(),
        "a permanently dead service must fail `up`: {detail}"
    );
    assert!(
        stderr.contains("next: slipfork logs web"),
        "a down service must fail with the exact log remedy: {detail}"
    );
    assert!(
        !stdout.contains("✓ Ready") && !stdout.contains('✓'),
        "a failed `up` must not print a Ready check mark: {detail}"
    );

    let json = run(&project, &env, &["up", "--json"]);
    let detail = failure_detail(&env, &json);
    let stdout = String::from_utf8_lossy(&json.stdout);
    let verdict_line = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_else(|| panic!("up --json printed no verdict: {stdout}\n{detail}"));
    let verdict: serde_json::Value = serde_json::from_str(verdict_line).unwrap_or_else(|error| {
        panic!("up --json verdict does not parse: {error}\n{stdout}\n{detail}")
    });
    assert!(
        !json.status.success(),
        "a permanently dead service must fail `up --json`: {verdict}\n{detail}"
    );
    assert_eq!(
        verdict["error"]["code"], "world-unhealthy",
        "the JSON verdict must carry the world failure code: {verdict}\n{detail}"
    );
    assert_eq!(
        verdict["error"]["next"], "slipfork logs web",
        "the JSON `next` must be the embedded log remedy, not a generic fallback: {verdict}\n{detail}"
    );
    assert!(
        verdict.get("url").is_none(),
        "an unhealthy world must carry no success `url`: {verdict}\n{detail}"
    );
    let message = verdict["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("web") && !message.contains("next:"),
        "the JSON message must name the dead service without a duplicate `next:` line: {verdict}\n{detail}"
    );
    assert!(
        !web_running("after up --json"),
        "the JSON verdict must match `ps`: web stays down\n{detail}"
    );
}
