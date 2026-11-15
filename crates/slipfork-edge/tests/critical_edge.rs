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
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use slipfork_edge::{
    EdgeError, HttpIngress, IngressRoute, ListenConfig, WorldRegistry, adopt_systemd_listener,
    bind_listen, resolve_listener_set,
};
use slipfork_engine::{
    ExecEnv, IncusEngine, SnapshotId, WorldEngine, WorldHandle, WorldSpec, current_uid_gid,
};
use slipfork_world::{LocalHostRoute, ProjectSlug, WorldTarget};

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
    Arc::new(WorldRegistry::new(Arc::new(engine)))
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
    let registry = WorldRegistry::new(Arc::clone(&engine));
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

    let api = registry
        .route("feat.acme.localhost", "/api/users?page=2")
        .expect("longest prefix wins");
    assert_eq!(api.port(), 3001);
    assert_eq!(api.world_id(), "acme:feat");
    assert_eq!(api.host(), "feat.acme.localhost");

    let catch_all = registry
        .route("feat.acme.localhost:8080", "/static/app.js")
        .expect("the empty prefix is the internal_ports[0] catch-all");
    assert_eq!(catch_all.port(), 3000);
    assert_eq!(
        catch_all.host(),
        "feat.acme.localhost:8080",
        "the raw Host header is preserved for the upstream request"
    );

    let err = registry
        .route("other.localhost", "/")
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
        .route("feat.acme.localhost", "/api")
        .expect_err("a released world must stop routing");
    assert!(matches!(err, EdgeError::UnknownHost { .. }), "got {err:?}");
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn registry_rejects_invalid_ingress_tables() {
    let state = cached_test_dir("registry-invalid");
    let harness = FakeIncus::start(&state);
    let engine = harness.engine();
    let registry = WorldRegistry::new(Arc::clone(&engine));
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
        .route("acme.localhost", "/")
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
    let registry = Arc::new(WorldRegistry::new(Arc::clone(&engine)));
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
        EdgeError::IngressSocketInUse { path } => {
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
    let listeners = bind_listen(&listen_local()).expect("bind --listen");
    let port = listeners.http_port().expect("bound ingress port");
    let ingress = HttpIngress::bind(&state, listeners, empty_registry(&state)).expect("bind");
    thread::spawn(move || {
        let _ = ingress.serve();
    });

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
