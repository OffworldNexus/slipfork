use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use crate::Architecture;
use crate::credentials::RegistryCredentials;
use crate::reference::ImageReference;

use super::{RegistryClient, RegistryScheme};

const MANIFEST: &str = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a","size":2},"layers":[]}"#;

/// Reads one request head and returns its first line (the request line).
fn read_request_line(stream: &mut TcpStream) -> (String, String) {
    let mut request = Vec::new();
    let mut buffer = [0u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut buffer).expect("read the request head");
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
    }
    let head = String::from_utf8_lossy(&request).into_owned();
    let line = head.lines().next().unwrap_or_default().to_owned();
    (line, head)
}

fn respond(stream: &mut TcpStream, status: &str, headers: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .expect("write the response head");
    stream.write_all(body).expect("write the response body");
    stream.flush().expect("flush the response");
}

/// Serves the exact ghcr shape: a placeholder-scoped `/v2/` ping, the scoped manifest
/// challenge, the token endpoint, then the retried manifest. Returns every request line.
fn serve_registry() -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the loopback registry");
    let port = listener
        .local_addr()
        .expect("read the loopback address")
        .port();
    let addr = format!("127.0.0.1:{port}");
    let realm = format!("http://{addr}/token");
    let handle = thread::spawn(move || {
        let mut seen = Vec::new();
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().expect("accept a registry request");
            let (line, head) = read_request_line(&mut stream);
            seen.push(line.clone());
            if line.starts_with("GET /v2/ HTTP") {
                respond(
                    &mut stream,
                    "401 Unauthorized",
                    &format!(
                        "WWW-Authenticate: Bearer realm=\"{realm}\",service=\"ghcr.io\",scope=\"repository:user/image:pull\"\r\n"
                    ),
                    b"",
                );
            } else if line.starts_with("GET /token") {
                respond(
                    &mut stream,
                    "200 OK",
                    "Content-Type: application/json\r\n",
                    br#"{"token":"t"}"#,
                );
            } else if head.contains("Authorization: Bearer t") {
                respond(
                    &mut stream,
                    "200 OK",
                    "Content-Type: application/vnd.oci.image.manifest.v1+json\r\n",
                    MANIFEST.as_bytes(),
                );
            } else {
                respond(
                    &mut stream,
                    "401 Unauthorized",
                    &format!(
                        "WWW-Authenticate: Bearer realm=\"{realm}\",service=\"ghcr.io\",scope=\"repository:owner/app:pull\"\r\n"
                    ),
                    b"",
                );
            }
        }
        seen
    });
    (addr, handle)
}

/// A ghcr-style ping advertises `repository:user/image:pull`; the ping must stay
/// unauthenticated and only the real manifest scope may ever be minted.
#[test]
fn the_v2_ping_never_mints_a_placeholder_token_and_the_manifest_scope_wins() {
    let (addr, server) = serve_registry();
    let reference =
        ImageReference::parse(&format!("{addr}/owner/app:latest")).expect("parse a reference");
    let client = RegistryClient::new(RegistryCredentials::default(), "slipfork/test".to_owned())
        .with_scheme(RegistryScheme::Http);
    let mut session = client.session(&reference);

    let resolved = session
        .manifest(Architecture::X86_64)
        .expect("the scoped manifest resolves after the placeholder ping");
    let seen = server.join().expect("the loopback registry exits");

    assert_eq!(resolved.manifest.layers.len(), 0);
    let token_requests: Vec<&String> = seen
        .iter()
        .filter(|line| line.starts_with("GET /token"))
        .collect();
    assert_eq!(token_requests.len(), 1, "exactly one token mint: {seen:?}");
    assert!(
        token_requests[0].contains("scope=repository:owner/app:pull"),
        "the token names the real repository: {:?}",
        token_requests[0]
    );
    assert!(
        !token_requests[0].contains("user/image"),
        "the placeholder scope is never minted: {:?}",
        token_requests[0]
    );
    assert!(
        seen.iter().any(|line| line.starts_with("GET /v2/ HTTP")),
        "the ping still runs: {seen:?}"
    );
}

/// Serves a scoped `/v2/` Bearer challenge then the token endpoint, capturing every head.
fn serve_login(token_status: &'static str) -> (String, thread::JoinHandle<Vec<(String, String)>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the loopback registry");
    let port = listener
        .local_addr()
        .expect("read the loopback address")
        .port();
    let addr = format!("127.0.0.1:{port}");
    let realm = format!("http://{addr}/token");
    let handle = thread::spawn(move || {
        let mut seen = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept a login request");
            let (line, head) = read_request_line(&mut stream);
            seen.push((line.clone(), head.clone()));
            if line.starts_with("GET /v2/ HTTP") {
                respond(
                    &mut stream,
                    "401 Unauthorized",
                    &format!(
                        "WWW-Authenticate: Bearer realm=\"{realm}\",service=\"ghcr.io\",scope=\"repository:user/image:pull\"\r\n"
                    ),
                    b"",
                );
            } else if line.starts_with("GET /token") {
                if token_status == "200 OK" {
                    respond(
                        &mut stream,
                        token_status,
                        "Content-Type: application/json\r\n",
                        br#"{"token":"t"}"#,
                    );
                } else {
                    respond(&mut stream, token_status, "", b"");
                }
            } else {
                respond(&mut stream, "404 Not Found", "", b"");
            }
        }
        seen
    });
    (addr, handle)
}

/// A loopback host selects HTTP by itself, and login mints a scope-less token with Basic.
#[test]
fn login_probe_mints_a_scopeless_token_over_auto_http() {
    let (addr, server) = serve_login("200 OK");
    let credentials = RegistryCredentials::default().with_host(&addr, "octo", "s3cret");
    let client = RegistryClient::new(credentials, "slipfork/test".to_owned());

    client
        .login(&addr)
        .expect("a loopback login probes over plain HTTP");

    let seen = server.join().expect("the loopback registry exits");
    let (line, head) = seen
        .iter()
        .find(|(line, _)| line.starts_with("GET /token"))
        .expect("exactly one token mint");
    assert!(line.contains("service=ghcr.io"), "{line}");
    assert!(
        !line.contains("scope="),
        "login never mints ghcr's placeholder scope: {line}"
    );
    assert!(
        head.contains("Authorization: Basic "),
        "login sends the injected Basic header: {head}"
    );
}

/// A refused login probe is one typed error whose `next:` carries the remedy, never a secret.
#[test]
fn login_probe_reports_bad_credentials_as_one_typed_error() {
    let (addr, server) = serve_login("401 Unauthorized");
    let credentials = RegistryCredentials::default().with_host(&addr, "octo", "wrong-secret");
    let client = RegistryClient::new(credentials, "slipfork/test".to_owned());

    let error = client
        .login(&addr)
        .expect_err("bad credentials must fail the login");

    assert!(
        matches!(error, crate::Error::LoginRejected { .. }),
        "{error:?}"
    );
    let text = error.to_string();
    assert!(text.contains("next: "), "{text}");
    assert!(
        !text.contains("wrong-secret"),
        "the secret stays out: {text}"
    );
    let _ = server.join();
}

/// A `401` with no `WWW-Authenticate` header is terminal: no identical retry is sent.
#[test]
fn login_probe_reports_a_challengeless_401_without_retrying() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the loopback registry");
    let port = listener
        .local_addr()
        .expect("read the loopback address")
        .port();
    let addr = format!("127.0.0.1:{port}");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept one login request");
        let (line, _) = read_request_line(&mut stream);
        respond(&mut stream, "401 Unauthorized", "", b"");
        line
    });

    let credentials = RegistryCredentials::default().with_host(&addr, "octo", "s3cret");
    let client = RegistryClient::new(credentials, "slipfork/test".to_owned());
    let error = client
        .login(&addr)
        .expect_err("a headerless 401 cannot authenticate");

    assert!(
        matches!(error, crate::Error::LoginRejected { .. }),
        "{error:?}"
    );
    let text = error.to_string();
    assert!(text.contains("next: "), "{text}");
    assert!(text.contains("HTTP 401"), "{text}");
    let line = server.join().expect("the loopback registry exits");
    assert!(
        line.starts_with("GET /v2/ HTTP"),
        "only one probe is sent: {line}"
    );
}
