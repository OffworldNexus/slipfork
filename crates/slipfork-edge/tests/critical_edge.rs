//! Protected critical tests for `slipfork-edge` (`M2.8`, `ADR-0008` §5, `H-011` #4).
//!
//! Every activation assertion is driven through a **real fd-3 handover**: the parent binds a
//! listening socket, hands it to a child process on `fd 0`, and the child's `sh` wrapper
//! duplicates it to `fd 3` before `exec` and exports `LISTEN_PID=$$ LISTEN_FDS=<n>` — exactly the
//! descriptor layout `systemd` produces (`docs/research/tunnel-weaver-and-systemd-facts.md` §5.2).
//! The child then proves the adopted descriptor is non-blocking and serves a real HTTP exchange
//! through [`HttpIngress`]. No descriptor is mocked and no environment is faked in-process.
//!
//! The registry is exercised over the real `IncusEngine` adapter against an in-tree `/1.0`
//! Unix-socket harness (the same pattern `slipfork-engine`'s critical suite uses; live `incusd`
//! stays gated on `H-004`), so `WorldHandle`s are minted by a real engine implementation.

#![forbid(unsafe_code)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use slipfork_edge::daemon::{
    AuthSummary, BootEvent, DaemonControl, DaemonErrorKind, DaemonStatus, DropForkRequest,
    DumpRequest, ExecTargetRequest, ForkRequest, ForwardRequest, ForwardSummary, HookRunRequest,
    LogsRequest, PruneReport, ResourceRequest, ResourceSummary, UpRequest, WorldExecTarget,
    WorldSelector, WorldState, WorldSummary, WorldUpSummary,
};
use slipfork_edge::{
    DaemonControlClient, DaemonControlServer, EdgeError, ForwardTargetSpec, HttpIngress,
    IngressRoute, ListenConfig, RouteDecision, RoutedWorld, SharedWorldInfo,
    UnavailableTunnelRegistrar, UnitSpec, WorldRegistry, adopt_systemd_listener, bind_listen,
    render_service_unit, render_socket_unit, resolve_listener_set,
};
use slipfork_engine::{
    EngineError, ExecEnv, IncusEngine, SnapshotId, StreamChunk, StreamSource, WorldEngine,
    WorldEntryCommand, WorldHandle, WorldSpec, current_uid_gid,
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

fn empty_registry(state: &Path) -> Arc<WorldRegistry> {
    let (uid, gid) = current_uid_gid();
    let engine = IncusEngine::new(
        state.join("unused-incusd.sock"),
        "default",
        "base",
        uid,
        gid,
    );
    Arc::new(WorldRegistry::new(
        Arc::new(engine),
        Arc::new(UnavailableTunnelRegistrar::new()),
        80,
        None,
    ))
}

fn listen_local() -> ListenConfig {
    ListenConfig {
        addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        forward_port: None,
    }
}

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
        listeners.http_port().expect("bound port"),
        expected_port,
        "the adopted fd 3 must be the parent's listening socket"
    );
    assert_would_block(&listeners.http_listeners[0], "HTTP ingress listener");

    let registry = empty_registry(&state);
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
    assert_eq!(listeners.http_port().expect("bound port"), expected_port);
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
    assert_ne!(without_forward.http_port().expect("bound port"), 0);
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
        with_forward.http_port().expect("http port"),
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

    let registry = empty_registry(&state);
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
    let registry = empty_registry(&state);
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

/// One in-tree `/1.0` Incus protocol harness: enough for `clone_world` and `destroy_world`.
struct FakeIncus {
    socket: PathBuf,
    deletes: Arc<AtomicUsize>,
}

impl FakeIncus {
    fn start(state: &Path) -> Self {
        let socket = state.join("fake-incusd.sock");
        let listener = UnixListener::bind(&socket).expect("bind the fake incusd socket");
        let deletes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&deletes);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let counter = Arc::clone(&counter);
                thread::spawn(move || {
                    let mut head = Vec::new();
                    let mut chunk = [0_u8; 4096];
                    loop {
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(read) => {
                                head.extend_from_slice(&chunk[..read]);
                                if head.windows(4).any(|window| window == b"\r\n\r\n") {
                                    break;
                                }
                            }
                        }
                    }
                    if head.starts_with(b"DELETE ") {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                    let body =
                        r#"{"type":"sync","status":"Success","status_code":200,"metadata":{}}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.shutdown(Shutdown::Both);
                });
            }
        });
        Self { socket, deletes }
    }

    fn engine(&self) -> Arc<dyn WorldEngine> {
        let (uid, gid) = current_uid_gid();
        Arc::new(IncusEngine::new(&self.socket, "default", "base", uid, gid))
    }
}

fn clone_test_world(engine: &dyn WorldEngine, state: &Path, target: WorldTarget) -> WorldHandle {
    let spec = WorldSpec {
        target,
        snapshot: SnapshotId("base".to_owned()),
        mounts: Vec::new(),
        base_env: ExecEnv::world_scoped(state.join("home")),
        local_ingress: None,
    };
    engine.clone_world(&spec).expect("clone a test world")
}

#[test]
fn registry_attach_rejects_duplicates_and_resolves_prefixes() {
    let state = cached_test_dir("registry-attach");
    let harness = FakeIncus::start(&state);
    let engine = harness.engine();
    let registry = WorldRegistry::new(
        Arc::clone(&engine),
        Arc::new(UnavailableTunnelRegistrar::new()),
        80,
        None,
    );
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
        harness.deletes.load(Ordering::SeqCst),
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
    let harness = FakeIncus::start(&state);
    let engine = harness.engine();
    let registry = WorldRegistry::new(
        Arc::clone(&engine),
        Arc::new(UnavailableTunnelRegistrar::new()),
        80,
        None,
    );
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
    let harness = FakeIncus::start(&state);
    let engine = harness.engine();
    let registry = Arc::new(WorldRegistry::new(
        Arc::clone(&engine),
        Arc::new(UnavailableTunnelRegistrar::new()),
        80,
        None,
    ));
    let project = acme();
    let target = WorldTarget::Canonical;
    let handle = clone_test_world(engine.as_ref(), &state, target.clone());
    registry
        .attach_world(
            &project,
            &target,
            Vec::new(),
            handle,
            vec![IngressRoute {
                prefix: "/api".to_owned(),
                port: 3000,
            }],
        )
        .expect("attach the canonical world");

    let listeners = bind_listen(&listen_local()).expect("bind --listen 127.0.0.1:0");
    let port = listeners.http_port().expect("bound ingress port");
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

    let unroutable_backend = tcp_request(port, "acme.localhost", "/api/users");
    assert!(
        unroutable_backend.starts_with("HTTP/1.1 502 Bad Gateway"),
        "a resolved world whose backend cannot be dialed must be a 502, got:\n{unroutable_backend}"
    );
    assert!(
        unroutable_backend.contains("world acme cannot reach its backend on port 3000"),
        "the 502 must name the world id, host, and port, got:\n{unroutable_backend}"
    );
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn unix_ingress_sock_refuses_live_socket() {
    let state = cached_test_dir("unix-live");
    let live = UnixListener::bind(state.join("http-ingress.sock")).expect("bind a live socket");
    let listeners = bind_listen(&listen_local()).expect("bind --listen");
    let err = HttpIngress::bind(&state, listeners, empty_registry(&state))
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
    let ingress = HttpIngress::bind(&state, listeners, empty_registry(&state))
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
    let harness = FakeIncus::start(&state);
    let engine = dial_engine(&harness);
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = Arc::new(WorldRegistry::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        registrar,
        80,
        None,
    ));
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

/// A `WorldEngine` that delegates everything to the in-tree Incus harness but dials real loopback
/// TCP backends for `dial_world_tcp` (live `incusd` dialing stays gated on `H-004`).
struct DialEngine {
    inner: IncusEngine,
}

impl DialEngine {
    fn new(harness: &FakeIncus) -> Self {
        let (uid, gid) = current_uid_gid();
        Self {
            inner: IncusEngine::new(&harness.socket, "default", "base", uid, gid),
        }
    }
}

impl WorldEngine for DialEngine {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn create_snapshot(
        &self,
        source_dir: &Path,
        id: &SnapshotId,
        mode: slipfork_engine::SnapshotMode,
    ) -> Result<(), EngineError> {
        self.inner.create_snapshot(source_dir, id, mode)
    }

    fn freeze_world(
        &self,
        world: &WorldHandle,
        id: &SnapshotId,
        mode: slipfork_engine::SnapshotMode,
    ) -> Result<(), EngineError> {
        self.inner.freeze_world(world, id, mode)
    }

    fn clone_world(&self, spec: &WorldSpec) -> Result<WorldHandle, EngineError> {
        self.inner.clone_world(spec)
    }

    fn spawn_service(
        &self,
        world: &WorldHandle,
        spec: &slipfork_engine::ServiceSpec,
    ) -> Result<slipfork_engine::ServiceHandle, EngineError> {
        self.inner.spawn_service(world, spec)
    }

    fn stop_service(&self, service: &slipfork_engine::ServiceHandle) -> Result<(), EngineError> {
        self.inner.stop_service(service)
    }

    fn service_logs(
        &self,
        world: &WorldHandle,
        id: &str,
    ) -> Result<slipfork_engine::ServiceLogs, EngineError> {
        self.inner.service_logs(world, id)
    }

    fn exec_in_world(
        &self,
        world: &WorldHandle,
        argv: &[String],
        cwd: &Path,
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<slipfork_engine::ExecOutput, EngineError> {
        self.inner
            .exec_in_world(world, argv, cwd, overlay_env, on_output)
    }

    fn entry_command(
        &self,
        world: &WorldHandle,
        cwd: &Path,
    ) -> Result<WorldEntryCommand, EngineError> {
        self.inner.entry_command(world, cwd)
    }

    fn dial_world_tcp(&self, world: &WorldHandle, port: u16) -> Result<TcpStream, EngineError> {
        TcpStream::connect((Ipv4Addr::LOCALHOST, port)).map_err(|err| EngineError::DialFailed {
            world: format!("{world:?}"),
            port,
            errno: err.raw_os_error().unwrap_or(0),
        })
    }

    fn destroy_world(&self, world: WorldHandle) -> Result<(), EngineError> {
        self.inner.destroy_world(world)
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

fn dial_engine(harness: &FakeIncus) -> Arc<DialEngine> {
    Arc::new(DialEngine::new(harness))
}

/// Attach a live world whose catch-all route points at `backend_port`.
fn attach_pointing_at(
    registry: &WorldRegistry,
    engine: &Arc<DialEngine>,
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
    let port = listeners.http_port().expect("bound ingress port");
    let ingress = HttpIngress::bind(state, listeners, registry)
        .expect("bind the ingress")
        .with_relay_timeout(relay_timeout);
    thread::spawn(move || {
        let _ = ingress.serve();
    });
    port
}

fn shared_registry(
    engine: Arc<DialEngine>,
    registrar: Arc<InMemoryTunnelRegistrar>,
    forward_port: Option<u16>,
) -> Arc<WorldRegistry> {
    Arc::new(WorldRegistry::new(
        engine as Arc<dyn WorldEngine>,
        registrar,
        80,
        forward_port,
    ))
}

#[test]
fn shared_proxy_preserves_host_and_forwarded_headers() {
    let state = cached_test_dir("shared-proxy");
    let harness = FakeIncus::start(&state);
    let engine = dial_engine(&harness);
    let backend = spawn_backend(BackendMode::Head);
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = shared_registry(Arc::clone(&engine), Arc::clone(&registrar), Some(7080));
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
    let harness = FakeIncus::start(&state);
    let engine = dial_engine(&harness);
    let backend = spawn_backend(BackendMode::Stream);
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = shared_registry(Arc::clone(&engine), registrar, Some(7080));
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
    let harness = FakeIncus::start(&state);
    let engine = dial_engine(&harness);
    let backend = spawn_backend(BackendMode::Head);
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = shared_registry(Arc::clone(&engine), registrar, Some(7080));
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
    let harness = FakeIncus::start(&state);
    let engine = dial_engine(&harness);
    let backend = spawn_backend(BackendMode::Head);
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = shared_registry(Arc::clone(&engine), registrar, Some(7080));
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
    let harness = FakeIncus::start(&state);
    let engine = dial_engine(&harness);
    let backend = spawn_backend(BackendMode::UpgradeEcho);
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = shared_registry(Arc::clone(&engine), registrar, Some(7080));
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
    let harness = FakeIncus::start(&state);
    let engine = dial_engine(&harness);
    let backend = spawn_backend(BackendMode::UpgradeEcho);
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = shared_registry(Arc::clone(&engine), registrar, Some(7080));
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
    let harness = FakeIncus::start(&state);
    let engine = dial_engine(&harness);
    let project = acme();
    let target = WorldTarget::Canonical;
    let backend = spawn_backend(BackendMode::Head);

    let no_forward = shared_registry(
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

    let unavailable = Arc::new(WorldRegistry::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        Arc::new(UnavailableTunnelRegistrar::new()),
        80,
        Some(7080),
    ));
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
    let looping = shared_registry(
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
    let harness = FakeIncus::start(&state);
    let engine = dial_engine(&harness);
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
    let harness = FakeIncus::start(&state);
    let engine = dial_engine(&harness);
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = shared_registry(Arc::clone(&engine), Arc::clone(&registrar), Some(7080));
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
    last_cwd: Mutex<Option<PathBuf>>,
    secret_values: Mutex<Vec<String>>,
}

impl TestControl {
    fn new(registry: Arc<WorldRegistry>, forward_port: u16) -> Self {
        Self {
            registry,
            project: acme(),
            target: WorldTarget::Canonical,
            forward_port,
            last_cwd: Mutex::new(None),
            secret_values: Mutex::new(Vec::new()),
        }
    }

    fn record_cwd(&self, cwd: &Path) {
        *self.last_cwd.lock().expect("last cwd") = Some(cwd.to_path_buf());
    }

    fn summary(&self) -> Result<WorldSummary, EdgeError> {
        Ok(WorldSummary {
            world: slipfork_edge::world_id(&self.project, &self.target),
            state: WorldState::Running,
            origin: self.registry.origin(&self.project, &self.target)?,
            worktree: PathBuf::from("/repo"),
        })
    }

    fn up_summary(&self) -> Result<WorldUpSummary, EdgeError> {
        Ok(WorldUpSummary {
            world: slipfork_edge::world_id(&self.project, &self.target),
            origin: self.registry.origin(&self.project, &self.target)?,
            resources: vec![ResourceSummary {
                id: "pg_databases.main".to_owned(),
                strategy: "prefer_remote".to_owned(),
                resolved: "local".to_owned(),
                reason: "OSPP unpaired".to_owned(),
            }],
        })
    }
}

impl DaemonControl for TestControl {
    fn status(&self) -> Result<DaemonStatus, EdgeError> {
        Ok(DaemonStatus {
            pid: std::process::id(),
            http_ingress_port: self.registry.http_ingress_port(),
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
        let summary = self.up_summary()?;
        on_event(BootEvent::Resources {
            resource: summary.resources[0].clone(),
        });
        on_event(BootEvent::Ready {
            component: "api".to_owned(),
            port: 8000,
        });
        Ok(summary)
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
        self.up_summary()
    }

    fn drop_fork(&self, req: &DropForkRequest) -> Result<bool, EdgeError> {
        self.record_cwd(&req.cwd);
        Ok(true)
    }

    fn list(&self, _repo_root: Option<&Path>) -> Result<Vec<WorldSummary>, EdgeError> {
        Ok(vec![self.summary()?])
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

    fn auth_login(&self, pairing_url: &str) -> Result<AuthSummary, EdgeError> {
        assert!(pairing_url.contains("#handle="));
        Ok(AuthSummary {
            key_id: "p256:00".to_owned(),
            backend: "software:file-0600".to_owned(),
            account: Some("developer".to_owned()),
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

fn ready_registry(state: &Path, harness: &FakeIncus) -> (Arc<WorldRegistry>, Arc<DialEngine>) {
    let engine = dial_engine(harness);
    let registrar = Arc::new(InMemoryTunnelRegistrar::new(
        "https://acme-abc.weaved.example",
    ));
    let registry = Arc::new(WorldRegistry::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        registrar,
        80,
        Some(7080),
    ));
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
    (registry, engine)
}

#[test]
fn daemon_control_round_trip_and_frame_cap() {
    let state = cached_test_dir("control-round-trip");
    let harness = FakeIncus::start(&state);
    let (registry, _engine) = ready_registry(&state, &harness);
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
            },
            &mut |event| events.push(event),
        )
        .expect("up");
    assert_eq!(up.world, "acme");
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
            .auth_login("https://ospp.example/_ospp/v1/pair#handle=abc")
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
fn daemon_control_forward_lifetime_closes_host_listener() {
    let state = cached_test_dir("control-forward");
    let harness = FakeIncus::start(&state);
    let (registry, _engine) = ready_registry(&state, &harness);
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
    let harness = FakeIncus::start(&state);
    let engine = dial_engine(&harness);
    // No forward listener: `share` fails closed and the typed kind crosses the socket.
    let registry = Arc::new(WorldRegistry::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        Arc::new(UnavailableTunnelRegistrar::new()),
        80,
        None,
    ));
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

    // Non-template hardening contract (ADR-0008 §5).
    for directive in [
        "\nCapabilityBoundingSet=\n",
        "\nNoNewPrivileges=yes\n",
        "\nProtectSystem=strict\n",
        "\nPrivateTmp=yes\n",
        "\nRestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX\n",
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
            assert_eq!(listeners.http_port().expect("bound :80"), 80);
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

#[test]
fn socket_activate_systemd_handover() {
    let state = cached_test_dir("socket-activate");
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

    let ready_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match ready_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(()) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(status) = child.try_wait().expect("poll systemd-socket-activate") {
                    let _ = stderr_reader.join();
                    panic!(
                        "systemd-socket-activate exited before listening ({status})\nstderr:\n{}",
                        stderr_log.lock().expect("tool stderr log").as_str()
                    );
                }
                if Instant::now() >= ready_deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "systemd-socket-activate never reported a listening socket within 30s\nstderr:\n{}",
                        stderr_log.lock().expect("tool stderr log").as_str()
                    );
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = stderr_reader.join();
                panic!(
                    "systemd-socket-activate closed its stderr before listening\nstderr:\n{}",
                    stderr_log.lock().expect("tool stderr log").as_str()
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
    let _ = fs::remove_dir_all(&state);
}
