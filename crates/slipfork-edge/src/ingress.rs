//! Bounded `httparse` HTTP/1.1 dispatch and the `0600` `AF_UNIX` ingress socket.
//!
//! Requests are parsed under a `<= 16 KiB` header cap with bounded read timeouts, routed by
//! [`crate::registry::WorldRegistry`]'s `IngressRoute` longest-prefix table into
//! `WorldEngine::dial_world_tcp`, and relayed byte-for-byte back to the client. Unknown hosts and
//! unmatched paths answer `404`; malformed requests, unknown methods, oversized heads, and
//! unreachable backends answer `400`/`405`/`431`/`502` respectively — never a partial response and
//! never a silent fallback.
//!
//! Alongside its TCP listeners [`HttpIngress`] binds the `0600` pathname `AF_UNIX` socket
//! `<state_dir>/http-ingress.sock` (`H-011` #4 Variant a). `slipfork-world-init` splices every
//! in-world `$BASE_URL` connection to that socket, and its accepted streams run through the exact
//! same dispatch loop as TCP — no second HTTP code path, no FD passing.
//!
//! Every connection is served by one `std::thread` (zero async runtime), and every listener is a
//! blocking kernel `accept`: adopted activation descriptors deliberately arrive non-blocking
//! (`ADR-0008` §5) and are normalized back to blocking here, so an idle listener sleeps in the
//! kernel instead of spinning.

use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use slipfork_engine::world_init::{UNIX_SOCKET_PATH_MAX, unix_socket_path_fits};

use crate::EdgeError;
use crate::activation::ListenerSet;
use crate::registry::{RoutedWorld, WorldRegistry};

/// Largest HTTP request head (request line + headers) the ingress accepts.
pub const HEADER_CAP: usize = 16 * 1024;

/// Lets the world-init bridge inspect the accepted socket file through `sun_path`.
const HTTP_INGRESS_SOCKET_FILE: &str = "http-ingress.sock";

/// Bound on reading one request head, so a stalled client cannot pin a thread forever.
const HEAD_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on one relay read, so an idle proxied connection is still reclaimed.
const RELAY_TIMEOUT: Duration = Duration::from_secs(60);

/// Chunk size for relayed request/response bytes in either direction.
const RELAY_CAP: usize = 16 * 1024;

/// Largest number of headers `httparse` may collect from one request head.
const MAX_HEADERS: usize = 64;

/// HTTP verbs the ingress dispatches. Everything else is `405 Method Not Allowed`.
const ALLOWED_METHODS: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

/// Hop-by-hop request headers the ingress replaces with its own `Connection: close`.
const CONNECTION_HEADERS: &[&str] = &["connection", "keep-alive", "proxy-connection"];

/// The `slipforkd` HTTP/WS ingress: every TCP listener plus `<state_dir>/http-ingress.sock`.
pub struct HttpIngress {
    registry: Arc<WorldRegistry>,
    http_listeners: Vec<TcpListener>,
    forward_listener: Option<TcpListener>,
    unix_listener: UnixListener,
    unix_path: PathBuf,
}

impl std::fmt::Debug for HttpIngress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpIngress")
            .field("http_listeners", &self.http_listeners)
            .field("forward_listener", &self.forward_listener)
            .field("unix_path", &self.unix_path)
            .finish_non_exhaustive()
    }
}

impl HttpIngress {
    /// Take ownership of the resolved listeners and bind `<state_dir>/http-ingress.sock` (`0600`).
    ///
    /// A stale socket file is replaced, but a socket a running daemon still serves is refused with
    /// [`EdgeError::IngressSocketInUse`] rather than silently stolen.
    pub fn bind(
        state_dir: &Path,
        listeners: ListenerSet,
        registry: Arc<WorldRegistry>,
    ) -> Result<Self, EdgeError> {
        let unix_path = state_dir.join(HTTP_INGRESS_SOCKET_FILE);
        if !unix_socket_path_fits(&unix_path) {
            return Err(EdgeError::IngressSocketPath {
                path: unix_path.display().to_string(),
                limit: UNIX_SOCKET_PATH_MAX,
            });
        }
        fs::create_dir_all(state_dir).map_err(|err| EdgeError::IngressStateDir {
            path: state_dir.display().to_string(),
            message: err.to_string(),
        })?;
        replace_stale_socket(&unix_path)?;
        let unix_listener =
            UnixListener::bind(&unix_path).map_err(|err| EdgeError::IngressSocket {
                path: unix_path.display().to_string(),
                message: err.to_string(),
            })?;
        fs::set_permissions(&unix_path, fs::Permissions::from_mode(0o600)).map_err(|err| {
            EdgeError::IngressSocket {
                path: unix_path.display().to_string(),
                message: format!("cannot restrict the socket to 0600: {err}"),
            }
        })?;

        let ListenerSet {
            http_listeners,
            forward_listener,
        } = listeners;
        // Adopted activation descriptors are non-blocking by contract; the synchronous accept
        // loops below want kernel-blocking accepts.
        for listener in http_listeners.iter().chain(forward_listener.iter()) {
            listener
                .set_nonblocking(false)
                .map_err(|err| EdgeError::AcceptFailed {
                    listener: listener_label(listener),
                    message: format!("cannot switch the listener to blocking mode: {err}"),
                })?;
        }
        unix_listener
            .set_nonblocking(false)
            .map_err(|err| EdgeError::AcceptFailed {
                listener: unix_path.display().to_string(),
                message: format!("cannot switch the ingress socket to blocking mode: {err}"),
            })?;

        Ok(Self {
            registry,
            http_listeners,
            forward_listener,
            unix_listener,
            unix_path,
        })
    }

    /// Path of the `0600` `AF_UNIX` ingress socket (`H-011` #4 Variant a).
    pub fn unix_socket_path(&self) -> &Path {
        &self.unix_path
    }

    /// Serve every listener forever, one `std::thread` per connection.
    ///
    /// Blocks for the daemon's whole lifetime and reports the first accept loop that fails: a
    /// partially served ingress would silently drop one listener's traffic, so the caller must treat
    /// this as fatal. Per-connection threads never affect the accept loops.
    pub fn serve(self) -> Result<(), EdgeError> {
        let registry = self.registry;
        // One monitor channel so a failure on *any* listener (not only the first joined one)
        // surfaces immediately instead of being masked by a healthy accept loop.
        let (failures, failure) = std::sync::mpsc::channel();
        for listener in self.http_listeners.into_iter().chain(self.forward_listener) {
            spawn_accept_loop(
                IngressListener::Tcp(listener),
                Arc::clone(&registry),
                &failures,
            );
        }
        spawn_accept_loop(
            IngressListener::Unix(self.unix_listener, self.unix_path.display().to_string()),
            registry,
            &failures,
        );
        drop(failures);

        match failure.recv() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(err),
            Err(_) => Err(EdgeError::IngressLoop {
                message: "every ingress accept loop ended".to_owned(),
            }),
        }
    }
}

/// Run one accept loop on its own thread and report how it ended to the monitor channel.
fn spawn_accept_loop(
    listener: IngressListener,
    registry: Arc<WorldRegistry>,
    failures: &std::sync::mpsc::Sender<Result<(), EdgeError>>,
) {
    let failures = failures.clone();
    thread::spawn(move || {
        let outcome = accept_loop(listener, &registry);
        let _ = failures.send(outcome);
    });
}

/// Accept connections from one TCP listener or the `AF_UNIX` ingress socket.
enum IngressListener {
    Tcp(TcpListener),
    Unix(UnixListener, String),
}

impl IngressListener {
    fn accept(&self) -> io::Result<ClientStream> {
        match self {
            Self::Tcp(listener) => listener
                .accept()
                .map(|(stream, _)| ClientStream::Tcp(stream)),
            Self::Unix(listener, _) => listener
                .accept()
                .map(|(stream, _)| ClientStream::Unix(stream)),
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Tcp(listener) => listener_label(listener),
            Self::Unix(_, label) => label.clone(),
        }
    }
}

fn listener_label(listener: &TcpListener) -> String {
    listener
        .local_addr()
        .map_or_else(|_| "tcp".to_owned(), |addr| addr.to_string())
}

fn accept_loop(listener: IngressListener, registry: &Arc<WorldRegistry>) -> Result<(), EdgeError> {
    let label = listener.label();
    loop {
        match listener.accept() {
            Ok(client) => {
                let registry = Arc::clone(registry);
                thread::spawn(move || handle_connection(&registry, client));
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => {
                return Err(EdgeError::AcceptFailed {
                    listener: label,
                    message: err.to_string(),
                });
            }
        }
    }
}

/// One accepted client stream, shared by TCP and `http-ingress.sock` connections.
enum ClientStream {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl ClientStream {
    fn set_timeout(&self, timeout: Duration) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => {
                stream.set_read_timeout(Some(timeout))?;
                stream.set_write_timeout(Some(timeout))
            }
            Self::Unix(stream) => {
                stream.set_read_timeout(Some(timeout))?;
                stream.set_write_timeout(Some(timeout))
            }
        }
    }

    fn try_clone(&self) -> io::Result<Self> {
        match self {
            Self::Tcp(stream) => stream.try_clone().map(Self::Tcp),
            Self::Unix(stream) => stream.try_clone().map(Self::Unix),
        }
    }

    fn shutdown_write(&self) {
        let _ = match self {
            Self::Tcp(stream) => stream.shutdown(Shutdown::Write),
            Self::Unix(stream) => stream.shutdown(Shutdown::Write),
        };
    }

    fn shutdown_both(&self) {
        let _ = match self {
            Self::Tcp(stream) => stream.shutdown(Shutdown::Both),
            Self::Unix(stream) => stream.shutdown(Shutdown::Both),
        };
    }

    fn peer(&self) -> String {
        match self {
            Self::Tcp(stream) => stream
                .peer_addr()
                .map_or_else(|_| "tcp peer".to_owned(), |addr| addr.to_string()),
            Self::Unix(_) => HTTP_INGRESS_SOCKET_FILE.to_owned(),
        }
    }
}

impl Read for ClientStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buf),
            Self::Unix(stream) => stream.read(buf),
        }
    }
}

impl Write for ClientStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write(buf),
            Self::Unix(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.flush(),
            Self::Unix(stream) => stream.flush(),
        }
    }
}

/// Serve one accepted stream: every failure becomes an HTTP status, never a dropped connection.
fn handle_connection(registry: &WorldRegistry, mut client: ClientStream) {
    if let Err(refusal) = dispatch(registry, &mut client) {
        let _ = refusal.respond(&mut client);
    }
}

fn dispatch(registry: &WorldRegistry, client: &mut ClientStream) -> Result<(), Refusal> {
    let peer = client.peer();
    client.set_timeout(HEAD_READ_TIMEOUT).map_err(|err| {
        Refusal::Internal(format!("cannot bound the request read for {peer}: {err}"))
    })?;

    let head = read_head(client, &peer)?;
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut request = httparse::Request::new(&mut headers);
    let head_len = match request.parse(&head) {
        Ok(httparse::Status::Complete(head_len)) => head_len,
        Ok(httparse::Status::Partial) => {
            return Err(Refusal::BadRequest(format!(
                "request from {peer} ended before its header block did"
            )));
        }
        Err(httparse::Error::TooManyHeaders) => {
            return Err(Refusal::TooLarge(format!(
                "request from {peer} carries more than {MAX_HEADERS} headers"
            )));
        }
        Err(err) => {
            return Err(Refusal::BadRequest(format!(
                "malformed HTTP request from {peer}: {err}"
            )));
        }
    };

    let method = request
        .method
        .ok_or_else(|| Refusal::BadRequest(format!("request from {peer} has no method")))?;
    let path = request
        .path
        .ok_or_else(|| Refusal::BadRequest(format!("request from {peer} has no target path")))?;
    if !ALLOWED_METHODS.contains(&method) {
        return Err(Refusal::MethodNotAllowed(format!(
            "method '{method}' from {peer} is not dispatched by slipforkd"
        )));
    }
    let host = host_header(&request, &peer)?;
    let routed = route_request(registry, &host, path)?;

    let mut upstream = registry
        .dial(&routed)
        .map_err(|err| Refusal::BadGateway(err.to_string()))?;
    let relay_timeout = |stream: &TcpStream| -> io::Result<()> {
        stream.set_read_timeout(Some(RELAY_TIMEOUT))?;
        stream.set_write_timeout(Some(RELAY_TIMEOUT))
    };
    relay_timeout(&upstream)
        .map_err(|err| Refusal::Internal(format!("cannot bound the upstream relay: {err}")))?;
    client
        .set_timeout(RELAY_TIMEOUT)
        .map_err(|err| Refusal::Internal(format!("cannot bound the client relay: {err}")))?;

    write_upstream_request(&mut upstream, method, path, &request).map_err(|err| {
        Refusal::Internal(format!(
            "cannot forward the request to {}: {err}",
            routed.world_id()
        ))
    })?;
    upstream
        .write_all(&head[head_len..])
        .map_err(|err| Refusal::Internal(format!("cannot forward the request body: {err}")))?;

    relay(client, upstream)
}

/// Resolve `Host` + `path`, mapping routing failures to their HTTP status.
fn route_request(registry: &WorldRegistry, host: &str, path: &str) -> Result<RoutedWorld, Refusal> {
    match registry.route(host, path) {
        Ok(routed) => Ok(routed),
        Err(EdgeError::UnknownHost { host }) => Err(Refusal::NotFound(format!(
            "no world is attached for host '{host}'"
        ))),
        Err(EdgeError::UnknownIngressPath { world, host, path }) => Err(Refusal::NotFound(
            format!("world {world} has no ingress route for '{path}' (host '{host}')"),
        )),
        Err(err) => Err(Refusal::Internal(err.to_string())),
    }
}

/// Read the request head, stopping at the first `CRLF CRLF` or the [`HEADER_CAP`] ceiling.
fn read_head(client: &mut ClientStream, peer: &str) -> Result<Vec<u8>, Refusal> {
    let mut head = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    loop {
        if head_end(&head).is_some() {
            return Ok(head);
        }
        if head.len() >= HEADER_CAP {
            return Err(Refusal::TooLarge(format!(
                "request from {peer} exceeded the {HEADER_CAP}-byte header cap"
            )));
        }
        let want = (HEADER_CAP - head.len()).min(chunk.len());
        match client.read(&mut chunk[..want]) {
            Ok(0) => {
                return Err(Refusal::BadRequest(format!(
                    "connection from {peer} closed before a complete HTTP request head"
                )));
            }
            Ok(read) => head.extend_from_slice(&chunk[..read]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(Refusal::Timeout(format!(
                    "request from {peer} did not arrive within {HEAD_READ_TIMEOUT:?}"
                )));
            }
            Err(err) => {
                return Err(Refusal::BadRequest(format!(
                    "cannot read the request from {peer}: {err}"
                )));
            }
        }
    }
}

/// `Some(head_end)` when `buf` already contains the `CRLF CRLF` terminator.
fn head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

/// Extract the single `Host` header (`400` when it is missing or repeated).
fn host_header(request: &httparse::Request<'_, '_>, peer: &str) -> Result<String, Refusal> {
    let mut host: Option<String> = None;
    for header in request.headers.iter() {
        if !header.name.eq_ignore_ascii_case("host") {
            continue;
        }
        if host.is_some() {
            return Err(Refusal::BadRequest(format!(
                "request from {peer} carries more than one Host header"
            )));
        }
        let value = std::str::from_utf8(header.value).map_err(|_| {
            Refusal::BadRequest(format!("request from {peer} has a non-UTF-8 Host header"))
        })?;
        if value.trim().is_empty() {
            return Err(Refusal::BadRequest(format!(
                "request from {peer} has an empty Host header"
            )));
        }
        host = Some(value.trim().to_owned());
    }
    host.ok_or_else(|| Refusal::BadRequest(format!("request from {peer} has no Host header")))
}

/// Rewrite the request head for one backend exchange: hop-by-hop headers dropped, `Host` kept,
/// `Connection: close` forced so the backend ends the stream after its response.
fn write_upstream_request(
    upstream: &mut TcpStream,
    method: &str,
    path: &str,
    request: &httparse::Request<'_, '_>,
) -> io::Result<()> {
    let mut out = Vec::with_capacity(512);
    out.extend_from_slice(method.as_bytes());
    out.push(b' ');
    out.extend_from_slice(path.as_bytes());
    out.extend_from_slice(b" HTTP/1.1\r\n");
    for header in request.headers.iter() {
        if CONNECTION_HEADERS
            .iter()
            .any(|name| header.name.eq_ignore_ascii_case(name))
        {
            continue;
        }
        out.extend_from_slice(header.name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(header.value);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"Connection: close\r\n\r\n");
    upstream.write_all(&out)
}

/// Relay the backend response to the client and the remaining client bytes to the backend.
fn relay(client: &mut ClientStream, upstream: TcpStream) -> Result<(), Refusal> {
    let mut upstream_read = upstream
        .try_clone()
        .map_err(|err| Refusal::Internal(format!("cannot clone the upstream stream: {err}")))?;
    let upstream_control = upstream
        .try_clone()
        .map_err(|err| Refusal::Internal(format!("cannot clone the upstream stream: {err}")))?;
    let mut client_read = client
        .try_clone()
        .map_err(|err| Refusal::Internal(format!("cannot clone the client stream: {err}")))?;
    let client_control = client
        .try_clone()
        .map_err(|err| Refusal::Internal(format!("cannot clone the client stream: {err}")))?;

    let mut upstream_writer = upstream;
    let uplink = thread::spawn(move || {
        copy(&mut client_read, &mut upstream_writer);
        let _ = upstream_writer.shutdown(Shutdown::Write);
    });
    copy(&mut upstream_read, client);
    client.shutdown_write();
    // Neither direction may outlive the exchange: close both sockets so the uplink thread's
    // pending read/write returns and the connection's threads are joined.
    client_control.shutdown_both();
    let _ = upstream_control.shutdown(Shutdown::Both);
    let _ = uplink.join();
    Ok(())
}

/// Copy until EOF or an error, flushing after every chunk.
fn copy<R: Read, W: Write>(reader: &mut R, writer: &mut W) {
    let mut buffer = [0_u8; RELAY_CAP];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                if writer.write_all(&buffer[..read]).is_err() || writer.flush().is_err() {
                    return;
                }
            }
        }
    }
}

/// One HTTP-level refusal, rendered to the client without leaking daemon internals.
enum Refusal {
    BadRequest(String),
    NotFound(String),
    MethodNotAllowed(String),
    Timeout(String),
    TooLarge(String),
    BadGateway(String),
    Internal(String),
}

impl Refusal {
    fn status(&self) -> (u16, &'static str) {
        match self {
            Self::BadRequest(_) => (400, "Bad Request"),
            Self::NotFound(_) => (404, "Not Found"),
            Self::MethodNotAllowed(_) => (405, "Method Not Allowed"),
            Self::Timeout(_) => (408, "Request Timeout"),
            Self::TooLarge(_) => (431, "Request Header Fields Too Large"),
            Self::BadGateway(_) => (502, "Bad Gateway"),
            Self::Internal(_) => (500, "Internal Server Error"),
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::BadRequest(message)
            | Self::NotFound(message)
            | Self::MethodNotAllowed(message)
            | Self::Timeout(message)
            | Self::TooLarge(message)
            | Self::BadGateway(message)
            | Self::Internal(message) => message,
        }
    }

    fn respond(self, client: &mut ClientStream) -> io::Result<()> {
        let (status, reason) = self.status();
        let body = format!("{reason}: {}\n", self.message());
        let allow = if matches!(self, Self::MethodNotAllowed(_)) {
            format!("Allow: {}\r\n", ALLOWED_METHODS.join(", "))
        } else {
            String::new()
        };
        let head = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n{allow}Connection: close\r\n\r\n{body}",
            body.len()
        );
        client.write_all(head.as_bytes())?;
        client.flush()
    }
}

/// Replace a stale socket file, but never steal one a live daemon still answers on.
fn replace_stale_socket(path: &Path) -> Result<(), EdgeError> {
    if !path.exists() {
        return Ok(());
    }
    if UnixStream::connect(path).is_ok() {
        return Err(EdgeError::IngressSocketInUse {
            path: path.display().to_string(),
        });
    }
    fs::remove_file(path).map_err(|err| EdgeError::IngressSocket {
        path: path.display().to_string(),
        message: format!("cannot remove the stale socket: {err}"),
    })
}
