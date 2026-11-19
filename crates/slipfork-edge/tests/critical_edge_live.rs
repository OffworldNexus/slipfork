//! Protected live critical tests for `slipfork-edge` routing, ingress, forward, and activation
//! (`M3.30.4.4`).
//!
//! Every assertion runs over a **real** `BtrfsEngine` + `KernelBtrfsDriver`, a real Btrfs snapshot,
//! and a real `slipfork-world-init` PID 1. The proxied hops, the WebSocket byte pipe, the 2 MiB
//! streamed upload, and the forward bridge dial in-world listeners through `dial_world_tcp`'s
//! SCM_RIGHTS handoff, so no engine, socket, or namespace is faked. A host without Btrfs fails
//! closed through [`WorldFixture`] with one line naming the missing substrate: this suite never
//! skips.

#![forbid(unsafe_code)]

use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use slipfork_edge::ingress::HEADER_CAP;
use slipfork_edge::{
    EdgeError, ForwardTargetSpec, HttpIngress, IngressRoute, ListenConfig, WorldRegistry,
    adopt_systemd_listener, bind_listen,
};
use slipfork_testkit::live::{WorldFixture, disk_tempdir, engine_at, engine_test_dir};
use slipfork_world::{LocalHostRoute, ProjectSlug, WorldEngine, WorldTarget};

/// World-local ports the in-world fixture server binds (a fresh netns, so they never collide).
const INGRESS_BACKEND_PORT: u16 = 18081;
/// World-local port the forward bridge echoes through.
const FORWARD_BACKEND_PORT: u16 = 18082;
/// World-local port that answers the WebSocket upgrade and echoes the post-101 byte stream.
const WS_BACKEND_PORT: u16 = 18083;
/// World-local port with no listener: a routed request there must become a 502.
const CLOSED_BACKEND_PORT: u16 = 18099;
/// Idle bound armed on the live ingress; the WebSocket journey exceeds it deliberately.
const RELAY_TIMEOUT: Duration = Duration::from_millis(200);
/// Silence an upgraded socket must survive (strictly longer than [`RELAY_TIMEOUT`]).
const IDLE_GAP: Duration = Duration::from_millis(500);
/// Payload echoed through the post-101 byte pipe, written in 63-byte fragments.
const WS_PAYLOAD: usize = 4096;
/// Upload size that must pass through the bounded relay without being buffered whole.
const UPLOAD_BYTES: usize = 2 * 1024 * 1024;
/// Client-supplied upgrade handshake proxied to the in-world WebSocket listener.
const WS_HANDSHAKE: &str = "GET /ws HTTP/1.1\r\nHost: acme.localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";

const LIVE_PROBE_MODE_ENV: &str = "SLIPFORK_EDGE_LIVE_PROBE";
const LIVE_PROBE_STATE_ENV: &str = "SLIPFORK_EDGE_LIVE_PROBE_STATE";
const LIVE_PROBE_PORT_ENV: &str = "SLIPFORK_EDGE_LIVE_PROBE_PORT";

/// The in-world server: an HTTP exchange on `http_port` that answers the head before draining the
/// body and reports both byte counts, a raw echo on `echo_port`, and a WebSocket upgrade echo on
/// `ws_port`. The script writes the ready file (inside the world's `repo` mount, so the host sees
/// it) only after all three listeners are bound.
const SERVER: &str = r#"
import os, socket, sys, threading, time
ready = sys.argv[1]
http_port = int(sys.argv[2])
echo_port = int(sys.argv[3])
ws_port = int(sys.argv[4])

def read_head(conn):
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = conn.recv(65536)
        if not chunk:
            break
        data += chunk
    head, _, rest = data.partition(b"\r\n\r\n")
    return head, rest

def http_exchange(conn):
    head, rest = read_head(conn)
    length = 0
    for line in head.split(b"\r\n")[1:]:
        name, _, value = line.partition(b":")
        if name.strip().lower() == b"content-length":
            length = int(value.strip())
    # Answer before the body is forwarded: the ingress relays this head first, then pipes the
    # remaining request bytes while the backend drains them (src/ingress.rs relay order).
    conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n")
    received = len(rest)
    while received < length:
        chunk = conn.recv(min(65536, length - received))
        if not chunk:
            break
        received += len(chunk)
    payload = b"HEAD_BYTES:" + str(len(head)).encode() + b" BODY_BYTES:" + str(received).encode() + b"\r\n" + head
    conn.sendall(payload)
    conn.close()

def raw_echo(conn):
    while True:
        chunk = conn.recv(65536)
        if not chunk:
            break
        conn.sendall(chunk)
    conn.close()

def ws_exchange(conn):
    head, rest = read_head(conn)
    low = head.lower()
    if b"upgrade" not in low or b"websocket" not in low:
        conn.sendall(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        conn.close()
        return
    conn.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
    if rest:
        conn.sendall(rest)
    raw_echo(conn)

def serve(port, handler):
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", port))
    listener.listen(16)
    while True:
        conn, _ = listener.accept()
        threading.Thread(target=handler, args=(conn,), daemon=True).start()

threading.Thread(target=serve, args=(http_port, http_exchange), daemon=True).start()
threading.Thread(target=serve, args=(echo_port, raw_echo), daemon=True).start()
threading.Thread(target=serve, args=(ws_port, ws_exchange), daemon=True).start()
open(ready, "w").write("ready")
while os.path.exists(ready):
    time.sleep(0.1)
"#;

fn acme() -> ProjectSlug {
    ProjectSlug::parse("acme").expect("valid project slug")
}

/// Read until the peer closes, tolerating a reset once the body has begun.
///
/// The in-world backend closes immediately after its last `sendall`, so the kernel can surface
/// `ECONNRESET` instead of a clean EOF; a reset after any byte is that close, not a lost reply.
/// A reset before the first byte is a real failure and names the byte count.
fn read_to_close(stream: &mut TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == ErrorKind::ConnectionReset && !bytes.is_empty() => break,
            Err(error) => panic!(
                "read the peer response after {} byte(s): {error}",
                bytes.len()
            ),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// One raw HTTP/1.1 request against the real ingress, returning the whole response.
fn tcp_request(port: u16, host: &str, path: &str) -> String {
    let mut stream =
        TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect to the real ingress");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .expect("write the request");
    read_to_close(&mut stream)
}

/// One raw HTTP/1.1 request over `<state_dir>/http-ingress.sock`.
fn unix_request(socket: &Path, host: &str, path: &str) -> String {
    let mut stream = UnixStream::connect(socket).expect("connect to http-ingress.sock");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("bound the socket read");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .expect("write the socket request");
    let mut text = String::new();
    stream
        .read_to_string(&mut text)
        .expect("read the socket response");
    text
}

fn send_raw(port: u16, request: &[u8]) -> String {
    let mut stream =
        TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect to the real ingress");
    stream.write_all(request).expect("write the raw request");
    read_to_close(&mut stream)
}

/// Read one response head byte-by-byte, so the post-101 byte pipe is never over-read.
fn read_head_bytes(stream: &mut TcpStream) -> Vec<u8> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream
            .read_exact(&mut byte)
            .expect("read the response head");
        head.push(byte[0]);
    }
    head
}

/// Bind one real ingress over `state_dir`, returning its TCP port and the unserved handle.
fn bind_ingress(
    state: &Path,
    registry: Arc<WorldRegistry>,
    relay_timeout: Option<Duration>,
) -> (u16, HttpIngress) {
    let listeners = bind_listen(&ListenConfig {
        addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        forward_port: None,
    })
    .expect("bind the real ingress listener");
    let port = listeners.http_addrs().expect("bound ingress port")[0].port();
    let ingress =
        HttpIngress::bind(state, listeners, registry).expect("bind the real ingress socket");
    let ingress = match relay_timeout {
        Some(timeout) => ingress.with_relay_timeout(timeout),
        None => ingress,
    };
    (port, ingress)
}

/// Stops the in-world fixture server on scope exit, including while a failed assertion unwinds.
///
/// The server serves while its `ready` file exists, so removing the file lets the blocked
/// `fixture.exec` return and `thread::scope` join instead of deadlocking the whole suite.
struct ReadyGuard<'a> {
    ready: &'a Path,
}

impl Drop for ReadyGuard<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.ready);
    }
}

/// Wait for the in-world server's ready file, failing with the server's own output if it exits.
fn await_server(ready: &Path, server: &thread::ScopedJoinHandle<'_, ()>) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if ready.exists() {
            return;
        }
        assert!(
            !server.is_finished(),
            "the in-world fixture server exited before binding its listeners"
        );
        thread::sleep(Duration::from_millis(50));
    }
    panic!("the in-world fixture server never became ready within 120s");
}

fn drain<R: Read>(reader: &mut Option<R>) -> String {
    let Some(reader) = reader else {
        return String::new();
    };
    let mut text = String::new();
    let _ = reader.read_to_string(&mut text);
    text
}

/// Wait for the fd-3 probe child's readiness marker, quoting its stderr if it died first.
fn await_probe_ready(child: &mut Child, ready: &Path) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if ready.exists() {
            return;
        }
        if let Some(status) = child.try_wait().expect("poll the probe child") {
            panic!(
                "probe child exited before becoming ready ({status})\nstderr:\n{}",
                drain(&mut child.stderr)
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!(
        "probe child did not become ready within 120s\nstderr:\n{}",
        drain(&mut child.stderr)
    );
}

/// Wait for the fd-3 probe child to exit, reaping it instead of leaving a process behind.
fn wait_for_probe_exit(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("poll the probe child") {
            return status;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!(
        "probe child did not exit within 120s\nstderr:\n{}",
        drain(&mut child.stderr)
    );
}

/// The child half of the real fd-3 handover: adopt the parent's listener, serve a real 404.
///
/// It only runs when the wrapper shell selected the probe mode; under the normal suite its
/// environment is bare and it returns without touching Btrfs.
#[test]
fn activation_probe_child_live() {
    let Ok(mode) = std::env::var(LIVE_PROBE_MODE_ENV) else {
        return;
    };
    assert_eq!(mode, "one_http_listener");
    let marker_dir = PathBuf::from(std::env::var(LIVE_PROBE_STATE_ENV).expect("probe marker dir"));
    let expected_port: u16 = std::env::var(LIVE_PROBE_PORT_ENV)
        .expect("probe port")
        .parse()
        .expect("numeric probe port");

    let listeners = adopt_systemd_listener()
        .expect("adoption succeeds")
        .expect("activation is present");
    assert_eq!(
        listeners.http_listeners.len(),
        1,
        "LISTEN_FDS=1 is exactly one HTTP listener"
    );
    assert!(
        listeners.forward_listener.is_none(),
        "LISTEN_FDS=1 must not invent a forward listener"
    );
    assert_eq!(
        listeners.http_addrs().expect("adopted port")[0].port(),
        expected_port,
        "the adopted fd 3 must be the parent's listening socket"
    );

    let state = engine_test_dir("edge-live-activation");
    let registry = Arc::new(WorldRegistry::new(engine_at(&state), 80));
    let ingress =
        HttpIngress::bind(&state, listeners, registry).expect("bind the adopted listener");
    fs::write(marker_dir.join("probe.ready"), b"ready\n").expect("write the readiness marker");
    thread::spawn(move || {
        let _ = ingress.serve();
    });

    let done = marker_dir.join("probe.done");
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline && !done.exists() {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(done.exists(), "the parent never finished its request");
}

/// The parent half: hand a real listening socket to fd 3 and assert the child serves the dispatch.
#[test]
fn activation_single_listener_handover_live() {
    let markers = disk_tempdir("edge-live-activation-markers");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind the probe listener");
    let port = listener.local_addr().expect("probe address").port();
    listener
        .set_nonblocking(false)
        .expect("start blocking like systemd");

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(
            "exec 3<&0; LISTEN_PID=$$; LISTEN_FDS=1; export LISTEN_PID LISTEN_FDS; \
             exec \"$0\" --exact activation_probe_child_live --test-threads=1 --nocapture",
        )
        .arg(std::env::current_exe().expect("current test executable"))
        .env(LIVE_PROBE_MODE_ENV, "one_http_listener")
        .env(LIVE_PROBE_STATE_ENV, markers.path())
        .env(LIVE_PROBE_PORT_ENV, port.to_string())
        .stdin(Stdio::from(OwnedFd::from(listener)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the activation probe child");

    await_probe_ready(&mut child, &markers.path().join("probe.ready"));
    let response = tcp_request(port, "nope.localhost", "/");
    fs::write(markers.path().join("probe.done"), b"done\n").expect("signal probe completion");
    let status = wait_for_probe_exit(&mut child);
    let stdout = drain(&mut child.stdout);
    let stderr = drain(&mut child.stderr);
    assert!(
        status.success(),
        "the fd-3 probe failed with {status}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        response.starts_with("HTTP/1.1 404 Not Found"),
        "the adopted fd 3 must serve the real dispatch, got:\n{response}"
    );
    assert!(
        response.contains("no world is attached for host 'nope.localhost'"),
        "the 404 must name the Host, got:\n{response}"
    );
}

#[test]
fn registry_ingress_and_forward_over_one_real_world() {
    let fixture = WorldFixture::new("edge-live");
    let ready = fixture.repo().join("edge-live.ready");
    let world = fixture.clone_world(WorldTarget::Canonical);
    let script = format!(
        "python3 -c '{}' {} {} {} {}",
        SERVER,
        ready.display(),
        INGRESS_BACKEND_PORT,
        FORWARD_BACKEND_PORT,
        WS_BACKEND_PORT
    );

    thread::scope(|scope| {
        let server = scope.spawn(|| {
            // Blocks in the world's namespace for the whole journey; the destroy below ends it.
            let _ = fixture.exec(&world, &script);
        });
        await_server(&ready, &server);
        // Declared after the server is ready, so removing the file can never race its creation;
        // the guard drops (removing the ready file) before the scope joins the server thread,
        // including while unwinding from a failed assertion below.
        let ready_guard = ReadyGuard { ready: &ready };

        let registry = Arc::new(WorldRegistry::new(fixture.engine().clone(), 80));
        let project = acme();
        let target = WorldTarget::Canonical;

        // One origin per world: port 80 renders port-less, an ephemeral ingress keeps its port,
        // and a `Forkfile` `domain` ids route is never the single public origin.
        let canonical = LocalHostRoute::new(project.clone(), target.clone(), None, &[])
            .expect("canonical route");
        assert_eq!(
            registry
                .mint_origin(&canonical, 80)
                .expect("port-80 origin")
                .url(),
            "http://acme.localhost"
        );
        assert_eq!(
            registry
                .mint_origin(&canonical, 8080)
                .expect("port-qualified origin")
                .url(),
            "http://acme.localhost:8080"
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
        let admin = LocalHostRoute::new(project.clone(), target.clone(), Some("admin"), &["admin"])
            .expect("declared snow domain");
        match registry.mint_origin(&admin, 80) {
            Err(EdgeError::Origin { world, host, .. }) => {
                assert_eq!(world, "acme");
                assert_eq!(host, "admin.acme.localhost");
            }
            other => panic!("a domains[].id ingress must never be the public origin: {other:?}"),
        }

        // Attach the real world; `/api` and `/dead` are routable until a catch-all is added.
        registry
            .attach_world(
                &project,
                &target,
                Vec::new(),
                world.clone(),
                vec![
                    IngressRoute {
                        prefix: "/api".to_owned(),
                        port: INGRESS_BACKEND_PORT,
                    },
                    IngressRoute {
                        prefix: "/dead".to_owned(),
                        port: CLOSED_BACKEND_PORT,
                    },
                ],
            )
            .expect("attach the real world");
        let duplicate = registry
            .attach_world(
                &project,
                &target,
                Vec::new(),
                world.clone(),
                vec![IngressRoute {
                    prefix: "/api".to_owned(),
                    port: INGRESS_BACKEND_PORT,
                }],
            )
            .expect_err("a duplicate registration must fail closed");
        assert!(
            matches!(&duplicate, EdgeError::WorldAlreadyRegistered { world } if world == "acme"),
            "got {duplicate:?}"
        );
        let api = registry
            .resolve("acme.localhost", "/api/users?page=2")
            .expect("the declared prefix serves");
        assert_eq!(api.port(), INGRESS_BACKEND_PORT);
        assert_eq!(api.world_id(), "acme");
        match registry.resolve("acme.localhost", "/nope") {
            Err(EdgeError::UnknownIngressPath { world, host, path }) => {
                assert_eq!(world, "acme");
                assert_eq!(host, "acme.localhost");
                assert_eq!(path, "/nope");
            }
            other => panic!("an unwritten path must fail closed: {other:?}"),
        }
        assert!(matches!(
            registry.resolve("missing.localhost", "/"),
            Err(EdgeError::UnknownHost { .. })
        ));

        // Direct-hop sanity: dial the world exactly as the ingress does and read the backend's raw
        // reply, so a later ingress failure is never mistaken for a broken world listener.
        let mut backend = registry.dial(&api).expect("dial the in-world listener");
        backend
            .write_all(
                b"GET /api/users?page=2 HTTP/1.1\r\nHost: acme.localhost\r\nConnection: close\r\n\r\n",
            )
            .expect("write the direct backend request");
        let direct = read_to_close(&mut backend);
        if !direct.starts_with("HTTP/1.1 200") || !direct.contains("HEAD_BYTES:") {
            eprintln!("direct world dial returned:\n{direct}");
        }
        assert!(
            direct.starts_with("HTTP/1.1 200"),
            "the world backend must answer a direct dial, got:\n{direct}"
        );
        assert!(
            direct.contains("HEAD_BYTES:"),
            "the direct hop must reach the in-world handler, got:\n{direct}"
        );

        // The live ingress: `0600` pathname socket, real dispatch on both transports.
        let (ingress_port, ingress) = bind_ingress(
            fixture.state_dir(),
            Arc::clone(&registry),
            Some(RELAY_TIMEOUT),
        );
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

        eprintln!("edge-live step: unknown-host");
        let unknown = tcp_request(ingress_port, "missing.localhost", "/");
        assert!(
            unknown.starts_with("HTTP/1.1 404 Not Found"),
            "an unknown host must be a 404, got:\n{unknown}"
        );
        assert!(
            unknown.contains("no world is attached for host 'missing.localhost'"),
            "the 404 must name the Host, got:\n{unknown}"
        );
        let over_unix = unix_request(&socket, "missing.localhost", "/");
        assert_eq!(
            over_unix, unknown,
            "TCP and http-ingress.sock must run the exact same dispatch"
        );

        // A traversal target is refused before any backend is dialed, with the typed wording.
        eprintln!("edge-live step: traversal");
        let traversal = tcp_request(ingress_port, "acme.localhost", "/api/../secret");
        assert!(
            traversal.starts_with("HTTP/1.1 400 Bad Request"),
            "a traversal target must fail closed with 400, got:\n{traversal}"
        );
        assert!(
            traversal
                .contains("refusing path traversal from host 'acme.localhost': '/api/../secret'"),
            "the 400 must name the host and target, got:\n{traversal}"
        );

        // A live socket is never stolen: a second daemon on the same state dir fails closed.
        let listeners = bind_listen(&ListenConfig {
            addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            forward_port: None,
        })
        .expect("bind a second ingress listener");
        match HttpIngress::bind(fixture.state_dir(), listeners, Arc::clone(&registry)) {
            Err(EdgeError::SocketInUse { path, .. }) => {
                assert!(path.ends_with("http-ingress.sock"), "got '{path}'");
            }
            other => panic!("a live ingress socket must never be stolen: {other:?}"),
        }

        // A dropped daemon leaves a stale file; the next bind replaces it and keeps it owner-only.
        let stale_dir = fixture.state_dir().join("stale");
        let stale_socket = {
            let (_, first) = bind_ingress(&stale_dir, Arc::clone(&registry), None);
            let path = first.unix_socket_path().to_path_buf();
            drop(first);
            path
        };
        assert!(
            stale_socket.exists(),
            "a dropped listener leaves a stale socket file"
        );
        let (_, replaced) = bind_ingress(&stale_dir, Arc::clone(&registry), None);
        let replaced_mode = fs::metadata(replaced.unix_socket_path())
            .expect("replacement socket exists")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            replaced_mode, 0o600,
            "a replaced stale socket stays owner-only"
        );
        drop(replaced);

        // Refusals: unmatched prefix, unknown verb, missing Host, oversized head, dead backend.
        let unmatched = tcp_request(ingress_port, "acme.localhost", "/nope");
        assert!(
            unmatched.starts_with("HTTP/1.1 404 Not Found"),
            "an unmatched ingress prefix must be a 404, got:\n{unmatched}"
        );
        assert!(
            unmatched
                .contains("world acme has no ingress route for '/nope' (host 'acme.localhost')"),
            "the 404 must name the world id, host, and path, got:\n{unmatched}"
        );
        eprintln!("edge-live step: 405");
        let method = send_raw(
            ingress_port,
            b"BREW /api HTTP/1.1\r\nHost: acme.localhost\r\n\r\n",
        );
        assert!(
            method.starts_with("HTTP/1.1 405 Method Not Allowed"),
            "an unknown verb must be a 405, got:\n{method}"
        );
        assert!(
            method.contains("Allow: GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS"),
            "the 405 must advertise the dispatched verbs, got:\n{method}"
        );
        let no_host = send_raw(
            ingress_port,
            b"GET /api HTTP/1.1\r\nConnection: close\r\n\r\n",
        );
        assert!(
            no_host.starts_with("HTTP/1.1 400 Bad Request"),
            "a request without Host must be a 400, got:\n{no_host}"
        );
        let mut oversized = TcpStream::connect((Ipv4Addr::LOCALHOST, ingress_port))
            .expect("connect for the oversized head");
        oversized
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("bound the refusal read");
        let mut request = b"GET /api HTTP/1.1\r\nHost: acme.localhost\r\nX-Pad: ".to_vec();
        request.resize(request.len() + HEADER_CAP + 1024, b'a');
        // The ingress refuses at the cap and closes, so a mid-write error is the refusal, not a bug.
        let _ = oversized.write_all(&request);
        let too_large = read_to_close(&mut oversized);
        assert!(
            too_large.starts_with("HTTP/1.1 431 Request Header Fields Too Large"),
            "a head past the {HEADER_CAP}-byte cap must be a 431, got:\n{too_large}"
        );
        eprintln!("edge-live step: 502");
        let unreachable = tcp_request(ingress_port, "acme.localhost", "/dead");
        assert!(
            unreachable.starts_with("HTTP/1.1 502 Bad Gateway"),
            "a resolved world whose backend cannot be dialed must be a 502, got:\n{unreachable}"
        );
        assert!(
            unreachable.contains(
                "world acme cannot reach its backend on port 18099 for host 'acme.localhost'"
            ),
            "the 502 must name the world, host, and port, got:\n{unreachable}"
        );

        // `replace_world` adds the catch-all without touching the live handle.
        registry
            .replace_world(
                &project,
                &target,
                Vec::new(),
                vec![
                    IngressRoute {
                        prefix: "/api".to_owned(),
                        port: INGRESS_BACKEND_PORT,
                    },
                    IngressRoute {
                        prefix: "/dead".to_owned(),
                        port: CLOSED_BACKEND_PORT,
                    },
                    IngressRoute {
                        prefix: "/ws".to_owned(),
                        port: WS_BACKEND_PORT,
                    },
                    IngressRoute {
                        prefix: "mail.*".to_owned(),
                        port: CLOSED_BACKEND_PORT,
                    },
                    IngressRoute {
                        prefix: String::new(),
                        port: INGRESS_BACKEND_PORT,
                    },
                ],
            )
            .expect("replace the route table");
        assert_eq!(
            registry
                .resolve("acme.localhost", "/nope")
                .expect("the catch-all serves")
                .port(),
            INGRESS_BACKEND_PORT
        );
        // A `<label>.*` entry owns the label.<world>.localhost host family on its own port.
        let mail = registry
            .resolve("mail.acme.localhost", "/")
            .expect("a mail.* route owns the mail.<world>.localhost host");
        assert_eq!(mail.port(), CLOSED_BACKEND_PORT);
        assert_eq!(mail.world_id(), "acme");
        assert!(
            fixture
                .engine()
                .world_is_live(&world)
                .expect("the handle stays live through a route replace"),
            "replacing routes must never destroy the live world"
        );

        // Every unroutable table is refused through `replace_world`, never partially applied.
        let invalid: [(&str, Vec<IngressRoute>); 6] = [
            ("at least one route", Vec::new()),
            (
                "port 0",
                vec![IngressRoute {
                    prefix: "/api".to_owned(),
                    port: 0,
                }],
            ),
            (
                "must be empty or start with '/'",
                vec![IngressRoute {
                    prefix: "api".to_owned(),
                    port: INGRESS_BACKEND_PORT,
                }],
            ),
            (
                "duplicate ingress prefix",
                vec![
                    IngressRoute {
                        prefix: String::new(),
                        port: INGRESS_BACKEND_PORT,
                    },
                    IngressRoute {
                        prefix: String::new(),
                        port: INGRESS_BACKEND_PORT + 1,
                    },
                ],
            ),
            (
                "must be empty or start with '/'",
                vec![IngressRoute {
                    prefix: "/bad path".to_owned(),
                    port: INGRESS_BACKEND_PORT,
                }],
            ),
            (
                "must be empty or start with '/'",
                vec![IngressRoute {
                    prefix: "/bad?query".to_owned(),
                    port: INGRESS_BACKEND_PORT,
                }],
            ),
        ];
        for (index, (expected, routes)) in invalid.into_iter().enumerate() {
            match registry.replace_world(&project, &target, Vec::new(), routes) {
                Err(EdgeError::InvalidIngressRoute { world, reason }) => {
                    assert_eq!(world, "acme");
                    assert!(
                        reason.contains(expected),
                        "case {index}: expected '{expected}' in '{reason}'"
                    );
                }
                other => panic!("case {index}: expected InvalidIngressRoute, got {other:?}"),
            }
        }
        assert_eq!(
            registry
                .resolve("acme.localhost", "/nope")
                .expect("a refused replace keeps the served table")
                .port(),
            INGRESS_BACKEND_PORT
        );

        // A real proxied hop: the ingress resolves, dials the in-world listener, and pipes it.
        eprintln!("edge-live step: proxied-200");
        let response = tcp_request(ingress_port, "acme.localhost", "/api/users?page=2");
        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "a .localhost world host must be proxied through the real engine, got:\n{response}"
        );
        for needle in [
            "GET /api/users?page=2 HTTP/1.1",
            "Host: acme.localhost",
            "X-Forwarded-Host: acme.localhost",
            "X-Forwarded-Proto: http",
        ] {
            assert!(
                response.contains(needle),
                "the proxied request must carry {needle}, got:\n{response}"
            );
        }

        // An `ssh -L` tunnel's local port must route exactly like the bare host: the in-world
        // backend sees the same request and the same forwarded host once the port is stripped.
        let tunneled = tcp_request(
            ingress_port,
            &format!("acme.localhost:{ingress_port}"),
            "/api/users?page=2",
        );
        assert!(
            tunneled.starts_with("HTTP/1.1 200 OK"),
            "a port-qualified .localhost Host must be proxied, got:\n{tunneled}"
        );
        let normalize = |text: &str| {
            let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text, ""));
            let echoed = body.split_once("\r\n").map_or(body, |(_, rest)| rest);
            format!("{head}\r\n\r\n{echoed}")
                .replace(&format!("acme.localhost:{ingress_port}"), "acme.localhost")
        };
        assert_eq!(
            normalize(&tunneled),
            normalize(&response),
            "Host: acme.localhost:<port> must route exactly like Host: acme.localhost"
        );

        // WebSocket proving: a 101 handshake, a fragmented 4 KiB echo, and an idle socket.
        eprintln!("edge-live step: ws-handshake");
        let mut ws =
            TcpStream::connect((Ipv4Addr::LOCALHOST, ingress_port)).expect("connect the websocket");
        ws.set_read_timeout(Some(Duration::from_secs(30)))
            .expect("bound the websocket reads");
        ws.write_all(WS_HANDSHAKE.as_bytes())
            .expect("write the upgrade handshake");
        let handshake = read_head_bytes(&mut ws);
        assert!(
            handshake.starts_with(b"HTTP/1.1 101 Switching Protocols"),
            "the .localhost world host must proxy WebSocket handshakes, got:\n{}",
            String::from_utf8_lossy(&handshake)
        );
        eprintln!("edge-live step: ws-fragments");
        let payload: Vec<u8> = (0..WS_PAYLOAD).map(|index| (index % 251) as u8).collect();
        for piece in payload.chunks(63) {
            ws.write_all(piece).expect("write a fragment");
            ws.flush().expect("flush a fragment");
            thread::sleep(Duration::from_millis(1));
        }
        let mut echoed = vec![0_u8; WS_PAYLOAD];
        ws.read_exact(&mut echoed)
            .expect("read the echoed fragments");
        assert_eq!(
            echoed, payload,
            "the byte pipe must not re-frame the stream"
        );
        // An upgraded connection holds an ingress relay slot: close it before the next phase.
        drop(ws);

        eprintln!("edge-live step: ws-idle");
        let mut idle = TcpStream::connect((Ipv4Addr::LOCALHOST, ingress_port))
            .expect("connect the idle websocket");
        idle.set_read_timeout(Some(Duration::from_secs(30)))
            .expect("bound the idle websocket reads");
        idle.write_all(WS_HANDSHAKE.as_bytes())
            .expect("write the idle upgrade handshake");
        let idle_handshake = read_head_bytes(&mut idle);
        assert!(
            idle_handshake.starts_with(b"HTTP/1.1 101 Switching Protocols"),
            "the idle socket must complete its upgrade, got:\n{}",
            String::from_utf8_lossy(&idle_handshake)
        );
        thread::sleep(IDLE_GAP);
        idle.write_all(b"after-idle")
            .expect("write after the idle gap");
        idle.flush().expect("flush after the idle gap");
        let mut after = [0_u8; 10];
        idle.read_exact(&mut after)
            .expect("the upgraded socket must survive the idle gap");
        assert_eq!(&after, b"after-idle");
        // An upgraded connection holds an ingress relay slot: close it before the upload phase.
        drop(idle);

        eprintln!("edge-live step: upload");
        // A 2 MiB upload streams through the bounded relay: the backend counts every byte.
        let (stream_port, stream_ingress) = bind_ingress(
            &fixture.state_dir().join("stream"),
            Arc::clone(&registry),
            None,
        );
        thread::spawn(move || {
            let _ = stream_ingress.serve();
        });
        let mut upload =
            TcpStream::connect((Ipv4Addr::LOCALHOST, stream_port)).expect("connect the upload");
        upload
            .set_read_timeout(Some(Duration::from_secs(60)))
            .expect("bound the upload read");
        upload
            .write_all(
                format!(
                    "POST /upload HTTP/1.1\r\nHost: acme.localhost\r\nContent-Length: {UPLOAD_BYTES}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .expect("write the upload head");
        let block = vec![b'z'; 64 * 1024];
        let mut sent = 0;
        while sent < UPLOAD_BYTES {
            let want = block.len().min(UPLOAD_BYTES - sent);
            upload
                .write_all(&block[..want])
                .expect("stream the request body");
            sent += want;
        }
        upload
            .shutdown(Shutdown::Write)
            .expect("half-close the upload");
        let upload_response = read_to_close(&mut upload);
        assert!(
            upload_response.contains(&format!("BODY_BYTES:{UPLOAD_BYTES}")),
            "a {UPLOAD_BYTES}-byte body must stream through the bounded relay intact, got:\n{upload_response}"
        );

        // A real forward bridge: host listener -> dial_world_tcp -> in-world raw echo.
        eprintln!("edge-live step: forward");
        let bridge = registry
            .forward(
                &project,
                &target,
                &[ForwardTargetSpec {
                    id: "echo".to_owned(),
                    port: FORWARD_BACKEND_PORT,
                    scheme: "tcp".to_owned(),
                    path: String::new(),
                }],
            )
            .expect("bind the real forward bridge");
        assert_eq!(bridge.summary().world, "acme");
        let host_addr = bridge.summary().targets[0].host_addr.clone();
        let mut stream = TcpStream::connect(&host_addr).expect("connect the forward bridge");
        stream.write_all(b"ping").expect("write through the bridge");
        let mut echoed = [0_u8; 4];
        stream
            .read_exact(&mut echoed)
            .expect("read the in-world echo");
        assert_eq!(&echoed, b"ping", "forward must be a raw bidirectional pipe");
        drop(stream);
        drop(bridge);

        // Releasing the world removes the route and destroys the real handle exactly once.
        eprintln!("edge-live step: release");
        assert!(
            registry
                .release_world(&project, &target)
                .expect("release the real world")
        );
        assert!(
            !registry
                .release_world(&project, &target)
                .expect("releasing an absent world is not an error")
        );
        assert!(matches!(
            registry.resolve("acme.localhost", "/api"),
            Err(EdgeError::UnknownHost { .. })
        ));
        drop(ready_guard);
        server.join().expect("the in-world server thread joins");
    });
}
