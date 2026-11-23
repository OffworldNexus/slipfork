//! One request/response exchange. The structure owns the `committed` flag, so a refusal can never
//! be written after a response head has started.

use std::io::{self, Write};
use std::net::TcpStream;

use super::body::copy_body;
use super::framing::{
    Framing, header, header_has_token, host_header, is_hop_by_hop, request_framing,
    request_keep_alive, response_framing,
};
use super::{Connection, MAX_HEADERS, Outcome, ProxyError, RELAY_TIMEOUT};
use crate::route::{IngressRegistry, RouteMatch};

/// One proxied connection's request/response state.
pub(crate) struct Exchange<'r> {
    registry: &'r IngressRegistry,
    committed: bool,
}

impl<'r> Exchange<'r> {
    pub(crate) const fn new(registry: &'r IngressRegistry) -> Self {
        Self {
            registry,
            committed: false,
        }
    }

    /// `true` once a response head has been written to the client.
    pub(crate) const fn committed(&self) -> bool {
        self.committed
    }

    /// Run one exchange on an already-buffered client connection.
    pub(crate) fn serve<F>(
        &mut self,
        client: &mut Connection,
        connect: &F,
    ) -> Result<Outcome, ProxyError>
    where
        F: Fn(&RouteMatch) -> io::Result<TcpStream>,
    {
        self.committed = false;
        let Some(head) = client.read_head()? else {
            return Ok(Outcome::Close);
        };
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut request = httparse::Request::new(&mut headers);
        let head_len = match request.parse(&head)? {
            httparse::Status::Complete(head_len) => head_len,
            httparse::Status::Partial => return Err(ProxyError::IncompleteHead),
        };
        let method = request.method.unwrap_or("GET");
        let target = request.path.ok_or(ProxyError::IncompleteHead)?;
        let host = host_header(&request)?;
        let routed = self.registry.route(&host, target)?;
        let framing = request_framing(&request)?;
        let upgrade = header_has_token(request.headers, "connection", "upgrade");
        let keep_alive = request_keep_alive(&request);
        let peer = client.peer.clone();

        if header_has_token(request.headers, "expect", "100-continue") {
            client
                .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .map_err(ProxyError::Relay)?;
            client.flush().map_err(ProxyError::Relay)?;
        }

        let stream = connect(&routed).map_err(|source| ProxyError::Dial {
            port: routed.port,
            source,
        })?;
        stream
            .set_read_timeout(Some(RELAY_TIMEOUT))
            .map_err(ProxyError::Relay)?;
        stream
            .set_write_timeout(Some(RELAY_TIMEOUT))
            .map_err(ProxyError::Relay)?;
        let mut upstream = Connection::new(stream);

        ForwardedRequest {
            method,
            target: &routed.target,
            host: &host,
            forwarded_for: &peer,
            request: &request,
            upgrade,
            framing,
        }
        .write_to(&mut upstream)
        .map_err(ProxyError::Relay)?;
        upstream
            .write_all(&head[head_len..])
            .map_err(ProxyError::Relay)?;
        copy_body(client, &mut upstream, framing).map_err(ProxyError::Relay)?;
        upstream.flush().map_err(ProxyError::Relay)?;

        let Some(response_head) = upstream.read_head()? else {
            return Err(ProxyError::Relay(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "upstream closed before sending a response head",
            )));
        };
        let mut response_headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut response = httparse::Response::new(&mut response_headers);
        match response.parse(&response_head)? {
            httparse::Status::Complete(_) => {}
            httparse::Status::Partial => {
                return Err(ProxyError::Relay(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "upstream response head ended early",
                )));
            }
        }
        let status = response.code.ok_or_else(|| {
            ProxyError::Relay(io::Error::new(
                io::ErrorKind::InvalidData,
                "upstream response has no status code",
            ))
        })?;
        let reason = response.reason.unwrap_or("Unknown");
        let response_framing = response_framing(status, method, &response)?;
        let tunnelled = status == 101;
        let stay_alive = keep_alive && response_framing != Framing::UntilEof && !tunnelled;

        let mut reply = Vec::with_capacity(response_head.len() + 128);
        write!(reply, "HTTP/1.1 {status} {reason}\r\n").map_err(ProxyError::Relay)?;
        for header in response.headers.iter() {
            if is_hop_by_hop(header.name) {
                continue;
            }
            reply.extend_from_slice(header.name.as_bytes());
            reply.extend_from_slice(b": ");
            reply.extend_from_slice(header.value);
            reply.extend_from_slice(b"\r\n");
        }
        if tunnelled {
            reply.extend_from_slice(b"Connection: Upgrade\r\n\r\n");
        } else if stay_alive {
            reply.extend_from_slice(b"Connection: keep-alive\r\n\r\n");
        } else {
            reply.extend_from_slice(b"Connection: close\r\n\r\n");
        }
        client.write_all(&reply).map_err(ProxyError::Relay)?;
        client.flush().map_err(ProxyError::Relay)?;
        self.committed = true;

        if tunnelled {
            let early_upstream = upstream.take_buffered();
            if !early_upstream.is_empty() {
                client
                    .write_all(&early_upstream)
                    .map_err(ProxyError::Relay)?;
            }
            let early_client = client.take_buffered();
            if !early_client.is_empty() {
                upstream
                    .write_all(&early_client)
                    .map_err(ProxyError::Relay)?;
            }
            return Ok(Outcome::Tunnel(upstream.into_stream()));
        }

        copy_body(&mut upstream, client, response_framing).map_err(ProxyError::Relay)?;
        client.flush().map_err(ProxyError::Relay)?;
        Ok(if stay_alive {
            Outcome::KeepAlive
        } else {
            Outcome::Close
        })
    }
}

/// The request head rewritten for one backend exchange.
struct ForwardedRequest<'a, 'h, 'b> {
    method: &'a str,
    target: &'a str,
    host: &'a str,
    forwarded_for: &'a str,
    request: &'a httparse::Request<'h, 'b>,
    upgrade: bool,
    framing: Framing,
}

impl ForwardedRequest<'_, '_, '_> {
    fn write_to(&self, upstream: &mut Connection) -> io::Result<()> {
        let mut head = Vec::with_capacity(512);
        head.extend_from_slice(self.method.as_bytes());
        head.push(b' ');
        head.extend_from_slice(self.target.as_bytes());
        head.extend_from_slice(b" HTTP/1.1\r\n");
        for header in self.request.headers.iter() {
            if is_hop_by_hop(header.name)
                || header.name.eq_ignore_ascii_case("content-length")
                || header.name.eq_ignore_ascii_case("expect")
                || header.name.eq_ignore_ascii_case("x-forwarded-for")
                || header.name.eq_ignore_ascii_case("x-forwarded-host")
                || header.name.eq_ignore_ascii_case("x-forwarded-proto")
            {
                continue;
            }
            head.extend_from_slice(header.name.as_bytes());
            head.extend_from_slice(b": ");
            head.extend_from_slice(header.value);
            head.extend_from_slice(b"\r\n");
        }
        if let Framing::Length(length) = self.framing {
            head.extend_from_slice(format!("Content-Length: {length}\r\n").as_bytes());
        }
        if self.upgrade {
            head.extend_from_slice(b"Connection: Upgrade\r\n");
        } else {
            head.extend_from_slice(b"Connection: close\r\n");
        }
        head.extend_from_slice(b"X-Forwarded-Host: ");
        head.extend_from_slice(self.host.as_bytes());
        head.extend_from_slice(b"\r\nX-Forwarded-For: ");
        if let Some(existing) = header(self.request.headers, "x-forwarded-for")
            && let Ok(existing) = std::str::from_utf8(existing)
            && !existing.trim().is_empty()
        {
            head.extend_from_slice(existing.trim().as_bytes());
            head.extend_from_slice(b", ");
        }
        head.extend_from_slice(self.forwarded_for.as_bytes());
        head.extend_from_slice(b"\r\nX-Forwarded-Proto: http\r\n\r\n");
        upstream.write_all(&head)
    }
}
