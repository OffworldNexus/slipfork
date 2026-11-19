//! Shared integration-test harness for the thin CLI and `git-remote-slipfork`.
//!
//! Every helper is real: a disk-backed scratch directory (never tmpfs), a real Git repository, and
//! the actual `slipfork` binary. A Btrfs host drives a real `slipforkd` child, or the same
//! [`slipfork::daemon::compose_daemon`] graph in-process over the real kernel
//! [`KernelBtrfsDriver`]; any other host fails closed through [`require_btrfs`] with one line naming
//! the missing substrate — never a fake driver.
//! The daemon's runtime directory is always `<state_dir>/run` (`ADR-0011` §2), and every helper
//! exports `XDG_CACHE_HOME=<state_dir parent>` so `git-remote-slipfork` derives the exact same
//! state/runtime roots without a client-side `--runtime-dir`.
//!
//! Each integration-test binary compiles this module independently and links only the subset it
//! uses, so the suites declare `pub mod support;`: the shared surface stays crate-reachable while
//! a genuinely dead private helper still trips the `dead_code` lint.

use std::fs::{self, OpenOptions};
use std::io::BufRead;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use slipfork::browser::Browser;
use slipfork::daemon::{DaemonWiring, compose_daemon, host_caches};
use slipfork::layout::{Layout, ensure_private_dir};
use slipfork::runtime::{runtime_assets, target_arch};
use slipfork_edge::{DaemonControlClient, DaemonControlServer, HttpIngress, ListenerSet};
use slipfork_engine::btrfs::{BTRFS_SUPER_MAGIC, KernelBtrfsDriver, mount_hint};
use slipfork_engine::current_uid_gid;
use slipfork_ospp::OsppEndpoint;
use slipfork_wire::control::IngressInfo;
use slipfork_world::InheritedVar;
// The one bounded-wait shape and its named deadlines come from the shared testkit, so no suite in
// this crate can drift into a second copy of the poll loop.
pub use slipfork_testkit::{
    DAEMON_STARTUP_DEADLINE, POLL_INTERVAL, SHUTDOWN_DEADLINE,
    poll_until_with_interval as poll_until,
};
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

/// The engine's `slipfork-world-init` helper Cargo built beside the integration-test binaries.
///
/// Cargo places every workspace binary in the same target directory as the test binaries it links,
/// so the helper is a sibling of `CARGO_BIN_EXE_slipforkd` and is always the one this build
/// produced — no test ever shells out to a nested `cargo build`. The `make rs-test` compile step
/// (`cargo test --workspace --bins`) builds it; a missing helper fails closed with the exact
/// remedy instead of silently booting a stale binary.
fn world_init_helper() -> PathBuf {
    let helper = PathBuf::from(env!("CARGO_BIN_EXE_slipforkd"))
        .parent()
        .expect("daemon binary parent")
        .join("slipfork-world-init");
    assert!(
        helper.is_file(),
        "{} is missing; run `make rs-test` (or `cargo build -p slipfork-engine --bins`) before \
         this suite",
        helper.display()
    );
    helper
}

/// The explicit roots one scratch directory serves.
///
/// `daemon.cli` exports exactly these values (`XDG_CACHE_HOME` / `XDG_DATA_HOME`) so an in-process
/// daemon, a spawned `slipforkd`, the thin client, and `git-remote-slipfork` resolve the same
/// layout from the tests' point of view.
#[derive(Debug, Clone)]
pub struct DaemonLayout {
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub data_home: PathBuf,
}

impl DaemonLayout {
    /// The default roots every CLI-critical suite uses (`runtime_dir` is always `<state>/run`).
    pub fn under(scratch: &Path) -> Self {
        let state_dir = scratch.join("slipfork");
        Self {
            runtime_dir: state_dir.join("run"),
            data_home: state_dir.join("xdg-data"),
            state_dir,
        }
    }
}

/// The one real `slipforkd` argv/env builder (never a `PATH` guess).
///
/// `slipfork` (the live suite's refusal proof) and [`Daemon`] (the child backend) both call this,
/// so the production composition root is invoked identically everywhere.
pub fn daemon_command(scratch: &Path, layout: &DaemonLayout) -> Command {
    daemon_command_with_ospp(scratch, layout, None)
}

/// The same argv builder with an optional boot-injected OSPP endpoint.
pub fn daemon_command_with_ospp(
    scratch: &Path,
    layout: &DaemonLayout,
    endpoint: Option<&OsppEndpoint>,
) -> Command {
    let (script, _) = browser_fixture(scratch);
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
        .env("HOSTNAME", "test-machine")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    if let Some(endpoint) = endpoint {
        command.arg("--ospp-endpoint").arg(endpoint.as_str());
    }
    // The daemon composes its own graph: give its root a real executable opener plus the host's
    // PATH, so the fixture is what `Browser::resolve` selects.
    let mut entries = Vec::new();
    if let Some(bin) = script.parent() {
        entries.push(bin.to_path_buf());
    }
    if let Some(existing) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&existing));
    }
    command.env(
        "PATH",
        std::env::join_paths(entries).expect("join the daemon PATH"),
    );
    command
}

/// A real 0755 `xdg-open` that records its argv (one argument per line) and writes nothing else.
///
/// The daemon root resolves it through `PATH`; no `Mock*`/`Marker*` type sits in the way.
fn browser_fixture(scratch: &Path) -> (PathBuf, PathBuf) {
    let bin = scratch.join("browser-bin");
    fs::create_dir_all(&bin).expect("browser bin directory");
    let record = scratch.join("browser-argv.txt");
    let script = bin.join("xdg-open");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\n: > '{}'\nfor arg in \"$@\"; do printf '%s\\n' \"$arg\" >> '{}'; done\n",
            record.display(),
            record.display()
        ),
    )
    .expect("write the browser fixture");
    let mut permissions = fs::metadata(&script)
        .expect("browser fixture metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("chmod the browser fixture");
    (script, record)
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
    let served = poll_until(DAEMON_STARTUP_DEADLINE, POLL_INTERVAL, || {
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

/// Ensure a daemon serves `layout`'s `<runtime_dir>/daemon.sock` for the calling test.
///
/// `command` names the `slipfork` invocation that needs it (for example `snow resolve`), so a host
/// that genuinely cannot serve a daemon fails closed naming the command instead of letting the CLI
/// print its own "no slipforkd on … — run `slipfork up`" line in a later assertion. A Btrfs state
/// root is required: the real `slipforkd` child serves it, or the same `slipfork::daemon`
/// composition in-process over the real kernel driver. Any other host fails closed through
/// [`require_btrfs`] with one line. The CLI still
/// never auto-spawns a read/diagnostic verb (`M3.28.1` R5, `ADR-0011` §2–§3); the harness starts the
/// daemon explicitly.
///
/// The caller binds the returned handle in the **test** scope (`let daemon = …`), never in a
/// per-invocation helper: the handle is the Drop guard. It kills and waits a real child daemon,
/// unlinks an in-process control socket, and reaps the worlds either daemon booted. A layout that
/// already has a served daemon is a harness bug: this test would not own the handle that reaps it,
/// so it panics instead of returning an unowned server.
pub fn ensure_daemon(scratch: &Path, layout: &DaemonLayout, command: &str) -> Daemon {
    assert!(
        !daemon_is_served(&layout.runtime_dir),
        "a daemon already serves `slipfork {command}` on {}; the harness must own the daemon it \
         reaps, so start this test on a fresh scratch layout",
        layout.runtime_dir.display()
    );
    Daemon::start_in(scratch, layout).unwrap_or_else(|log| {
        panic!(
            "the test harness cannot serve `slipfork {command}`, which needs a daemon on {}: {log}",
            layout.runtime_dir.display()
        )
    })
}

/// `true` when a daemon is serving `<runtime_dir>/daemon.sock` right now (the production probe).
pub fn daemon_is_served(runtime_dir: &Path) -> bool {
    DaemonControlClient::bind(runtime_dir).is_served()
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
/// A Btrfs host gets the real `slipforkd` child process (the production composition root), or the
/// same [`slipfork::daemon`] graph in-process over the real kernel [`KernelBtrfsDriver`]. A
/// non-Btrfs host fails closed through [`require_btrfs`] instead of pretending the Btrfs substrate
/// exists.
pub struct Daemon {
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
    /// The argv the injected real `xdg-open` fixture recorded (one argument per line).
    pub browser_argv: PathBuf,
    /// The suite scratch root this daemon's worlds are booted from (the process reaping scope).
    scratch: PathBuf,
    /// The kernel adapter this daemon's worlds are written through, so teardown can destroy exactly
    /// the subvolumes the daemon created instead of trusting a hard-coded state path.
    driver: KernelBtrfsDriver,
    /// The world-init reap budget captured when this daemon was constructed, so teardown never
    /// reads the host's load average while it is already failing.
    reap_budget: ReapBudget,
    backend: Backend,
}

enum Backend {
    /// Real `slipforkd` child process (Btrfs hosts and every explicit-`PATH` daemon).
    Child(Child),
    /// In-process `slipfork::daemon` over the real kernel [`KernelBtrfsDriver`].
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

    /// Start a daemon whose single boot-injected OSPP endpoint is `endpoint`.
    pub fn start_with_ospp(scratch: &Path, endpoint: OsppEndpoint) -> Self {
        let layout = DaemonLayout::under(scratch);
        Self::start_in_with_ospp(scratch, &layout, Some(endpoint))
            .unwrap_or_else(|message| panic!("{message}"))
    }

    /// Serve `layout` for `scratch`: the real child daemon on a Btrfs host, the in-process
    /// composition elsewhere.
    pub fn start_in(scratch: &Path, layout: &DaemonLayout) -> Result<Self, String> {
        Self::start_in_with_ospp(scratch, layout, None)
    }

    /// Same as [`Daemon::start_in`], with an optional OSPP endpoint.
    pub fn start_in_with_ospp(
        scratch: &Path,
        layout: &DaemonLayout,
        endpoint: Option<OsppEndpoint>,
    ) -> Result<Self, String> {
        ensure_private_dir(&layout.state_dir)?;
        ensure_private_dir(&layout.runtime_dir)?;
        if host_is_btrfs(&layout.state_dir) {
            Self::spawn_child(scratch, layout, None, endpoint.as_ref())
        } else {
            Ok(Self::serve_in_process(scratch, layout, endpoint))
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
        Self::spawn_child(scratch, &layout, path, None)
    }

    /// `true` when this daemon is served by [`slipfork::daemon`] in the test process.
    pub fn is_in_process(&self) -> bool {
        matches!(self.backend, Backend::InProcess { .. })
    }

    /// Start the real `slipforkd` child with an explicit `PATH` and one OSPP endpoint.
    pub fn try_start_with_ospp_path(
        scratch: &Path,
        path: Option<&Path>,
        endpoint: OsppEndpoint,
    ) -> Result<Self, String> {
        let layout = DaemonLayout::under(scratch);
        ensure_private_dir(&layout.state_dir)?;
        ensure_private_dir(&layout.runtime_dir)?;
        Self::spawn_child(scratch, &layout, path, Some(&endpoint))
    }

    /// The adapter this daemon's worlds were written through (the one it injected).
    pub fn driver(&self) -> &KernelBtrfsDriver {
        &self.driver
    }

    /// Reap this daemon's world residue (processes first, then subvolumes) and panic on survivors.
    ///
    /// The per-invocation suite proof for a test that owns a [`Daemon`] handle; `context` names the
    /// offending invocation.
    pub fn assert_no_world_residue(&self, context: &str) {
        let failures = reap_world_residue(
            &self.scratch,
            &self.state_dir,
            self.driver(),
            self.reap_budget,
        );
        if !failures.is_empty() {
            panic!("{}\n{context}", failures.join("\n"));
        }
    }

    /// Stop serving `<runtime_dir>/daemon.sock` (idempotent) and reap every world it booted.
    ///
    /// An in-process server cannot be un-wound, so its socket path is unlinked: every client then
    /// observes a dead daemon exactly like a killed child. A child is killed and reaped. Either
    /// way the worlds the daemon booted are real `slipfork-world-init` processes and writable Btrfs
    /// subvolumes that outlive a `SIGKILL`ed daemon (its engine `Drop` never runs), so both are
    /// reaped here and their survival fails the test. [`reap_world_residue`] runs the process reap
    /// first: a live world init still mounts its world, and a mounted subvolume cannot be destroyed.
    pub fn shutdown(&mut self) {
        let mut failures = Vec::new();
        match &mut self.backend {
            Backend::Child(child) => {
                // A `kill` error is not a reap failure: an already-exited child reports `ESRCH`,
                // and `wait` below is what proves (and performs) the reap.
                let _ = child.kill();
                if let Err(error) = child.wait() {
                    failures.push(format!(
                        "the harness-owned slipforkd (pid {}) could not be reaped: {error}",
                        child.id()
                    ));
                }
            }
            Backend::InProcess { closed, .. } => {
                if !*closed {
                    let _ = fs::remove_file(self.runtime_dir.join("daemon.sock"));
                    // The in-process ingress listener keeps its pathname socket bound until the
                    // `Backend` drops; unlink it so a restarted daemon on the same state root can
                    // bind a fresh one.
                    let _ = fs::remove_file(self.state_dir.join("http-ingress.sock"));
                    *closed = true;
                }
            }
        }
        failures.extend(reap_world_residue(
            &self.scratch,
            &self.state_dir,
            self.driver(),
            self.reap_budget,
        ));
        if failures.is_empty() {
            return;
        }
        let report = failures.join("\n");
        // A panic while the test is already unwinding would abort the whole test binary and hide
        // the real failure, so that one case reports the leak instead of re-panicking.
        if std::thread::panicking() {
            eprintln!("slipfork test harness: {report}");
        } else {
            panic!("{report}");
        }
    }

    fn spawn_child(
        scratch: &Path,
        layout: &DaemonLayout,
        path: Option<&Path>,
        endpoint: Option<&OsppEndpoint>,
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
        let mut command = daemon_command_with_ospp(scratch, layout, endpoint);
        command.stderr(stderr);
        if let Some(path) = path {
            command.env("PATH", path);
        }
        let mut child = command.spawn().expect("spawn slipforkd");
        await_daemon_served(&mut child, &layout.runtime_dir, &log_path)?;
        Ok(Self {
            state_dir: layout.state_dir.clone(),
            runtime_dir: layout.runtime_dir.clone(),
            browser_argv: scratch.join("browser-argv.txt"),
            scratch: scratch.to_path_buf(),
            // The child backend only exists on a Btrfs state root (`slipforkd` refuses any other
            // `--state-dir` before serving), and this is the exact adapter its composition root
            // builds for the worlds it snapshots.
            driver: KernelBtrfsDriver::new(),
            reap_budget: ReapBudget::current_host(),
            backend: Backend::Child(child),
        })
    }

    /// Serve `slipfork::daemon` in-process over the real kernel [`KernelBtrfsDriver`].
    ///
    /// The production [`compose_daemon`] graph is reused verbatim; [`require_btrfs`] fails closed
    /// with one line when the state root is not on Btrfs, so this path never fakes a substrate.
    fn serve_in_process(
        scratch: &Path,
        spec: &DaemonLayout,
        endpoint: Option<OsppEndpoint>,
    ) -> Self {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set"));
        let layout = Layout::explicit(
            spec.state_dir.clone(),
            spec.runtime_dir.clone(),
            &home,
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
        let ingress = IngressInfo::new(vec![
            http_listener
                .local_addr()
                .expect("ingress listener address"),
        ])
        .expect("describe the bound ingress");
        let inherited = InheritedVar::ALL
            .into_iter()
            .filter_map(|var| std::env::var(var.name()).ok().map(|value| (var, value)))
            .collect();
        let driver = require_btrfs(&spec.state_dir);
        let (script, browser_argv) = browser_fixture(scratch);
        let graph = compose_daemon(DaemonWiring {
            layout,
            driver,
            browser: Browser::new(script, true),
            ingress,
            forward_port: None,
            host_caches: host_caches(&home),
            inherited,
            runtime,
            target_arch: target_arch.to_owned(),
            uid,
            gid,
            ospp_endpoint: endpoint,
            hostname: "test-machine".to_owned(),
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
            browser_argv,
            scratch: scratch.to_path_buf(),
            driver,
            reap_budget: ReapBudget::current_host(),
            backend: Backend::InProcess {
                _ingress: ingress,
                closed: false,
            },
        }
    }

    pub fn cli(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(cwd, args).output().expect("spawn slipfork")
    }

    /// The same CLI invocation with one extra environment variable (for example `NO_COLOR=1`).
    pub fn cli_with_env(&self, cwd: &Path, args: &[&str], key: &str, value: &str) -> Output {
        self.command(cwd, args)
            .env(key, value)
            .output()
            .expect("spawn slipfork")
    }

    /// The same CLI invocation with `PATH` pointed at `path` (a missing-opener host).
    pub fn cli_with_path(&self, cwd: &Path, path: &Path, args: &[&str]) -> Output {
        self.command(cwd, args)
            .env("PATH", path)
            .output()
            .expect("spawn slipfork")
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
            .args(args)
            .current_dir(cwd)
            .env(
                "XDG_CACHE_HOME",
                self.state_dir.parent().expect("state parent"),
            )
            .env("XDG_DATA_HOME", self.state_dir.join("xdg-data"));
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

/// The real kernel driver for `state_dir`, or one fail-closed line naming the missing substrate.
///
/// `state_dir` must already exist. The probe is the production `statfs` magic check, so a suite can
/// never silently skip (or fake) the Btrfs substrate: running a world-booting suite off Btrfs fails
/// here with a single line that names the path, the found filesystem, and the required one.
pub fn require_btrfs(state_dir: &Path) -> KernelBtrfsDriver {
    let driver = KernelBtrfsDriver::new();
    let facts = driver.probe_facts(state_dir).unwrap_or_else(|error| {
        panic!(
            "slipfork test harness requires a Btrfs state dir at {} (statfs probe failed: {error}); \
             mount one with `user_subvol_rm_allowed,noatime` and rerun this suite",
            state_dir.display()
        )
    });
    assert!(
        facts.fstype == BTRFS_SUPER_MAGIC,
        "slipfork test harness requires a Btrfs state dir at {} (statfs reports f_type {:#x}; \
         BTRFS_SUPER_MAGIC is {:#x}); fix: {}",
        facts.probed_path.display(),
        facts.fstype,
        BTRFS_SUPER_MAGIC,
        mount_hint(&facts.probed_path)
    );
    driver
}

/// The `ADR-0010` §2 sealed-lineage directory the world reaper never touches.
const SNAPSHOTS_DIR: &str = "snapshots";

/// Reap every piece of world residue a suite's daemon can leave behind.
///
/// The process reap runs first: a live world init still mounts its world, and a mounted subvolume
/// cannot be destroyed. One composed helper keeps that order in a single place for both the
/// per-daemon teardown and the per-invocation proofs. Returns one diagnostic per failed reap
/// (empty means clean).
pub fn reap_world_residue(
    scratch: &Path,
    state_dir: &Path,
    driver: &KernelBtrfsDriver,
    budget: ReapBudget,
) -> Vec<String> {
    let mut failures = Vec::new();
    if let Err(error) = reap_world_init_processes_within(scratch, budget) {
        failures.push(error);
    }
    if let Err(error) = reap_world_subvolumes_with(driver, state_dir) {
        failures.push(error);
    }
    failures
}

/// Assert that one suite invocation left no world residue; `context` names the invocation.
pub fn assert_no_world_residue(
    scratch: &Path,
    state_dir: &Path,
    driver: &KernelBtrfsDriver,
    budget: ReapBudget,
    context: &str,
) {
    let failures = reap_world_residue(scratch, state_dir, driver, budget);
    if !failures.is_empty() {
        panic!("{}\n{context}", failures.join("\n"));
    }
}

/// Destroy every subvolume the kernel `driver` reports under `state_dir`.
///
/// `<state_dir>/snapshots` holds the sealed base lineage (`ADR-0010` §§2–3) and is excluded; every
/// other subvolume under the suite's state root is a live world the daemon did not reclaim (or
/// scratch residue), so it is destroyed. The walk asks the driver at every directory instead of
/// assuming one world path, so a regression that moves a world elsewhere still fails. A missing
/// state root is a proof (no daemon ever wrote there), not a skip. A destroy failure is reported
/// with the surviving subvolume names and the driver's own error, never dropped.
pub fn reap_world_subvolumes_with(
    driver: &KernelBtrfsDriver,
    state_dir: &Path,
) -> Result<(), String> {
    if !state_dir.is_dir() {
        return Ok(());
    }
    let sealed_lineage = state_dir.join(SNAPSHOTS_DIR);
    let mut survivors = Vec::new();
    let mut pending = vec![state_dir.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let children = driver.list_children(&directory).map_err(|error| {
            format!(
                "cannot enumerate the subvolumes under {}: {error}",
                directory.display()
            )
        })?;
        for path in children {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                survivors.push(format!("{} (not a UTF-8 subvolume name)", path.display()));
                continue;
            };
            if let Err(error) = driver.destroy_subvolume(&directory, name) {
                survivors.push(format!("{} ({error})", path.display()));
            }
        }
        let entries = fs::read_dir(&directory)
            .map_err(|error| format!("cannot inspect {}: {error}", directory.display()))?;
        for entry in entries {
            let entry = entry
                .map_err(|error| format!("cannot inspect {}: {error}", directory.display()))?;
            let file_type = entry
                .file_type()
                .map_err(|error| format!("cannot inspect {}: {error}", entry.path().display()))?;
            // `file_type` never follows a symlink, so a world's `rootfs` link cannot loop the walk.
            // Only the sealed-lineage root is skipped; a world directory that happens to be named
            // `snapshots` deeper in the tree is still reaped.
            if !file_type.is_dir() || entry.path() == sealed_lineage {
                continue;
            }
            pending.push(entry.path());
        }
    }
    if survivors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "world subvolumes survived teardown under {} (the daemon was killed before its engine \
             could reclaim them; only {} is retained): {}",
            state_dir.display(),
            SNAPSHOTS_DIR,
            survivors.join("; ")
        ))
    }
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
    poll_until(SHUTDOWN_DEADLINE, POLL_INTERVAL, || {
        if client.is_served() {
            Err(format!("{} still accepts connections", socket.display()))
        } else {
            Ok(())
        }
    })
    .map_err(|last| format!("{last} after {SHUTDOWN_DEADLINE:?}"))
}

/// The observable outcome of `SIGTERM`-ing one CLI child's process group.
pub struct ProcessGroupTermination {
    /// `true` when `kill -TERM -- -<pgid>` was delivered to at least one member.
    pub term_delivered: bool,
    /// `true` when a `kill -0 -- -<pgid>` probe still finds a live member.
    pub members_alive: bool,
}

/// `SIGTERM` the process group `process_group` and probe whether it is gone.
///
/// A CLI child spawned with `process_group(0)` owns a group that also carries the `slipforkd` it
/// auto-spawned and every world that daemon booted, so one group signal reaches all of them. A
/// diagnostic-only invocation never spawns a daemon, so its group is already empty and `kill`
/// reports `ESRCH`; [`ProcessGroupTermination`] lets each caller assert its own policy.
pub fn terminate_process_group(process_group: u32) -> ProcessGroupTermination {
    let group = format!("-{process_group}");
    ProcessGroupTermination {
        term_delivered: kill(&["-TERM", "--", &group]),
        members_alive: kill(&["-0", "--", &group]),
    }
}

/// The bounded wait a world-init reap is given before it reports a leak.
///
/// The windows are test-side only (no production signature changes): the harness owns how long it
/// waits for a `SIGKILL`ed world namespace to disappear.
///
/// A world PID 1 ignores `SIGTERM` (`pid_namespaces(7)`), so [`ReapBudget::term_grace`] only has to
/// outlast one scheduling round trip for the `unshare` supervisor. `SIGKILL` is not maskable, so
/// [`ReapBudget::kill_grace`] only waits for the kernel to tear the PID namespace down. A process
/// sleeping uninterruptibly (`D`) cannot act on any signal until its syscall returns, so
/// [`ReapBudget::d_state_grace`] is an extra window granted only while a survivor really is `D`;
/// the reap retries it with backoff instead of failing fast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReapBudget {
    term_grace: Duration,
    kill_grace: Duration,
    d_state_grace: Duration,
}

/// Largest load multiplier [`ReapBudget::for_load`] applies, so a bogus load average cannot turn a
/// failing reap into an unbounded wait.
const MAX_REAP_LOAD_FACTOR: f64 = 8.0;

/// Largest backoff between two `D`-state retries.
const MAX_REAP_BACKOFF: Duration = Duration::from_secs(2);

impl ReapBudget {
    /// The idle-host windows: 1 s after `SIGTERM`, 10 s after the first `SIGKILL`, plus 20 s while a
    /// survivor is uninterruptible.
    pub const IDLE: Self = Self {
        term_grace: Duration::from_secs(1),
        kill_grace: Duration::from_secs(10),
        d_state_grace: Duration::from_secs(20),
    };

    /// Derive the windows from a 1-minute load average and CPU count.
    ///
    /// A host whose runnable load exceeds its CPU count starves even a dying process, so every
    /// window scales by `ceil(load / cpus)`, clamped to [`MAX_REAP_LOAD_FACTOR`]. An idle,
    /// non-finite, or negative load keeps [`ReapBudget::IDLE`] unchanged.
    pub fn for_load(load_1m: f64, cpus: usize) -> Self {
        let cpus = cpus.max(1);
        let factor = if load_1m.is_finite() && load_1m > 0.0 {
            (load_1m / cpus as f64)
                .ceil()
                .clamp(1.0, MAX_REAP_LOAD_FACTOR)
        } else {
            1.0
        };
        Self {
            term_grace: Self::IDLE.term_grace.mul_f64(factor),
            kill_grace: Self::IDLE.kill_grace.mul_f64(factor),
            d_state_grace: Self::IDLE.d_state_grace.mul_f64(factor),
        }
    }

    /// The budget derived from this host's real 1-minute load average and CPU count.
    ///
    /// An unreadable `/proc/loadavg` is a failed inspection, not a quiet fall back to a constant:
    /// the harness fails closed instead of waiting an unjustified amount of time.
    pub fn current_host() -> Self {
        let raw = fs::read_to_string("/proc/loadavg").unwrap_or_else(|error| {
            panic!("cannot derive the world-init reap budget: /proc/loadavg is unreadable: {error}")
        });
        let load_1m = raw
            .split_whitespace()
            .next()
            .and_then(|field| field.parse::<f64>().ok())
            .unwrap_or_else(|| {
                panic!(
                    "cannot derive the world-init reap budget: unexpected /proc/loadavg contents \
                     {raw:?}"
                )
            });
        let cpus = std::thread::available_parallelism()
            .map(|parallelism| parallelism.get())
            .unwrap_or_else(|error| {
                panic!("cannot derive the world-init reap budget: CPU count is unknown: {error}")
            });
        Self::for_load(load_1m, cpus)
    }

    /// How long the `SIGTERM` window lasts.
    pub fn term_grace(&self) -> Duration {
        self.term_grace
    }

    /// The windows this budget applies, for a failure diagnostic.
    pub fn describe(&self) -> String {
        format!(
            "term {:?}, kill {:?}, uninterruptible {:?}",
            self.term_grace, self.kill_grace, self.d_state_grace
        )
    }
}

/// The kernel's process state character and process group, read from `/proc/<pid>/stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessStat {
    /// Field 3: `R`, `S`, `D`, `Z`, `T`, …
    state: char,
    /// Field 5: the process group id.
    pgid: u32,
}

/// One `slipfork-world-init` PID 1 or its `unshare` supervisor, matched by a suite's scratch root.
struct WorldInitProcess {
    pid: u32,
    /// `None` when `/proc/<pid>/stat` could not be read: the process is still reported (and
    /// signalled directly) instead of being silently dropped as "no survivor".
    stat: Option<ProcessStat>,
    cmdline: String,
}

impl WorldInitProcess {
    /// `true` when the kernel reports this process in uninterruptible sleep (`D`).
    ///
    /// A `D` process has not acted on the `SIGKILL` it was sent: it is inside a kernel sleep and
    /// only runs the signal when that call returns, so the reap must keep waiting rather than
    /// declare it a leak at the first deadline.
    fn is_uninterruptible(&self) -> bool {
        self.stat.is_some_and(|stat| stat.state == 'D')
    }

    fn process_group(&self) -> Option<u32> {
        self.stat.map(|stat| stat.pgid)
    }
}

impl std::fmt::Display for WorldInitProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.stat {
            Some(stat) => write!(
                formatter,
                "pid {} (state {} = {}, pgid {}): {}",
                self.pid,
                stat.state,
                process_state_name(stat.state),
                stat.pgid,
                self.cmdline
            ),
            None => write!(
                formatter,
                "pid {} (state unknown, pgid unknown): {}",
                self.pid, self.cmdline
            ),
        }
    }
}

/// Human name of a `/proc/<pid>/stat` state character (`proc(5)` §"/proc/pid/stat").
fn process_state_name(state: char) -> &'static str {
    match state {
        'R' => "running",
        'S' => "interruptible sleep",
        'D' => "uninterruptible sleep",
        'Z' => "zombie",
        'T' => "stopped",
        't' => "tracing stop",
        'X' | 'x' => "dead",
        'I' => "idle",
        'P' => "parked",
        _ => "unknown",
    }
}

/// Boundedly reap every `slipfork-world-init` and `unshare` supervisor whose cmdline names
/// `scratch`, returning the survivors when any outlive the captured `budget`.
///
/// A world PID 1 is `SIGNAL_UNKILLABLE` to `SIGTERM` (`pid_namespaces(7)`), and a supervisor that
/// is `SIGKILL`ed can no longer forward `--kill-child`, so a world can outlive both its daemon and
/// the `SIGTERM` that killed the daemon's process group. Every *wait* re-scans `/proc` by cmdline,
/// so no wait trusts a stale pid; the initial `SIGTERM` pass signals the first scan, and every later
/// pass re-scans before signalling. The error names every survivor with the state character the
/// kernel reports for it. `budget` is captured by the caller (a [`Daemon`] captures it at
/// construction), so teardown never reads the host's load average while it is already failing.
pub fn reap_world_init_processes_within(scratch: &Path, budget: ReapBudget) -> Result<(), String> {
    let started = Instant::now();
    let targets = world_init_processes(scratch);
    if targets.is_empty() {
        return Ok(());
    }
    signal_world_init(&targets, "-TERM");
    if poll_until(budget.term_grace(), POLL_INTERVAL, || {
        no_world_init(scratch)
    })
    .is_ok()
    {
        return Ok(());
    }
    let kill_deadline = Instant::now() + budget.kill_grace;
    // Granted once, only when a survivor is really uninterruptible: a `D` process cannot run the
    // pending `SIGKILL` until the kernel returns it from its syscall. The extension always starts
    // later than the base `SIGKILL` deadline (`d_state_grace` is the larger window), so it is the
    // only deadline that can apply once it is set.
    let mut d_state_deadline: Option<Instant> = None;
    let mut backoff = POLL_INTERVAL;
    loop {
        let survivors = world_init_processes(scratch);
        if survivors.is_empty() {
            return Ok(());
        }
        // A signal can be lost to a process that is momentarily unsignalable (an uninterruptible
        // syscall), so every pass re-signals the survivors it just re-scanned by cmdline.
        signal_world_init(&survivors, "-KILL");
        let uninterruptible = survivors.iter().any(WorldInitProcess::is_uninterruptible);
        if uninterruptible {
            d_state_deadline.get_or_insert_with(|| Instant::now() + budget.d_state_grace);
        }
        let deadline = d_state_deadline.unwrap_or(kill_deadline);
        if Instant::now() >= deadline {
            return Err(format!(
                "world init processes for {} survived SIGTERM and SIGKILL for {:?} (states read \
                 from /proc/<pid>/stat; reap budget {}): {}",
                scratch.display(),
                started.elapsed(),
                budget.describe(),
                describe_world_init(&survivors)
            ));
        }
        match d_state_deadline {
            // An uninterruptible survivor gets exponential backoff between retries.
            Some(_) => {
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(MAX_REAP_BACKOFF);
            }
            None => std::thread::sleep(POLL_INTERVAL),
        }
    }
}

/// `Ok` when no `slipfork-world-init`/`unshare` process names `scratch` (`/proc` re-scanned).
fn no_world_init(scratch: &Path) -> Result<(), String> {
    let alive = world_init_processes(scratch);
    if alive.is_empty() {
        Ok(())
    } else {
        Err(describe_world_init(&alive))
    }
}

/// Every `slipfork-world-init`/`unshare` process whose argv names `scratch`.
fn world_init_processes(scratch: &Path) -> Vec<WorldInitProcess> {
    let mut found = Vec::new();
    // A reap helper may never report "no survivors" on ignorance: an unreadable `/proc` is a
    // failed inspection, not an empty process table.
    let entries =
        fs::read_dir("/proc").expect("/proc must be readable to reap world init processes");
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(raw) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let argv: Vec<String> = raw
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect();
        let Some(program) = argv.first() else {
            continue;
        };
        let supervised = program.ends_with("slipfork-world-init") || program.ends_with("unshare");
        if !supervised || !argv.iter().any(|arg| Path::new(arg).starts_with(scratch)) {
            continue;
        }
        // A matched process whose stat is unreadable is still a survivor: report it (with an
        // unknown state and process group) instead of dropping it from the proof.
        found.push(WorldInitProcess {
            pid,
            stat: process_stat(pid),
            cmdline: argv.join(" "),
        });
    }
    found.sort_by_key(|process| process.pid);
    found
}

/// Signal every matched process: its whole process group unless that group is the test's own.
///
/// A world init inherits its spawner's process group, and the harness daemon boots worlds from
/// this test process, so the test's own group is never signalled: each matched pid is signalled
/// directly instead (killing the namespace's PID 1 tears the whole namespace down). A group owned
/// by a CLI child (`process_group(0)`) or an earlier run is signalled as a group, so the PID 1 and
/// its supervisor both receive the signal. A process whose group could not be read is signalled by
/// pid only, because signalling an unknown group could hit the test runner.
fn signal_world_init(processes: &[WorldInitProcess], signal: &str) {
    // Without the test's own group, a group signal could kill the test runner: fail closed rather
    // than guess (the reaper may never act on ignorance).
    let own_group = process_stat(std::process::id())
        .expect("/proc/self/stat must be readable to avoid signalling the test's own group")
        .pgid;
    let mut signalled: Vec<u32> = Vec::new();
    for process in processes {
        match process.process_group() {
            Some(pgid) if pgid != own_group => {
                if !signalled.contains(&pgid) {
                    signalled.push(pgid);
                    kill(&[signal, "--", &format!("-{pgid}")]);
                }
            }
            _ => {
                kill(&[signal, &process.pid.to_string()]);
            }
        }
    }
}

/// The state character and process group of `pid` (`/proc/<pid>/stat` fields 3 and 5).
fn process_stat(pid: u32) -> Option<ProcessStat> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `comm` (field 2) is parenthesized and may itself contain spaces or `)`, so split after its
    // last `)`; the remaining fields start with `state ppid pgrp …`.
    let mut fields = stat.rsplit_once(')')?.1.split_whitespace();
    let state = fields.next()?.chars().next()?;
    fields.next()?; // ppid
    let pgid = fields.next()?.parse().ok()?;
    Some(ProcessStat { state, pgid })
}

fn describe_world_init(processes: &[WorldInitProcess]) -> String {
    processes
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// `true` when `kill <args>` reported success (a missing pid or group reports `ESRCH`).
fn kill(args: &[&str]) -> bool {
    Command::new("kill")
        .args(args)
        .stderr(Stdio::null())
        .status()
        .expect("run kill")
        .success()
}

/// Bound on one streamed child line.
///
/// This is the one remaining blocking `recv_timeout` wait in this harness: the producer thread
/// blocks on the child's stdout pipe and the consumer must block on the channel until the next
/// line arrives, so a `poll_until` loop would only re-implement `recv_timeout` on the same
/// channel. The blocking bound is therefore a named deadline, not a polling interval.
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

#[cfg(test)]
mod tests {
    use super::*;
    use slipfork_world::EngineError;

    /// One disk-backed state root with `worlds/<name>` subvolumes created through the real kernel
    /// driver; the tempdir is returned so the caller keeps it alive. Worlds are real Btrfs
    /// subvolumes, so a non-Btrfs host fails closed through [`require_btrfs`], never a fake driver.
    fn world_fixture(names: &[&str]) -> (TempDir, PathBuf, KernelBtrfsDriver) {
        let tmp = disk_tempdir("world-reaper-");
        let state_dir = tmp.path().join("state");
        let worlds = state_dir.join("worlds");
        fs::create_dir_all(&worlds).expect("worlds root");
        let driver = require_btrfs(&state_dir);
        for name in names {
            driver
                .create_subvolume(&worlds, name)
                .expect("create world subvolume");
        }
        (tmp, state_dir, driver)
    }

    #[test]
    fn reap_budget_is_idle_derived_under_no_load_and_scales_with_load() {
        assert_eq!(ReapBudget::for_load(0.0, 8), ReapBudget::IDLE);
        assert_eq!(
            ReapBudget::for_load(2.5, 8),
            ReapBudget::IDLE,
            "a load below one process per CPU keeps the idle windows"
        );
        let loaded = ReapBudget::for_load(16.0, 8);
        assert_eq!(
            loaded.term_grace(),
            Duration::from_secs(2),
            "a 2x-overcommitted host gets a 2x longer SIGTERM window"
        );
        let extreme = ReapBudget::for_load(1_000.0, 1);
        assert_eq!(
            extreme.term_grace(),
            Duration::from_secs(8),
            "the multiplier is clamped"
        );
    }

    #[test]
    fn process_stat_reads_the_live_state_character_and_process_group() {
        let own = process_stat(std::process::id()).expect("/proc/self/stat is readable");
        assert!(
            own.state.is_ascii_alphabetic(),
            "the kernel reports one state character, got {:?}",
            own.state
        );
        assert!(own.pgid > 0, "a live test process has a process group");
    }

    #[test]
    fn survivor_diagnostic_reports_the_kernel_state_instead_of_a_bare_timeout() {
        let process = WorldInitProcess {
            pid: 4242,
            stat: Some(ProcessStat {
                state: 'D',
                pgid: 4243,
            }),
            cmdline: "slipfork-world-init --config /state/ctl/w/config.json".to_owned(),
        };
        assert!(process.is_uninterruptible());
        let reported = process.to_string();
        for needle in ["state D", "uninterruptible sleep", "pgid 4243"] {
            assert!(reported.contains(needle), "missing {needle}: {reported}");
        }
        let unknown = WorldInitProcess {
            pid: 4244,
            stat: None,
            cmdline: "unshare".to_owned(),
        };
        assert!(!unknown.is_uninterruptible());
        assert!(unknown.to_string().contains("state unknown"));
    }

    #[test]
    fn world_reaper_destroys_what_the_injected_driver_reports() {
        let (_tmp, state_dir, driver) = world_fixture(&["acme-app-1", "acme-app-2"]);

        reap_world_subvolumes_with(&driver, &state_dir).expect("reap the reported worlds");
        assert_eq!(
            driver
                .list_children(&state_dir.join("worlds"))
                .expect("re-list worlds"),
            Vec::<PathBuf>::new(),
            "no world subvolume may survive the reaper"
        );
    }

    #[test]
    fn world_reaper_fails_on_a_real_missing_world_path_instead_of_dropping_it() {
        let (_tmp, state_dir, driver) = world_fixture(&["acme-app-7"]);
        let worlds = state_dir.join("worlds");
        let vanished = worlds.join("acme-app-7");
        driver
            .destroy_subvolume(&worlds, "acme-app-7")
            .expect("destroy the world");
        assert!(!vanished.exists(), "the world path is really gone");
        assert_eq!(
            fs::read_dir(&worlds).expect("read the world root").count(),
            0,
            "no world subvolume may survive the direct destroy"
        );
        // A real kernel `ENOENT` from the real driver: the reaper never invents a failure.
        let error = driver
            .destroy_subvolume(&worlds, "acme-app-7")
            .expect_err("destroying the vanished world reports the kernel's own error");
        assert!(
            matches!(&error, EngineError::Ioctl { errno: 2, .. }),
            "a vanished world is the kernel's real ENOENT, got: {error}"
        );
        reap_world_subvolumes_with(&driver, &state_dir)
            .expect("the empty world root is a clean reap");
    }

    #[test]
    fn world_reaper_is_a_proof_when_no_world_root_exists() {
        let tmp = disk_tempdir("empty-state-");
        let state_dir = tmp.path().join("state");
        fs::create_dir_all(&state_dir).expect("state root");
        let driver = require_btrfs(&state_dir);
        assert!(
            reap_world_subvolumes_with(&driver, &state_dir).is_ok(),
            "a state dir without worlds/ proves no world ever booted"
        );
    }
}
