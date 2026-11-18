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

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::EdgeError;
use crate::activation::ListenerSet;
use crate::registry::WorldRegistry;
use crate::unix_socket::{SocketRole, bind_private};
use slipfork_engine::relay::{Duplex, pipe};

/// Largest HTTP request head (request line + headers) the ingress accepts.
pub const HEADER_CAP: usize = 16 * 1024;

/// Lets the world-init bridge inspect the accepted socket file through `sun_path`.
const HTTP_INGRESS_SOCKET_FILE: &str = "http-ingress.sock";

/// Bound on reading one request head, so a stalled client cannot pin a thread forever.
const HEAD_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on one relay read, so an idle proxied connection is still reclaimed.
const RELAY_TIMEOUT: Duration = Duration::from_secs(60);

/// Largest number of headers `httparse` may collect from one request head.
const MAX_HEADERS: usize = 64;

/// HTTP verbs the ingress dispatches. Everything else is `405 Method Not Allowed`.
const ALLOWED_METHODS: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

/// Hop-by-hop request headers the ingress replaces with its own `Connection` framing.
const CONNECTION_HEADERS: &[&str] = &["connection", "keep-alive", "proxy-connection"];

/// Existing `X-Forwarded-For` values this hop appends to; host and proto are restated per hop.
const FORWARDED_FOR: &str = "x-forwarded-for";

/// The `slipforkd` HTTP/WS ingress: every TCP listener plus `<state_dir>/http-ingress.sock`.
pub struct HttpIngress {
    registry: Arc<WorldRegistry>,
    http_listeners: Vec<TcpListener>,
    forward_listener: Option<TcpListener>,
    unix_listener: std::os::unix::net::UnixListener,
    unix_path: PathBuf,
    relay_timeout: Duration,
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
    /// [`EdgeError::SocketInUse`] rather than silently stolen.
    pub fn bind(
        state_dir: &Path,
        listeners: ListenerSet,
        registry: Arc<WorldRegistry>,
    ) -> Result<Self, EdgeError> {
        let unix_path = state_dir.join(HTTP_INGRESS_SOCKET_FILE);
        let unix_listener = bind_private(&unix_path, SocketRole::HttpIngress)?;

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
            relay_timeout: RELAY_TIMEOUT,
        })
    }

    /// Override the idle bound armed on a proxied connection (60 s by default).
    ///
    /// A successful `101 Switching Protocols` clears the bound for that connection, so an idle
    /// WebSocket is never torn down; this setting bounds everything else (and the tests).
    pub fn with_relay_timeout(mut self, timeout: Duration) -> Self {
        self.relay_timeout = timeout;
        self
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
        let relay_timeout = self.relay_timeout;
        // One monitor channel so a failure on *any* listener (not only the first joined one)
        // surfaces immediately instead of being masked by a healthy accept loop.
        let (failures, failure) = std::sync::mpsc::channel();
        for listener in self.http_listeners.into_iter().chain(self.forward_listener) {
            spawn_accept_loop(
                IngressListener::Tcp(listener),
                Arc::clone(&registry),
                relay_timeout,
                &failures,
            );
        }
        spawn_accept_loop(
            IngressListener::Unix(self.unix_listener, self.unix_path.display().to_string()),
            registry,
            relay_timeout,
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
    relay_timeout: Duration,
    failures: &std::sync::mpsc::Sender<Result<(), EdgeError>>,
) {
    let failures = failures.clone();
    thread::spawn(move || {
        let outcome = accept_loop(listener, &registry, relay_timeout);
        let _ = failures.send(outcome);
    });
}

/// Accept connections from one TCP listener or the `AF_UNIX` ingress socket.
enum IngressListener {
    Tcp(TcpListener),
    Unix(std::os::unix::net::UnixListener, String),
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

fn accept_loop(
    listener: IngressListener,
    registry: &Arc<WorldRegistry>,
    relay_timeout: Duration,
) -> Result<(), EdgeError> {
    let label = listener.label();
    loop {
        match listener.accept() {
            Ok(client) => {
                let registry = Arc::clone(registry);
                thread::spawn(move || handle_connection(&registry, client, relay_timeout));
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
    fn peer(&self) -> String {
        match self {
            Self::Tcp(stream) => stream
                .peer_addr()
                .map_or_else(|_| "tcp peer".to_owned(), |addr| addr.to_string()),
            Self::Unix(_) => HTTP_INGRESS_SOCKET_FILE.to_owned(),
        }
    }

    /// Address reported in `X-Forwarded-For`.
    ///
    /// An `http-ingress.sock` request originates from the in-world `$BASE_URL` bridge, which dials
    /// the world's own loopback, so `127.0.0.1` is the honest value there.
    fn forwarded_for(&self) -> String {
        match self {
            Self::Tcp(stream) => stream
                .peer_addr()
                .map_or_else(|_| "127.0.0.1".to_owned(), |addr| addr.ip().to_string()),
            Self::Unix(_) => "127.0.0.1".to_owned(),
        }
    }
}

impl Duplex for ClientStream {
    fn try_clone_duplex(&self) -> io::Result<Self> {
        match self {
            Self::Tcp(stream) => stream.try_clone().map(Self::Tcp),
            Self::Unix(stream) => stream.try_clone().map(Self::Unix),
        }
    }

    fn shutdown_write_half(&self) {
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

    fn set_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => {
                stream.set_read_timeout(timeout)?;
                stream.set_write_timeout(timeout)
            }
            Self::Unix(stream) => {
                stream.set_read_timeout(timeout)?;
                stream.set_write_timeout(timeout)
            }
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
fn handle_connection(registry: &WorldRegistry, mut client: ClientStream, relay_timeout: Duration) {
    if let Err(refusal) = dispatch(registry, &mut client, relay_timeout) {
        let _ = refusal.respond(&mut client);
    }
}

fn dispatch(
    registry: &WorldRegistry,
    client: &mut ClientStream,
    relay_timeout: Duration,
) -> Result<(), Refusal> {
    let peer = client.peer();
    client.set_timeout(Some(HEAD_READ_TIMEOUT)).map_err(|err| {
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
    let host = host_header(&request, &peer)?;
    let upgrade = is_upgrade(&request);

    // The routing decision comes before the method allowlist, so an unroutable host is never
    // dispatched against a world backend.
    let routed = registry
        .resolve(&host, path)
        .map_err(|err| route_refusal(&host, err))?;
    if !ALLOWED_METHODS.contains(&method) {
        return Err(Refusal::MethodNotAllowed(format!(
            "method '{method}' from {peer} is not dispatched by slipforkd"
        )));
    }

    let mut upstream = registry
        .dial(&routed)
        .map_err(|err| Refusal::BadGateway(err.to_string()))?;
    upstream
        .set_timeout(Some(relay_timeout))
        .map_err(|err| Refusal::Internal(format!("cannot bound the upstream relay: {err}")))?;
    client
        .set_timeout(Some(relay_timeout))
        .map_err(|err| Refusal::Internal(format!("cannot bound the client relay: {err}")))?;

    let forwarded_for = client.forwarded_for();
    write_upstream_request(
        &mut upstream,
        method,
        path,
        &request,
        &host,
        &forwarded_for,
        upgrade,
    )
    .map_err(|err| {
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

/// Map one routing failure to its HTTP status.
fn route_refusal(host: &str, err: EdgeError) -> Refusal {
    match err {
        EdgeError::UnknownHost { host } => {
            Refusal::NotFound(format!("no world is attached for host '{host}'"))
        }
        EdgeError::UnknownIngressPath { world, host, path } => Refusal::NotFound(format!(
            "world {world} has no ingress route for '{path}' (host '{host}')"
        )),
        other => Refusal::Internal(format!("cannot route host '{host}': {other}")),
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

/// `true` when the request asks for an HTTP `Upgrade` (`Connection` carries the `upgrade` token).
fn is_upgrade(request: &httparse::Request<'_, '_>) -> bool {
    request.headers.iter().any(|header| {
        header.name.eq_ignore_ascii_case("connection") && header_has_token(header.value, "upgrade")
    })
}

/// The single client-supplied `X-Forwarded-For` value this hop appends to, if any.
fn existing_forwarded_for(request: &httparse::Request<'_, '_>) -> Option<String> {
    request
        .headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(FORWARDED_FOR))
        .and_then(|header| std::str::from_utf8(header.value).ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// `true` when a comma-separated header value carries `token` (ASCII case-insensitive).
fn header_has_token(value: &[u8], token: &str) -> bool {
    std::str::from_utf8(value).is_ok_and(|value| {
        value
            .split(',')
            .any(|candidate| candidate.trim().eq_ignore_ascii_case(token))
    })
}

/// Rewrite the request head for one backend exchange.
///
/// Hop-by-hop headers the ingress owns are dropped, `Host` is preserved verbatim,
/// `X-Forwarded-Host`/`X-Forwarded-Proto` are restated from this hop, and the peer is appended to
/// any incoming `X-Forwarded-For` chain. A plain exchange forces `Connection: close` so the backend
/// ends the stream after its response; an `Upgrade` exchange preserves the `Connection: Upgrade`
/// negotiation instead (`ADR-0005` §1, WebSocket proxying).
fn write_upstream_request(
    upstream: &mut TcpStream,
    method: &str,
    path: &str,
    request: &httparse::Request<'_, '_>,
    host: &str,
    forwarded_for: &str,
    upgrade: bool,
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
        if header.name.eq_ignore_ascii_case(FORWARDED_FOR)
            || header.name.eq_ignore_ascii_case("x-forwarded-host")
            || header.name.eq_ignore_ascii_case("x-forwarded-proto")
        {
            continue;
        }
        out.extend_from_slice(header.name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(header.value);
        out.extend_from_slice(b"\r\n");
    }
    if upgrade {
        out.extend_from_slice(b"Connection: Upgrade\r\n");
    } else {
        out.extend_from_slice(b"Connection: close\r\n");
    }
    out.extend_from_slice(b"X-Forwarded-Host: ");
    out.extend_from_slice(host.as_bytes());
    out.extend_from_slice(b"\r\nX-Forwarded-For: ");
    if let Some(existing) = existing_forwarded_for(request) {
        out.extend_from_slice(existing.as_bytes());
        out.extend_from_slice(b", ");
    }
    out.extend_from_slice(forwarded_for.as_bytes());
    out.extend_from_slice(b"\r\nX-Forwarded-Proto: http\r\n\r\n");
    upstream.write_all(&out)
}

/// Relay the backend response to the client and the remaining client bytes to the backend.
///
/// The response head is read first so a successful `101 Switching Protocols` clears the idle bound
/// on both sockets: an upgraded WebSocket legitimately stays silent for minutes, while every other
/// exchange keeps the default 60 s idle bound.
fn relay(client: &mut ClientStream, mut upstream: TcpStream) -> Result<(), Refusal> {
    let Some(head) = read_response_head(&mut upstream) else {
        return Ok(());
    };
    client
        .write_all(&head)
        .and_then(|()| client.flush())
        .map_err(|err| Refusal::Internal(format!("cannot relay the response head: {err}")))?;
    if response_is_switching(&head) {
        client.set_timeout(None).map_err(|err| {
            Refusal::Internal(format!("cannot clear the client idle bound: {err}"))
        })?;
        upstream.set_timeout(None).map_err(|err| {
            Refusal::Internal(format!("cannot clear the upstream idle bound: {err}"))
        })?;
    }
    pipe(client, &mut upstream)
        .map_err(|err| Refusal::Internal(format!("cannot relay the response: {err}")))
}

/// Read the backend response head (bounded by [`HEADER_CAP`]); `None` when the backend sent nothing.
///
/// Any bytes already read past the head are kept in the returned buffer and relayed verbatim.
fn read_response_head(upstream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut head = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    loop {
        if head_end(&head).is_some() || head.len() >= HEADER_CAP {
            return Some(head);
        }
        let want = (HEADER_CAP - head.len()).min(chunk.len());
        match upstream.read(&mut chunk[..want]) {
            Ok(0) | Err(_) => {
                return if head.is_empty() { None } else { Some(head) };
            }
            Ok(read) => head.extend_from_slice(&chunk[..read]),
        }
    }
}

/// `true` when the backend switched protocols (`101`), which permits an unbounded idle connection.
fn response_is_switching(head: &[u8]) -> bool {
    let line_end = head
        .windows(2)
        .position(|window| window == b"\r\n")
        .unwrap_or(head.len());
    let status_line = &head[..line_end];
    status_line.starts_with(b"HTTP/1.1 101") || status_line.starts_with(b"HTTP/1.0 101")
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
