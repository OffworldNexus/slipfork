//! Streaming HTTP/1.1 and WebSocket relay for one accepted `.localhost` connection.
//!
//! Heads are parsed with `httparse` under a hard [`HEADER_CAP`]; bodies and tunnelled WebSocket
//! bytes move through fixed [`RELAY_CAP`] buffers. Routing is delegated to
//! [`crate::route::IngressRegistry`], and the upstream connection is dialed by the injected
//! closure from the full [`crate::route::RouteMatch`] (so the caller can select the world's
//! namespace, not just a port). This module never binds, resolves, or reads ambient state.

use std::io::{self, BufRead, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::route::{IngressRegistry, RouteError, RouteMatch};

mod body;
mod exchange;
mod framing;
mod tunnel;

pub(crate) use exchange::Exchange;

/// Largest HTTP head (request or response) this proxy buffers.
pub const HEADER_CAP: usize = 16 * 1024;

/// Fixed buffer size for every relayed body byte and tunnelled WebSocket byte.
pub const RELAY_CAP: usize = 64 * 1024;

/// Largest number of headers `httparse` may collect from one head.
pub(crate) const MAX_HEADERS: usize = 64;

/// Size of the buffered read window used for chunked framing and body copies.
pub(crate) const READ_BUFFER: usize = 8 * 1024;

/// Bound on reading one request head so a stalled client cannot pin a connection.
pub(crate) const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on one relayed body chunk or idle upstream exchange.
pub(crate) const RELAY_TIMEOUT: Duration = Duration::from_secs(60);

/// Most client bytes drained before a refusal closes the connection.
const DRAIN_CAP: usize = 64 * 1024;

/// A proxy refusal, rendered to the client unless the response head was already committed.
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    /// The client died or reset while its head was being read.
    #[error("cannot read the request head: {0}")]
    ReadHead(#[source] io::Error),
    /// The client stalled past [`HEAD_TIMEOUT`].
    #[error("the request head did not arrive within {0:?}")]
    HeadTimeout(Duration),
    /// The head exceeded [`HEADER_CAP`].
    #[error("the request head exceeded the {HEADER_CAP}-byte cap")]
    HeadTooLarge,
    /// `httparse` rejected the head.
    #[error("malformed HTTP: {0}")]
    Malformed(#[from] httparse::Error),
    /// The peer closed before the head terminator arrived.
    #[error("the connection closed before a complete HTTP request head")]
    IncompleteHead,
    /// Routing needs one authority.
    #[error("the request carries no Host header")]
    MissingHost,
    /// Two `Host` headers would make routing non-deterministic.
    #[error("the request carries more than one Host header")]
    DuplicateHost,
    /// Two `Content-Length` headers disagree.
    #[error("the request carries conflicting Content-Length headers")]
    ConflictingLength,
    /// The route table has no match.
    #[error(transparent)]
    Route(#[from] RouteError),
    /// The injected dialer failed.
    #[error("cannot dial upstream port {port}: {source}")]
    Dial {
        /// The routed upstream port.
        port: u16,
        /// The dialer's error.
        #[source]
        source: io::Error,
    },
    /// `Content-Length` was not a decimal length.
    #[error("invalid Content-Length header `{0}`")]
    InvalidLength(String),
    /// The exchange failed after routing.
    #[error("cannot relay the exchange: {0}")]
    Relay(#[source] io::Error),
}

impl ProxyError {
    fn status(&self) -> (u16, &'static str) {
        match self {
            Self::ReadHead(_)
            | Self::IncompleteHead
            | Self::Malformed(_)
            | Self::MissingHost
            | Self::DuplicateHost
            | Self::ConflictingLength
            | Self::InvalidLength(_) => (400, "Bad Request"),
            Self::HeadTimeout(_) => (408, "Request Timeout"),
            Self::HeadTooLarge => (431, "Request Header Fields Too Large"),
            Self::Dial { .. } | Self::Relay(_) => (502, "Bad Gateway"),
            Self::Route(error) => match error {
                RouteError::UnknownHost { .. } => (421, "Misdirected Request"),
                RouteError::UnknownPath { .. } => (404, "Not Found"),
                RouteError::HostOutsideLocalhost { .. }
                | RouteError::NotOriginForm { .. }
                | RouteError::PathTraversal { .. } => (400, "Bad Request"),
            },
        }
    }

    fn respond(&self, connection: &mut Connection) {
        let (status, reason) = self.status();
        let body = if status >= 500 {
            format!("{reason}\n")
        } else {
            format!("{reason}: {self}\n")
        };
        let head = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = connection.write_all(head.as_bytes());
        let _ = connection.write_all(body.as_bytes());
        let _ = connection.flush();
        // Drain the peer's in-flight bytes for a moment before closing: closing a TCP socket with
        // unread input sends an RST and can discard the refusal the client is about to read.
        let _ = connection
            .stream
            .set_read_timeout(Some(Duration::from_millis(50)));
        let mut sink = [0_u8; 4096];
        let mut drained = 0;
        while drained < DRAIN_CAP {
            match connection.read(&mut sink) {
                Ok(0) | Err(_) => break,
                Ok(read) => drained += read,
            }
        }
        let _ = connection.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// What the proxy should do with the client connection after one exchange.
pub(crate) enum Outcome {
    Close,
    KeepAlive,
    /// The upstream switched protocols (`101`); the two sockets are now a raw byte pipe.
    Tunnel(TcpStream),
}

/// Serve one accepted `:80` connection.
///
/// `connect` receives the full [`RouteMatch`] — the caller selects the world's network namespace
/// from `world`/`port`; every other input is injected. Returns when the client or exchange ends.
///
/// # Errors
///
/// Returns a [`ProxyError`] after writing the matching HTTP refusal to the client (skipped when a
/// response head was already committed).
pub fn serve_connection<F>(
    client: TcpStream,
    registry: &IngressRegistry,
    connect: F,
) -> Result<(), ProxyError>
where
    F: Fn(&RouteMatch) -> io::Result<TcpStream>,
{
    let mut connection = Connection::new(client);
    let mut exchange = Exchange::new(registry);
    loop {
        connection
            .stream
            .set_read_timeout(Some(HEAD_TIMEOUT))
            .map_err(ProxyError::ReadHead)?;
        connection
            .stream
            .set_write_timeout(Some(RELAY_TIMEOUT))
            .map_err(ProxyError::Relay)?;
        match exchange.serve(&mut connection, &connect) {
            Ok(Outcome::KeepAlive) => {}
            Ok(Outcome::Close) => return Ok(()),
            Ok(Outcome::Tunnel(upstream)) => {
                return tunnel::tunnel(connection.into_stream(), upstream)
                    .map_err(ProxyError::Relay);
            }
            Err(error) => {
                if !exchange.committed() {
                    error.respond(&mut connection);
                }
                return Err(error);
            }
        }
    }
}

/// An accepted stream plus the bytes already read past the last consumed head.
pub(crate) struct Connection {
    pub(crate) stream: TcpStream,
    pub(crate) peer: String,
    buffer: Vec<u8>,
    offset: usize,
}

impl Connection {
    fn new(stream: TcpStream) -> Self {
        let peer = stream
            .peer_addr()
            .map_or_else(|_| "unknown".to_owned(), |addr| addr.ip().to_string());
        Self {
            stream,
            peer,
            buffer: Vec::new(),
            offset: 0,
        }
    }

    fn into_stream(self) -> TcpStream {
        self.stream
    }

    /// Read one head, leaving any bytes past the terminator buffered for the body copy.
    pub(crate) fn read_head(&mut self) -> Result<Option<Vec<u8>>, ProxyError> {
        let mut head = Vec::with_capacity(1024);
        let mut chunk = [0_u8; 1024];
        loop {
            if let Some(end) = head_end(&head) {
                self.prepend(&head[end..]);
                head.truncate(end);
                return Ok(Some(head));
            }
            if head.len() >= HEADER_CAP {
                return Err(ProxyError::HeadTooLarge);
            }
            let want = (HEADER_CAP - head.len()).min(chunk.len());
            match self.read(&mut chunk[..want]) {
                Ok(0) => {
                    return if head.is_empty() {
                        Ok(None)
                    } else {
                        Err(ProxyError::IncompleteHead)
                    };
                }
                Ok(read) => head.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(ProxyError::HeadTimeout(HEAD_TIMEOUT));
                }
                Err(error) => return Err(ProxyError::ReadHead(error)),
            }
        }
    }

    /// Put unconsumed bytes back in front of whatever is still buffered.
    fn prepend(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut combined = Vec::with_capacity(bytes.len() + self.buffer.len() - self.offset);
        combined.extend_from_slice(bytes);
        combined.extend_from_slice(&self.buffer[self.offset..]);
        self.buffer = combined;
        self.offset = 0;
    }

    /// Take every unconsumed buffered byte.
    pub(crate) fn take_buffered(&mut self) -> Vec<u8> {
        let rest = self.buffer[self.offset..].to_vec();
        self.buffer.clear();
        self.offset = 0;
        rest
    }
}

impl Read for Connection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.offset < self.buffer.len() {
            let available = self.buffer.len() - self.offset;
            let count = available.min(buf.len());
            buf[..count].copy_from_slice(&self.buffer[self.offset..self.offset + count]);
            self.offset += count;
            return Ok(count);
        }
        self.stream.read(buf)
    }
}

impl BufRead for Connection {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.offset >= self.buffer.len() {
            self.buffer.clear();
            self.buffer.resize(READ_BUFFER, 0);
            self.offset = 0;
            let read = self.stream.read(&mut self.buffer)?;
            self.buffer.truncate(read);
        }
        Ok(&self.buffer[self.offset..])
    }

    fn consume(&mut self, amount: usize) {
        self.offset = (self.offset + amount).min(self.buffer.len());
    }
}

impl Write for Connection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// `Some(end)` of the `CRLF CRLF` terminator.
pub(crate) fn head_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}
