//! Lab-first ingress journeys: real loopback listeners, a real upstream, and a real descriptor
//! handoff. No mocks, no in-memory streams, no fake clocks.

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use slipfork_forkfile::{ImageItem, Position, Route, RouteKind, ServiceItem, Span, TopItem};
use slipfork_ingress::{
    IngressHandoff, IngressRegistry, IngressRoute, IngressTable, recv_fd, send_fd, serve_connection,
};

const TIMEOUT: Duration = Duration::from_secs(20);

const BASE: &str = r#"# forkfile: 0.1
domain main
domain mail
domain assets-cdn

image backend . {
  service web { port 9006
    route /
  }
  service api { port 9000
    route /api
    route /healthz exact /healthz
  }
  service legacy { port 9001
    route /old rewrite /v1
  }
  service files { port 9002
    route /files strip-prefix
  }
  service portal { port 9005
    route /portal domain main
  }
  service mailer { port 9003
    route / domain mail
  }
  service cdn { port 9004
    route /cdn-assets domain assets-cdn
  }
}
"#;

const FORK: &str = r#"# forkfile: 0.1
image backend . {
  service worker { port 9010
    route /
  }
}
"#;

const STRICT: &str = r#"# forkfile: 0.1
image backend . {
  service api { port 9000
    route /api
  }
}
"#;

fn span() -> Span {
    Span::at(Position::new(1, 1, 0))
}

fn catch_all(port: u16) -> IngressRoute {
    IngressRoute::new(
        port,
        Route {
            path: Some("/".to_owned()),
            kind: RouteKind::Preserve,
            domain: None,
            span: span(),
        },
    )
}

/// Collect the real parsed routes of every top-level service.
fn routes(source: &str) -> Vec<IngressRoute> {
    let document = slipfork_forkfile::parse(source).expect("test Forkfile parses");
    let mut routes = Vec::new();
    for item in &document.items.items {
        let TopItem::Image(image) = item else {
            continue;
        };
        for item in &image.body.items {
            let ImageItem::Service(service) = item else {
                continue;
            };
            let port = service
                .body
                .items
                .iter()
                .find_map(|item| match item {
                    ServiceItem::Port(port) => Some(*port),
                    _ => None,
                })
                .expect("service declares `port`");
            for item in &service.body.items {
                if let ServiceItem::Route(route) = item {
                    routes.push(IngressRoute::new(port, route.clone()));
                }
            }
        }
    }
    routes
}

fn retarget(routes: Vec<IngressRoute>, port: u16) -> Vec<IngressRoute> {
    retarget_with(routes, |_| port)
}

fn retarget_with(routes: Vec<IngressRoute>, port_for: impl Fn(u16) -> u16) -> Vec<IngressRoute> {
    routes
        .into_iter()
        .map(|route| IngressRoute::new(port_for(route.port), route.route))
        .collect()
}

fn registry_with_port(port: u16) -> IngressRegistry {
    IngressRegistry::new([
        IngressTable::new(
            "demo",
            Some("main"),
            vec![
                "main".to_owned(),
                "mail".to_owned(),
                "assets-cdn".to_owned(),
            ],
            retarget(routes(BASE), port),
        )
        .expect("demo table is routable"),
        IngressTable::new(
            "feat-auth--demo",
            None,
            Vec::new(),
            retarget(routes(FORK), port),
        )
        .expect("fork table is routable"),
        IngressTable::new("strict", None, Vec::new(), retarget(routes(STRICT), port))
            .expect("strict table is routable"),
    ])
    .expect("registry is routable")
}

fn single_world(world: &str, port: u16) -> IngressRegistry {
    IngressRegistry::new([
        IngressTable::new(world, None, Vec::new(), [catch_all(port)]).expect("table is routable"),
    ])
    .expect("registry is routable")
}

fn spawn_proxy(registry: IngressRegistry, connections: usize) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("proxy binds loopback");
    let address = listener.local_addr().expect("proxy address");
    let handle = thread::spawn(move || {
        for stream in listener.incoming().take(connections) {
            let stream = stream.expect("proxy accepts");
            // A routed refusal still writes its HTTP status and returns an error to log; the accept
            // loop itself must keep serving.
            let _ = serve_connection(stream, &registry, |routed| {
                TcpStream::connect(("127.0.0.1", routed.port))
            });
        }
    });
    (address, handle)
}

fn echo_upstream_n(count: usize) -> (u16, Receiver<String>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("upstream binds loopback");
    let port = listener.local_addr().expect("upstream address").port();
    let (seen, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        for _ in 0..count {
            let (mut stream, _) = listener.accept().expect("upstream accepts");
            stream.set_read_timeout(Some(TIMEOUT)).expect("timeout");
            let (head, _body) = read_head(&mut stream);
            seen.send(request_target(&head)).expect("report target");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nX-Upstream-Port: 1\r\n\
                      Connection: close\r\n\r\nrouted",
                )
                .expect("upstream replies");
        }
    });
    (port, receiver, handle)
}

fn echo_upstream() -> (u16, Receiver<String>, JoinHandle<()>) {
    echo_upstream_n(1)
}

/// A one-shot upstream that answers with its own name, so a journey can name who replied.
fn echo_upstream_body(body: &'static str) -> (u16, Receiver<String>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("upstream binds loopback");
    let port = listener.local_addr().expect("upstream address").port();
    let (seen, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("upstream accepts");
        stream.set_read_timeout(Some(TIMEOUT)).expect("timeout");
        let (head, _body) = read_head(&mut stream);
        seen.send(request_target(&head)).expect("report target");
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("upstream replies");
    });
    (port, receiver, handle)
}

fn connect_client(address: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(address).expect("client connects");
    stream.set_read_timeout(Some(TIMEOUT)).expect("timeout");
    stream
}

/// Send one origin-form request and read the whole response.
fn request(address: SocketAddr, host: &str, target: &str) -> String {
    let mut stream = connect_client(address);
    write!(
        stream,
        "GET {target} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .expect("client writes the request");
    stream.flush().expect("client flushes");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("client reads the response");
    response
}

fn read_head(stream: &mut TcpStream) -> (Vec<u8>, Vec<u8>) {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        if let Some(end) = buffer
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
        {
            let body = buffer.split_off(end);
            return (buffer, body);
        }
        let read = stream.read(&mut chunk).expect("read head");
        assert!(read > 0, "stream closed before a head arrived");
        buffer.extend_from_slice(&chunk[..read]);
    }
}

/// Read one response on a connection that stays open.
fn read_response(stream: &mut TcpStream) -> (String, Vec<u8>) {
    let (head, mut body) = read_head(stream);
    let length = content_length(&head);
    while body.len() < length {
        let mut chunk = [0_u8; 8192];
        let read = stream.read(&mut chunk).expect("read body");
        assert!(read > 0, "body ended early");
        body.extend_from_slice(&chunk[..read]);
    }
    (String::from_utf8(head).expect("UTF-8 head"), body)
}

fn request_target(head: &[u8]) -> String {
    let text = std::str::from_utf8(head).expect("head is UTF-8");
    text.lines()
        .next()
        .expect("request line")
        .split(' ')
        .nth(1)
        .expect("request target")
        .to_owned()
}

fn content_length(head: &[u8]) -> usize {
    let text = std::str::from_utf8(head).expect("head is UTF-8");
    text.lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|value| value.trim().parse().expect("numeric Content-Length"))
        })
        .expect("Content-Length header")
}

fn next_byte(stream: &mut TcpStream, prefix: &mut Vec<u8>) -> u8 {
    if prefix.is_empty() {
        let mut byte = [0_u8; 1];
        stream.read_exact(&mut byte).expect("body byte");
        byte[0]
    } else {
        prefix.remove(0)
    }
}

fn read_line_stream(stream: &mut TcpStream, prefix: &mut Vec<u8>) -> Vec<u8> {
    let mut line = Vec::new();
    loop {
        line.push(next_byte(stream, prefix));
        if line.ends_with(b"\r\n") {
            return line;
        }
    }
}

fn read_exact_stream(stream: &mut TcpStream, prefix: &mut Vec<u8>, out: &mut [u8]) {
    for slot in out.iter_mut() {
        *slot = next_byte(stream, prefix);
    }
}

/// Dechunk one upload, returning the decoded bytes.
fn dechunk(stream: &mut TcpStream, mut prefix: Vec<u8>) -> Vec<u8> {
    let mut body = Vec::new();
    loop {
        let line = read_line_stream(stream, &mut prefix);
        let text = std::str::from_utf8(&line).expect("chunk line is ASCII");
        let size =
            usize::from_str_radix(text.trim_end_matches("\r\n"), 16).expect("hex chunk size");
        if size == 0 {
            while read_line_stream(stream, &mut prefix).as_slice() != b"\r\n" {}
            return body;
        }
        let mut data = vec![0_u8; size];
        read_exact_stream(stream, &mut prefix, &mut data);
        body.extend_from_slice(&data);
        let mut crlf = [0_u8; 2];
        read_exact_stream(stream, &mut prefix, &mut crlf);
        assert_eq!(crlf, *b"\r\n");
    }
}

/// Send two descriptors in one `sendmsg` so the receiver must reject the truncation.
fn send_two_fds(
    socket: &UnixStream,
    first: std::os::fd::BorrowedFd<'_>,
    second: std::os::fd::BorrowedFd<'_>,
) {
    let frame = IngressHandoff::new(11).encode();
    let mut iov = libc::iovec {
        iov_base: frame.as_ptr().cast_mut().cast(),
        iov_len: frame.len(),
    };
    let descriptor = size_of::<libc::c_int>();
    let space = unsafe { libc::CMSG_SPACE((2 * descriptor) as libc::c_uint) as usize };
    let mut control = vec![0_u8; space];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len();
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN((2 * descriptor) as libc::c_uint) as usize;
        let data = libc::CMSG_DATA(header).cast::<libc::c_int>();
        std::ptr::write_unaligned(data, first.as_raw_fd());
        std::ptr::write_unaligned(data.add(1), second.as_raw_fd());
        message.msg_controllen = (*header).cmsg_len;
    }
    let sent = unsafe { libc::sendmsg(socket.as_raw_fd(), &message, 0) };
    assert!(sent > 0, "two-descriptor sendmsg failed");
}

fn send_fails(peer: &UnixStream) -> bool {
    let byte = [0_u8; 1];
    let sent = unsafe {
        libc::send(
            peer.as_raw_fd(),
            byte.as_ptr().cast(),
            byte.len(),
            libc::MSG_NOSIGNAL,
        )
    };
    sent < 0
}

#[test]
fn every_authority_and_route_mode_maps_to_the_expected_upstream_path() {
    let cases = [
        ("demo.localhost", "/api/users?page=2", "/api/users?page=2"),
        ("demo.localhost:80", "/api", "/api"),
        ("demo.localhost", "/healthz?probe=1", "/healthz?probe=1"),
        ("demo.localhost", "/files/a/b.css?x=1", "/a/b.css?x=1"),
        ("demo.localhost", "/old/users", "/v1/users"),
        ("demo.localhost", "/anything", "/anything"),
        ("demo.localhost", "/portal?q=1", "/portal?q=1"),
        ("mail.demo.localhost", "/inbox", "/inbox"),
        (
            "assets-cdn.demo.localhost",
            "/cdn-assets/logo.svg",
            "/cdn-assets/logo.svg",
        ),
        ("feat-auth--demo.localhost", "/x", "/x"),
    ];
    for (host, target, expected) in cases {
        let (port, seen, upstream) = echo_upstream();
        let (address, proxy) = spawn_proxy(registry_with_port(port), 1);
        let response = request(address, host, target);
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "{host}{target} was not proxied: {response}"
        );
        assert_eq!(
            seen.recv_timeout(TIMEOUT)
                .expect("upstream saw the request"),
            expected,
            "{host}{target} was rewritten incorrectly"
        );
        upstream.join().expect("upstream");
        proxy.join().expect("proxy");
    }
}

#[test]
fn hosts_and_paths_fail_closed_with_distinct_statuses() {
    let (address, proxy) = spawn_proxy(registry_with_port(1), 5);
    let unknown_world = request(address, "nowhere.localhost", "/");
    assert!(
        unknown_world.starts_with("HTTP/1.1 421 Misdirected Request"),
        "{unknown_world}"
    );
    let unrouted = request(address, "strict.localhost", "/nope");
    assert!(unrouted.starts_with("HTTP/1.1 404"), "{unrouted}");
    let foreign = request(address, "example.com", "/");
    assert!(foreign.starts_with("HTTP/1.1 400"), "{foreign}");
    let duplicate = request(address, "main.demo.localhost", "/api");
    assert!(
        duplicate.starts_with("HTTP/1.1 404"),
        "the first declared domain is the primary authority, never a duplicate host: {duplicate}"
    );
    let leaked = request(address, "assets-cdn.demo.localhost", "/assets/x");
    assert!(
        leaked.starts_with("HTTP/1.1 404"),
        "an unfiltered route must not leak onto a secondary domain: {leaked}"
    );
    proxy.join().expect("proxy");
}

#[test]
fn a_declared_domain_never_falls_through_to_the_primary_upstream() {
    // The api upstream is bound but never accepted: a real leak would leave a pending connection.
    let api = TcpListener::bind("127.0.0.1:0").expect("api listener binds");
    api.set_nonblocking(true).expect("non-blocking probe");
    let api_port = api.local_addr().expect("api address").port();
    let (mail_port, mail_seen, mail_upstream) = echo_upstream_body("mailpit");
    let registry = IngressRegistry::new([IngressTable::new(
        "demo",
        Some("main"),
        vec![
            "main".to_owned(),
            "mail".to_owned(),
            "assets-cdn".to_owned(),
        ],
        retarget_with(routes(BASE), |port| {
            if port == 9003 { mail_port } else { api_port }
        }),
    )
    .expect("demo table is routable")])
    .expect("registry is routable");
    let (address, proxy) = spawn_proxy(registry, 2);

    let mail = request(address, "mail.demo.localhost", "/api");
    assert!(
        mail.starts_with("HTTP/1.1 200") && mail.ends_with("mailpit"),
        "mail's own catch-all must answer: {mail}"
    );
    assert_eq!(
        mail_seen
            .recv_timeout(TIMEOUT)
            .expect("mailpit saw the request"),
        "/api"
    );

    let leaked = request(address, "assets-cdn.demo.localhost", "/api");
    assert!(
        leaked.starts_with("HTTP/1.1 404"),
        "`assets-cdn` declares only `/cdn-assets`: {leaked}"
    );

    proxy.join().expect("proxy");
    mail_upstream.join().expect("mailpit upstream");
    let error = api
        .accept()
        .expect_err("no request may ever reach the primary api upstream");
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
}

#[test]
fn keep_alive_serves_two_requests_and_pipelined_requests_are_not_lost() {
    let (port, seen, upstream) = echo_upstream_n(4);
    let (address, proxy) = spawn_proxy(single_world("demo", port), 2);

    let mut sequential = connect_client(address);
    for target in ["/one", "/two"] {
        write!(
            sequential,
            "GET {target} HTTP/1.1\r\nHost: demo.localhost\r\n\r\n"
        )
        .expect("request");
        let (head, body) = read_response(&mut sequential);
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(head.contains("Connection: keep-alive"), "{head}");
        assert_eq!(body, b"routed");
        assert_eq!(seen.recv_timeout(TIMEOUT).expect("target"), target);
    }
    sequential.shutdown(Shutdown::Both).expect("close");

    let mut pipelined = connect_client(address);
    pipelined
        .write_all(
            b"GET /three HTTP/1.1\r\nHost: demo.localhost\r\n\r\n\
              GET /four HTTP/1.1\r\nHost: demo.localhost\r\n\r\n",
        )
        .expect("pipelined requests");
    for target in ["/three", "/four"] {
        let (head, body) = read_response(&mut pipelined);
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert_eq!(body, b"routed");
        assert_eq!(seen.recv_timeout(TIMEOUT).expect("target"), target);
    }
    pipelined.shutdown(Shutdown::Both).expect("close");
    upstream.join().expect("upstream");
    proxy.join().expect("proxy");
}

#[test]
fn an_oversized_header_is_refused_with_431() {
    let (address, proxy) = spawn_proxy(registry_with_port(1), 1);
    let mut client = connect_client(address);
    let filler = "a".repeat(20 * 1024);
    write!(
        client,
        "GET / HTTP/1.1\r\nHost: demo.localhost\r\nX-Oversized: {filler}\r\n\r\n"
    )
    .expect("request");
    let mut response = String::new();
    client.read_to_string(&mut response).expect("response");
    assert!(
        response.starts_with("HTTP/1.1 431 Request Header Fields Too Large"),
        "{response}"
    );
    proxy.join().expect("proxy");
}

#[test]
fn a_ten_mebibyte_upload_streams_without_full_body_buffering() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("upstream binds");
    let port = listener.local_addr().expect("upstream address").port();
    let body_length: usize = 10 * 1024 * 1024;
    let (forwarded, saw_first) = mpsc::channel::<()>();
    let upstream = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("upstream accepts");
        stream.set_read_timeout(Some(TIMEOUT)).expect("timeout");
        let (head, body) = read_head(&mut stream);
        assert_eq!(content_length(&head), body_length);
        let mut seen = body.len();
        let mut buffer = [0_u8; 64 * 1024];
        while seen < 1024 {
            let read = stream.read(&mut buffer).expect("early body");
            assert!(read > 0, "upload ended early");
            seen += read;
        }
        forwarded.send(()).expect("signal first chunk");
        while seen < body_length {
            let read = stream.read(&mut buffer).expect("body");
            assert!(read > 0, "upload ended at {seen} of {body_length} bytes");
            seen += read;
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .expect("upstream replies");
    });
    let (address, proxy) = spawn_proxy(single_world("demo", port), 1);
    let mut client = connect_client(address);
    write!(
        client,
        "PUT /upload HTTP/1.1\r\nHost: demo.localhost\r\nContent-Length: {body_length}\r\n\
         Connection: close\r\n\r\n"
    )
    .expect("head");
    client.write_all(&[7_u8; 1024]).expect("first KiB");
    client.flush().expect("flush");
    saw_first
        .recv_timeout(TIMEOUT)
        .expect("the proxy forwarded the first 1 KiB before the 10 MiB body completed");
    let payload = [7_u8; 64 * 1024];
    let mut sent = 1024;
    while sent < body_length {
        let chunk = (body_length - sent).min(payload.len());
        client.write_all(&payload[..chunk]).expect("body chunk");
        sent += chunk;
    }
    let mut response = Vec::new();
    client
        .read_to_end(&mut response)
        .expect("client reads the response");
    assert!(response.starts_with(b"HTTP/1.1 200"));
    upstream.join().expect("upstream");
    proxy.join().expect("proxy");
}

#[test]
fn chunked_uploads_and_responses_stream_verbatim() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("upstream binds");
    let port = listener.local_addr().expect("upstream address").port();
    let payload = vec![3_u8; 512 * 1024];
    let expected = payload.clone();
    let upstream = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("upstream accepts");
        stream.set_read_timeout(Some(TIMEOUT)).expect("timeout");
        let (head, body) = read_head(&mut stream);
        let text = String::from_utf8_lossy(&head);
        assert!(text.contains("Transfer-Encoding: chunked"), "{text}");
        assert!(
            !text.to_ascii_lowercase().contains("content-length"),
            "{text}"
        );
        let decoded = dechunk(&mut stream, body);
        assert_eq!(decoded, expected);
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n\
                  5\r\nhello\r\n0\r\n\r\n",
            )
            .expect("upstream replies");
    });
    let (address, proxy) = spawn_proxy(single_world("demo", port), 1);
    let mut client = connect_client(address);
    write!(
        client,
        "POST /upload HTTP/1.1\r\nHost: demo.localhost\r\nTransfer-Encoding: chunked\r\n\
         Content-Length: 999999\r\nConnection: close\r\n\r\n"
    )
    .expect("head");
    for chunk in payload.chunks(8 * 1024) {
        write!(client, "{:x}\r\n", chunk.len()).expect("chunk size");
        client.write_all(chunk).expect("chunk data");
        client.write_all(b"\r\n").expect("chunk terminator");
    }
    client.write_all(b"0\r\n\r\n").expect("last chunk");
    client.flush().expect("flush");
    let mut response = String::new();
    client.read_to_string(&mut response).expect("response");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response.contains("Transfer-Encoding: chunked"),
        "{response}"
    );
    assert!(response.contains("5\r\nhello\r\n0\r\n\r\n"), "{response}");
    upstream.join().expect("upstream");
    proxy.join().expect("proxy");
}

#[test]
fn conflicting_content_lengths_are_rejected() {
    let (address, proxy) = spawn_proxy(registry_with_port(1), 1);
    let mut client = connect_client(address);
    write!(
        client,
        "POST /api HTTP/1.1\r\nHost: demo.localhost\r\nContent-Length: 3\r\n\
         Content-Length: 4\r\n\r\nabc"
    )
    .expect("request");
    let mut response = String::new();
    client.read_to_string(&mut response).expect("response");
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    proxy.join().expect("proxy");
}

#[test]
fn expect_continue_is_answered_locally() {
    let (port, seen, upstream) = echo_upstream();
    let (address, proxy) = spawn_proxy(single_world("demo", port), 1);
    let mut client = connect_client(address);
    write!(
        client,
        "POST /upload HTTP/1.1\r\nHost: demo.localhost\r\nContent-Length: 3\r\n\
         Expect: 100-continue\r\nConnection: close\r\n\r\n"
    )
    .expect("head");
    let (head, _body) = read_head(&mut client);
    assert!(
        String::from_utf8_lossy(&head).starts_with("HTTP/1.1 100 Continue"),
        "{}",
        String::from_utf8_lossy(&head)
    );
    client.write_all(b"abc").expect("body");
    client.flush().expect("flush");
    let mut response = String::new();
    client.read_to_string(&mut response).expect("response");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert_eq!(seen.recv_timeout(TIMEOUT).expect("target"), "/upload");
    upstream.join().expect("upstream");
    proxy.join().expect("proxy");
}

#[test]
fn a_close_delimited_upstream_response_streams_to_the_client() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("upstream binds");
    let port = listener.local_addr().expect("upstream address").port();
    let upstream = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("upstream accepts");
        stream.set_read_timeout(Some(TIMEOUT)).expect("timeout");
        let _ = read_head(&mut stream);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nhello")
            .expect("close-delimited response");
    });
    let (address, proxy) = spawn_proxy(single_world("demo", port), 1);
    let mut client = connect_client(address);
    write!(
        client,
        "GET / HTTP/1.1\r\nHost: demo.localhost\r\nConnection: close\r\n\r\n"
    )
    .expect("request");
    let mut response = String::new();
    client.read_to_string(&mut response).expect("response");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("hello"), "{response}");
    upstream.join().expect("upstream");
    proxy.join().expect("proxy");
}

#[test]
fn websocket_bytes_tunnel_bidirectionally_after_a_hand_rolled_101() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("upstream binds");
    let port = listener.local_addr().expect("upstream address").port();
    let upstream = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("upstream accepts");
        stream.set_read_timeout(Some(TIMEOUT)).expect("timeout");
        let (head, body) = read_head(&mut stream);
        assert!(body.is_empty());
        let text = String::from_utf8_lossy(&head);
        assert!(text.contains("Connection: Upgrade"), "{text}");
        assert!(text.contains("Upgrade: websocket"), "{text}");
        stream
            .write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                  Connection: Upgrade\r\n\r\nserver-first",
            )
            .expect("101 with an immediate frame");
        let mut frame = [0_u8; 12];
        stream.read_exact(&mut frame).expect("client frame");
        assert_eq!(&frame, b"client-frame");
        stream.write_all(b"client-frame").expect("echo frame");
        let mut sink = [0_u8; 1];
        let _ = stream.read(&mut sink);
    });
    let (address, proxy) = spawn_proxy(single_world("demo", port), 1);
    let mut client = connect_client(address);
    write!(
        client,
        "GET /ws HTTP/1.1\r\nHost: demo.localhost\r\nConnection: Upgrade\r\n\
         Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    )
    .expect("upgrade request");
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        client.read_exact(&mut byte).expect("response head");
        head.push(byte[0]);
    }
    assert!(
        head.starts_with(b"HTTP/1.1 101"),
        "{}",
        String::from_utf8_lossy(&head)
    );
    let mut pushed = [0_u8; 12];
    client.read_exact(&mut pushed).expect("immediate frame");
    assert_eq!(&pushed, b"server-first");
    client.write_all(b"client-frame").expect("client frame");
    let mut echoed = [0_u8; 12];
    client.read_exact(&mut echoed).expect("echoed frame");
    assert_eq!(&echoed, b"client-frame");
    client.shutdown(Shutdown::Both).expect("client closes");
    upstream.join().expect("upstream");
    proxy.join().expect("proxy");
}

#[test]
fn an_accepted_connection_fd_hands_off_over_a_socketpair() {
    let (port, seen, upstream) = echo_upstream();
    let (sender, receiver) = UnixStream::pair().expect("socketpair");
    let listener = TcpListener::bind("127.0.0.1:0").expect("front listener");
    let front = listener.local_addr().expect("front address");
    let registry = single_world("demo", port);
    let client_thread = thread::spawn(move || request(front, "demo.localhost", "/handoff"));
    let (accepted, _) = listener.accept().expect("accept");
    IngressHandoff::new(7)
        .send(&sender, accepted.as_fd())
        .expect("send the accepted fd");
    drop(accepted);
    let (handoff, stream) = IngressHandoff::accept(&receiver).expect("receive the accepted fd");
    assert_eq!(handoff.sequence(), 7);
    serve_connection(stream, &registry, |routed| {
        TcpStream::connect(("127.0.0.1", routed.port))
    })
    .expect("serve the handed-off connection");
    let response = client_thread.join().expect("client");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert_eq!(
        seen.recv_timeout(TIMEOUT)
            .expect("upstream saw the request"),
        "/handoff"
    );
    upstream.join().expect("upstream");
}

#[test]
fn two_descriptors_and_a_missing_descriptor_are_rejected() {
    let (sender, receiver) = UnixStream::pair().expect("socketpair");
    let (first_send, first_peer) = UnixStream::pair().expect("first pair");
    let (second_send, second_peer) = UnixStream::pair().expect("second pair");
    send_two_fds(&sender, first_send.as_fd(), second_send.as_fd());
    drop(first_send);
    drop(second_send);
    let error = recv_fd(&receiver).expect_err("two descriptors are rejected");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(
        send_fails(&first_peer),
        "the first received duplicate stayed open"
    );
    assert!(
        send_fails(&second_peer),
        "the second received duplicate stayed open"
    );

    let (sender, receiver) = UnixStream::pair().expect("socketpair");
    send_fd(&sender, &IngressHandoff::new(9).encode(), None).expect("payload without a descriptor");
    let error = IngressHandoff::accept(&receiver).expect_err("a missing descriptor is rejected");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}
