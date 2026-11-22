//! Live lab journey: a real world's services, ports, and routes through `status` and `ps`, plus a
//! real PTY `DECSTBM` boot panel, orphaned-pod reconcile, and zero-escape plain paths.
//!
//! Builds a real Btrfs subvolume world from a `Forkfile`, boots it with `slipfork up` on a real
//! `openpty` PTY, and asserts the scroll region, 24-cell panel, one row per task, region restore on
//! success/failure, and `SIGWINCH` re-application. A second journey edits the `Forkfile`, proves the
//! undeclared pod is yellow `orphaned`, then reaps it with `slipfork up`.
//! The lab is opt-in: without `SLIPFORK_TEST_ROOT` the test fails closed with the exact remedy.

use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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
image box from python:3.13-slim {\n  pre-deploy: sh -c 'echo rolling-tail-1; sleep 1.4; echo rolling-tail-2'\n  service worker {\n    run: sleep 3600\n  }\n}\n";

const FAIL_FORKFILE: &str = "# forkfile: 0.1\n\
image box from python:3.13-slim {\n  pre-deploy: sh -c 'exit 3'\n  service worker {\n    run: sleep 3600\n  }\n}\n";

const ONE_SERVICE: &str = "# forkfile: 0.1\n\
postgres pg {\n  version 18\n}\n\
redis cache\n\
s3 uploads {\n  policy public-read\n  cdn assets-cdn\n}\n\
image box from python:3.13-slim {\n\
\x20 service alpha {\n    port 8101\n    route /alpha\n    run: python3 -m http.server 8101\n  }\n\
}\n";

const BRAILLE: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

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
    assert!(
        is_btrfs_path(&root).unwrap_or(false),
        "fail closed: `{}` is not on Btrfs.\nnext: SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test",
        root.display()
    );
    root
}

/// The one runtime/daemon shared by every test in this binary (`:80` can have a single owner).
fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/dev/shm/slipfork-live-runtime"))
}

/// A `{pid}-{nanos}` suffix so two live tests never share a project or world.
fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock reads after the epoch")
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn scoped_world(suffix: &str) -> String {
    format!("live{}", suffix.replace('-', ""))
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

fn run(project: &Path, runtime: &Path, args: &[&str]) -> Output {
    let mut child = Command::new(binary())
        .args(args)
        .current_dir(project)
        .env("XDG_RUNTIME_DIR", runtime)
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
}

/// Runs `args` under a real `openpty` PTY (120x40 unless resized); `script` is not needed.
fn run_pty(
    project: &Path,
    runtime: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    resize: Option<(Duration, u16, u16)>,
) -> PtyRun {
    let mut master = 0;
    let mut slave = 0;
    let winsize = libc::winsize {
        ws_row: 40,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `openpty` fills both descriptors and reads one valid `winsize`.
    let allocated = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &winsize,
        )
    };
    assert_eq!(allocated, 0, "openpty must allocate a real PTY");
    // SAFETY: the descriptors are freshly owned by this process.
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    // SAFETY: `master` is owned here and closed when `File` drops.
    let master = unsafe { fs::File::from_raw_fd(master) };
    let master_fd = master.as_raw_fd();
    let mut command = Command::new(binary());
    command
        .args(args)
        .current_dir(project)
        .env("XDG_RUNTIME_DIR", runtime)
        .env_remove("NO_COLOR")
        .env("TERM", "xterm-256color")
        .stdin(Stdio::from(slave.try_clone().expect("clone PTY slave")))
        .stdout(Stdio::from(slave.try_clone().expect("clone PTY slave")))
        .stderr(Stdio::from(slave));
    for (key, value) in envs {
        command.env(key, value);
    }
    // SAFETY: the closure only performs `setsid(2)`/`ioctl(TIOCSCTTY)` between `fork` and `exec`.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("spawn the PTY child");
    // `Command` still owns the parent's three slave descriptors; until it is dropped the master
    // never sees EOF, so closing them here is what lets the reader finish.
    drop(command);
    let finished = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&finished);
    let reader = std::thread::spawn(move || {
        let mut master = master;
        let mut out = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            let mut poll = libc::pollfd {
                fd: master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid `pollfd` lives across the call; a 100ms tick bounds the wait so a
            // finished child can never leave the reader blocked on a held slave descriptor.
            let ready = unsafe { libc::poll(&mut poll, 1, 100) };
            if ready < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            if ready == 0 {
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                continue;
            }
            if poll.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
                continue;
            }
            match master.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => out.extend_from_slice(&chunk[..read]),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    continue;
                }
                Err(_) => break,
            }
        }
        out
    });
    if let Some((delay, rows, cols)) = resize {
        std::thread::sleep(delay);
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: the master descriptor stays open until the reader thread finishes.
        unsafe { libc::ioctl(master_fd, libc::TIOCSWINSZ, &size) };
    }
    let status = wait_bounded(&mut child, args);
    finished.store(true, Ordering::Relaxed);
    let bytes = reader.join().expect("join the PTY reader");
    PtyRun {
        output: String::from_utf8_lossy(&bytes).into_owned(),
        status,
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

/// Stops **this world** and deletes **this project** on every exit path.
///
/// The runtime/daemon is shared by the whole binary (one `:80` owner), so the guard never shuts it
/// down or kills it; the outer lab cleanup owns that process.
struct WorldGuard {
    project: PathBuf,
    runtime: PathBuf,
    store_root: PathBuf,
    world: String,
    /// Set when step 1 fails: leave the project on disk for the lab diagnosis.
    keep: bool,
}

impl Drop for WorldGuard {
    fn drop(&mut self) {
        if self.runtime.join("daemon.sock").exists() {
            let _ = run_quiet(&self.project, &self.runtime, &["down"]);
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
    }
}

/// One best-effort CLI run with a short budget; the guard must never hang or panic.
fn run_quiet(project: &Path, runtime: &Path, args: &[&str]) -> bool {
    let Ok(mut child) = Command::new(binary())
        .args(args)
        .current_dir(project)
        .env("XDG_RUNTIME_DIR", runtime)
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

fn services(world: &serde_json::Value) -> Vec<serde_json::Value> {
    world["services"].as_array().cloned().unwrap_or_default()
}

#[test]
fn ansi_split_bar_halves_measure_as_one_24_cell_run() {
    let styled = "\u{1b}[1;36m██████████████\u{1b}[0m\u{1b}[2m░░░░░░░░░░\u{1b}[0m";
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
    fs::create_dir_all(&dir).expect("create the PTY scratch directory");
    let began = Instant::now();
    let out = run_pty(&dir, &dir, &["--help"], &[], None);
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
    let runtime = runtime_dir();
    fs::create_dir_all(&runtime).expect("create the live runtime directory");
    let suffix = unique_suffix();
    let project_name = format!("live-{suffix}");
    let world_id = project_name.clone();
    let project = root.join(&project_name);
    prepare(&project, &store_root, &world_id, SUITE_FORKFILE);
    let mut guard = WorldGuard {
        project: project.clone(),
        runtime: runtime.clone(),
        store_root,
        world: world_id.clone(),
        keep: false,
    };

    // 1. One rich colour-PTY boot: DECSTBM region, 24-cell panel maths, one row per task.
    let boot = run_pty(&project, &runtime, &["up"], &[], None);
    if !boot.status.success() {
        guard.keep = true;
        let supervisor = fs::read_to_string(project.join(".slipfork/supervisor.log"))
            .unwrap_or_else(|error| format!("<no supervisor.log: {error}>"));
        panic!(
            "PTY `slipfork up` failed\nstdout: {:?}\nstderr: {:?}\nsupervisor.log:\n{supervisor}",
            boot.output, boot.output
        );
    }
    assert!(
        boot.output.contains("\u{1b}[1;39r"),
        "a 40-row PTY must set `DECSTBM 1;39`: {:?}",
        boot.output
    );
    assert!(
        boot.output.contains("\u{1b}[40;1H"),
        "the panel must live on row 40: {:?}",
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
        let expected = (done * BAR_CELLS + total / 2) / total;
        assert_eq!(
            filled, expected,
            "filled cells must be round({done}/{total}*{BAR_CELLS}) = {expected}: {readable}"
        );
    }
    assert!(
        readable.contains("✓ resource") && readable.contains("pg ready"),
        "the boot must print a finished row for the golden PostgreSQL resource: {readable}"
    );
    assert_eq!(
        readable.matches("✓ resource").count(),
        3,
        "exactly one aligned row per finished task: {readable}"
    );
    assert!(
        readable.contains("alpha") && readable.contains("beta"),
        "both services must render a finished row: {readable}"
    );
    assert!(
        !readable.contains("sha256") && !readable.contains("dockerfile"),
        "build/log bytes must never reach the terminal: {readable}"
    );
    let verdict_at = boot
        .output
        .find("✓ Ready")
        .unwrap_or_else(|| panic!("the final verdict must print: {:?}", boot.output));
    let before_verdict = &boot.output[..verdict_at];
    assert!(
        before_verdict.contains("\u{1b}[2K") && before_verdict.contains("\u{1b}[r"),
        "the panel row must be cleared and `ESC[r` restored before the verdict: {:?}",
        boot.output
    );
    assert!(
        boot.output[verdict_at..].contains(&format!("{world_id}.localhost")),
        "the verdict must carry the world URL: {:?}",
        boot.output
    );

    // 2. The typed status/ps surfaces: rows, resources, colours, and `--json` fields.
    let status = run(&project, &runtime, &["status", "--json"]);
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

    let human = run(&project, &runtime, &["status"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).expect("status is UTF-8");
    assert!(human.contains(&format!("● {world_id}")), "{human}");
    assert!(human.contains("postgres 18"), "{human}");
    assert!(human.contains("redis 7"), "{human}");
    assert!(human.contains("s3 (public-read)"), "{human}");
    assert!(!human.contains('\u{1b}'), "{human}");

    let ps = run(&project, &runtime, &["ps", "--json"]);
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
    let scoped = run(&project, &runtime, &["ps"]);
    assert!(scoped.status.success());
    assert!(String::from_utf8_lossy(&scoped.stdout).contains("postgres 18"));

    // 3. Delete `beta`: the live pod stays visible yellow `orphaned`, then `up` reaps it.
    fs::write(project.join("Forkfile"), ONE_SERVICE).expect("write the reduced Forkfile");
    assert!(project.join("Forkfile").is_file());
    let ps = run(&project, &runtime, &["ps", "--json"]);
    let reply: serde_json::Value = serde_json::from_slice(&ps.stdout).expect("ps --json");
    let rows = services(&reply["worlds"][0]);
    let beta = rows
        .iter()
        .find(|row| row["id"] == "beta")
        .unwrap_or_else(|| panic!("orphaned `beta` must stay visible: {rows:?}"));
    assert_eq!(beta["orphaned"], true, "{beta}");
    assert_eq!(beta["running"], true, "{beta}");
    let human = run(&project, &runtime, &["ps"]);
    let human = String::from_utf8(human.stdout).expect("ps is UTF-8");
    assert!(human.contains("orphaned"), "{human}");
    let coloured = run_pty(&project, &runtime, &["ps"], &[], None);
    assert!(
        coloured.output.contains("\u{1b}[33morphaned"),
        "`orphaned` must be yellow on a colour TTY: {:?}",
        coloured.output
    );

    let up = run(&project, &runtime, &["up"]);
    assert!(
        up.status.success(),
        "reconcile `up` failed: {}",
        String::from_utf8_lossy(&up.stderr)
    );
    assert!(!String::from_utf8_lossy(&up.stdout).contains('\u{1b}'));
    let ps = run(&project, &runtime, &["ps", "--json"]);
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
        &runtime,
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

    // 4. A hook boot on the colour PTY: no hook bytes, and `SIGWINCH` re-applies the region.
    fs::write(project.join("Forkfile"), HOOK_FORKFILE).expect("write the hook Forkfile");
    assert!(project.join("Forkfile").is_file());
    let hook_boot = run_pty(
        &project,
        &runtime,
        &["up"],
        &[],
        Some((Duration::from_millis(700), 30, 100)),
    );
    assert!(
        hook_boot.status.success(),
        "PTY `slipfork up` with hook failed: {:?}",
        hook_boot.output
    );
    let hook_readable = plain(&hook_boot.output);
    assert!(
        !hook_readable.contains("rolling-tail-"),
        "hook log bytes must never reach the terminal: {hook_readable}"
    );
    assert!(
        hook_boot.output.contains("\u{1b}[1;39r"),
        "the initial 40-row region must be set: {:?}",
        hook_boot.output
    );
    assert!(
        hook_boot.output.contains("\u{1b}[1;29r"),
        "SIGWINCH must re-apply the region for the 30-row terminal: {:?}",
        hook_boot.output
    );
    assert!(
        hook_boot.output.contains("\u{1b}[30;1H"),
        "the panel must follow the resized bottom row: {:?}",
        hook_boot.output
    );

    // 4b. A failing hook restores the region before the one crisp failure line.
    let stopped = run(&project, &runtime, &["down"]);
    assert!(stopped.status.success(), "{stopped:?}");
    fs::write(project.join("Forkfile"), FAIL_FORKFILE).expect("write the failing Forkfile");
    assert!(project.join("Forkfile").is_file());
    let failed = run_pty(&project, &runtime, &["up"], &[], None);
    assert!(
        !failed.status.success(),
        "a failing hook must fail `up`: {:?}",
        failed.output
    );
    assert!(
        failed.output.rfind("\u{1b}[r").is_some(),
        "the failure path must restore the scroll region: {:?}",
        failed.output
    );
    let failed_readable = plain(&failed.output);
    assert!(
        failed_readable.contains("✗ hook") && failed_readable.contains("failed"),
        "the failed row must render: {failed_readable}"
    );
    assert!(
        failed_readable.contains("✗ Failed to run hook")
            && failed_readable.contains("check logs with: slipfork logs"),
        "the one failure sentence must point at the log: {failed_readable}"
    );
    // A failed boot may have no published live entry; best-effort cleanup before the next boot.
    let _ = run(&project, &runtime, &["down"]);

    // 4c. `NO_COLOR`/`TERM=dumb` on a real PTY emit zero escape bytes.
    fs::write(project.join("Forkfile"), ONE_SERVICE).expect("restore the service Forkfile");
    assert!(project.join("Forkfile").is_file());
    for (args, envs) in [
        (&["up"][..], &[("NO_COLOR", "1")][..]),
        (&["restart"][..], &[("TERM", "dumb")][..]),
    ] {
        let output = run_pty(&project, &runtime, args, envs, None);
        assert!(
            output.status.success(),
            "{args:?} on a plain PTY failed: {:?}",
            output.output
        );
        assert!(
            !output.output.contains("\u{1b}["),
            "{args:?} with {envs:?} emitted an escape: {:?}",
            output.output
        );
    }

    // 5. `down` keeps captures: every logs shape replays history, and `-f` follows the next `up`.
    let stopped = run(&project, &runtime, &["down"]);
    assert!(stopped.status.success(), "{stopped:?}");
    fs::write(project.join("Forkfile"), ONE_SERVICE).expect("restore the service Forkfile");
    let history = run(&project, &runtime, &["logs", "alpha"]);
    assert!(history.status.success(), "{history:?}");
    let history_text = String::from_utf8_lossy(&history.stdout).into_owned();
    assert!(history_text.contains("alpha"), "{history_text:?}");
    let dividers = history_text.matches("── alpha started ──").count();
    assert!(
        dividers > 0,
        "alpha history must carry a boot divider: {history_text:?}"
    );

    let bare = run(&project, &runtime, &["logs"]);
    assert!(bare.status.success(), "{bare:?}");
    assert!(
        String::from_utf8_lossy(&bare.stdout).contains("alpha │"),
        "{:?}",
        String::from_utf8_lossy(&bare.stdout)
    );
    let windowed = run(&project, &runtime, &["logs", "--since", "5m"]);
    assert!(windowed.status.success(), "{windowed:?}");
    assert!(
        String::from_utf8_lossy(&windowed.stdout).contains("alpha │"),
        "{:?}",
        String::from_utf8_lossy(&windowed.stdout)
    );
    let all = run(&project, &runtime, &["logs", "-a"]);
    assert!(all.status.success(), "{all:?}");
    assert!(
        String::from_utf8_lossy(&all.stdout).contains("alpha │"),
        "{:?}",
        String::from_utf8_lossy(&all.stdout)
    );

    // A follower started while the world is down picks up the next boot's capture divider.
    let mut follower = Command::new(binary())
        .args(["logs", "-f", "alpha"])
        .current_dir(&project)
        .env("XDG_RUNTIME_DIR", &runtime)
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
    let up = run(&project, &runtime, &["up"]);
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

    let stopped = run(&project, &runtime, &["down"]);
    assert!(stopped.status.success(), "{stopped:?}");
    wait_until(
        "the world to disappear from `ps`",
        Duration::from_secs(60),
        || {
            let ps = run(&project, &runtime, &["ps", "--json"]);
            ps.status.success() && String::from_utf8_lossy(&ps.stdout).contains("\"worlds\":[]")
        },
    );
}
