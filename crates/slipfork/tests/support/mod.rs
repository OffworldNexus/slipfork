//! Shared integration-test harness for the thin CLI and `git-remote-slipfork`.
//!
//! Every helper is real: a disk-backed scratch directory (never tmpfs), a real Git repository, and
//! the actual `slipfork` binary. A Btrfs host drives a real `slipforkd` child; every other host
//! serves the same `slipfork::daemon` in-process over the one
//! [`slipfork::daemon::compose_daemon`] graph with `BtrfsEngine::with_driver(TestTreeDriver)`.
//! The daemon's runtime directory lives under `$XDG_RUNTIME_DIR/slipfork` (or the explicit
//! `--runtime-dir`) so the `git-remote-slipfork` helper resolves exactly the same control socket.
//!
//! Each integration-test binary compiles this module independently and links only the subset it
//! uses, so the suites declare `pub mod support;`: the shared surface stays crate-reachable while
//! a genuinely dead private helper still trips the `dead_code` lint.

use std::fs::{self, OpenOptions};
use std::io::BufRead;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use slipfork::daemon::{DaemonWiring, compose_daemon, host_caches};
use slipfork::layout::{Layout, ensure_private_dir};
use slipfork::runtime::{runtime_assets, target_arch};
use slipfork_edge::{DaemonControlClient, DaemonControlServer, HttpIngress, ListenerSet};
use slipfork_engine::btrfs::{BTRFS_SUPER_MAGIC, KernelBtrfsDriver, SubvolumeDriver};
use slipfork_engine::{InheritedVar, current_uid_gid};
use slipfork_ospp::{BrowserOpener, LivePairingUrl, OsppError};
use slipfork_testkit::btrfs::TestTreeDriver;
use slipfork_testkit::{DAEMON_STARTUP_DEADLINE, SHUTDOWN_DEADLINE, poll_until};
use tempfile::TempDir;

/// Disk-backed scratch directory with a short path (AF_UNIX `sun_path` budget), never `/tmp`.
pub fn disk_tempdir(prefix: &str) -> TempDir {
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME")
        .join(".cache")
        .join("slipfork-cli-tests");
    fs::create_dir_all(&base).expect("create test cache base");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(base)
        .expect("create disk tempdir")
}

pub fn run_git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Slipfork Test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
            // The suites clone and add local repositories as submodules, which git's default
            // `protocol.file` policy refuses; the fixtures are paths this test created itself.
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Test fixture `Dockerfile`: a real supported `L0` stage with the client tools the CLI
/// critical tests exercise. Every world boot builds this through the production
/// `DockerfileStageBuilder`; nothing mocks Docker or the pinned static `crun`.
const FIXTURE_DOCKERFILE: &str = "\
FROM alpine:3.21 AS app
RUN apk add --no-cache python3
WORKDIR /workspace
";

/// One git repository carrying an explicit `snow.yml` body and a buildable `Dockerfile`.
pub fn repo_with_manifest(scratch: &Path, project: &str, snow: &str) -> PathBuf {
    let repo = scratch.join(project);
    fs::create_dir_all(&repo).expect("create repo");
    run_git(&repo, &["init", "-q"]);
    fs::write(repo.join("snow.yml"), snow).expect("write snow.yml");
    fs::write(repo.join("Dockerfile"), FIXTURE_DOCKERFILE).expect("write Dockerfile");
    fs::write(
        repo.join("slipfork.toml"),
        format!("project = \"{project}\"\n"),
    )
    .expect("write slipfork.toml");
    run_git(&repo, &["add", "snow.yml", "slipfork.toml", "Dockerfile"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

/// Locate (or build once) the engine's `slipfork-world-init` helper next to the test binaries.
///
/// The helper is rebuilt when it is missing **or** older than any `slipfork-engine/src/**/*.rs`
/// file, so a standalone `cargo test -p slipfork --test …` can never execute a helper built before
/// the last engine edit. The staleness check runs at most once per test process
/// ([`std::sync::Once`]) and shells out only when a rebuild is actually required.
fn world_init_helper() -> PathBuf {
    fn helper_path() -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_slipforkd"))
            .parent()
            .expect("daemon binary parent")
            .join("slipfork-world-init")
    }

    static BUILD_ONCE: std::sync::Once = std::sync::Once::new();
    BUILD_ONCE.call_once(|| {
        let helper = helper_path();
        if helper_is_current(&helper) {
            return;
        }
        let output = Command::new(env!("CARGO"))
            .args(["build", "-p", "slipfork-engine", "--bins"])
            .output()
            .unwrap_or_else(|error| {
                panic!(
                    "cannot run `cargo build -p slipfork-engine --bins` to refresh {}: {error}",
                    helper.display()
                )
            });
        assert!(
            output.status.success(),
            "`cargo build -p slipfork-engine --bins` failed ({}), so {} would be stale:\n{}",
            output.status,
            helper.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            helper.is_file(),
            "expected `cargo build -p slipfork-engine --bins` to produce {}",
            helper.display()
        );
    });
    helper_path()
}

/// `true` when `helper` exists and is at least as new as every `slipfork-engine/src/**/*.rs` file.
///
/// An unreadable timestamp (or source tree) counts as stale: without mtimes the helper's freshness
/// cannot be proven, so it is rebuilt instead of silently trusted.
fn helper_is_current(helper: &Path) -> bool {
    let Ok(helper_modified) = fs::metadata(helper).and_then(|metadata| metadata.modified()) else {
        return false;
    };
    let mut pending = vec![
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the workspace crates directory")
            .join("slipfork-engine")
            .join("src"),
    ];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            return false;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return false;
            };
            let Ok(file_type) = entry.file_type() else {
                return false;
            };
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if entry.path().extension().is_some_and(|ext| ext == "rs") {
                let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
                    return false;
                };
                if modified > helper_modified {
                    return false;
                }
            }
        }
    }
    true
}

/// The explicit roots one scratch directory serves.
///
/// `daemon.cli` exports exactly these values (`XDG_DATA_HOME`/`XDG_RUNTIME_DIR`) so an in-process
/// daemon and a spawned `slipforkd` resolve the same layout from the tests' point of view.
#[derive(Debug, Clone)]
pub struct DaemonLayout {
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub data_home: PathBuf,
}

impl DaemonLayout {
    /// The default roots every CLI-critical suite uses.
    pub fn under(scratch: &Path) -> Self {
        Self {
            state_dir: scratch.join("state"),
            runtime_dir: scratch.join("xdg-runtime").join("slipfork"),
            data_home: scratch.join("state").join("xdg-data"),
        }
    }
}

/// The one real `slipforkd` argv/env builder (never a `PATH` guess).
///
/// `slipfork` (the live suite's refusal proof) and [`Daemon`] (the child backend) both call this,
/// so the production composition root is invoked identically everywhere.
pub fn daemon_command(scratch: &Path, layout: &DaemonLayout) -> (Command, PathBuf) {
    let (browser, browser_marker) = browser_opener(scratch);
    let mut command = Command::new(env!("CARGO_BIN_EXE_slipforkd"));
    command
        .arg("--state-dir")
        .arg(&layout.state_dir)
        .arg("--runtime-dir")
        .arg(&layout.runtime_dir)
        .arg("--listen")
        .arg("127.0.0.1:0")
        .arg("--world-init")
        .arg(world_init_helper())
        .arg("--crun")
        .arg(pinned_crun())
        .env("XDG_DATA_HOME", &layout.data_home)
        .env("XDG_RUNTIME_DIR", runtime_parent(&layout.runtime_dir))
        .env("BROWSER", browser)
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    (command, browser_marker)
}

/// Wait for a spawned `slipforkd` to serve `<runtime_dir>/daemon.sock`.
///
/// An exited child is terminal, not a retryable state: fail immediately with its log instead of
/// sleeping through the whole deadline.
fn await_daemon_served(
    child: &mut Child,
    runtime_dir: &Path,
    log_path: &Path,
) -> Result<(), String> {
    let client = DaemonControlClient::bind(runtime_dir);
    let served = poll_until(DAEMON_STARTUP_DEADLINE, || {
        if client.is_served() {
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(daemon_log(
                log_path,
                &format!(
                    "slipforkd exited with {status} before serving {}",
                    client.socket_path().display()
                ),
            ));
        }
        Err(format!(
            "{} is not served yet",
            client.socket_path().display()
        ))
    });
    if let Err(last) = served {
        let _ = child.kill();
        let _ = child.wait();
        return Err(daemon_log(
            log_path,
            &format!(
                "slipforkd did not serve {} within {DAEMON_STARTUP_DEADLINE:?} ({last})",
                client.socket_path().display()
            ),
        ));
    }
    Ok(())
}

/// Ensure a daemon serves `layout`'s `<runtime_dir>/daemon.sock` for this test process.
///
/// On a Btrfs host the CLI auto-spawns the real child, so this is a no-op. Everywhere else the
/// in-process composition starts once and lives until the test process exits.
pub fn ensure_daemon(scratch: &Path, layout: &DaemonLayout) {
    if host_is_btrfs(&layout.state_dir) {
        return;
    }
    if DaemonControlClient::bind(&layout.runtime_dir).is_served() {
        return;
    }
    let daemon = Daemon::start_in(scratch, layout).expect("start the test daemon in-process");
    // An in-process control server cannot be un-wound and a child daemon must outlive this
    // helper, so the test process itself owns the daemon until it exits: never run `Drop`.
    std::mem::forget(daemon);
}

/// The single `EngineError::BtrfsRequired` diagnostic contract every critical suite asserts.
pub fn assert_btrfs_refusal(diagnostic: &str) {
    for needle in [
        "is not on Btrfs",
        "0x9123683e",
        "BTRFS_SUPER_MAGIC",
        "user_subvol_rm_allowed,noatime",
    ] {
        assert!(
            diagnostic.contains(needle),
            "the refusal must name {needle}: {diagnostic}"
        );
    }
    assert!(
        !diagnostic.to_lowercase().contains("reflink"),
        "a refused boot must never advertise a fallback substrate: {diagnostic}"
    );
}

/// A live daemon plus the explicit roots it serves.
///
/// A Btrfs host gets the real `slipforkd` child process (the production composition root). A
/// non-Btrfs host gets [`slipfork::daemon`] served in-process over the shared, test-only
/// `TestTreeDriver`, so the CLI-critical suites still drive the real `slipfork` /
/// `git-remote-slipfork` binaries over `daemon.sock` instead of pretending the Btrfs substrate
/// exists.
pub struct Daemon {
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub browser_marker: PathBuf,
    backend: Backend,
}

enum Backend {
    /// Real `slipforkd` child process (Btrfs hosts and every explicit-`PATH` daemon).
    Child(Child),
    /// In-process `slipfork::daemon` over `BtrfsEngine::with_driver(TestTreeDriver)`.
    InProcess {
        /// The bound `<state_dir>/http-ingress.sock` (`0600`) ingress server.
        _ingress: HttpIngress,
        /// Set once [`Daemon::shutdown`] unlinks `daemon.sock`.
        closed: bool,
    },
}

impl Daemon {
    pub fn start(scratch: &Path) -> Self {
        let layout = DaemonLayout::under(scratch);
        Self::start_in(scratch, &layout).unwrap_or_else(|message| panic!("{message}"))
    }

    /// Serve `layout` for `scratch`: the real child daemon on a Btrfs host, the in-process
    /// composition elsewhere.
    pub fn start_in(scratch: &Path, layout: &DaemonLayout) -> Result<Self, String> {
        ensure_private_dir(&layout.state_dir)?;
        ensure_private_dir(&layout.runtime_dir)?;
        if host_is_btrfs(&layout.state_dir) {
            Self::spawn_child(scratch, layout, None)
        } else {
            Ok(Self::serve_in_process(scratch, layout))
        }
    }

    /// Start the real `slipforkd` child with an explicit `PATH`.
    ///
    /// `PATH` isolation is a property of a separate daemon process (the `jj` adapter probes the
    /// ambient `PATH`), so this never uses the in-process backend. The daemon log is returned
    /// verbatim when `slipforkd` refuses the host before serving (a non-Btrfs `--state-dir`).
    pub fn try_start_with_path(scratch: &Path, path: Option<&Path>) -> Result<Self, String> {
        let layout = DaemonLayout::under(scratch);
        ensure_private_dir(&layout.state_dir)?;
        ensure_private_dir(&layout.runtime_dir)?;
        Self::spawn_child(scratch, &layout, path)
    }

    /// `true` when this daemon is served by [`slipfork::daemon`] in the test process.
    pub fn is_in_process(&self) -> bool {
        matches!(self.backend, Backend::InProcess { .. })
    }

    /// Stop serving `<runtime_dir>/daemon.sock` (idempotent).
    ///
    /// An in-process server cannot be un-wound, so its socket path is unlinked: every client then
    /// observes a dead daemon exactly like a killed child. A child is killed and reaped.
    pub fn shutdown(&mut self) {
        match &mut self.backend {
            Backend::Child(child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
            Backend::InProcess { closed, .. } => {
                if !*closed {
                    let _ = fs::remove_file(self.runtime_dir.join("daemon.sock"));
                    *closed = true;
                }
            }
        }
    }

    fn spawn_child(
        scratch: &Path,
        layout: &DaemonLayout,
        path: Option<&Path>,
    ) -> Result<Self, String> {
        let log_path = scratch.join("daemon.log");
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&log_path)
            .map_err(|err| format!("cannot open {}: {err}", log_path.display()))?;
        let stderr = log
            .try_clone()
            .map_err(|err| format!("cannot duplicate {}: {err}", log_path.display()))?;
        let (mut command, browser_marker) = daemon_command(scratch, layout);
        command.stderr(stderr);
        if let Some(path) = path {
            command.env("PATH", path);
        }
        let mut child = command.spawn().expect("spawn slipforkd");
        await_daemon_served(&mut child, &layout.runtime_dir, &log_path)?;
        Ok(Self {
            state_dir: layout.state_dir.clone(),
            runtime_dir: layout.runtime_dir.clone(),
            browser_marker,
            backend: Backend::Child(child),
        })
    }

    /// Serve `slipfork::daemon` in-process over the shared test `SubvolumeDriver`.
    ///
    /// The production [`compose_daemon`] graph is reused verbatim: only the driver
    /// ([`TestTreeDriver`]) and the browser opener (a marker stub) differ from `slipforkd`.
    fn serve_in_process(scratch: &Path, spec: &DaemonLayout) -> Self {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set"));
        let runtime_parent = runtime_parent(&spec.runtime_dir);
        let layout = Layout::resolve(
            Some(spec.state_dir.clone()),
            Some(spec.runtime_dir.clone()),
            &home,
            None,
            Some(runtime_parent),
            Some(&spec.data_home),
        );
        let (uid, gid) = current_uid_gid();
        let runtime = runtime_assets(
            &spec.state_dir,
            Some(pinned_crun()),
            Some(world_init_helper()),
        )
        .expect("in-process runtime assets");
        let target_arch = target_arch().expect("target architecture");
        // The HTTP ingress listener is bound for real (the Btrfs engine refuses a world whose
        // `local_ingress` socket is missing) and held without accepting: the minted origins stay
        // real without serving HTTP from the test process.
        let http_listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind the ingress listener");
        let http_ingress_port = http_listener
            .local_addr()
            .expect("ingress listener address")
            .port();
        let inherited = InheritedVar::ALL
            .into_iter()
            .filter_map(|var| std::env::var(var.name()).ok().map(|value| (var, value)))
            .collect();
        let browser_marker = scratch.join("browser-used");
        let graph = compose_daemon(DaemonWiring {
            layout,
            driver: Arc::new(TestTreeDriver::btrfs()),
            browser: Arc::new(MarkerBrowserOpener::new(browser_marker.clone())),
            http_ingress_port,
            forward_port: None,
            host_caches: host_caches(&home),
            inherited,
            runtime,
            target_arch: target_arch.to_owned(),
            uid,
            gid,
            xdg_runtime_dir: Some(runtime_parent.to_path_buf()),
        })
        .expect("in-process daemon graph");
        let server = DaemonControlServer::bind(&spec.runtime_dir, graph.service)
            .expect("bind the in-process daemon control socket");
        let ingress = HttpIngress::bind(
            &spec.state_dir,
            ListenerSet {
                http_listeners: vec![http_listener],
                forward_listener: None,
            },
            graph.registry,
        )
        .expect("bind the in-process HTTP ingress socket");
        // `DaemonControlServer::serve` owns its accept loop; the process ending is the shutdown.
        std::thread::spawn(move || {
            let _ = server.serve();
        });
        Self {
            state_dir: spec.state_dir.clone(),
            runtime_dir: spec.runtime_dir.clone(),
            browser_marker,
            backend: Backend::InProcess {
                _ingress: ingress,
                closed: false,
            },
        }
    }

    pub fn cli(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(cwd, args).output().expect("spawn slipfork")
    }

    pub fn cli_with_stdin(&self, cwd: &Path, args: &[&str], stdin: &str) -> Output {
        use std::io::Write;
        let mut child = self
            .command(cwd, args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn slipfork");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stdin.as_bytes())
            .expect("write stdin");
        child.wait_with_output().expect("wait for slipfork")
    }

    /// Spawn a streaming CLI command with piped stdout (for `logs --follow` / `forward`).
    pub fn cli_spawn(&self, cwd: &Path, args: &[&str]) -> Child {
        self.command(cwd, args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn slipfork stream")
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_slipfork"));
        command
            .arg("--state-dir")
            .arg(&self.state_dir)
            .arg("--runtime-dir")
            .arg(&self.runtime_dir)
            .args(args)
            .current_dir(cwd)
            .env("XDG_DATA_HOME", self.state_dir.join("xdg-data"))
            .env(
                "XDG_RUNTIME_DIR",
                self.runtime_dir.parent().expect("runtime parent"),
            );
        command
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// `true` when `path` (or its nearest existing ancestor) is on a real Btrfs mount.
pub fn host_is_btrfs(path: &Path) -> bool {
    KernelBtrfsDriver::new()
        .probe_facts(path)
        .is_ok_and(|facts| facts.fstype == BTRFS_SUPER_MAGIC)
}

/// The daemon log appended to a failed child start.
fn daemon_log(log_path: &Path, message: &str) -> String {
    format!(
        "{message}:\n{}",
        fs::read_to_string(log_path).unwrap_or_else(|error| format!("<logs unavailable: {error}>"))
    )
}

/// `XDG_RUNTIME_DIR` for a daemon runtime directory (`<runtime_dir>`'s parent).
fn runtime_parent(runtime_dir: &Path) -> &Path {
    runtime_dir.parent().expect("runtime directory parent")
}

/// In-process [`BrowserOpener`]: records that the pairing page was opened by creating the empty
/// marker file the browser stub writes in child mode (never persists the live handle).
struct MarkerBrowserOpener {
    marker: PathBuf,
}

impl MarkerBrowserOpener {
    fn new(marker: PathBuf) -> Self {
        Self { marker }
    }
}

impl BrowserOpener for MarkerBrowserOpener {
    fn open_pairing_url(&self, _url: LivePairingUrl) -> Result<(), OsppError> {
        fs::write(&self.marker, b"").map_err(|err| OsppError::PairingUrl {
            reason: format!("cannot record the browser marker: {err}"),
        })
    }
}

fn browser_opener(scratch: &Path) -> (PathBuf, PathBuf) {
    let marker = scratch.join("browser-used");
    let script = scratch.join("browser.sh");
    fs::write(&script, format!("#!/bin/sh\n: > '{}'\n", marker.display()))
        .expect("write browser stub");
    let mut permissions = fs::metadata(&script)
        .expect("browser stub metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("make browser stub executable");
    (script, marker)
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn assert_success(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} failed with {}:\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
}

/// Wait until the daemon serving `runtime_dir` stops accepting on `<runtime_dir>/daemon.sock`.
///
/// Probes through the production [`DaemonControlClient::is_served`] the CLI's zero-setup auto-spawn
/// uses, so this proof can never drift from the check a later invocation would really make.
/// Returns the still-served socket description once [`SHUTDOWN_DEADLINE`] expires, so each caller
/// panics with its own captured invocation output.
pub fn wait_daemon_teardown(runtime_dir: &Path) -> Result<(), String> {
    let client = DaemonControlClient::bind(runtime_dir);
    let socket = client.socket_path().to_owned();
    poll_until(SHUTDOWN_DEADLINE, || {
        if client.is_served() {
            Err(format!("{} still accepts connections", socket.display()))
        } else {
            Ok(())
        }
    })
    .map_err(|last| format!("{last} after {SHUTDOWN_DEADLINE:?}"))
}

/// Bound on one streamed child line.
///
/// This is the one legitimate non-poll wait: the producer thread blocks on the child's stdout pipe
/// and the consumer must block on the channel until the next line arrives, so a `poll_until` loop
/// would only re-implement `recv_timeout` on the same channel. The blocking bound is therefore a
/// named deadline, not a polling interval.
const STREAM_LINE_DEADLINE: Duration = Duration::from_secs(30);

/// Read lines from a child's stdout until `predicate` accepts one or the deadline expires.
pub fn wait_for_line(child: &mut Child, predicate: impl Fn(&str) -> bool) -> String {
    let stdout = child.stdout.take().expect("child stdout");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + STREAM_LINE_DEADLINE;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(line) if predicate(&line) => return line,
            Ok(_) => {}
            Err(error) => panic!(
                "streaming command produced no matching line within {STREAM_LINE_DEADLINE:?}: {error}"
            ),
        }
    }
}

/// Compile-time pin mirrored for the CLI tests (`slipfork-engine::rootfs::pin` is the source).
const CRUN_FILE_NAME: &str = "crun-1.20-x86_64-ff8407185ec4.bin";
const CRUN_SHA256: &str = "ff8407185ec474d7c60f697032b7b1fc1fb028fbd0bf13c7d5da9a7d9a479e38";
const CRUN_URL: &str = "https://github.com/containers/crun/releases/download/1.20/crun-1.20-linux-amd64-disable-systemd";

/// The pinned static `crun`, downloaded once into `~/.cache/slipfork/test-runtime` and verified.
fn pinned_crun() -> PathBuf {
    let cache = PathBuf::from(std::env::var("HOME").expect("HOME"))
        .join(".cache")
        .join("slipfork")
        .join("test-runtime");
    fs::create_dir_all(&cache).expect("create runtime cache");
    let path = cache.join(CRUN_FILE_NAME);
    if verified(&path) {
        return path;
    }
    let temporary = cache.join(format!("{CRUN_FILE_NAME}.download"));
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "2", "-o"])
        .arg(&temporary)
        .arg(CRUN_URL)
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "failed to fetch the pinned static crun: {error}\n  fix: install curl, or place {CRUN_FILE_NAME} (sha256 {CRUN_SHA256}) in {}",
                cache.display()
            )
        });
    assert!(status.success(), "failed to download {CRUN_URL}");
    assert!(
        verified(&temporary),
        "downloaded crun does not match the compile-time pin {CRUN_SHA256}"
    );
    fs::rename(&temporary, &path).expect("install pinned crun");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod crun");
    path
}

fn verified(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes)) == CRUN_SHA256
}

/// Locate one executable on the current `PATH`.
fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}

/// The Jujutsu binary: `jj` on `PATH`, else the developer cache used by this workspace.
pub fn jj_binary() -> Option<PathBuf> {
    find_on_path("jj").or_else(|| {
        let cached = PathBuf::from(std::env::var_os("HOME")?)
            .join(".cache")
            .join("slipfork-tools")
            .join("jj")
            .join("jj");
        cached.is_file().then_some(cached)
    })
}

/// A directory exposing only `git` (so a `.jj` repository can be resolved while `jj` is absent).
pub fn git_only_path(scratch: &Path) -> PathBuf {
    let directory = scratch.join("git-only-bin");
    fs::create_dir_all(&directory).expect("create git-only bin dir");
    let git = find_on_path("git").expect("git must be on PATH for the .jj routing test");
    std::os::unix::fs::symlink(&git, directory.join("git")).expect("symlink git");
    directory
}
