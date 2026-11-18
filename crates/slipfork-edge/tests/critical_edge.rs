//! Protected critical tests for `slipfork-edge` (`M2.8`, `ADR-0008` §5, `H-011` #4).
//!
//! Every activation assertion is driven through a **real fd-3 handover**: the parent binds a
//! listening socket, hands it to a child process on `fd 0`, and the child's `sh` wrapper
//! duplicates it to `fd 3` before `exec` and exports `LISTEN_PID=$$ LISTEN_FDS=<n>` — exactly the
//! descriptor layout `systemd` produces (`docs/research/tunnel-weaver-and-systemd-facts.md` §5.2).
//! The child then proves the adopted descriptor is non-blocking and serves a real HTTP exchange
//! through [`HttpIngress`]. No descriptor is mocked and no environment is faked in-process.
//!
//! The registry, ingress, proxy, forward, and share paths run against a registry-level
//! [`WorldEngine`] double ([`StubWorldEngine`]) that models world handles, their lifecycle, and the
//! snapshot set a clone may come from, while every proxied hop dials a real loopback `TcpStream` —
//! no socket, filesystem, or namespace is faked.

#![forbid(unsafe_code)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use slipfork_edge::daemon::{
    AuthSummary, BaseLineage, BootEvent, CacheOutcome, DaemonControl, DaemonError, DaemonErrorKind,
    DaemonStatus, DropForkRequest, DumpRequest, ExecTargetRequest, ForkRequest, ForwardRequest,
    ForwardSummary, HookRunRequest, IngressInfo, LogsRequest, PruneReport, ResourceRequest,
    ResourceSummary, RootfsLineage, SeedLineage, ServiceImageLineage, ServiceKind, UpRequest,
    WorldExecTarget, WorldLineage, WorldProcessReport, WorldSelector, WorldServiceSummary,
    WorldState, WorldSummary, WorldUpSummary,
};
use slipfork_edge::{
    DaemonControlClient, DaemonControlServer, EdgeError, ForwardTargetSpec, HttpIngress,
    IngressRoute, ListenConfig, RouteDecision, RoutedWorld, SharedWorldInfo,
    UnavailableTunnelRegistrar, UnitSpec, WorldRegistry, adopt_systemd_listener, bind_listen,
    render_service_unit, render_socket_unit, resolve_listener_set,
};
use slipfork_engine::conformance::assert_world_engine_conformance;
use slipfork_engine::rootfs::{
    ElfAbi, ImageContractMetadata, LibcFamily, OsRelease, RootfsDigest, WorldRootfs,
};
use slipfork_engine::{
    BootLineage, EngineError, ExecEnv, ExecOutput, ServiceHandle, ServiceLogs, ServiceSpec,
    ServiceState, SnapshotId, StreamChunk, StreamSource, WorldEngine, WorldEntryCommand,
    WorldHandle, WorldServiceInfo, WorldSpec,
};
use slipfork_world::{
    LocalHostRoute, ProjectSlug, TunnelError, TunnelLease, TunnelRegistrar, TunnelRequest,
    WorldOrigin, WorldTarget,
};

const PROBE_MODE_ENV: &str = "SLIPFORK_EDGE_PROBE";
const PROBE_STATE_ENV: &str = "SLIPFORK_EDGE_STATE_DIR";
const PROBE_READY_ENV: &str = "SLIPFORK_EDGE_READY_FILE";
const PROBE_PORT_ENV: &str = "SLIPFORK_EDGE_EXPECTED_PORT";

const CHILD_ARGS: &str = "--exact activation_probe_child --test-threads=1 --nocapture";

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

fn cached_test_dir(label: &str) -> PathBuf {
    let home = std::env::var("HOME").expect("HOME is required for the edge test scratch space");
    // Short and unique: `<state_dir>/http-ingress.sock` must fit the 107-byte AF_UNIX budget.
    let dir = PathBuf::from(home)
        .join(".cache")
        .join("sfedge")
        .join(format!("{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create the edge test state directory");
    dir
}

fn acme() -> ProjectSlug {
    ProjectSlug::parse("acme").expect("valid project slug")
}

/// The one registry builder: ingress port 80, `registrar`, and an optional forward port.
fn registry_over(
    engine: Arc<StubWorldEngine>,
    registrar: Arc<dyn TunnelRegistrar>,
    forward_port: Option<u16>,
) -> Arc<WorldRegistry> {
    Arc::new(WorldRegistry::new(engine, registrar, 80, forward_port))
}

/// The registry the non-sharing suites use: no tunnel registrar, no forward port.
fn stub_registry(engine: Arc<StubWorldEngine>) -> Arc<WorldRegistry> {
    registry_over(engine, Arc::new(UnavailableTunnelRegistrar::new()), None)
}

fn listen_local() -> ListenConfig {
    ListenConfig {
        addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        forward_port: None,
    }
}

/// A loopback port nothing listens on: bind an ephemeral listener, read its port, drop it.
fn closed_loopback_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind a throwaway listener");
    let port = listener.local_addr().expect("throwaway address").port();
    drop(listener);
    port
}

/// Bounded attempts to observe a refused loopback dial.
///
/// A concurrent binder can take the freshly reserved port between [`closed_loopback_port`] and the
/// dial, exactly like the reserved-port race `socket_activate_systemd_handover` re-runs on a fresh
/// port. The world is re-attached on a new port instead of weakening the 502 assertion.
const REFUSED_DIAL_ATTEMPTS: u32 = 5;

// ---------------------------------------------------------------------------------------------
// Real fd-3 activation handover
// ---------------------------------------------------------------------------------------------

/// Build the `sh` wrapper: duplicate the inherited socket onto `fd 3` (`fd 3..=n+2` when several
/// descriptors are requested), export the systemd handover variables, and re-exec this test binary
/// as the single `activation_probe_child` test.
fn probe_script(prefix: &str) -> String {
    format!("{prefix}; exec \"$0\" {CHILD_ARGS}")
}

fn probe_command(prefix: &str, mode: &str, state: &Path, port: Option<u16>) -> Command {
    let executable = std::env::current_exe().expect("current test executable");
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(probe_script(prefix))
        .arg(executable)
        .env(PROBE_MODE_ENV, mode)
        .env(PROBE_STATE_ENV, state)
        .env(PROBE_READY_ENV, state.join("probe.ready"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(port) = port {
        command.env(PROBE_PORT_ENV, port.to_string());
    }
    command
}

/// Run a non-serving probe child to completion and fail with its output when it refuses to pass.
fn run_probe(prefix: &str, mode: &str, state: &Path) -> Output {
    let output = probe_command(prefix, mode, state, None)
        .stdin(Stdio::null())
        .output()
        .expect("spawn the activation probe child");
    assert_probe_ok(mode, &output);
    output
}

fn assert_probe_ok(mode: &str, output: &Output) {
    assert!(
        output.status.success(),
        "activation probe '{mode}' failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Spawn a serving probe child whose adopted fd-3 listener belongs to this process.
fn spawn_serving_probe(
    prefix: &str,
    mode: &str,
    state: &Path,
    port: u16,
    listener: TcpListener,
) -> Child {
    listener
        .set_nonblocking(false)
        .expect("the inherited listener starts blocking, as systemd hands it over");
    let stdout = Stdio::piped();
    let stderr = Stdio::piped();
    probe_command(prefix, mode, state, Some(port))
        .stdin(Stdio::from(OwnedFd::from(listener)))
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .expect("spawn the serving activation probe child")
}

fn await_ready(child: &mut Child, ready: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if ready.exists() {
            return;
        }
        if let Some(status) = child.try_wait().expect("poll the probe child") {
            panic!(
                "probe child exited before becoming ready ({status})\nstdout:\n{}\nstderr:\n{}",
                drain(&mut child.stdout),
                drain(&mut child.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("probe child did not become ready within 30s");
}

fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("poll the probe child") {
            return status;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!(
        "probe child did not exit within 30s\nstdout:\n{}\nstderr:\n{}",
        drain(&mut child.stdout),
        drain(&mut child.stderr)
    );
}

fn drain<R: Read>(reader: &mut Option<R>) -> String {
    let Some(reader) = reader else {
        return String::new();
    };
    let mut text = String::new();
    let _ = reader.read_to_string(&mut text);
    text
}

/// The child half of the real fd-3 handover. It only runs when the wrapper shell selected a mode.
#[test]
fn activation_probe_child() {
    let Ok(mode) = std::env::var(PROBE_MODE_ENV) else {
        return;
    };
    match mode.as_str() {
        "one_http_listener" => probe_one_http_listener(),
        "descriptor_order" => probe_descriptor_order(),
        "pid_mismatch" => probe_pid_mismatch(),
        "zero_descriptors" => probe_zero_descriptors(),
        "broken_descriptor" => probe_broken_descriptor(),
        "no_listener" => probe_no_listener(),
        "listen_mode" => probe_listen_mode(),
        "systemd_socket_activate" => probe_systemd_socket_activate(),
        other => panic!("unknown probe mode '{other}'"),
    }
}

fn probe_state_dir() -> PathBuf {
    PathBuf::from(std::env::var(PROBE_STATE_ENV).expect("probe state directory"))
}

fn probe_expected_port() -> u16 {
    std::env::var(PROBE_PORT_ENV)
        .expect("probe expected port")
        .parse()
        .expect("numeric probe port")
}

fn assert_would_block(listener: &TcpListener, role: &str) {
    let err = listener
        .accept()
        .expect_err("a non-blocking adopted listener with no pending connection must not accept");
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::WouldBlock,
        "adopted {role} must be non-blocking"
    );
}

/// Assert the kernel reports `O_NONBLOCK` on an adopted descriptor.
///
/// The live `systemd-socket-activate` handover already has the triggering connection queued, so
/// `accept` cannot probe blocking mode; `/proc/self/fdinfo` reports the real descriptor flags
/// (`flags:` is octal, `O_NONBLOCK` is `0o4000` on Linux) without `unsafe` or a new dependency.
fn assert_nonblocking(listener: &TcpListener, role: &str) {
    use std::os::fd::AsRawFd as _;

    let fd = listener.as_raw_fd();
    let info = fs::read_to_string(format!("/proc/self/fdinfo/{fd}"))
        .expect("read the descriptor flags from /proc/self/fdinfo");
    let flags = info
        .lines()
        .find_map(|line| line.strip_prefix("flags:"))
        .expect("fdinfo reports a flags line");
    let bits = u32::from_str_radix(flags.trim(), 8).expect("fdinfo flags are octal");
    assert_ne!(
        bits & 0o4000,
        0,
        "adopted {role} must be O_NONBLOCK (fdinfo flags {bits:#o})"
    );
}

fn probe_one_http_listener() {
    let state = probe_state_dir();
    let expected_port = probe_expected_port();
    let listeners = adopt_systemd_listener()
        .expect("adoption succeeds")
        .expect("activation is present");
    assert_eq!(listeners.http_listeners.len(), 1);
    assert!(
        listeners.forward_listener.is_none(),
        "LISTEN_FDS=1 must not invent a forward listener"
    );
    assert_eq!(
        listeners.http_addrs().expect("bound port")[0].port(),
        expected_port,
        "the adopted fd 3 must be the parent's listening socket"
    );
    assert_would_block(&listeners.http_listeners[0], "HTTP ingress listener");

    let registry = stub_registry(Arc::new(StubWorldEngine::new()));
    let ingress =
        HttpIngress::bind(&state, listeners, registry).expect("bind the adopted listener + socket");
    fs::write(state.join("probe.ready"), b"ready\n").expect("write the readiness marker");
    thread::spawn(move || {
        let _ = ingress.serve();
    });

    let done = state.join("probe.done");
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !done.exists() {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(done.exists(), "the parent never finished its request");
}

fn probe_descriptor_order() {
    let expected_port = probe_expected_port();
    let listeners = adopt_systemd_listener()
        .expect("adoption succeeds")
        .expect("activation is present");
    assert_eq!(
        listeners.http_listeners.len(),
        2,
        "LISTEN_FDS=3 keeps two leading HTTP listeners"
    );
    for (index, listener) in listeners.http_listeners.iter().enumerate() {
        assert_would_block(listener, &format!("HTTP ingress listener {index}"));
    }
    let forward = listeners
        .forward_listener
        .as_ref()
        .expect("the trailing LISTEN_FDS=3 descriptor is the forward listener");
    assert_would_block(forward, "forward listener");
    assert_eq!(
        listeners.http_addrs().expect("bound port")[0].port(),
        expected_port
    );
    assert_eq!(
        forward.local_addr().expect("forward address").port(),
        expected_port
    );
}

fn probe_pid_mismatch() {
    let listen = listen_local();
    let err = resolve_listener_set(Some(&listen))
        .expect_err("a foreign LISTEN_PID must be refused, never replaced by a self-bind");
    match err {
        EdgeError::ActivationMismatch {
            listen_pid,
            self_pid,
        } => {
            assert_eq!(listen_pid, "1");
            assert_eq!(self_pid, std::process::id());
        }
        other => panic!("expected ActivationMismatch, got {other:?}"),
    }
}

fn probe_zero_descriptors() {
    let err = adopt_systemd_listener().expect_err("LISTEN_FDS=0 advertises no listener");
    assert!(
        matches!(&err, EdgeError::ActivationBroken { reason } if reason.contains("LISTEN_FDS=0")),
        "expected ActivationBroken about LISTEN_FDS=0, got {err:?}"
    );
}

fn probe_broken_descriptor() {
    // The activation variables alone select activation mode: a descriptor that is not a socket is
    // fatal even though a usable `--listen` address was supplied, so a broken handover can never
    // degrade into an unrelated self-bind.
    let listen = listen_local();
    let err = resolve_listener_set(Some(&listen))
        .expect_err("a broken activation environment must never fall back to --listen");
    assert!(
        matches!(&err, EdgeError::ActivationBroken { reason } if reason.contains("fd 3")),
        "expected ActivationBroken naming fd 3, got {err:?}"
    );
}

fn probe_no_listener() {
    assert!(
        adopt_systemd_listener()
            .expect("absent activation is not an error")
            .is_none(),
        "no activation variables means no adopted listener set"
    );
    let err = resolve_listener_set(None).expect_err("startup must fail closed without a listener");
    assert!(matches!(err, EdgeError::NoListener), "got {err:?}");
}

fn probe_listen_mode() {
    let without_forward = resolve_listener_set(Some(&listen_local())).expect("bind --listen");
    assert_eq!(without_forward.http_listeners.len(), 1);
    assert!(
        without_forward.forward_listener.is_none(),
        "--listen without --forward-port must not bind a forward listener"
    );
    assert_ne!(
        without_forward.http_addrs().expect("bound port")[0].port(),
        0
    );
    assert_would_block(
        &without_forward.http_listeners[0],
        "--listen HTTP ingress listener",
    );

    let with_forward = resolve_listener_set(Some(&ListenConfig {
        addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        forward_port: Some(0),
    }))
    .expect("bind --listen --forward-port");
    let forward = with_forward
        .forward_listener
        .as_ref()
        .expect("forward listener");
    assert_eq!(
        forward.local_addr().expect("forward address").ip(),
        Ipv4Addr::LOCALHOST,
        "the planned forward listener is loopback-only"
    );
    assert_ne!(
        with_forward.http_addrs().expect("http port")[0].port(),
        forward.local_addr().expect("forward address").port()
    );
}

/// The child half of the **real** `systemd-socket-activate` handover: `LISTEN_PID` must name this
/// process, exactly one descriptor (`fd 3`) must arrive, it must be adopted non-blocking, and it
/// must serve the real HTTP dispatch for the parent's request.
fn probe_systemd_socket_activate() {
    let state = probe_state_dir();
    let self_pid = std::process::id();
    let listen_pid = std::env::var("LISTEN_PID").expect("systemd-socket-activate sets LISTEN_PID");
    assert_eq!(
        listen_pid.parse::<u32>().expect("numeric LISTEN_PID"),
        self_pid,
        "LISTEN_PID must name this child, never another process"
    );
    assert_eq!(
        std::env::var("LISTEN_FDS").expect("systemd-socket-activate sets LISTEN_FDS"),
        "1",
        "the live test is the HTTP-only activation role"
    );

    let listeners = adopt_systemd_listener()
        .expect("adoption succeeds")
        .expect("activation is present");
    assert_eq!(listeners.http_listeners.len(), 1);
    assert!(
        listeners.forward_listener.is_none(),
        "LISTEN_FDS=1 must not invent a forward listener"
    );
    assert_nonblocking(
        &listeners.http_listeners[0],
        "systemd-activated HTTP ingress listener",
    );

    let registry = stub_registry(Arc::new(StubWorldEngine::new()));
    let ingress =
        HttpIngress::bind(&state, listeners, registry).expect("bind the adopted listener + socket");
    fs::write(state.join("probe.ready"), b"ready\n").expect("write the readiness marker");
    thread::spawn(move || {
        let _ = ingress.serve();
    });

    let done = state.join("probe.done");
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !done.exists() {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(done.exists(), "the parent never finished its request");
}

#[test]
fn activation_single_listener_handover() {
    let state = cached_test_dir("activation-single");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind the probe listener");
    let port = listener.local_addr().expect("probe address").port();
    let prefix = "exec 3<&0; LISTEN_PID=$$; LISTEN_FDS=1; export LISTEN_PID LISTEN_FDS";

    let mut child = spawn_serving_probe(prefix, "one_http_listener", &state, port, listener);
    await_ready(&mut child, &state.join("probe.ready"));
    let response = tcp_request(port, "nope.localhost", "/");
    fs::write(state.join("probe.done"), b"done\n").expect("signal probe completion");
    let status = wait_for_exit(&mut child);
    assert!(
        status.success(),
        "serving probe failed with {status}\nstdout:\n{}\nstderr:\n{}",
        drain(&mut child.stdout),
        drain(&mut child.stderr)
    );
    assert!(
        response.starts_with("HTTP/1.1 404 Not Found"),
        "adopted fd 3 must serve the ingress dispatch, got:\n{response}"
    );
    assert!(
        response.contains("no world is attached for host 'nope.localhost'"),
        "404 body must name the Host, got:\n{response}"
    );
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn activation_descriptor_order_splits_forward_listener() {
    let state = cached_test_dir("activation-order");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind the probe listener");
    let port = listener.local_addr().expect("probe address").port();
    // `UnitSpec`'s order is `127.0.0.1:80`, `[::1]:80`, `127.0.0.1:<forward_port>`; the role
    // assignment must not inspect ports, so three duplicated descriptors prove the split.
    let prefix = "exec 3<&0 4<&0 5<&0; LISTEN_PID=$$; LISTEN_FDS=3; export LISTEN_PID LISTEN_FDS";
    listener
        .set_nonblocking(false)
        .expect("start blocking like systemd");
    let output = probe_command(prefix, "descriptor_order", &state, Some(port))
        .stdin(Stdio::from(OwnedFd::from(listener)))
        .output()
        .expect("spawn the descriptor-order probe");
    assert_probe_ok("descriptor_order", &output);
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn activation_listen_mode_without_forward_port() {
    let state = cached_test_dir("activation-listen");
    run_probe("unset LISTEN_PID LISTEN_FDS", "listen_mode", &state);
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn fail_closed_pid_mismatch_never_falls_back() {
    let state = cached_test_dir("fail-closed-pid");
    run_probe(
        "exec 3<&0; LISTEN_PID=1; LISTEN_FDS=1; export LISTEN_PID LISTEN_FDS",
        "pid_mismatch",
        &state,
    );
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn fail_closed_zero_descriptors() {
    let state = cached_test_dir("fail-closed-zero");
    run_probe(
        "LISTEN_PID=$$; LISTEN_FDS=0; export LISTEN_PID LISTEN_FDS",
        "zero_descriptors",
        &state,
    );
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn fail_closed_non_socket_descriptor() {
    let state = cached_test_dir("fail-closed-broken");
    run_probe(
        "exec 3</dev/null; LISTEN_PID=$$; LISTEN_FDS=1; export LISTEN_PID LISTEN_FDS",
        "broken_descriptor",
        &state,
    );
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn fail_closed_no_listener_at_all() {
    let state = cached_test_dir("fail-closed-none");
    run_probe("unset LISTEN_PID LISTEN_FDS", "no_listener", &state);
    let _ = fs::remove_dir_all(&state);
}

// ---------------------------------------------------------------------------------------------
// WorldRegistry
// ---------------------------------------------------------------------------------------------

#[test]
fn registry_mints_one_origin_per_world() {
    let state = cached_test_dir("registry-origin");
    let registry = stub_registry(Arc::new(StubWorldEngine::new()));
    let project = acme();

    let canonical = LocalHostRoute::new(project.clone(), WorldTarget::Canonical, None, &[])
        .expect("canonical route");
    assert_eq!(
        registry
            .mint_origin(&canonical, 80)
            .expect("port-80 origin")
            .url(),
        "http://acme.localhost",
        "port 80 is omitted from the origin"
    );
    assert_eq!(
        registry
            .mint_origin(&canonical, 8080)
            .expect("port-qualified origin")
            .url(),
        "http://acme.localhost:8080",
        "an ephemeral ingress port must be reachable from the origin"
    );

    let fork = LocalHostRoute::new(
        project.clone(),
        WorldTarget::UserFork("feat".to_owned()),
        None,
        &[],
    )
    .expect("fork route");
    assert_eq!(
        registry
            .mint_origin(&fork, 8080)
            .expect("fork origin")
            .url(),
        "http://feat.acme.localhost:8080"
    );

    let admin = LocalHostRoute::new(project, WorldTarget::Canonical, Some("admin"), &["admin"])
        .expect("declared snow domain");
    let err = registry
        .mint_origin(&admin, 80)
        .expect_err("a domains[].id ingress is never the single public origin");
    match err {
        EdgeError::Origin { world, host, .. } => {
            assert_eq!(world, "acme");
            assert_eq!(host, "admin.acme.localhost");
        }
        other => panic!("expected Origin, got {other:?}"),
    }
    let _ = fs::remove_dir_all(&state);
}

/// Registry-level [`WorldEngine`] double for the `slipfork-edge` suites.
///
/// The registry, ingress, proxy, forward, and share paths under test are real; only the engine they
/// are injected with is a double (`ARCHITECTURE.md` §1 test-scaffolding rule). It models world
/// handles, their lifecycle, the snapshot set a clone may come from, and an entry command that runs
/// the argv directly (a stub world has no namespaces to enter), and it dials a real loopback
/// `TcpStream`. It passes the shared
/// [`assert_world_engine_conformance`](slipfork_engine::conformance::assert_world_engine_conformance)
/// matrix; the service capabilities it never exercises are typed `Unsupported` naming the engine.
struct StubWorldEngine {
    live: Mutex<Vec<WorldHandle>>,
    frozen: Mutex<Vec<WorldHandle>>,
    snapshots: Mutex<Vec<SnapshotId>>,
    destroyed: AtomicUsize,
    next_seq: AtomicU64,
}

impl StubWorldEngine {
    fn new() -> Self {
        Self {
            live: Mutex::new(Vec::new()),
            frozen: Mutex::new(Vec::new()),
            snapshots: Mutex::new(Vec::new()),
            destroyed: AtomicUsize::new(0),
            next_seq: AtomicU64::new(0),
        }
    }

    /// Worlds destroyed through [`WorldEngine::destroy_world`] so far.
    fn destroyed(&self) -> usize {
        self.destroyed.load(Ordering::SeqCst)
    }

    /// Record `id` as a snapshot this engine can clone from (idempotent).
    fn record_snapshot(&self, id: &SnapshotId) {
        let mut snapshots = self.snapshots.lock().expect("stub snapshots");
        if !snapshots.contains(id) {
            snapshots.push(id.clone());
        }
    }

    /// Whether `id` is a snapshot this engine can clone from.
    fn has_snapshot(&self, id: &SnapshotId) -> bool {
        self.snapshots.lock().expect("stub snapshots").contains(id)
    }

    fn live_world(&self, world: &WorldHandle) -> Result<(), EngineError> {
        let live = self.live.lock().expect("stub live worlds");
        if live.contains(world) {
            Ok(())
        } else {
            Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            })
        }
    }

    fn unsupported<T>(&self, capability: &'static str) -> Result<T, EngineError> {
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability,
        })
    }
}

impl WorldEngine for StubWorldEngine {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn create_snapshot(&self, _source_dir: &Path, id: &SnapshotId) -> Result<(), EngineError> {
        id.validate()?;
        self.record_snapshot(id);
        Ok(())
    }

    fn snapshot_exists(&self, id: &SnapshotId) -> Result<bool, EngineError> {
        id.validate()?;
        Ok(self.has_snapshot(id))
    }

    fn freeze_world(&self, world: &WorldHandle, id: &SnapshotId) -> Result<(), EngineError> {
        self.live_world(world)?;
        id.validate()?;
        let mut frozen = self.frozen.lock().expect("stub frozen worlds");
        if frozen.contains(world) {
            return Err(EngineError::WorldNotFreezable {
                world: format!("{world:?}"),
            });
        }
        frozen.push(world.clone());
        drop(frozen);
        self.record_snapshot(id);
        Ok(())
    }

    fn clone_world(&self, spec: &WorldSpec) -> Result<WorldHandle, EngineError> {
        spec.validate()?;
        if !self.has_snapshot(&spec.snapshot) {
            return Err(EngineError::SnapshotNotFound(spec.snapshot.0.clone()));
        }
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let world = self.mint_world_handle(seq);
        self.live
            .lock()
            .expect("stub live worlds")
            .push(world.clone());
        Ok(world)
    }

    fn spawn_service(
        &self,
        _world: &WorldHandle,
        _spec: &ServiceSpec,
    ) -> Result<ServiceHandle, EngineError> {
        self.unsupported("spawn_service")
    }

    fn stop_service(&self, _service: &ServiceHandle) -> Result<(), EngineError> {
        self.unsupported("stop_service")
    }

    fn stop_world_services(&self, world: &WorldHandle) -> Result<(), EngineError> {
        // The double spawns no service, so a live world has nothing to stop; an unknown or
        // destroyed handle still fails closed like the real engine.
        self.live_world(world)
    }

    fn service_logs(&self, _world: &WorldHandle, _id: &str) -> Result<ServiceLogs, EngineError> {
        self.unsupported("service_logs")
    }

    fn world_processes(&self, world: &WorldHandle) -> Result<Vec<WorldServiceInfo>, EngineError> {
        // The double spawns no service (`spawn_service` is `Unsupported`), so a live world has an
        // empty row set; a destroyed or unknown handle is gone, exactly like the real engine.
        self.live_world(world)?;
        Ok(Vec::new())
    }

    fn world_lineage(&self, world: &WorldHandle) -> Result<WorldLineage, EngineError> {
        // A stub world records no boot fact: every field is `None`/empty, never a fabricated value.
        self.live_world(world)?;
        Ok(empty_lineage())
    }

    fn exec_in_service(
        &self,
        _service: &ServiceHandle,
        _argv: &[String],
        _overlay_env: &ExecEnv,
        _on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        self.unsupported("exec_in_service")
    }

    fn service_entry_command(
        &self,
        _service: &ServiceHandle,
    ) -> Result<WorldEntryCommand, EngineError> {
        self.unsupported("service_entry_command")
    }

    fn exec_in_world(
        &self,
        _world: &WorldHandle,
        _argv: &[String],
        _cwd: &Path,
        _overlay_env: &ExecEnv,
        _on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        self.unsupported("exec_in_world")
    }

    fn entry_command(
        &self,
        world: &WorldHandle,
        cwd: &Path,
    ) -> Result<WorldEntryCommand, EngineError> {
        self.live_world(world)?;
        // A stub world has no namespaces to enter, so entering it runs the argv directly.
        Ok(WorldEntryCommand {
            program: PathBuf::from("env"),
            args_prefix: vec!["--".to_owned()],
            cwd: cwd.to_path_buf(),
        })
    }

    fn dial_world_tcp(&self, world: &WorldHandle, port: u16) -> Result<TcpStream, EngineError> {
        self.live_world(world)?;
        TcpStream::connect((Ipv4Addr::LOCALHOST, port)).map_err(|err| EngineError::DialFailed {
            world: format!("{world:?}"),
            port,
            errno: err.raw_os_error().unwrap_or(0),
        })
    }

    fn destroy_world(&self, world: WorldHandle) -> Result<(), EngineError> {
        let mut live = self.live.lock().expect("stub live worlds");
        let index = live
            .iter()
            .position(|candidate| candidate == &world)
            .ok_or_else(|| EngineError::WorldGone {
                world: format!("{world:?}"),
            })?;
        live.remove(index);
        drop(live);
        self.destroyed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// The stub never inspects the rootfs; only the provenance value is required by the `WorldSpec`
/// contract.
fn test_rootfs() -> WorldRootfs {
    WorldRootfs {
        digest: RootfsDigest::from_hex("e".repeat(64)).expect("valid digest"),
        app_rootfs: PathBuf::from("/nonexistent/slipfork-edge-test/app"),
        crun_bin: PathBuf::from("/.slipfork/bin/crun"),
        service_images: std::collections::BTreeMap::new(),
        metadata: ImageContractMetadata {
            os_release: OsRelease {
                id: "alpine".to_owned(),
                version_id: "3.21".to_owned(),
                version_codename: String::new(),
            },
            elf_abi: ElfAbi {
                arch: "x86_64".to_owned(),
                libc: LibcFamily::Musl,
                libc_version: "1.2".to_owned(),
            },
            exposed_ports: std::collections::BTreeSet::new(),
        },
        host_caches: Vec::new(),
        // The double never builds a rootfs; the fresh test tree is a miss with no measured build.
        cache: CacheOutcome::Miss,
        millis: None,
    }
}

/// The one snapshot every test world is cloned from; `clone_test_world` creates it first.
fn base_snapshot() -> SnapshotId {
    SnapshotId("base".to_owned())
}

/// The lineage of a stub world that recorded no boot fact: every fact is `None`, `services` is
/// empty, so a renderer prints `-`. This is a test fixture's own record set — production code
/// records its facts on the world's engine handle.
fn empty_lineage() -> WorldLineage {
    WorldLineage {
        rootfs: None,
        services: Vec::new(),
        seed: None,
        base: None,
    }
}

fn clone_test_world(engine: &dyn WorldEngine, state: &Path, target: WorldTarget) -> WorldHandle {
    engine
        .create_snapshot(state, &base_snapshot())
        .expect("record the base snapshot");
    let spec = WorldSpec {
        target,
        rootfs: test_rootfs(),
        snapshot: base_snapshot(),
        mounts: Vec::new(),
        base_env: ExecEnv::world_scoped(state.join("home")),
        local_ingress: None,
        // The spec is synthetic: it seeds no base and restores no dump.
        boot: BootLineage::default(),
    };
    engine.clone_world(&spec).expect("clone a test world")
}

#[test]
fn clone_world_refuses_an_unrecorded_snapshot() {
    let engine = StubWorldEngine::new();
    let spec = WorldSpec {
        target: WorldTarget::Canonical,
        rootfs: test_rootfs(),
        snapshot: SnapshotId("missing".to_owned()),
        mounts: Vec::new(),
        base_env: ExecEnv::world_scoped(PathBuf::from("/nonexistent/slipfork-edge-test/home")),
        local_ingress: None,
        // The spec is synthetic: it seeds no base and restores no dump.
        boot: BootLineage::default(),
    };
    let err = engine
        .clone_world(&spec)
        .expect_err("an unrecorded snapshot must fail closed");
    assert!(
        matches!(&err, EngineError::SnapshotNotFound(id) if id == "missing"),
        "expected SnapshotNotFound, got {err:?}"
    );
}

#[test]
fn stub_world_engine_passes_the_shared_conformance_matrix() {
    let state = cached_test_dir("stub-conformance");
    assert_world_engine_conformance(&StubWorldEngine::new(), &state, &test_rootfs());
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn registry_replace_world_swaps_routes_without_touching_the_handle() {
    let state = cached_test_dir("registry-replace");
    let engine = Arc::new(StubWorldEngine::new());
    let registry = stub_registry(Arc::clone(&engine));
    let project = acme();
    let target = WorldTarget::Canonical;
    let handle = clone_test_world(engine.as_ref(), &state, target.clone());

    let unknown = registry
        .replace_world(
            &project,
            &target,
            Vec::new(),
            vec![IngressRoute {
                prefix: String::new(),
                port: 8000,
            }],
        )
        .expect_err("an unattached world's routes cannot be replaced");
    assert!(
        matches!(&unknown, EdgeError::WorldNotAttached { world } if world == "acme"),
        "got {unknown:?}"
    );

    registry
        .attach_world(
            &project,
            &target,
            vec!["apps.acme.localhost".to_owned()],
            handle.clone(),
            vec![IngressRoute {
                prefix: String::new(),
                port: 8000,
            }],
        )
        .expect("attach the world");

    // The new `snow.yml` table: a longer prefix at a new port, plus a new domain.
    registry
        .replace_world(
            &project,
            &target,
            vec!["api.acme.localhost".to_owned()],
            vec![
                IngressRoute {
                    prefix: "/api".to_owned(),
                    port: 9000,
                },
                IngressRoute {
                    prefix: String::new(),
                    port: 9100,
                },
            ],
        )
        .expect("replace the routes of an attached world");
    let api = forward_decision(
        registry
            .resolve("acme.localhost", "/api/users", false)
            .expect("the edited prefix serves"),
    );
    assert_eq!(api.port(), 9000);
    let root = forward_decision(
        registry
            .resolve("acme.localhost", "/", false)
            .expect("the edited catch-all serves"),
    );
    assert_eq!(root.port(), 9100);
    assert_eq!(
        engine.destroyed(),
        0,
        "replacing routes must never destroy the live handle"
    );

    // The old table is gone, and the world's origin stays its `.localhost` origin.
    assert_eq!(
        registry.origin(&project, &target).expect("origin").url(),
        "http://acme.localhost"
    );
    // Invalid replacement tables still fail closed.
    let invalid = registry
        .replace_world(
            &project,
            &target,
            Vec::new(),
            vec![IngressRoute {
                prefix: "api".to_owned(),
                port: 9000,
            }],
        )
        .expect_err("a prefix without a leading '/' is refused");
    assert!(
        matches!(invalid, EdgeError::InvalidIngressRoute { .. }),
        "got {invalid:?}"
    );
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn registry_attach_rejects_duplicates_and_resolves_prefixes() {
    let state = cached_test_dir("registry-attach");
    let engine = Arc::new(StubWorldEngine::new());
    let registry = stub_registry(Arc::clone(&engine));
    let project = acme();
    let target = WorldTarget::UserFork("feat".to_owned());
    let routes = vec![
        IngressRoute {
            prefix: "/api".to_owned(),
            port: 3001,
        },
        IngressRoute {
            prefix: String::new(),
            port: 3000,
        },
    ];

    let handle = clone_test_world(engine.as_ref(), &state, target.clone());
    registry
        .attach_world(
            &project,
            &target,
            Vec::new(),
            handle.clone(),
            routes.clone(),
        )
        .expect("attach the live world");

    let err = registry
        .attach_world(&project, &target, Vec::new(), handle, routes)
        .expect_err("a duplicate active registration must fail closed");
    match err {
        EdgeError::WorldAlreadyRegistered { world } => assert_eq!(world, "acme:feat"),
        other => panic!("expected WorldAlreadyRegistered, got {other:?}"),
    }

    let api = forward_decision(
        registry
            .resolve("feat.acme.localhost", "/api/users?page=2", false)
            .expect("longest prefix wins"),
    );
    assert_eq!(api.port(), 3001);
    assert_eq!(api.world_id(), "acme:feat");
    assert_eq!(api.host(), "feat.acme.localhost");

    let catch_all = forward_decision(
        registry
            .resolve("feat.acme.localhost:8080", "/static/app.js", false)
            .expect("the empty prefix is the internal_ports[0] catch-all"),
    );
    assert_eq!(catch_all.port(), 3000);
    assert_eq!(
        catch_all.host(),
        "feat.acme.localhost:8080",
        "the raw Host header is preserved for the upstream request"
    );

    let err = registry
        .resolve("other.localhost", "/", false)
        .expect_err("an unattached project must not route");
    assert!(
        matches!(&err, EdgeError::UnknownHost { host } if host == "other.localhost"),
        "got {err:?}"
    );

    assert!(
        registry
            .release_world(&project, &target)
            .expect("release the attached world")
    );
    assert_eq!(
        engine.destroyed(),
        1,
        "release must destroy the in-process handle through the engine"
    );
    assert!(
        !registry
            .release_world(&project, &target)
            .expect("releasing an absent world is not an error"),
        "the second release reports nothing was registered"
    );
    let err = registry
        .resolve("feat.acme.localhost", "/api", false)
        .expect_err("a released world must stop routing");
    assert!(matches!(err, EdgeError::UnknownHost { .. }), "got {err:?}");
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn registry_rejects_invalid_ingress_tables() {
    let state = cached_test_dir("registry-invalid");
    let engine = Arc::new(StubWorldEngine::new());
    let registry = stub_registry(Arc::clone(&engine));
    let project = acme();
    let target = WorldTarget::Canonical;
    let handle = clone_test_world(engine.as_ref(), &state, target.clone());

    let invalid = [
        (Vec::new(), "at least one route"),
        (
            vec![IngressRoute {
                prefix: "/api".to_owned(),
                port: 0,
            }],
            "port 0",
        ),
        (
            vec![IngressRoute {
                prefix: "api".to_owned(),
                port: 3000,
            }],
            "must be empty or start with '/'",
        ),
        (
            vec![
                IngressRoute {
                    prefix: String::new(),
                    port: 3000,
                },
                IngressRoute {
                    prefix: String::new(),
                    port: 3001,
                },
            ],
            "duplicate ingress prefix",
        ),
    ];
    for (index, (routes, expected)) in invalid.into_iter().enumerate() {
        let err = registry
            .attach_world(&project, &target, Vec::new(), handle.clone(), routes)
            .expect_err("an unroutable ingress table must fail closed");
        match err {
            EdgeError::InvalidIngressRoute { world, reason } => {
                assert_eq!(world, "acme");
                assert!(
                    reason.contains(expected),
                    "case {index}: expected '{expected}' in '{reason}'"
                );
            }
            other => panic!("case {index}: expected InvalidIngressRoute, got {other:?}"),
        }
    }
    let err = registry
        .resolve("acme.localhost", "/", false)
        .expect_err("no refused table may leave a registered world behind");
    assert!(matches!(err, EdgeError::UnknownHost { .. }), "got {err:?}");
    let _ = fs::remove_dir_all(&state);
}

// ---------------------------------------------------------------------------------------------
// HTTP dispatch over TCP and <state_dir>/http-ingress.sock
// ---------------------------------------------------------------------------------------------

fn read_until_close(stream: &mut impl Read) -> String {
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read the ingress response");
    response
}

fn tcp_request(port: u16, host: &str, path: &str) -> String {
    let mut stream =
        TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect to the ingress");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("bound the response read");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .expect("write the request");
    read_until_close(&mut stream)
}

fn unix_request(socket: &Path, host: &str, path: &str) -> String {
    let mut stream = UnixStream::connect(socket).expect("connect to http-ingress.sock");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("bound the response read");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .expect("write the request");
    read_until_close(&mut stream)
}

fn send_raw(port: u16, request: &[u8]) -> String {
    let mut stream =
        TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect to the ingress");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("bound the response read");
    stream.write_all(request).expect("write the raw request");
    read_until_close(&mut stream)
}

#[test]
fn unix_ingress_sock_is_0600_and_serves_identical_404() {
    let state = cached_test_dir("unix-ingress");
    let engine = Arc::new(StubWorldEngine::new());
    let registry = stub_registry(Arc::clone(&engine));
    let project = acme();
    let target = WorldTarget::Canonical;
    let handle = clone_test_world(engine.as_ref(), &state, target.clone());
    let mut unroutable_port = closed_loopback_port();
    registry
        .attach_world(
            &project,
            &target,
            Vec::new(),
            handle,
            vec![IngressRoute {
                prefix: "/api".to_owned(),
                port: unroutable_port,
            }],
        )
        .expect("attach the canonical world");

    let listeners = bind_listen(&listen_local()).expect("bind --listen 127.0.0.1:0");
    let port = listeners.http_addrs().expect("bound ingress port")[0].port();
    let ingress =
        HttpIngress::bind(&state, listeners, Arc::clone(&registry)).expect("bind the ingress");
    let socket = ingress.unix_socket_path().to_path_buf();
    let mode = fs::metadata(&socket)
        .expect("http-ingress.sock exists")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "the ingress socket must be owner-only");
    thread::spawn(move || {
        let _ = ingress.serve();
    });

    let over_tcp = tcp_request(port, "missing.localhost", "/");
    let over_unix = unix_request(&socket, "missing.localhost", "/");
    assert_eq!(
        over_tcp, over_unix,
        "TCP and http-ingress.sock must run the exact same dispatch"
    );
    assert!(
        over_tcp.starts_with("HTTP/1.1 404 Not Found"),
        "unknown host must be a 404, got:\n{over_tcp}"
    );
    assert!(
        over_tcp.contains("no world is attached for host 'missing.localhost'"),
        "the 404 must name the Host, got:\n{over_tcp}"
    );

    let unmatched = tcp_request(port, "acme.localhost", "/nope");
    assert!(
        unmatched.starts_with("HTTP/1.1 404 Not Found"),
        "an unmatched ingress prefix must be a 404, got:\n{unmatched}"
    );
    assert!(
        unmatched.contains("world acme has no ingress route for '/nope' (host 'acme.localhost')"),
        "the 404 must name the world id, host, and path, got:\n{unmatched}"
    );

    // A resolved world whose backend cannot be dialed is a 502. The route points at a freshly
    // reserved loopback port, re-checked at dial time: if a concurrent binder took it in between,
    // the world is re-attached on a new port a bounded number of times (the reserved-port race
    // `socket_activate_systemd_handover` mirrors) rather than weakening the assertion.
    let mut attempts = 0;
    let unroutable_backend = loop {
        attempts += 1;
        let response = tcp_request(port, "acme.localhost", "/api/users");
        if response.starts_with("HTTP/1.1 502 Bad Gateway") {
            break response;
        }
        assert!(
            attempts < REFUSED_DIAL_ATTEMPTS,
            "a resolved world whose backend cannot be dialed must be a 502 on all \
             {REFUSED_DIAL_ATTEMPTS} attempts: a concurrent binder kept taking the freshly reserved \
             port; last response:\n{response}"
        );
        assert!(
            registry
                .release_world(&project, &target)
                .expect("release the raced world"),
            "the raced attempt's world must still be attached"
        );
        unroutable_port = closed_loopback_port();
        let handle = clone_test_world(engine.as_ref(), &state, target.clone());
        registry
            .attach_world(
                &project,
                &target,
                Vec::new(),
                handle,
                vec![IngressRoute {
                    prefix: "/api".to_owned(),
                    port: unroutable_port,
                }],
            )
            .expect("re-attach the world on a fresh port");
    };
    assert!(
        unroutable_backend.starts_with("HTTP/1.1 502 Bad Gateway"),
        "a resolved world whose backend cannot be dialed must be a 502, got:\n{unroutable_backend}"
    );
    assert!(
        unroutable_backend.contains(&format!(
            "world acme cannot reach its backend on port {unroutable_port}"
        )),
        "the 502 must name the world id, host, and port, got:\n{unroutable_backend}"
    );
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn unix_ingress_sock_refuses_live_socket() {
    let state = cached_test_dir("unix-live");
    let live = UnixListener::bind(state.join("http-ingress.sock")).expect("bind a live socket");
    let listeners = bind_listen(&listen_local()).expect("bind --listen");
    let err = HttpIngress::bind(
        &state,
        listeners,
        stub_registry(Arc::new(StubWorldEngine::new())),
    )
    .expect_err("a live ingress socket must not be stolen");
    match err {
        EdgeError::SocketInUse { path, .. } => {
            assert!(path.ends_with("http-ingress.sock"), "got '{path}'");
        }
        other => panic!("expected IngressSocketInUse, got {other:?}"),
    }
    drop(live);
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn unix_ingress_sock_replaces_stale_file() {
    let state = cached_test_dir("unix-stale");
    let socket = state.join("http-ingress.sock");
    {
        let stale = UnixListener::bind(&socket).expect("bind the stale socket");
        drop(stale);
    }
    assert!(
        socket.exists(),
        "a dropped listener leaves a stale socket file"
    );

    let listeners = bind_listen(&listen_local()).expect("bind --listen");
    let ingress = HttpIngress::bind(
        &state,
        listeners,
        stub_registry(Arc::new(StubWorldEngine::new())),
    )
    .expect("a stale socket is replaced");
    let mode = fs::metadata(ingress.unix_socket_path())
        .expect("replacement socket exists")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn routing_skeleton_rejects_unknown_methods_and_oversized_heads() {
    let state = cached_test_dir("ingress-refusals");
    let engine = Arc::new(StubWorldEngine::new());
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = registry_over(Arc::clone(&engine), registrar, None);
    attach_pointing_at(
        &registry,
        &engine,
        &state,
        &acme(),
        &WorldTarget::Canonical,
        Vec::new(),
        spawn_backend(BackendMode::Head).port,
    );
    let port = start_ingress(&state, registry);

    let unknown_method = send_raw(port, b"BREW / HTTP/1.1\r\nHost: acme.localhost\r\n\r\n");
    assert!(
        unknown_method.starts_with("HTTP/1.1 405 Method Not Allowed"),
        "an unknown verb must be a 405, got:\n{unknown_method}"
    );
    assert!(
        unknown_method.contains("Allow: GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS"),
        "a 405 must advertise the allowed methods, got:\n{unknown_method}"
    );

    let missing_host = send_raw(port, b"GET / HTTP/1.1\r\n\r\n");
    assert!(
        missing_host.starts_with("HTTP/1.1 400 Bad Request"),
        "a request without Host must be a 400, got:\n{missing_host}"
    );

    let mut oversized = String::from("GET / HTTP/1.1\r\nHost: acme.localhost\r\nX-Pad: ");
    let pad = slipfork_edge::ingress::HEADER_CAP - oversized.len();
    oversized.push_str(&"a".repeat(pad));
    let too_large = send_raw(port, oversized.as_bytes());
    assert!(
        too_large.starts_with("HTTP/1.1 431 Request Header Fields Too Large"),
        "a head at the 16 KiB cap must be refused, got:\n{too_large}"
    );
    let _ = fs::remove_dir_all(&state);
}
// ---------------------------------------------------------------------------------------------
// Shared worlds: proxying, 308/421 rules, WebSocket byte piping
// ---------------------------------------------------------------------------------------------

/// One proxied decision; the other variants are asserted explicitly where they are expected.
fn forward_decision(decision: RouteDecision) -> RoutedWorld {
    match decision {
        RouteDecision::Forward(routed) => routed,
        other => panic!("expected a proxied route, got {other:?}"),
    }
}

/// An in-memory `TunnelRegistrar` (`tests/` only, per `M2.9`) that records the registration.
struct InMemoryTunnelRegistrar {
    url: String,
    calls: Mutex<Vec<(String, u16)>>,
    revoked: Arc<AtomicUsize>,
    minted: AtomicUsize,
}

impl InMemoryTunnelRegistrar {
    fn new(url: &str) -> Self {
        Self {
            url: url.to_owned(),
            calls: Mutex::new(Vec::new()),
            revoked: Arc::new(AtomicUsize::new(0)),
            minted: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> Vec<(String, u16)> {
        self.calls.lock().expect("registrar calls").clone()
    }
}

impl TunnelRegistrar for InMemoryTunnelRegistrar {
    fn register(&self, request: &TunnelRequest<'_>) -> Result<Box<dyn TunnelLease>, TunnelError> {
        assert_eq!(
            request.route.snow_domain(),
            None,
            "a shared world is always the default-ingress route"
        );
        let minted = self.minted.fetch_add(1, Ordering::SeqCst) + 1;
        self.calls
            .lock()
            .expect("registrar calls")
            .push((request.route.host(), request.local_port));
        Ok(Box::new(InMemoryLease {
            url: self.url.clone(),
            id: format!("lease-{minted}"),
            revoked: Arc::clone(&self.revoked),
        }))
    }
}

struct InMemoryLease {
    url: String,
    id: String,
    revoked: Arc<AtomicUsize>,
}

impl TunnelLease for InMemoryLease {
    fn public_url(&self) -> &str {
        &self.url
    }

    fn lease_id(&self) -> &str {
        &self.id
    }
}

impl Drop for InMemoryLease {
    fn drop(&mut self) {
        self.revoked.fetch_add(1, Ordering::SeqCst);
    }
}

/// One real loopback HTTP/TCP backend standing in for a world-local service.
struct Backend {
    port: u16,
    connections: Arc<AtomicUsize>,
    body_bytes: Arc<AtomicUsize>,
}

#[derive(Clone, Copy)]
enum BackendMode {
    /// Reply with the received request head (plus the body length) as the body.
    Head,
    /// Reply `EARLY` before reading the body, then drain and count it.
    Stream,
    /// Answer `101 Switching Protocols`, then echo every byte immediately (tiny reads/writes).
    UpgradeEcho,
}

fn spawn_backend(mode: BackendMode) -> Backend {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind the backend");
    let port = listener.local_addr().expect("backend address").port();
    let connections = Arc::new(AtomicUsize::new(0));
    let body_bytes = Arc::new(AtomicUsize::new(0));
    let accepts = Arc::clone(&connections);
    let body = Arc::clone(&body_bytes);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            accepts.fetch_add(1, Ordering::SeqCst);
            let body = Arc::clone(&body);
            thread::spawn(move || handle_backend(stream, mode, &body));
        }
    });
    Backend {
        port,
        connections,
        body_bytes,
    }
}

fn handle_backend(mut stream: TcpStream, mode: BackendMode, body: &AtomicUsize) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(20)));
    let Some((head, head_len)) = read_backend_head(&mut stream) else {
        return;
    };
    let text = String::from_utf8_lossy(&head).into_owned();
    match mode {
        BackendMode::Head => {
            let length = content_length(&text);
            let received = drain_body(&mut stream, &head[head_len..], length);
            body.fetch_add(received, Ordering::SeqCst);
            let payload = format!("{text}\nBODY:{received}\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.shutdown(Shutdown::Both);
        }
        BackendMode::Stream => {
            let length = content_length(&text);
            // The response leaves before any request body is read: a proxy that buffers the whole
            // request before forwarding would deadlock the client here.
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nEARLY",
            );
            let _ = stream.flush();
            let received = drain_body(&mut stream, &head[head_len..], length);
            body.fetch_add(received, Ordering::SeqCst);
            let _ = stream.shutdown(Shutdown::Both);
        }
        BackendMode::UpgradeEcho => {
            let _ = stream.write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
            );
            let _ = stream.flush();
            let mut buffer = [0_u8; 7];
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => return,
                    Ok(read) => {
                        if stream.write_all(&buffer[..read]).is_err() {
                            return;
                        }
                        let _ = stream.flush();
                    }
                }
            }
        }
    }
}

fn read_backend_head(stream: &mut TcpStream) -> Option<(Vec<u8>, usize)> {
    let mut head = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        head.extend_from_slice(&chunk[..read]);
        if let Some(index) = head.windows(4).position(|window| window == b"\r\n\r\n") {
            return Some((head, index + 4));
        }
    }
}

fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())
                .flatten()
        })
        .unwrap_or(0)
}

fn drain_body(stream: &mut TcpStream, already_read: &[u8], length: usize) -> usize {
    let mut received = already_read.len().min(length);
    let mut buffer = [0_u8; 16 * 1024];
    while received < length {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => received = (received + read).min(length),
        }
    }
    received
}

/// Attach a live world whose catch-all route points at `backend_port`.
fn attach_pointing_at(
    registry: &WorldRegistry,
    engine: &Arc<StubWorldEngine>,
    state: &Path,
    project: &ProjectSlug,
    target: &WorldTarget,
    snow_domains: Vec<String>,
    backend_port: u16,
) {
    let handle = clone_test_world(engine.as_ref(), state, target.clone());
    registry
        .attach_world(
            project,
            target,
            snow_domains,
            handle,
            vec![IngressRoute {
                prefix: String::new(),
                port: backend_port,
            }],
        )
        .expect("attach the world pointing at the test backend");
}

fn start_ingress(state: &Path, registry: Arc<WorldRegistry>) -> u16 {
    start_ingress_with_timeout(state, registry, Duration::from_secs(20))
}

fn start_ingress_with_timeout(
    state: &Path,
    registry: Arc<WorldRegistry>,
    relay_timeout: Duration,
) -> u16 {
    let listeners = bind_listen(&listen_local()).expect("bind --listen 127.0.0.1:0");
    let port = listeners.http_addrs().expect("bound ingress port")[0].port();
    let ingress = HttpIngress::bind(state, listeners, registry)
        .expect("bind the ingress")
        .with_relay_timeout(relay_timeout);
    thread::spawn(move || {
        let _ = ingress.serve();
    });
    port
}

#[test]
fn shared_proxy_preserves_host_and_forwarded_headers() {
    let state = cached_test_dir("shared-proxy");
    let engine = Arc::new(StubWorldEngine::new());
    let backend = spawn_backend(BackendMode::Head);
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = registry_over(Arc::clone(&engine), registrar.clone(), Some(7080));
    let project = acme();
    attach_pointing_at(
        &registry,
        &engine,
        &state,
        &project,
        &WorldTarget::Canonical,
        Vec::new(),
        backend.port,
    );
    let shared = registry
        .share_world(&project, &WorldTarget::Canonical)
        .expect("share the world");
    assert_eq!(shared.public_url(), "https://acme-abc.weaved.example");
    assert_eq!(
        registrar.calls(),
        vec![("acme.localhost".to_owned(), 7080)],
        "the lease is registered against the planned static forward port, never a .localhost host"
    );
    let port = start_ingress(&state, Arc::clone(&registry));

    // A tunnel hostname arrives with the public `Host` and any client-supplied forwarding headers;
    // the ingress must preserve `Host`, replace `X-Forwarded-*`, and never redirect.
    let response = send_raw(
        port,
        b"GET /api/users?page=2 HTTP/1.1\r\nHost: acme-abc.weaved.example\r\nX-Forwarded-For: 203.0.113.9\r\nConnection: close\r\n\r\n",
    );
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "a shared public host must be proxied, got:\n{response}"
    );
    assert!(
        response.contains("Host: acme-abc.weaved.example\r\n"),
        "the public Host must reach the backend verbatim, got:\n{response}"
    );
    assert!(
        response.contains("X-Forwarded-Host: acme-abc.weaved.example\r\n"),
        "X-Forwarded-Host must name the public host, got:\n{response}"
    );
    assert!(
        response.contains("X-Forwarded-For: 203.0.113.9, 127.0.0.1\r\n"),
        "X-Forwarded-For must preserve the front hop and append this one, got:\n{response}"
    );
    assert!(
        response.contains("X-Forwarded-Proto: http\r\n"),
        "X-Forwarded-Proto must be set, got:\n{response}"
    );
    assert!(
        response.contains("GET /api/users?page=2 HTTP/1.1\r\n"),
        "the request target and query must be forwarded unchanged, got:\n{response}"
    );
    assert_eq!(
        backend.connections.load(Ordering::SeqCst),
        1,
        "the shared host must be dialed exactly once"
    );
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn shared_proxy_streams_body_larger_than_buffer() {
    const BODY: usize = 2 * 1024 * 1024;
    let state = cached_test_dir("shared-stream");
    let engine = Arc::new(StubWorldEngine::new());
    let backend = spawn_backend(BackendMode::Stream);
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = registry_over(Arc::clone(&engine), registrar, Some(7080));
    let project = acme();
    attach_pointing_at(
        &registry,
        &engine,
        &state,
        &project,
        &WorldTarget::Canonical,
        Vec::new(),
        backend.port,
    );
    registry
        .share_world(&project, &WorldTarget::Canonical)
        .expect("share the world");
    let port = start_ingress(&state, Arc::clone(&registry));

    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("bound reads");
    stream
        .write_all(
            format!(
                "POST /upload HTTP/1.1\r\nHost: acme-abc.weaved.example\r\nContent-Length: {BODY}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .expect("write the head");

    // The backend answers before the body exists; receiving that answer proves the ingress
    // forwarded the head instead of buffering the whole request.
    let mut early = Vec::new();
    let mut chunk = [0_u8; 512];
    while !early.windows(5).any(|window| window == b"EARLY") {
        let read = stream.read(&mut chunk).expect("read the early response");
        assert!(read > 0, "the ingress closed before forwarding the head");
        early.extend_from_slice(&chunk[..read]);
    }

    let block = vec![b'z'; 64 * 1024];
    let mut sent = 0;
    while sent < BODY {
        let want = block.len().min(BODY - sent);
        stream
            .write_all(&block[..want])
            .expect("stream the request body");
        sent += want;
    }
    stream.shutdown(Shutdown::Write).expect("half-close");

    let mut rest = Vec::new();
    stream
        .read_to_end(&mut rest)
        .expect("read the response remainder");
    assert_eq!(
        backend.body_bytes.load(Ordering::SeqCst),
        BODY,
        "a {BODY}-byte body must stream through the bounded relay intact"
    );
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn redirect_308_for_every_method_and_before_100_continue() {
    let state = cached_test_dir("redirect");
    let engine = Arc::new(StubWorldEngine::new());
    let backend = spawn_backend(BackendMode::Head);
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = registry_over(Arc::clone(&engine), registrar, Some(7080));
    let project = acme();
    attach_pointing_at(
        &registry,
        &engine,
        &state,
        &project,
        &WorldTarget::Canonical,
        vec!["admin".to_owned()],
        backend.port,
    );
    registry
        .share_world(&project, &WorldTarget::Canonical)
        .expect("share the world");
    let port = start_ingress(&state, Arc::clone(&registry));

    for method in [
        "GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "BREW",
    ] {
        for host in [
            "acme.localhost",
            "admin.acme.localhost",
            "acme.localhost:8080",
        ] {
            let response = send_raw(
                port,
                format!(
                    "{method} /deep/path?x=1 HTTP/1.1\r\nHost: {host}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            );
            assert!(
                response.starts_with("HTTP/1.1 308 Permanent Redirect"),
                "{method} on a shared world's {host} must be a 308, got:\n{response}"
            );
            assert!(
                response.contains("Location: https://acme-abc.weaved.example/deep/path?x=1\r\n"),
                "the 308 must preserve the path and query, got:\n{response}"
            );
        }
    }
    // `HEAD` keeps the status and `Location` but carries neither a body nor `Content-Length`.
    let head = send_raw(
        port,
        b"HEAD /deep/path?x=1 HTTP/1.1\r\nHost: acme.localhost\r\nConnection: close\r\n\r\n",
    );
    assert!(
        head.starts_with("HTTP/1.1 308 Permanent Redirect"),
        "HEAD must be a 308, got:\n{head}"
    );
    assert!(
        head.contains("Location: https://acme-abc.weaved.example/deep/path?x=1\r\n"),
        "HEAD must keep Location, got:\n{head}"
    );
    assert!(
        !head.contains("Content-Length"),
        "HEAD must not advertise a Content-Length, got:\n{head}"
    );
    let (head_part, body_part) = head
        .split_once("\r\n\r\n")
        .expect("a complete response head");
    assert!(head_part.starts_with("HTTP/1.1 308"));
    assert!(
        body_part.is_empty(),
        "HEAD must not emit a body, got:\n{head}"
    );

    assert_eq!(
        backend.connections.load(Ordering::SeqCst),
        0,
        "a shared world's *.localhost host must never be proxied"
    );

    // `Expect: 100-continue` is answered with the redirect before any `100 Continue` and without
    // waiting for (or reading) the body.
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("bound reads");
    stream
        .write_all(
            b"POST /big HTTP/1.1\r\nHost: acme.localhost\r\nContent-Length: 1048576\r\nExpect: 100-continue\r\n\r\n",
        )
        .expect("write the head only");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read 308");
    assert!(
        response.starts_with("HTTP/1.1 308 Permanent Redirect"),
        "the 308 must precede 100 Continue, got:\n{response}"
    );
    assert!(
        !response.contains("100 Continue"),
        "the ingress must never send 100 Continue for a shared world, got:\n{response}"
    );
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn upgrade_refused_421_on_shared_localhost() {
    let state = cached_test_dir("upgrade-refused");
    let engine = Arc::new(StubWorldEngine::new());
    let backend = spawn_backend(BackendMode::Head);
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = registry_over(Arc::clone(&engine), registrar, Some(7080));
    let project = acme();
    attach_pointing_at(
        &registry,
        &engine,
        &state,
        &project,
        &WorldTarget::Canonical,
        Vec::new(),
        backend.port,
    );
    registry
        .share_world(&project, &WorldTarget::Canonical)
        .expect("share the world");
    let port = start_ingress(&state, Arc::clone(&registry));

    let response = send_raw(
        port,
        b"GET /ws HTTP/1.1\r\nHost: acme.localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
    );
    assert!(
        response.starts_with("HTTP/1.1 421 Misdirected Request"),
        "an Upgrade on a shared world's *.localhost host must be a 421, got:\n{response}"
    );
    assert!(
        response.contains("https://acme-abc.weaved.example"),
        "the 421 body must name the shared origin, got:\n{response}"
    );
    assert_eq!(
        backend.connections.load(Ordering::SeqCst),
        0,
        "an Upgrade on a shared world's *.localhost host must never be proxied"
    );
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn websocket_pipe_survives_fragmentation() {
    const PAYLOAD: usize = 4096;
    let state = cached_test_dir("websocket-pipe");
    let engine = Arc::new(StubWorldEngine::new());
    let backend = spawn_backend(BackendMode::UpgradeEcho);
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = registry_over(Arc::clone(&engine), registrar, Some(7080));
    let project = acme();
    attach_pointing_at(
        &registry,
        &engine,
        &state,
        &project,
        &WorldTarget::Canonical,
        Vec::new(),
        backend.port,
    );
    registry
        .share_world(&project, &WorldTarget::Canonical)
        .expect("share the world");
    let port = start_ingress(&state, Arc::clone(&registry));

    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("bound reads");
    stream
        .write_all(
            b"GET /ws HTTP/1.1\r\nHost: acme-abc.weaved.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        )
        .expect("write the upgrade handshake");

    let mut head = Vec::new();
    let mut chunk = [0_u8; 128];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk).expect("read the 101");
        assert!(read > 0, "the backend never answered the upgrade");
        head.extend_from_slice(&chunk[..read]);
    }
    let handshake = String::from_utf8_lossy(&head);
    assert!(
        handshake.starts_with("HTTP/1.1 101 Switching Protocols"),
        "a shared public host must proxy WebSocket handshakes, got:\n{handshake}"
    );

    // Fragmented writes in both directions prove the post-101 connection is a raw byte pipe.
    let payload: Vec<u8> = (0..PAYLOAD).map(|index| (index % 251) as u8).collect();
    for piece in payload.chunks(63) {
        stream.write_all(piece).expect("write a fragment");
        stream.flush().expect("flush a fragment");
        thread::sleep(Duration::from_millis(1));
    }
    let mut echoed = vec![0_u8; PAYLOAD];
    stream
        .read_exact(&mut echoed)
        .expect("read the echoed bytes");
    assert_eq!(
        echoed, payload,
        "the byte pipe must not re-frame the stream"
    );
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn websocket_pipe_survives_an_idle_gap_longer_than_the_relay_timeout() {
    const RELAY_TIMEOUT: Duration = Duration::from_millis(200);
    const IDLE_GAP: Duration = Duration::from_millis(500);
    let state = cached_test_dir("websocket-idle");
    let engine = Arc::new(StubWorldEngine::new());
    let backend = spawn_backend(BackendMode::UpgradeEcho);
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = registry_over(Arc::clone(&engine), registrar, Some(7080));
    let project = acme();
    attach_pointing_at(
        &registry,
        &engine,
        &state,
        &project,
        &WorldTarget::Canonical,
        Vec::new(),
        backend.port,
    );
    registry
        .share_world(&project, &WorldTarget::Canonical)
        .expect("share the world");
    let port = start_ingress_with_timeout(&state, Arc::clone(&registry), RELAY_TIMEOUT);

    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("bound reads");
    stream
        .write_all(
            b"GET /ws HTTP/1.1\r\nHost: acme-abc.weaved.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        )
        .expect("write the upgrade handshake");

    let mut head = Vec::new();
    let mut chunk = [0_u8; 128];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk).expect("read the 101");
        assert!(read > 0, "the backend never answered the upgrade");
        head.extend_from_slice(&chunk[..read]);
    }
    assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 101 Switching Protocols"));

    // Silence longer than the (injected) idle bound: an upgraded socket must stay open.
    thread::sleep(IDLE_GAP);
    stream
        .write_all(b"after-idle")
        .expect("write after the gap");
    stream.flush().expect("flush after the gap");
    let mut echoed = [0_u8; 10];
    stream
        .read_exact(&mut echoed)
        .expect("the upgraded socket must survive the idle gap");
    assert_eq!(&echoed, b"after-idle");
    let _ = fs::remove_dir_all(&state);
}

// ---------------------------------------------------------------------------------------------
// share_world / unshare_world asymmetry (ADR-0005 §2)
// ---------------------------------------------------------------------------------------------

#[test]
fn share_world_fails_closed_without_forward_listener_or_registrar() {
    let state = cached_test_dir("share-fail-closed");
    let engine = Arc::new(StubWorldEngine::new());
    let project = acme();
    let target = WorldTarget::Canonical;
    let backend = spawn_backend(BackendMode::Head);

    let no_forward = registry_over(
        Arc::clone(&engine),
        Arc::new(InMemoryTunnelRegistrar::new(
            "https://acme-abc.weaved.example",
        )),
        None,
    );
    attach_pointing_at(
        &no_forward,
        &engine,
        &state,
        &project,
        &target,
        Vec::new(),
        backend.port,
    );
    let err = no_forward
        .share_world(&project, &target)
        .expect_err("sharing without a forward listener must fail closed");
    assert!(matches!(err, EdgeError::NoForwardListener), "got {err:?}");

    let unavailable = registry_over(
        Arc::clone(&engine),
        Arc::new(UnavailableTunnelRegistrar::new()),
        Some(7080),
    );
    attach_pointing_at(
        &unavailable,
        &engine,
        &state,
        &project,
        &target,
        Vec::new(),
        backend.port,
    );
    let err = unavailable
        .share_world(&project, &target)
        .expect_err("sharing without a tunnel registrar must fail closed");
    assert!(
        matches!(err, EdgeError::TunnelUnavailable { ref world, .. } if world == "acme"),
        "got {err:?}"
    );
    let decision = unavailable
        .resolve("acme.localhost", "/", false)
        .expect("the world still routes");
    assert!(
        matches!(decision, RouteDecision::Forward(_)),
        "a failed share must leave the world Local, got {decision:?}"
    );

    // A lease URL inside `*.localhost` would redirect-loop back into this ingress (`H-008`).
    let looping = registry_over(
        Arc::clone(&engine),
        Arc::new(InMemoryTunnelRegistrar::new("http://acme.localhost")),
        Some(7080),
    );
    attach_pointing_at(
        &looping,
        &engine,
        &state,
        &project,
        &target,
        Vec::new(),
        backend.port,
    );
    let err = looping
        .share_world(&project, &target)
        .expect_err("a .localhost lease URL must never be recorded");
    assert!(
        matches!(err, EdgeError::TunnelUnavailable { .. }),
        "got {err:?}"
    );
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn unshare_world_is_idempotent_on_local_world() {
    let state = cached_test_dir("unshare-idempotent");
    let engine = Arc::new(StubWorldEngine::new());
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registrar_port: Arc<dyn TunnelRegistrar> =
        Arc::clone(&registrar) as Arc<dyn TunnelRegistrar>;
    let registry = Arc::new(WorldRegistry::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        registrar_port,
        8080,
        Some(7080),
    ));
    let project = acme();
    let target = WorldTarget::Canonical;
    let backend = spawn_backend(BackendMode::Head);
    attach_pointing_at(
        &registry,
        &engine,
        &state,
        &project,
        &target,
        Vec::new(),
        backend.port,
    );

    for _ in 0..2 {
        let origin = registry
            .unshare_world(&project, &target)
            .expect("unsharing a local world is an idempotent success");
        assert_eq!(
            origin,
            WorldOrigin::local(
                &LocalHostRoute::new(project.clone(), target.clone(), None, &[])
                    .expect("canonical route"),
                8080
            )
            .expect("local origin"),
            "an unshared world keeps its single .localhost origin"
        );
    }
    assert_eq!(
        registrar.revoked.load(Ordering::SeqCst),
        0,
        "unsharing a world with no lease must not revoke anything"
    );

    registry
        .share_world(&project, &target)
        .expect("share the world");
    let redirect = registry
        .resolve("acme.localhost", "/a?b=1", false)
        .expect("the shared world redirects");
    match redirect {
        RouteDecision::Redirect { location, .. } => {
            assert_eq!(location, "https://acme-abc.weaved.example/a?b=1");
        }
        other => panic!("expected a 308 redirect, got {other:?}"),
    }
    let origin = registry
        .unshare_world(&project, &target)
        .expect("unshare the world");
    assert_eq!(origin.url(), "http://acme.localhost:8080");
    assert_eq!(
        registrar.revoked.load(Ordering::SeqCst),
        1,
        "unsharing must drop (revoke) the tunnel lease"
    );
    let decision = registry
        .resolve("acme.localhost", "/a", false)
        .expect("the world routes locally again");
    assert!(
        matches!(decision, RouteDecision::Forward(_)),
        "got {decision:?}"
    );
    let err = registry
        .resolve("acme-abc.weaved.example", "/a", false)
        .expect_err("the public hostname must stop proxying after unshare");
    assert!(matches!(err, EdgeError::UnknownHost { .. }), "got {err:?}");
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn release_world_clears_the_share_and_revokes_the_lease() {
    let state = cached_test_dir("release-share");
    let engine = Arc::new(StubWorldEngine::new());
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = registry_over(Arc::clone(&engine), registrar.clone(), Some(7080));
    let project = acme();
    let target = WorldTarget::Canonical;
    let backend = spawn_backend(BackendMode::Head);
    attach_pointing_at(
        &registry,
        &engine,
        &state,
        &project,
        &target,
        Vec::new(),
        backend.port,
    );
    registry
        .share_world(&project, &target)
        .expect("share the world");
    assert!(registry.release_world(&project, &target).expect("release"));
    assert_eq!(
        registrar.revoked.load(Ordering::SeqCst),
        1,
        "release_world must drop the lease"
    );
    let err = registry
        .resolve("acme-abc.weaved.example", "/", false)
        .expect_err("a released world's public host must stop proxying");
    assert!(matches!(err, EdgeError::UnknownHost { .. }), "got {err:?}");
    let _ = fs::remove_dir_all(&state);
}

// ---------------------------------------------------------------------------------------------
// DaemonControl over <runtime_dir>/daemon.sock
// ---------------------------------------------------------------------------------------------

/// A deterministic in-process `DaemonControl`: `share`/`unshare` delegate 1:1 to the registry,
/// `forward` uses the real daemon-owned bridge, and the remaining calls prove the wire contract.
struct TestControl {
    registry: Arc<WorldRegistry>,
    project: ProjectSlug,
    target: WorldTarget,
    forward_port: u16,
    ingress: IngressInfo,
    last_cwd: Mutex<Option<PathBuf>>,
    secret_values: Mutex<Vec<String>>,
}

impl TestControl {
    fn new(registry: Arc<WorldRegistry>, forward_port: u16) -> Self {
        // `registry_over` models a `:80` ingress, so the status report mirrors that bind.
        let ingress = ingress_info(80);
        Self {
            registry,
            project: acme(),
            target: WorldTarget::Canonical,
            forward_port,
            ingress,
            last_cwd: Mutex::new(None),
            secret_values: Mutex::new(Vec::new()),
        }
    }

    fn record_cwd(&self, cwd: &Path) {
        *self.last_cwd.lock().expect("last cwd") = Some(cwd.to_path_buf());
    }

    fn world_text(&self) -> String {
        slipfork_edge::world_id(&self.project, &self.target)
    }

    fn summary(&self) -> Result<WorldSummary, EdgeError> {
        Ok(WorldSummary {
            world: self.world_text(),
            parent: None,
            state: WorldState::Running,
            origin: self.registry.origin(&self.project, &self.target)?,
            worktree: PathBuf::from("/repo"),
            lineage: empty_lineage(),
        })
    }

    fn up_summary(
        &self,
        changed: bool,
        restarted: Vec<String>,
    ) -> Result<WorldUpSummary, EdgeError> {
        let world = self.world_text();
        Ok(WorldUpSummary {
            world: world.clone(),
            state: WorldState::Running,
            changed,
            restarted,
            worktree: PathBuf::from("/repo"),
            origin: self.registry.origin(&self.project, &self.target)?,
            lineage: empty_lineage(),
            services: vec![
                service_row(
                    &world,
                    "pg_databases.main",
                    ServiceKind::BackingService,
                    ServiceState::Running,
                ),
                service_row(&world, "api", ServiceKind::Component, ServiceState::Running),
            ],
            resources: vec![ResourceSummary {
                id: "pg_databases.main".to_owned(),
                strategy: "prefer_remote".to_owned(),
                resolved: "local".to_owned(),
                reason: "OSPP unpaired".to_owned(),
            }],
            next: slipfork_edge::world_next_commands(WorldState::Running),
        })
    }
}

impl DaemonControl for TestControl {
    fn status(&self) -> Result<DaemonStatus, EdgeError> {
        Ok(DaemonStatus {
            pid: std::process::id(),
            ingress: self.ingress.clone(),
            forward_port: self.registry.planned_forward_port(),
            worlds: vec![self.summary()?],
        })
    }

    fn up(
        &self,
        req: &UpRequest,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError> {
        assert_eq!(req.dump, DumpRequest::NoDump);
        assert_eq!(req.resources, ResourceRequest::Local);
        self.record_cwd(&req.cwd);
        // The world is already running: the idempotent `up` reports `changed: false` and names
        // nothing it restarted.
        let summary = self.up_summary(false, Vec::new())?;
        on_event(BootEvent::Resources {
            resource: summary.resources[0].clone(),
        });
        on_event(BootEvent::Ready {
            component: "api".to_owned(),
            port: 8000,
        });
        Ok(summary)
    }

    fn restart(
        &self,
        req: &WorldSelector,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError> {
        self.record_cwd(&req.cwd);
        if req
            .world
            .as_deref()
            .is_some_and(|selected| selected != self.world_text())
        {
            // A world this control does not hold is never booted by `restart`.
            let selected = req.world.as_deref().unwrap_or_default();
            return Err(EdgeError::Control(DaemonError::new(
                DaemonErrorKind::NotRunning,
                format!("world '{selected}' is not running; boot it with `slipfork up`"),
            )));
        }
        on_event(BootEvent::Hook {
            id: "api:pre-deploy".to_owned(),
            argv: vec!["migrate".to_owned()],
        });
        self.up_summary(true, vec!["pg_databases.main".to_owned(), "api".to_owned()])
    }

    fn down(&self, req: &WorldSelector) -> Result<bool, EdgeError> {
        self.record_cwd(&req.cwd);
        Ok(true)
    }

    fn fork(
        &self,
        req: &ForkRequest,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError> {
        self.record_cwd(&req.cwd);
        on_event(BootEvent::Hook {
            id: "api:pre-deploy".to_owned(),
            argv: vec!["migrate".to_owned()],
        });
        self.up_summary(true, vec!["api".to_owned()])
    }

    fn drop_fork(&self, req: &DropForkRequest) -> Result<bool, EdgeError> {
        self.record_cwd(&req.cwd);
        Ok(true)
    }

    fn list(&self, _repo_root: Option<&Path>) -> Result<Vec<WorldSummary>, EdgeError> {
        Ok(vec![self.summary()?])
    }

    fn world_processes(&self, req: &WorldSelector) -> Result<WorldProcessReport, EdgeError> {
        self.record_cwd(&req.cwd);
        let world = self.world_text();
        match req.world.as_deref() {
            // The only world this control holds; anything else is unknown, never an empty report.
            None => {}
            Some(selected) if selected == world => {}
            Some(selected) => {
                return Err(EdgeError::Control(DaemonError::new(
                    DaemonErrorKind::UnknownWorld,
                    format!("no world '{selected}' here; boot it with `slipfork up`"),
                )));
            }
        }
        // One running component and one stopped backing service: a stopped row keeps its row.
        WorldProcessReport::new(
            WorldState::Running,
            empty_lineage(),
            vec![
                service_row(
                    &world,
                    "pg_databases.main",
                    ServiceKind::BackingService,
                    ServiceState::Stopped,
                ),
                service_row(&world, "api", ServiceKind::Component, ServiceState::Running),
            ],
        )
    }

    fn prune(&self, dry_run: bool) -> Result<PruneReport, EdgeError> {
        Ok(PruneReport {
            dry_run,
            worlds: vec!["acme:orphan".to_owned()],
            snapshots: Vec::new(),
        })
    }

    fn share(&self, req: &WorldSelector) -> Result<SharedWorldInfo, EdgeError> {
        self.record_cwd(&req.cwd);
        self.registry.share_world(&self.project, &self.target)
    }

    fn unshare(&self, req: &WorldSelector) -> Result<WorldOrigin, EdgeError> {
        self.record_cwd(&req.cwd);
        self.registry.unshare_world(&self.project, &self.target)
    }

    fn secret_set(
        &self,
        _repo_root: &Path,
        key: &str,
        value: &str,
        _force: bool,
    ) -> Result<(), EdgeError> {
        assert!(!value.is_empty(), "the daemon receives the secret value");
        self.secret_values
            .lock()
            .expect("secret values")
            .push(key.to_owned());
        Ok(())
    }

    fn secret_unset(&self, _repo_root: &Path, _key: &str) -> Result<bool, EdgeError> {
        Ok(true)
    }

    fn secret_list(&self, _repo_root: &Path) -> Result<Vec<String>, EdgeError> {
        Ok(self.secret_values.lock().expect("secret values").clone())
    }

    fn logs(
        &self,
        _req: &LogsRequest,
        on_chunk: &mut dyn FnMut(StreamChunk),
    ) -> Result<(), EdgeError> {
        // Larger than one frame's safe chunk size, so the server must split it.
        on_chunk(StreamChunk {
            source: StreamSource::Stdout,
            bytes: vec![b'x'; 20 * 1024],
        });
        Ok(())
    }

    fn exec_target(&self, req: &ExecTargetRequest) -> Result<WorldExecTarget, EdgeError> {
        self.record_cwd(&req.cwd);
        let mut env = ExecEnv::world_scoped("/world/home");
        env.set_secret("TOKEN", "must-not-leak")
            .map_err(|err| EdgeError::ControlProtocol {
                reason: err.to_string(),
            })?;
        Ok(WorldExecTarget {
            world: slipfork_edge::world_id(&self.project, &self.target),
            origin: self.registry.origin(&self.project, &self.target)?,
            entry: WorldEntryCommand {
                program: PathBuf::from("nsenter"),
                args_prefix: vec!["--target".to_owned()],
                cwd: PathBuf::from("/world"),
            },
            env,
            target_argv: req.argv.clone(),
        })
    }

    fn hook_run(
        &self,
        _req: &HookRunRequest,
        on_chunk: &mut dyn FnMut(StreamChunk),
    ) -> Result<i32, EdgeError> {
        on_chunk(StreamChunk {
            source: StreamSource::Stderr,
            bytes: b"hook output\n".to_vec(),
        });
        Ok(0)
    }

    fn forward(
        &self,
        req: &ForwardRequest,
        wait_disconnect: &mut dyn FnMut(ForwardSummary),
    ) -> Result<(), EdgeError> {
        self.record_cwd(&req.cwd);
        let id = req
            .targets
            .first()
            .cloned()
            .unwrap_or_else(|| "echo".to_owned());
        let bridge = self.registry.forward(
            &self.project,
            &self.target,
            &[ForwardTargetSpec {
                id,
                port: self.forward_port,
                scheme: "tcp".to_owned(),
                path: String::new(),
            }],
        )?;
        wait_disconnect(bridge.summary());
        Ok(())
    }

    fn auth_login(&self, endpoint_url: Option<&str>) -> Result<AuthSummary, EdgeError> {
        assert_eq!(endpoint_url, Some("wss://ospp.example/_ospp/v1"));
        Ok(AuthSummary {
            key_id: "p256:00".to_owned(),
            backend: "software:file-0600".to_owned(),
            account: Some("developer".to_owned()),
            machine: Some("dev-laptop".to_owned()),
            pairing_url: slipfork_edge::daemon::ControlSecret::new(
                "https://ospp.example/pair#handle=abc",
            ),
        })
    }
}

fn start_control_server(runtime: &Path, control: Arc<TestControl>) -> PathBuf {
    let server = DaemonControlServer::bind(runtime, control).expect("bind the control socket");
    let path = server.socket_path().to_path_buf();
    let mode = fs::metadata(&path)
        .expect("daemon.sock exists")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "daemon.sock must be owner-only");
    thread::spawn(move || {
        let _ = server.serve();
    });
    path
}

/// A raw TCP echo server standing in for one world-local service behind `forward`.
fn spawn_tcp_echo() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind the echo backend");
    let port = listener.local_addr().expect("echo address").port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            thread::spawn(move || {
                let Ok(mut read_half) = stream.try_clone() else {
                    return;
                };
                let mut buffer = [0_u8; 1024];
                loop {
                    match read_half.read(&mut buffer) {
                        Ok(0) | Err(_) => return,
                        Ok(read) => {
                            if stream.write_all(&buffer[..read]).is_err() {
                                return;
                            }
                            let _ = stream.flush();
                        }
                    }
                }
            });
        }
    });
    port
}

fn ready_registry(state: &Path) -> Arc<WorldRegistry> {
    let engine = Arc::new(StubWorldEngine::new());
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = registry_over(Arc::clone(&engine), registrar, Some(7080));
    let backend = spawn_backend(BackendMode::Head);
    attach_pointing_at(
        &registry,
        &engine,
        state,
        &acme(),
        &WorldTarget::Canonical,
        Vec::new(),
        backend.port,
    );
    registry
}

#[test]
fn daemon_control_round_trip_and_frame_cap() {
    let state = cached_test_dir("control-round-trip");
    let registry = ready_registry(&state);
    let control = Arc::new(TestControl::new(registry, spawn_tcp_echo()));
    let socket = start_control_server(&state, Arc::clone(&control));
    let client = DaemonControlClient::bind(&state);
    assert_eq!(client.socket_path(), socket);
    let cwd = PathBuf::from("/tmp/opencode/project");

    let status = client.status().expect("status");
    assert_eq!(status.pid, std::process::id());
    assert_eq!(status.forward_port, Some(7080));
    assert_eq!(status.worlds.len(), 1);
    assert_eq!(status.worlds[0].origin.url(), "http://acme.localhost");

    let mut events = Vec::new();
    let up = client
        .up(
            &UpRequest {
                cwd: cwd.clone(),
                world: None,
                dump: DumpRequest::NoDump,
                resources: ResourceRequest::Local,
                host_caches: false,
            },
            &mut |event| events.push(event),
        )
        .expect("up");
    assert_eq!(up.world, "acme");
    assert_eq!(up.state, WorldState::Running);
    assert!(
        !up.changed,
        "the second `up` of a running world must report `changed: false`"
    );
    assert!(
        up.restarted.is_empty(),
        "an unchanged `up` must not claim it restarted anything"
    );
    assert_eq!(
        up.next,
        vec![
            "slipfork open".to_owned(),
            "slipfork db".to_owned(),
            "slipfork logs".to_owned(),
            "slipfork restart".to_owned(),
            "slipfork down".to_owned(),
        ]
    );
    assert_eq!(up.worktree, PathBuf::from("/repo"));
    assert_eq!(up.resources[0].id, "pg_databases.main");
    assert_eq!(
        events.len(),
        2,
        "both boot events must stream before the summary"
    );
    assert_eq!(
        *control.last_cwd.lock().expect("last cwd"),
        Some(cwd.clone())
    );

    // `restart` reaches the socket and streams its events before the terminal summary. The
    // fixture is a double, so only the wire contract is asserted here; the real restart semantics
    // are proven against `DaemonService` in `slipfork::daemon`'s own tests.
    let mut restart_events = Vec::new();
    let restarted = client
        .restart(
            &WorldSelector {
                cwd: cwd.clone(),
                world: None,
            },
            &mut |event| restart_events.push(event),
        )
        .expect("restart");
    assert_eq!(restarted.world, "acme");
    assert_eq!(
        restart_events.len(),
        1,
        "restart streams its boot events before the summary"
    );
    assert_eq!(
        *control.last_cwd.lock().expect("last cwd"),
        Some(cwd.clone())
    );

    // `restart` never boots a world it does not hold: the refusal crosses the wire typed.
    let refusal = client
        .restart(
            &WorldSelector {
                cwd: cwd.clone(),
                world: Some("acme:gone".to_owned()),
            },
            &mut |_| {},
        )
        .expect_err("restart of a world this daemon never booted must fail closed");
    match refusal {
        EdgeError::Control(error) => {
            assert_eq!(error.kind, DaemonErrorKind::NotRunning);
            assert!(
                error.message.contains("boot it with `slipfork up`"),
                "the refusal must name `slipfork up`, got: {}",
                error.message
            );
        }
        other => panic!("expected a typed control refusal, got {other:?}"),
    }

    assert!(
        client
            .down(&WorldSelector {
                cwd: cwd.clone(),
                world: Some("acme".to_owned()),
            })
            .expect("down")
    );
    assert_eq!(
        *control.last_cwd.lock().expect("last cwd"),
        Some(cwd.clone()),
        "the typed cwd must cross the wire, never resolved by the daemon"
    );

    let fork = client
        .fork(
            &ForkRequest {
                cwd: cwd.clone(),
                fork: "feat".to_owned(),
                base: None,
                dump: DumpRequest::Default,
                resources: ResourceRequest::Default,
                host_caches: false,
            },
            &mut |_| {},
        )
        .expect("fork");
    assert_eq!(fork.world, "acme");
    assert!(
        client
            .drop_fork(&DropForkRequest {
                cwd: cwd.clone(),
                fork: "feat".to_owned(),
            })
            .expect("drop_fork")
    );
    assert_eq!(client.list(None).expect("list").len(), 1);
    let prune = client.prune(true).expect("prune");
    assert!(prune.dry_run);
    assert_eq!(prune.worlds, vec!["acme:orphan".to_owned()]);

    let shared = client
        .share(&WorldSelector {
            cwd: cwd.clone(),
            world: None,
        })
        .expect("share");
    assert_eq!(shared.public_url(), "https://acme-abc.weaved.example");
    assert_eq!(shared.lease_id, "lease-1");
    let origin = client
        .unshare(&WorldSelector {
            cwd: cwd.clone(),
            world: None,
        })
        .expect("unshare");
    assert_eq!(origin.url(), "http://acme.localhost");
    let again = client
        .unshare(&WorldSelector {
            cwd: cwd.clone(),
            world: None,
        })
        .expect("unshare is idempotent");
    assert_eq!(again.url(), "http://acme.localhost");

    client
        .secret_set(Path::new("/repo"), "SECRET_KEY", "value", false)
        .expect("secret_set");
    assert_eq!(
        client.secret_list(Path::new("/repo")).expect("secret_list"),
        vec!["SECRET_KEY".to_owned()]
    );
    assert!(
        client
            .secret_unset(Path::new("/repo"), "SECRET_KEY")
            .expect("secret_unset")
    );

    let mut chunks = Vec::new();
    client
        .logs(
            &LogsRequest {
                cwd: cwd.clone(),
                world: None,
                target: None,
                follow: false,
                stderr: false,
                lines: None,
            },
            &mut |chunk| chunks.push(chunk),
        )
        .expect("logs");
    assert!(
        chunks.len() > 1,
        "a 20 KiB chunk must be split under the 64 KiB frame cap"
    );
    assert_eq!(
        chunks.iter().map(|chunk| chunk.bytes.len()).sum::<usize>(),
        20 * 1024
    );

    let target = client
        .exec_target(&ExecTargetRequest {
            cwd: cwd.clone(),
            world: None,
            service: None,
            argv: vec!["psql".to_owned()],
        })
        .expect("exec_target");
    assert_eq!(target.target_argv, vec!["psql".to_owned()]);
    assert_eq!(target.entry.program, PathBuf::from("nsenter"));
    assert!(
        !format!("{:?}", target.env).contains("must-not-leak"),
        "ExecEnv Debug must redact secrets"
    );

    assert_eq!(
        client
            .hook_run(
                &HookRunRequest {
                    cwd: cwd.clone(),
                    world: None,
                    hook: "api:pre-deploy".to_owned(),
                },
                &mut |_| {},
            )
            .expect("hook_run"),
        0
    );
    assert_eq!(
        client
            .auth_login(Some("wss://ospp.example/_ospp/v1"))
            .expect("auth_login")
            .key_id,
        "p256:00"
    );

    // The 64 KiB frame cap is enforced on requests: an oversized frame is refused, never truncated.
    let mut raw = UnixStream::connect(&socket).expect("connect raw");
    raw.set_read_timeout(Some(Duration::from_secs(20)))
        .expect("bound reads");
    raw.write_all(&vec![b'a'; slipfork_edge::daemon::MAX_CONTROL_FRAME + 16])
        .expect("write an oversized frame");
    raw.write_all(b"\n").expect("terminate the oversized frame");
    let mut refusal = String::new();
    raw.read_to_string(&mut refusal).expect("read the refusal");
    assert!(
        refusal.contains("\"kind\":\"frame_too_large\""),
        "an oversized frame must be refused with a typed error, got:\n{refusal}"
    );
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn world_next_commands_follow_the_one_state_table() {
    let verbs = |state| -> Vec<String> { slipfork_edge::world_next_commands(state) };
    assert_eq!(
        verbs(WorldState::Running),
        vec![
            "slipfork open".to_owned(),
            "slipfork db".to_owned(),
            "slipfork logs".to_owned(),
            "slipfork restart".to_owned(),
            "slipfork down".to_owned(),
        ]
    );
    assert_eq!(
        verbs(WorldState::Failed),
        vec![
            "slipfork logs".to_owned(),
            "slipfork restart".to_owned(),
            "slipfork down".to_owned(),
        ]
    );
    assert_eq!(
        verbs(WorldState::Stopped),
        vec![
            "slipfork up".to_owned(),
            "slipfork logs".to_owned(),
            "slipfork down".to_owned(),
        ]
    );
}

#[test]
fn world_up_summary_carries_the_documented_fields_and_joins_restarted_ids() {
    let summary = WorldUpSummary {
        world: "acme".to_owned(),
        state: WorldState::Running,
        changed: true,
        restarted: vec!["pg_databases.main".to_owned(), "api".to_owned()],
        worktree: PathBuf::from("/repo"),
        origin: test_origin("acme", 80),
        lineage: empty_lineage(),
        services: vec![
            service_row(
                "acme",
                "pg_databases.main",
                ServiceKind::BackingService,
                ServiceState::Running,
            ),
            service_row("acme", "api", ServiceKind::Component, ServiceState::Running),
        ],
        resources: Vec::new(),
        next: slipfork_edge::world_next_commands(WorldState::Running),
    };
    let json = serde_json::to_value(&summary).expect("serialize the up summary");
    let mut keys: Vec<&str> = json
        .as_object()
        .expect("the summary is one object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "changed",
            "lineage",
            "next",
            "origin",
            "resources",
            "restarted",
            "services",
            "state",
            "worktree",
            "world",
        ],
        "the up/restart/fork document carries exactly its documented keys"
    );
    assert_eq!(json["state"], serde_json::json!("running"));
    assert_eq!(json["changed"], serde_json::json!(true));
    assert!(
        json.get("origin").is_some(),
        "the world's origin is present"
    );
    assert_eq!(
        serde_json::from_value::<WorldUpSummary>(json).expect("the summary round-trips"),
        summary
    );
    for id in &summary.restarted {
        assert!(
            summary.services.iter().any(|row| row.info.id == *id),
            "restarted ids must join services[].id: {id}"
        );
    }
    let mut unique = summary.restarted.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), summary.restarted.len(), "ids are unique");
}

#[test]
fn daemon_control_forward_lifetime_closes_host_listener() {
    let state = cached_test_dir("control-forward");
    let registry = ready_registry(&state);
    let echo = spawn_tcp_echo();
    let control = Arc::new(TestControl::new(registry, echo));
    let _socket = start_control_server(&state, Arc::clone(&control));
    let client = DaemonControlClient::bind(&state);

    let mut published: Option<ForwardSummary> = None;
    let mut bridged = None;
    client
        .forward(
            &ForwardRequest {
                cwd: PathBuf::from("/tmp/opencode/project"),
                world: None,
                targets: vec!["pg_databases.main".to_owned()],
            },
            &mut |summary| {
                let target = summary.targets[0].clone();
                assert_eq!(target.id, "pg_databases.main");
                assert!(
                    target.host_addr.starts_with("127.0.0.1:"),
                    "the daemon binds host 127.0.0.1:0 in-process, got {}",
                    target.host_addr
                );
                assert_eq!(target.uri, format!("tcp://{}", target.host_addr));
                let mut bridge =
                    TcpStream::connect(&target.host_addr).expect("connect the forward bridge");
                bridge
                    .set_read_timeout(Some(Duration::from_secs(20)))
                    .expect("bound reads");
                bridge.write_all(b"ping").expect("write through the bridge");
                let mut echoed = [0_u8; 4];
                bridge
                    .read_exact(&mut echoed)
                    .expect("read the echoed bytes");
                assert_eq!(&echoed, b"ping", "forward must be a raw bidirectional pipe");
                bridged = Some(echoed);
                published = Some(summary);
            },
        )
        .expect("forward");
    assert_eq!(bridged, Some(*b"ping"));
    let summary = published.expect("the summary was published before blocking");
    assert_eq!(summary.world, "acme");

    // Returning from the callback drops the control connection: the daemon-owned listener closes.
    let addr: SocketAddr = summary.targets[0]
        .host_addr
        .parse()
        .expect("socket address");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if TcpStream::connect(addr).is_err() {
            let _ = fs::remove_dir_all(&state);
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("the forward listener at {addr} stayed open after the control connection closed");
}

#[test]
fn daemon_control_preserves_typed_errors() {
    let state = cached_test_dir("control-errors");
    let engine = Arc::new(StubWorldEngine::new());
    // No forward listener: `share` fails closed and the typed kind crosses the socket.
    let registry = stub_registry(Arc::clone(&engine));
    attach_pointing_at(
        &registry,
        &engine,
        &state,
        &acme(),
        &WorldTarget::Canonical,
        Vec::new(),
        spawn_backend(BackendMode::Head).port,
    );
    let control = Arc::new(TestControl::new(registry, spawn_tcp_echo()));
    start_control_server(&state, control);
    let client = DaemonControlClient::bind(&state);
    let cwd = PathBuf::from("/tmp/opencode/project");

    let err = client
        .share(&WorldSelector {
            cwd: cwd.clone(),
            world: None,
        })
        .expect_err("sharing without a forward listener fails closed");
    match err {
        EdgeError::Control(error) => {
            assert_eq!(error.kind, DaemonErrorKind::NoForwardListener);
            assert!(
                error.message.contains("forward listener"),
                "the diagnostic must be actionable, got '{}'",
                error.message
            );
        }
        other => panic!("expected a typed remote error, got {other:?}"),
    }
    let origin = client
        .unshare(&WorldSelector { cwd, world: None })
        .expect("unshare stays an idempotent success");
    assert_eq!(origin.url(), "http://acme.localhost");
    let _ = fs::remove_dir_all(&state);
}

// ---------------------------------------------------------------------------------------------
// slipforkd.socket / slipforkd.service non-template units (M2.10, ADR-0008 §5)
// ---------------------------------------------------------------------------------------------

/// Fixed test account for the golden pair: any real account name renders identically, and this one
/// can never be a systemd `%`-template.
const GOLDEN_USER: &str = "slipforktest";

fn golden_unit_spec() -> UnitSpec {
    UnitSpec {
        user: GOLDEN_USER.to_owned(),
        listen_addrs: vec![
            SocketAddr::from((Ipv4Addr::LOCALHOST, 80)),
            SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 80)),
        ],
        forward_port: 7080,
        executable: PathBuf::from("/usr/local/bin/slipforkd"),
        state_dir: PathBuf::from("/home/slipforktest/.local/share/slipfork"),
        runtime_dir: PathBuf::from("/run/user/1000/slipfork"),
    }
}

#[test]
fn unit_golden_renders_byte_exactly() {
    let spec = golden_unit_spec();
    spec.validate()
        .expect("the shipped production pair (127.0.0.1:80 + [::1]:80 + forward 7080) validates");
    let socket = render_socket_unit(&spec).expect("render slipforkd.socket");
    let service = render_service_unit(&spec).expect("render slipforkd.service");

    assert_eq!(
        socket,
        include_str!("golden/slipforkd.socket"),
        "slipforkd.socket must render byte-exactly"
    );
    assert_eq!(
        service,
        include_str!("golden/slipforkd.service"),
        "slipforkd.service must render byte-exactly"
    );

    // Exactly one service type, and it is the one the daemon actually implements.
    assert_eq!(
        service.matches("Type=").count(),
        1,
        "a unit must declare exactly one Type=:\n{service}"
    );
    assert!(service.contains("\nType=simple\n"), "got:\n{service}");

    // Explicit account, never a template specifier.
    assert!(
        service.contains(&format!("\nUser={GOLDEN_USER}\n")),
        "the service must name the resolved account:\n{service}"
    );
    assert!(
        !service.contains('%') && !socket.contains('%'),
        "non-template units must never contain a systemd %-specifier"
    );

    // Non-template hardening contract (ADR-0008 §5). The emitted `RestrictAddressFamilies=` line is
    // pinned by the golden fixture compared byte-exactly above, and its `AF_NETLINK` requirement is
    // pinned by name in `unit_service_sandbox_allows_the_netlink_family_world_init_needs`.
    for directive in [
        "\nCapabilityBoundingSet=\n",
        "\nNoNewPrivileges=yes\n",
        "\nProtectSystem=strict\n",
        "\nPrivateTmp=yes\n",
        "\nLimitNOFILE=65536\n",
    ] {
        assert!(
            service.contains(directive),
            "missing {directive:?}:\n{service}"
        );
    }
    assert!(
        service.contains(&format!(
            "\nExecStart={} --state-dir {} --runtime-dir {}\n",
            spec.executable.display(),
            spec.state_dir.display(),
            spec.runtime_dir.display()
        )),
        "ExecStart must be the executable plus both explicit directories:\n{service}"
    );
    assert!(
        service.contains(&format!(
            "\nReadWritePaths={} {}\n",
            spec.state_dir.display(),
            spec.runtime_dir.display()
        )),
        "ReadWritePaths must cover exactly the state and runtime directories:\n{service}"
    );

    // Activation fd order is the M2.8 contract: ingress listeners first, forward listener last.
    assert_eq!(
        socket
            .lines()
            .filter(|line| line.starts_with("ListenStream="))
            .collect::<Vec<_>>(),
        vec![
            "ListenStream=127.0.0.1:80",
            "ListenStream=[::1]:80",
            "ListenStream=127.0.0.1:7080",
        ],
        "ListenStream= order must match the activation fd order:\n{socket}"
    );
    assert!(socket.contains("\nBacklog=1024\n"), "got:\n{socket}");
    assert!(socket.contains("\nNoDelay=true\n"), "got:\n{socket}");
    assert!(
        socket.contains("\nService=slipforkd.service\n"),
        "the socket must name the service it activates:\n{socket}"
    );
}

/// The service sandbox must keep `AF_NETLINK`, or no world can boot through the installed unit.
///
/// `slipfork-world-init` brings loopback up with `ip link set lo up`, which opens an
/// `AF_NETLINK`/`NETLINK_ROUTE` socket. `M3.28.12` proved the failure live on the DO VM: with
/// `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX` the installed `slipforkd.service` reported
/// "slipfork-world-init: `ip link set lo up` failed with exit status: 1: Cannot open netlink
/// socket: Address family not supported by protocol" and every boot died, while the same engine
/// outside the unit (and with `AF_NETLINK` added via a `systemd-run` scope) returned `rc 0`.
/// This assertion names the family, so trimming it back fails here and not only on a VM; the exact
/// list and its order stay pinned by the golden fixture through `unit_golden_renders_byte_exactly`.
#[test]
fn unit_service_sandbox_allows_the_netlink_family_world_init_needs() {
    let service = render_service_unit(&golden_unit_spec()).expect("render slipforkd.service");
    let families = service
        .lines()
        .find_map(|line| line.strip_prefix("RestrictAddressFamilies="))
        .expect("the service must restrict address families");
    assert!(
        families
            .split_whitespace()
            .any(|family| family == "AF_NETLINK"),
        "the service sandbox must allow AF_NETLINK for `ip link set lo up`:\n{service}"
    );
}

#[test]
fn unit_validate_rejects_invalid_specs() {
    let base = golden_unit_spec();

    for user in ["", "   ", "%i", "%u", "dev user", "dev\tuser"] {
        let mut spec = base.clone();
        spec.user = user.to_owned();
        match spec.validate() {
            Err(EdgeError::UnitInvalidUser { user: refused }) => assert_eq!(refused, user),
            other => panic!("user {user:?} must be refused with UnitInvalidUser, got {other:?}"),
        }
        assert!(
            matches!(
                render_socket_unit(&spec),
                Err(EdgeError::UnitInvalidUser { .. })
            ),
            "an invalid user must never render a socket unit"
        );
        assert!(
            matches!(
                render_service_unit(&spec),
                Err(EdgeError::UnitInvalidUser { .. })
            ),
            "an invalid user must never render a service unit"
        );
    }

    let mut no_listener = base.clone();
    no_listener.listen_addrs.clear();
    assert!(matches!(
        no_listener.validate(),
        Err(EdgeError::UnitNoListener)
    ));
    assert!(matches!(
        render_socket_unit(&no_listener),
        Err(EdgeError::UnitNoListener)
    ));

    let mut colliding_v4 = base.clone();
    colliding_v4.forward_port = 80;
    match colliding_v4.validate() {
        Err(EdgeError::UnitPortCollision {
            forward_port,
            listener,
        }) => {
            assert_eq!(forward_port, 80);
            assert_eq!(listener, SocketAddr::from((Ipv4Addr::LOCALHOST, 80)));
        }
        other => panic!("a v4 forward/ingress collision must fail closed, got {other:?}"),
    }
    assert!(matches!(
        render_socket_unit(&colliding_v4),
        Err(EdgeError::UnitPortCollision { .. })
    ));

    let mut colliding_v6 = golden_unit_spec();
    colliding_v6.listen_addrs = vec![SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 80))];
    colliding_v6.forward_port = 80;
    match colliding_v6.validate() {
        Err(EdgeError::UnitPortCollision { listener, .. }) => {
            assert_eq!(
                listener,
                SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 80))
            );
        }
        other => panic!("a v6 forward/ingress collision must fail closed, got {other:?}"),
    }

    // A non-colliding forward port on an otherwise valid spec passes.
    let mut fine = base.clone();
    fine.forward_port = 7080;
    fine.validate()
        .expect("7080 does not collide with the :80 ingress");
}

#[test]
fn unit_privileged_80_is_allowed_and_renders() {
    // `systemd` (root, in the `.socket` unit) binds :80 and hands fd 3.. to the unprivileged
    // service, so a privileged loopback listener is a legitimate spec, never a validation error.
    let spec = golden_unit_spec();
    spec.validate()
        .expect(":80 on loopback must validate: systemd binds it, not slipforkd");
    let socket = render_socket_unit(&spec).expect("render the :80 socket unit");
    assert!(socket.contains("\nListenStream=127.0.0.1:80\n"), "{socket}");
    assert!(socket.contains("\nListenStream=[::1]:80\n"), "{socket}");

    // The "cannot bind unprivileged" hazard is a runtime diagnostic, never a spec rejection.
    match resolve_listener_set(Some(&ListenConfig {
        addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 80)),
        forward_port: None,
    })) {
        Ok(listeners) => {
            // A privileged runner (root in CI) may bind :80; that is the successful path.
            assert_eq!(listeners.http_addrs().expect("bound :80")[0].port(), 80);
        }
        Err(EdgeError::BindListener { addr, message }) => {
            assert_eq!(addr, "127.0.0.1:80");
            assert!(
                !message.is_empty(),
                "the diagnostic must carry the OS error, got '{message}'"
            );
        }
        Err(other) => panic!(
            "an unprivileged bind of :80 must surface BindListener, not an unrelated error: {other:?}"
        ),
    }
}

/// Reserve one loopback port by binding `127.0.0.1:0` and releasing it for the real tool.
///
/// `systemd-socket-activate` 257 rejects `--listen=127.0.0.1:0`
/// (`Failed to parse socket address "127.0.0.1:0": Invalid argument`), so the kernel picks the
/// ephemeral port here and the tool binds that exact port a moment later.
fn free_loopback_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("bind an ephemeral loopback port")
        .local_addr()
        .expect("ephemeral loopback address")
        .port()
}

/// Bounded attempts for the real handover, each on a freshly reserved loopback port.
///
/// The reservation and the tool's own `bind` are two separate syscalls, so any concurrent binder on
/// the host (another case of this binary's parallel phase, another test binary, or an unrelated
/// process) can take the port in between. Losing that race is a harness artefact, never a product
/// defect, so the whole handover is re-run on a fresh port a bounded number of times instead of
/// weakening any assertion or padding a wait.
const HANDOVER_ATTEMPTS: u32 = 5;

/// Bound on `systemd-socket-activate` reporting its listening socket.
const HANDOVER_READY_DEADLINE: Duration = Duration::from_secs(30);

/// Poll interval for the tool's `Listening on …` readiness line.
const HANDOVER_READY_POLL: Duration = Duration::from_millis(100);

/// One attempt of the real fd-3 handover.
enum Handover {
    /// The tool listened on the reserved port and the probe child served the real dispatch.
    Served,
    /// The tool could not bind the reserved port, carrying its captured stderr. Only
    /// `Address already in use` is retryable; every other diagnostic stays fatal.
    PortTaken(String),
}

/// Whether captured tool stderr reports the exact lost race: the reserved port was taken.
///
/// This English diagnostic is the **only** classification input: a different wording makes the
/// helper return `false`, so the handover fails loudly with the tool's captured stderr instead of
/// silently retrying something it did not understand.
///
/// `systemd-socket-activate` 257 prints
/// `Failed to listen on 127.0.0.1:<port>: Address already in use` before exiting without ever
/// printing `Listening on …`. Only that bind failure is retried, so a missing binary, an invalid
/// address, or a permission error still fails closed with the real output quoted.
fn lost_reserved_port(stderr: &str) -> bool {
    stderr.lines().any(|line| {
        line.starts_with("Failed to listen on ") && line.ends_with(": Address already in use")
    })
}

/// Run the real `systemd-socket-activate` handover once on a freshly reserved loopback port.
fn socket_activate_handover_once(state: &Path) -> Handover {
    let executable = std::env::current_exe().expect("current test executable");
    let port = free_loopback_port();

    let mut command = Command::new("systemd-socket-activate");
    command
        .arg(format!("--listen=127.0.0.1:{port}"))
        // The tool starts children from a minimal environment, so the probe selection crosses the
        // handover explicitly (the same mechanism systemd uses for `Environment=`).
        .arg(format!("--setenv={PROBE_MODE_ENV}=systemd_socket_activate"))
        .arg(format!("--setenv={PROBE_STATE_ENV}={}", state.display()))
        .arg(executable)
        .args([
            "--exact",
            "activation_probe_child",
            "--test-threads=1",
            "--nocapture",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap_or_else(|err| {
        panic!(
            "systemd-socket-activate is required for the live activation conformance test but could not be spawned: {err}; install systemd (this test fails closed instead of skipping)"
        )
    });

    // The tool prints the listening socket on stderr and only execs the child on the first
    // connection, exactly like a user manager (`facts.md` §5.2).
    let stderr = child.stderr.take().expect("piped stderr");
    let stderr_log = Arc::new(Mutex::new(String::new()));
    let (ready_tx, ready_rx) = mpsc::channel();
    let stderr_reader = {
        let stderr_log = Arc::clone(&stderr_log);
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if line.starts_with("Listening on ") {
                    let _ = ready_tx.send(());
                }
                let mut log = stderr_log.lock().expect("tool stderr log");
                log.push_str(&line);
                log.push('\n');
            }
        })
    };
    let stdout = child.stdout.take().expect("piped stdout");
    let stdout_log = Arc::new(Mutex::new(String::new()));
    let stdout_reader = {
        let stdout_log = Arc::clone(&stdout_log);
        thread::spawn(move || {
            let mut text = String::new();
            let _ = BufReader::new(stdout).read_to_string(&mut text);
            stdout_log.lock().expect("tool stdout log").push_str(&text);
        })
    };

    let ready_deadline = Instant::now() + HANDOVER_READY_DEADLINE;
    loop {
        match ready_rx.recv_timeout(HANDOVER_READY_POLL) {
            Ok(()) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(status) = child.try_wait().expect("poll systemd-socket-activate") {
                    let _ = stderr_reader.join();
                    let _ = stdout_reader.join();
                    let stderr_text = stderr_log.lock().expect("tool stderr log").clone();
                    if lost_reserved_port(&stderr_text) {
                        return Handover::PortTaken(stderr_text);
                    }
                    panic!(
                        "systemd-socket-activate exited before listening ({status})\nstderr:\n{stderr_text}"
                    );
                }
                if Instant::now() >= ready_deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "systemd-socket-activate never reported a listening socket within {}s\nstderr:\n{}",
                        HANDOVER_READY_DEADLINE.as_secs(),
                        stderr_log.lock().expect("tool stderr log").as_str()
                    );
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = stderr_reader.join();
                let stderr_text = stderr_log.lock().expect("tool stderr log").clone();
                // Reap the tool before joining the stdout reader: a descendant that still holds
                // stdout would otherwise keep that join blocked past every deadline.
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                if lost_reserved_port(&stderr_text) {
                    return Handover::PortTaken(stderr_text);
                }
                panic!(
                    "systemd-socket-activate closed its stderr before listening\nstderr:\n{stderr_text}"
                );
            }
        }
    }

    let response = tcp_request(port, "nope.localhost", "/");
    assert!(
        response.starts_with("HTTP/1.1 404 Not Found"),
        "the socket-activated daemon must serve the real dispatch, got:\n{response}"
    );
    assert!(
        response.contains("no world is attached for host 'nope.localhost'"),
        "the 404 must name the Host, got:\n{response}"
    );

    fs::write(state.join("probe.done"), b"done\n").expect("signal probe completion");
    let status = wait_for_exit(&mut child);
    let _ = stderr_reader.join();
    let _ = stdout_reader.join();
    let stderr_text = stderr_log.lock().expect("tool stderr log").clone();
    let stdout_text = stdout_log.lock().expect("tool stdout log").clone();
    assert!(
        status.success(),
        "the socket-activated probe failed with {status}\nstdout:\n{stdout_text}\nstderr:\n{stderr_text}"
    );
    assert!(
        stderr_text.contains("Communication attempt on fd 3."),
        "the handover must come from the real tool, got:\n{stderr_text}"
    );
    assert!(
        stdout_text.contains("activation_probe_child"),
        "the child half of the probe must have run, got:\n{stdout_text}"
    );
    Handover::Served
}

#[test]
fn socket_activate_systemd_handover() {
    let state = cached_test_dir("socket-activate");
    let mut attempts = 0;
    loop {
        attempts += 1;
        let attempt_state = state.join(format!("attempt-{attempts}"));
        fs::create_dir_all(&attempt_state).expect("create the handover attempt state directory");
        match socket_activate_handover_once(&attempt_state) {
            Handover::Served => break,
            Handover::PortTaken(stderr) => {
                assert!(
                    attempts < HANDOVER_ATTEMPTS,
                    "the real socket handover lost its freshly reserved loopback port on all \
                     {HANDOVER_ATTEMPTS} attempts: a concurrent binder kept taking the port between \
                     the reservation and the tool's bind, so no live handover could be observed; \
                     last stderr:\n{stderr}"
                );
            }
        }
    }
    let _ = fs::remove_dir_all(&state);
}

// ---------------------------------------------------------------------------------------------
// M3.28.3 daemon truth: bound ingress, world parent, per-world lineage & process rows
// ---------------------------------------------------------------------------------------------

/// One `*.localhost` origin for the wire-shape tests (`:80` renders port-less).
fn test_origin(project: &str, port: u16) -> WorldOrigin {
    let project = ProjectSlug::parse(project).expect("valid project slug");
    let route = LocalHostRoute::new(project, WorldTarget::Canonical, None, &[])
        .expect("default-ingress route");
    WorldOrigin::local(&route, port).expect("local origin")
}

/// The one ingress fixture: the `:80` model bind `registry_over` builds.
fn ingress_info(port: u16) -> IngressInfo {
    IngressInfo::new(vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))]).expect("valid ingress")
}

/// The one service-row fixture: a wire row with no engine-measured facts yet.
fn service_row(
    world: &str,
    id: &str,
    kind: ServiceKind,
    state: ServiceState,
) -> WorldServiceSummary {
    WorldServiceSummary {
        world: world.to_owned(),
        info: WorldServiceInfo {
            id: id.to_owned(),
            kind,
            state,
            pid: None,
            port: None,
            uptime_secs: None,
        },
    }
}

const FULL_DIGEST: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

#[test]
fn ingress_info_round_trips_port_80_and_ephemeral_binds() {
    // Socket activation: `127.0.0.1:80` and `[::1]:80`; the order of `addrs` is non-semantic.
    let eighty: Vec<SocketAddr> = vec![
        "127.0.0.1:80".parse().expect("v4 port 80"),
        "[::1]:80".parse().expect("v6 port 80"),
    ];
    let info = IngressInfo::new(eighty.clone()).expect("a real :80 listener set");
    assert_eq!(info.port(), 80);
    assert_eq!(info.addrs(), eighty.as_slice());
    let mut reversed = eighty.clone();
    reversed.reverse();
    assert_eq!(
        IngressInfo::new(reversed)
            .expect("order is non-semantic")
            .port(),
        info.port()
    );
    let json = serde_json::to_value(&info).expect("serialize ingress info");
    assert_eq!(
        json,
        serde_json::json!({ "addrs": ["127.0.0.1:80", "[::1]:80"], "port": 80 })
    );
    assert_eq!(
        serde_json::from_value::<IngressInfo>(json).expect("round-trip ingress info"),
        info
    );

    // Ephemeral `--listen 127.0.0.1:0`: the bound port (never `0`) is the origin port.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind an ephemeral listener");
    let bound = listener.local_addr().expect("bound address");
    assert_ne!(bound.port(), 0, "a bound listener never reports :0");
    let ephemeral = IngressInfo::new(vec![bound]).expect("a real ephemeral bind");
    assert_eq!(ephemeral.port(), bound.port());
    assert!(
        ephemeral
            .addrs()
            .iter()
            .any(|addr| addr.port() == ephemeral.port())
    );
    let ephemeral_json = serde_json::to_value(&ephemeral).expect("serialize ephemeral ingress");
    assert_eq!(
        serde_json::from_value::<IngressInfo>(ephemeral_json).expect("ephemeral round-trip"),
        ephemeral
    );

    // Fail closed: no listener, an unbound `:0`, or listeners that disagree on the port.
    assert!(matches!(
        IngressInfo::new(Vec::new()),
        Err(EdgeError::IngressMissing)
    ));
    assert!(matches!(
        IngressInfo::new(vec!["127.0.0.1:0".parse().expect("port 0")]),
        Err(EdgeError::IngressPortInvalid { .. })
    ));
    assert!(matches!(
        IngressInfo::new(vec![
            "127.0.0.1:80".parse().expect("port 80"),
            "[::1]:8080".parse().expect("port 8080"),
        ]),
        Err(EdgeError::IngressPortInvalid { .. })
    ));
    // A forged frame whose `port` is not carried by `addrs` is a typed error, never a silent pass.
    let forged = serde_json::json!({ "addrs": ["127.0.0.1:80"], "port": 81 });
    let err = serde_json::from_value::<IngressInfo>(forged)
        .expect_err("an origin port absent from addrs must fail closed");
    assert!(
        err.to_string()
            .contains("not carried by any bound listener"),
        "the refusal must name the unbound port, got: {err}"
    );
}

#[test]
fn world_summary_parent_and_lineage_survive_serde() {
    let canonical = WorldSummary {
        world: "acme".to_owned(),
        parent: None,
        state: WorldState::Running,
        origin: test_origin("acme", 80),
        worktree: PathBuf::from("/repo"),
        lineage: empty_lineage(),
    };
    let fork = WorldSummary {
        world: "acme:feat-ux".to_owned(),
        parent: Some("acme".to_owned()),
        state: WorldState::Stopped,
        origin: test_origin("acme", 80),
        worktree: PathBuf::from("/repo/.worktrees/feat-ux"),
        lineage: empty_lineage(),
    };

    let canonical_json = serde_json::to_value(&canonical).expect("serialize canonical summary");
    assert_eq!(
        canonical_json.get("parent"),
        Some(&serde_json::Value::Null),
        "the canonical world's parent must be present and null, never omitted"
    );
    assert_eq!(
        canonical_json.get("lineage"),
        Some(&serde_json::json!({
            "rootfs": null,
            "services": [],
            "seed": null,
            "base": null,
        })),
        "an unknown lineage must carry every key with null/[] and no zeroed fact"
    );
    let fork_json = serde_json::to_value(&fork).expect("serialize fork summary");
    assert_eq!(
        fork_json.get("parent"),
        Some(&serde_json::Value::String("acme".to_owned()))
    );
    assert_eq!(
        serde_json::from_value::<WorldSummary>(fork_json).expect("fork summary round-trip"),
        fork
    );

    // Every `Option` fact present: nulls stay null, a measured 0 stays 0, enums keep their spelling.
    let measured = WorldLineage {
        rootfs: Some(RootfsLineage {
            digest: FULL_DIGEST.to_owned(),
            cache: CacheOutcome::Hit,
            millis: None,
        }),
        services: vec![ServiceImageLineage {
            id: "postgres:18".to_owned(),
            cache: CacheOutcome::Miss,
            millis: Some(12),
        }],
        seed: Some(SeedLineage {
            digest: None,
            rebuilt: Some(false),
            millis: None,
        }),
        base: Some(BaseLineage {
            key: FULL_DIGEST.to_owned(),
            millis: Some(0),
        }),
    };
    let measured_json = serde_json::to_value(&measured).expect("serialize measured lineage");
    assert_eq!(measured_json["rootfs"]["cache"], serde_json::json!("hit"));
    assert_eq!(measured_json["rootfs"]["millis"], serde_json::Value::Null);
    assert_eq!(
        measured_json["services"][0]["cache"],
        serde_json::json!("miss")
    );
    assert_eq!(
        measured_json["services"][0]["millis"],
        serde_json::json!(12)
    );
    assert_eq!(measured_json["seed"]["digest"], serde_json::Value::Null);
    assert_eq!(measured_json["seed"]["rebuilt"], serde_json::json!(false));
    assert_eq!(measured_json["base"]["millis"], serde_json::json!(0));
    assert_eq!(
        serde_json::from_value::<WorldLineage>(measured_json).expect("lineage round-trip"),
        measured
    );

    // `DaemonStatus` carries `ingress` and never a daemon-wide lineage or the retired port field.
    let status = DaemonStatus {
        pid: 1420,
        ingress: ingress_info(80),
        forward_port: Some(7080),
        worlds: vec![canonical, fork],
    };
    let status_json = serde_json::to_value(&status).expect("serialize daemon status");
    assert_eq!(status_json["ingress"]["port"], serde_json::json!(80));
    assert_eq!(
        status_json["ingress"]["addrs"],
        serde_json::json!(["127.0.0.1:80"])
    );
    assert!(
        status_json.get("lineage").is_none(),
        "there is no daemon-wide lineage: a world's facts live on its own summary"
    );
    assert!(
        status_json.get("http_ingress_port").is_none(),
        "the retired scalar port must not reappear beside the typed ingress"
    );
    assert_eq!(
        serde_json::from_value::<DaemonStatus>(status_json).expect("status round-trip"),
        status
    );
}

#[test]
fn world_process_reports_are_world_scoped_and_keep_stopped_rows() {
    let canonical = WorldProcessReport::new(
        WorldState::Running,
        empty_lineage(),
        vec![
            service_row("acme", "api", ServiceKind::Component, ServiceState::Running),
            service_row(
                "acme",
                "pg_databases.main",
                ServiceKind::BackingService,
                ServiceState::Stopped,
            ),
        ],
    )
    .expect("unique ids");
    let fork = WorldProcessReport::new(
        WorldState::Stopped,
        empty_lineage(),
        vec![service_row(
            "acme:feat",
            "api",
            ServiceKind::Component,
            ServiceState::Exited,
        )],
    )
    .expect("unique ids");

    // Two worlds never share a row: every row carries its own world id.
    assert!(
        canonical
            .services
            .iter()
            .all(|service| service.world == "acme")
    );
    assert!(
        fork.services
            .iter()
            .all(|service| service.world == "acme:feat")
    );
    for left in &canonical.services {
        assert!(
            !fork
                .services
                .iter()
                .any(|right| right.world == left.world && right.info.id == left.info.id),
            "a row may never appear in two worlds"
        );
    }

    // A stopped service keeps its row; its unknown facts stay `None` (rendered `-`, never `0`).
    let stopped = canonical
        .services
        .iter()
        .find(|service| service.info.id == "pg_databases.main")
        .expect("the stopped backing service keeps its row");
    assert_eq!(stopped.info.state, ServiceState::Stopped);
    assert_eq!(stopped.info.kind, ServiceKind::BackingService);
    assert_eq!(stopped.info.pid, None);
    assert_eq!(stopped.info.port, None);
    assert_eq!(stopped.info.uptime_secs, None);

    // The engine's `ServiceState` crosses the wire as its snake_case spelling, fail-closed.
    let json = serde_json::to_value(&canonical).expect("serialize the process report");
    assert_eq!(json["state"], serde_json::json!("running"));
    assert_eq!(json["services"][1]["state"], serde_json::json!("stopped"));
    assert_eq!(
        json["services"][1]["kind"],
        serde_json::json!("backing_service")
    );
    assert_eq!(json["services"][1]["pid"], serde_json::Value::Null);
    assert_eq!(
        serde_json::from_value::<WorldProcessReport>(json).expect("process report round-trip"),
        canonical
    );
    let mut unknown_state = serde_json::to_value(&canonical).expect("serialize the process report");
    unknown_state["services"][0]["state"] = serde_json::json!("zombie");
    assert!(
        serde_json::from_value::<WorldProcessReport>(unknown_state).is_err(),
        "an unknown service state must fail closed, never default"
    );

    // Duplicate ids are refused: `services[].id` stays a 1:1 join key.
    let duplicate = WorldProcessReport::new(
        WorldState::Running,
        empty_lineage(),
        vec![
            service_row("acme", "api", ServiceKind::Component, ServiceState::Running),
            service_row(
                "acme",
                "api",
                ServiceKind::BackingService,
                ServiceState::Running,
            ),
        ],
    )
    .expect_err("two rows with one id must fail closed");
    assert!(
        duplicate.to_string().contains("unique ids"),
        "the refusal must name the id-uniqueness rule, got: {duplicate}"
    );
}

#[test]
fn daemon_control_reports_world_processes_over_the_socket() {
    let state = cached_test_dir("world-processes");
    let registry = ready_registry(&state);
    let control = Arc::new(TestControl::new(registry, spawn_tcp_echo()));
    start_control_server(&state, Arc::clone(&control));
    let client = DaemonControlClient::bind(&state);
    let cwd = PathBuf::from("/tmp/opencode/project");

    let report = client
        .world_processes(&WorldSelector {
            cwd: cwd.clone(),
            world: None,
        })
        .expect("world_processes");
    assert_eq!(report.state, WorldState::Running);
    // The interim lineage is the shape the CLI renders: every fact null, no service-image rows.
    assert_eq!(
        serde_json::to_value(&report.lineage).expect("serialize the interim lineage"),
        serde_json::json!({
            "rootfs": null,
            "services": [],
            "seed": null,
            "base": null,
        })
    );
    assert_eq!(report.services.len(), 2);
    assert!(
        report
            .services
            .iter()
            .all(|service| service.world == "acme")
    );
    assert!(
        report
            .services
            .iter()
            .any(|service| service.info.state == ServiceState::Stopped),
        "a stopped service keeps its row"
    );

    // An unknown world is a typed error across the wire, never an empty report.
    let unknown = client
        .world_processes(&WorldSelector {
            cwd: cwd.clone(),
            world: Some("acme:ghost".to_owned()),
        })
        .expect_err("an unknown world must fail closed");
    match unknown {
        EdgeError::Control(error) => {
            assert_eq!(error.kind, DaemonErrorKind::UnknownWorld);
            assert!(
                error.message.contains("acme:ghost"),
                "the refusal must name the world, got: {}",
                error.message
            );
        }
        other => panic!("expected a typed UnknownWorld error, got {other:?}"),
    }
}
