//! In-tree `ospp-v1` reference server: an independent implementation of
//! `specs/05-ospp-v1.md` (`version: 2`, P-256 auth, ALTCHA pairing, RFC 9180
//! sealing, sealed dump streaming) that the protected suite drives with the
//! production client.
//!
//! The fixture deliberately re-derives every wire constant from the spec instead
//! of importing the client's helpers: transcripts, HPKE `info`/AAD strings, frame
//! JSON, and the ALTCHA hash are all local. It speaks real loopback WebSocket
//! frames with real P-256 verification and real HPKE sealing, pushes live
//! `env_updated` frames, and serves a sealed dump stream over loopback HTTP.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hpke::aead::AesGcm256;
use hpke::kdf::HkdfSha256;
use hpke::kem::{DhP256HkdfSha256, Kem};
use hpke::rand_core::{TryCryptoRng, TryRng};
use hpke::{Deserializable, OpModeS, Serializable, setup_sender_with_rng};
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use slipfork_ospp::{OsppClaims, OsppEndpoint, OsppError};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::HeaderValue;
use tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tungstenite::{WebSocket, accept_hdr};

const POLL_SLICE: Duration = Duration::from_millis(25);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HTTP_HEADERS: usize = 16 * 1024;
const SECRET_INFO: &[u8] = b"ospp-v1-secret";
const DUMP_INFO: &[u8] = b"ospp-v1-dump";
const SECRET_AUTH: &str = "p256";
const SECRET_ENCRYPTION: &str = "hpke-p256";
const PROTOCOL_SUBPROTOCOL: &str = "ospp-v1";

/// One authoritative server-side environment value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestValue {
    /// Resolved plaintext.
    Plain(String),
    /// A secret always sealed to the subscription key.
    Secret(String),
}

/// Reference server configuration: authoritative server-side values.
#[derive(Debug, Clone, Default)]
pub struct ReferenceServerConfig {
    /// Compressed P-256 public keys allowed to open an authenticated session.
    pub accepted_keys: Vec<String>,
    /// Authoritative environment values by component id.
    pub components: BTreeMap<String, BTreeMap<String, TestValue>>,
    /// Resource dumps by resource path.
    pub dumps: BTreeMap<String, Vec<u8>>,
}

/// One observed WebSocket handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRequest {
    /// Request path.
    pub path: String,
    /// Offered `Sec-WebSocket-Protocol` value.
    pub subprotocol: Option<String>,
}

/// One accepted subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionRecord {
    /// Subscription id.
    pub id: String,
    /// Verbatim Git remote string.
    pub remote: String,
    /// The evaluated `Forkfile` payload the client subscribed with.
    pub forkfile: String,
    /// ID-only claims.
    pub claims: OsppClaims,
    /// Client's ephemeral HPKE public key hex.
    pub public_key: String,
}

/// One pairing attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingRecord {
    /// Single-use handle hex.
    pub handle: String,
    /// Proposed hostname.
    pub hostname: String,
    /// Enrolling public key hex.
    pub public_key: String,
    /// Whether the browser approved after the handle was issued.
    pub approved: bool,
}

/// Everything the suite observes on the reference server.
#[derive(Debug, Clone, Default)]
pub struct ServerState {
    /// WebSocket handshakes, in arrival order.
    pub requests: Vec<ServerRequest>,
    /// Accepted subscriptions by id.
    pub subscriptions: BTreeMap<String, SubscriptionRecord>,
    /// Pairing attempts, in arrival order.
    pub pairings: Vec<PairingRecord>,
    /// Client frame types received, in arrival order.
    pub frames: Vec<String>,
    /// Number of `dump_request` frames served with a URL.
    pub dump_urls: usize,
}

/// A running reference server.
pub struct ReferenceOsppServer {
    endpoint: OsppEndpoint,
    shared: Arc<Shared>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    stop: Arc<AtomicBool>,
}

impl ReferenceOsppServer {
    /// Bind loopback WebSocket and dump listeners and serve until dropped.
    pub fn start(config: ReferenceServerConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind reference ospp listener");
        let port = listener.local_addr().expect("ws local addr").port();
        let http_listener = TcpListener::bind("127.0.0.1:0").expect("bind reference dump listener");
        let http_addr = http_listener.local_addr().expect("dump local addr");
        listener
            .set_nonblocking(true)
            .expect("nonblocking ws listener");
        http_listener
            .set_nonblocking(true)
            .expect("nonblocking dump listener");

        let endpoint = OsppEndpoint::parse(&format!("ws://127.0.0.1:{port}/_ospp/v1"))
            .expect("valid loopback endpoint");
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared {
            config: Mutex::new(config),
            state: Mutex::new(ServerState::default()),
            subscriptions: Mutex::new(BTreeMap::new()),
            dumps: Mutex::new(BTreeMap::new()),
            decisions: Mutex::new(BTreeSet::new()),
            http_addr,
            stop: Arc::clone(&stop),
        });

        let ws_thread = {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name("ospp-reference-server".to_owned())
                .spawn(move || shared.serve_websocket(listener))
                .expect("spawn reference ws server")
        };
        let http_thread = {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name("ospp-reference-dumps".to_owned())
                .spawn(move || shared.serve_dumps(http_listener))
                .expect("spawn reference dump server")
        };

        Self {
            endpoint,
            shared,
            threads: Mutex::new(vec![ws_thread, http_thread]),
            stop,
        }
    }

    /// The validated `ws://127.0.0.1:<port>/_ospp/v1` endpoint.
    pub fn endpoint(&self) -> OsppEndpoint {
        self.endpoint.clone()
    }

    /// Snapshot of everything observed so far.
    pub fn state(&self) -> ServerState {
        lock(&self.shared.state).clone()
    }

    /// Replace one server-side secret value.
    pub fn set_secret(&self, key: &str, value: &str) {
        for component in lock(&self.shared.config).components.values_mut() {
            if let Some(slot) = component.get_mut(key) {
                *slot = TestValue::Secret(value.to_owned());
            }
        }
    }

    /// Re-resolve and push `env_updated` for every active subscription.
    pub fn push_update(&self) -> Result<(), OsppError> {
        let subscriptions: Vec<ActiveSubscription> =
            lock(&self.shared.subscriptions).values().cloned().collect();
        for subscription in subscriptions {
            let config = lock(&self.shared.config).clone();
            let resolved = resolve_components(&config, &subscription.record.claims);
            let components = seal_components(&resolved, &subscription.record.public_key)?;
            let _ = subscription.outbound.send(ServerFrame::EnvUpdated {
                id: subscription.record.id.clone(),
                components,
            });
        }
        Ok(())
    }

    /// Simulate the browser approving a pending handle.
    pub fn approve_pairing(&self, handle: &str) {
        lock(&self.shared.decisions).insert(handle.to_owned());
    }

    /// Stop accepting connections and join the listener threads.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let threads = std::mem::take(&mut *lock(&self.threads));
        for handle in threads {
            let _ = handle.join();
        }
    }
}

impl Drop for ReferenceOsppServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Shared {
    config: Mutex<ReferenceServerConfig>,
    state: Mutex<ServerState>,
    subscriptions: Mutex<BTreeMap<String, ActiveSubscription>>,
    dumps: Mutex<BTreeMap<String, DumpEntry>>,
    decisions: Mutex<BTreeSet<String>>,
    http_addr: SocketAddr,
    stop: Arc<AtomicBool>,
}

#[derive(Clone)]
struct ActiveSubscription {
    record: SubscriptionRecord,
    outbound: Sender<ServerFrame>,
}

struct DumpEntry {
    public_key: String,
    subscription_id: String,
    resource: String,
    bytes: Vec<u8>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn serve_websocket(self: &Arc<Self>, listener: TcpListener) {
        while !self.stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _peer)) => {
                    let shared = Arc::clone(self);
                    let _ = thread::Builder::new()
                        .name("ospp-reference-conn".to_owned())
                        .spawn(move || shared.handle_connection(stream));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(POLL_SLICE);
                }
                Err(_) => break,
            }
        }
    }

    #[allow(clippy::result_large_err)] // `tungstenite` fixes the handshake callback's error type
    fn handle_connection(self: &Arc<Self>, stream: TcpStream) {
        let recorder = Arc::clone(self);
        let callback =
            move |request: &Request, mut response: Response| -> Result<Response, ErrorResponse> {
                let protocol = request
                    .headers()
                    .get(SEC_WEBSOCKET_PROTOCOL)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                lock(&recorder.state).requests.push(ServerRequest {
                    path: request.uri().path().to_owned(),
                    subprotocol: protocol.clone(),
                });
                let offered = protocol.unwrap_or_default();
                if !offered
                    .split(',')
                    .any(|value| value.trim() == PROTOCOL_SUBPROTOCOL)
                {
                    return Err(tungstenite::http::Response::builder()
                        .status(400)
                        .body(Some("missing Sec-WebSocket-Protocol: ospp-v1".to_owned()))
                        .expect("static error response"));
                }
                response.headers_mut().insert(
                    SEC_WEBSOCKET_PROTOCOL,
                    HeaderValue::from_static(PROTOCOL_SUBPROTOCOL),
                );
                Ok(response)
            };
        let socket: WebSocket<TcpStream> = match accept_hdr(stream, callback) {
            Ok(socket) => socket,
            Err(_) => return,
        };
        let _ = socket.get_ref().set_read_timeout(Some(POLL_SLICE));
        let _ = Session::new(socket, self).serve();
    }

    fn serve_dumps(self: &Arc<Self>, listener: TcpListener) {
        while !self.stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _peer)) => self.handle_dump(stream),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(POLL_SLICE);
                }
                Err(_) => break,
            }
        }
    }

    fn handle_dump(self: &Arc<Self>, mut stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(clone) => clone,
            Err(_) => return,
        });
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).is_err() {
            return;
        }
        let mut headers = 0usize;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(read) => {
                    headers += read;
                    if headers > MAX_HTTP_HEADERS || line == "\r\n" || line == "\n" {
                        break;
                    }
                }
                Err(_) => return,
            }
        }
        let path = request_line.split_whitespace().nth(1).unwrap_or_default();
        let Some(token) = path.strip_prefix("/dumps/") else {
            let _ = Self::respond(&mut stream, 404, b"not found");
            return;
        };
        let Some(entry) = lock(&self.dumps).remove(token) else {
            let _ = Self::respond(&mut stream, 404, b"unknown dump token");
            return;
        };
        match seal_dump(
            &entry.public_key,
            &entry.subscription_id,
            &entry.resource,
            &entry.bytes,
        ) {
            Ok(body) => {
                let _ = Self::respond(&mut stream, 200, &body);
            }
            Err(_) => {
                let _ = Self::respond(&mut stream, 500, b"sealing failure");
            }
        }
    }

    fn respond(stream: &mut TcpStream, status: u16, body: &[u8]) -> std::io::Result<()> {
        let reason = match status {
            200 => "OK",
            404 => "Not Found",
            _ => "Internal Server Error",
        };
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\ncontent-length: {}\r\ncontent-type: application/octet-stream\r\nconnection: close\r\n\r\n",
            body.len()
        )?;
        stream.write_all(body)?;
        stream.flush()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Challenge {
    nonce: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthResponse {
    public_key: String,
    client_nonce: String,
    signature: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Encryption {
    method: String,
    public_key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientFrame {
    Hello {
        version: u32,
        auth_methods: Vec<String>,
    },
    AuthResponse {
        response: AuthResponse,
    },
    PairHello {
        version: u32,
        key_methods: Vec<String>,
    },
    PairBegin {
        altcha: String,
        hostname: String,
        response: AuthResponse,
    },
    Subscribe {
        id: String,
        remote: String,
        encryption: Encryption,
        forkfile: String,
        claims: OsppClaims,
    },
    Unsubscribe {
        id: String,
    },
    DumpRequest {
        id: String,
        subscription_id: String,
        resource: String,
    },
}

type Components = BTreeMap<String, BTreeMap<String, serde_json::Value>>;

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerFrame {
    AuthChallenge {
        method: String,
        challenge: Challenge,
    },
    AuthOk {
        encryption_methods: Vec<String>,
    },
    PairChallenge {
        method: String,
        altcha: serde_json::Value,
        challenge: Challenge,
    },
    PairHandle {
        url: String,
        handle: String,
    },
    PairApproved {
        account: String,
        machine: String,
    },
    EnvResolved {
        id: String,
        components: Components,
    },
    EnvUpdated {
        id: String,
        components: Components,
    },
    DumpReady {
        id: String,
        url: String,
    },
    Error {
        id: Option<String>,
        code: String,
        message: String,
    },
}

impl ServerFrame {
    fn to_json(&self) -> Result<String, OsppError> {
        serde_json::to_string(self).map_err(|error| OsppError::Protocol(error.to_string()))
    }
}

fn frame_type(frame: &ClientFrame) -> &'static str {
    match frame {
        ClientFrame::Hello { .. } => "hello",
        ClientFrame::AuthResponse { .. } => "auth_response",
        ClientFrame::PairHello { .. } => "pair_hello",
        ClientFrame::PairBegin { .. } => "pair_begin",
        ClientFrame::Subscribe { .. } => "subscribe",
        ClientFrame::Unsubscribe { .. } => "unsubscribe",
        ClientFrame::DumpRequest { .. } => "dump_request",
    }
}

type SubscribeParts = (String, String, Encryption, String, OsppClaims);

struct Session<'a> {
    socket: WebSocket<TcpStream>,
    shared: &'a Arc<Shared>,
}

impl<'a> Session<'a> {
    fn new(socket: WebSocket<TcpStream>, shared: &'a Arc<Shared>) -> Self {
        Self { socket, shared }
    }

    fn serve(mut self) -> Result<(), OsppError> {
        let result = self.run();
        let _ = self.socket.close(None);
        result
    }

    fn run(&mut self) -> Result<(), OsppError> {
        let first = loop {
            if self.shared.stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            match self.read()? {
                Some(frame) => break frame,
                None => continue,
            }
        };
        self.record(frame_type(&first));
        match first {
            ClientFrame::Hello {
                version,
                auth_methods,
            } => self.authenticated_session(version, auth_methods),
            ClientFrame::PairHello {
                version,
                key_methods,
            } => self.pairing_session(version, key_methods),
            other => {
                self.send_error(None, "bad_frame", "expected hello or pair_hello")?;
                Err(OsppError::Protocol(format!(
                    "unexpected first frame {}",
                    frame_type(&other)
                )))
            }
        }
    }

    fn authenticated_session(
        &mut self,
        version: u32,
        auth_methods: Vec<String>,
    ) -> Result<(), OsppError> {
        if version != 2 {
            self.send_error(None, "unsupported_version", "only version 2 is supported")?;
            return Err(OsppError::Protocol("unsupported version".to_owned()));
        }
        if !auth_methods.iter().any(|method| method == SECRET_AUTH) {
            self.send_error(None, "unsupported_auth", "only p256 is supported")?;
            return Err(OsppError::Protocol("unsupported auth method".to_owned()));
        }
        let mut server_nonce = [0u8; 32];
        random_bytes(&mut server_nonce)?;
        self.send(&ServerFrame::AuthChallenge {
            method: SECRET_AUTH.to_owned(),
            challenge: Challenge {
                nonce: hex::encode(server_nonce),
            },
        })?;
        let response = match self.read()? {
            Some(ClientFrame::AuthResponse { response }) => response,
            _ => {
                self.send_error(None, "bad_frame", "expected auth_response")?;
                return Err(OsppError::Protocol("expected auth_response".to_owned()));
            }
        };
        self.record("auth_response");
        self.authenticate_response(&server_nonce, &response, false)?;
        self.send(&ServerFrame::AuthOk {
            encryption_methods: vec![SECRET_ENCRYPTION.to_owned()],
        })?;
        self.session_loop()
    }

    fn authenticate_response(
        &mut self,
        server_nonce: &[u8; 32],
        response: &AuthResponse,
        pairing: bool,
    ) -> Result<(), OsppError> {
        let client_nonce = hex::decode(&response.client_nonce)
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .ok_or_else(|| OsppError::Protocol("client_nonce must be 32 bytes".to_owned()))?;
        if !pairing {
            let accepted = lock(&self.shared.config).accepted_keys.clone();
            if !accepted.iter().any(|key| key == &response.public_key) {
                self.send_error(None, "unauthorized_key", "machine key is not enrolled")?;
                return Err(OsppError::Protocol("unauthorized key".to_owned()));
            }
        }
        let transcript = if pairing {
            pairing_bytes(server_nonce, &client_nonce)
        } else {
            auth_bytes(server_nonce, &client_nonce)
        };
        if !verify_signature(&response.public_key, &transcript, &response.signature) {
            self.send_error(None, "bad_proof", "signature did not verify")?;
            return Err(OsppError::InvalidSignature);
        }
        Ok(())
    }

    fn session_loop(&mut self) -> Result<(), OsppError> {
        let (outbound, outbound_rx) = std::sync::mpsc::channel();
        loop {
            if self.shared.stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            while let Ok(frame) = outbound_rx.try_recv() {
                self.send(&frame)?;
            }
            let Some(frame) = self.read()? else {
                continue;
            };
            self.record(frame_type(&frame));
            match frame {
                ClientFrame::Subscribe {
                    id,
                    remote,
                    encryption,
                    forkfile,
                    claims,
                } => self.subscribe(&outbound, (id, remote, encryption, forkfile, claims))?,
                ClientFrame::Unsubscribe { id } => {
                    lock(&self.shared.subscriptions).remove(&id);
                    lock(&self.shared.state).subscriptions.remove(&id);
                }
                ClientFrame::DumpRequest {
                    id,
                    subscription_id,
                    resource,
                } => self.dump(id, subscription_id, resource)?,
                other => {
                    self.send_error(
                        None,
                        "bad_frame",
                        "frame is not valid in an authenticated session",
                    )?;
                    return Err(OsppError::Protocol(format!(
                        "unexpected frame {}",
                        frame_type(&other)
                    )));
                }
            }
        }
    }

    fn subscribe(
        &mut self,
        outbound: &Sender<ServerFrame>,
        parts: SubscribeParts,
    ) -> Result<(), OsppError> {
        let (id, remote, encryption, forkfile, claims) = parts;
        if encryption.method != SECRET_ENCRYPTION {
            self.send_error(
                Some(&id),
                "unsupported_encryption",
                "only hpke-p256 is supported",
            )?;
            return Ok(());
        }
        let config = lock(&self.shared.config).clone();
        let resolved = resolve_components(&config, &claims);
        let components = seal_components(&resolved, &encryption.public_key)?;
        self.send(&ServerFrame::EnvResolved {
            id: id.clone(),
            components,
        })?;
        let record = SubscriptionRecord {
            id: id.clone(),
            remote,
            forkfile,
            claims,
            public_key: encryption.public_key,
        };
        lock(&self.shared.subscriptions).insert(
            id.clone(),
            ActiveSubscription {
                record: record.clone(),
                outbound: outbound.clone(),
            },
        );
        lock(&self.shared.state).subscriptions.insert(id, record);
        Ok(())
    }

    fn dump(
        &mut self,
        id: String,
        subscription_id: String,
        resource: String,
    ) -> Result<(), OsppError> {
        let public_key = lock(&self.shared.subscriptions)
            .get(&subscription_id)
            .map(|subscription| subscription.record.public_key.clone());
        let bytes = lock(&self.shared.config).dumps.get(&resource).cloned();
        let (Some(public_key), Some(bytes)) = (public_key, bytes) else {
            self.send_error(
                Some(&id),
                "dump_unavailable",
                "no dump is available for that resource",
            )?;
            return Ok(());
        };
        let mut token_bytes = [0u8; 16];
        random_bytes(&mut token_bytes)?;
        let token = hex::encode(token_bytes);
        lock(&self.shared.dumps).insert(
            token.clone(),
            DumpEntry {
                public_key,
                subscription_id,
                resource,
                bytes,
            },
        );
        lock(&self.shared.state).dump_urls += 1;
        self.send(&ServerFrame::DumpReady {
            id,
            url: format!("http://{}/dumps/{}", self.shared.http_addr, token),
        })
    }

    fn pairing_session(&mut self, version: u32, key_methods: Vec<String>) -> Result<(), OsppError> {
        if version != 2 {
            self.send_error(None, "unsupported_version", "only version 2 is supported")?;
            return Err(OsppError::Protocol("unsupported version".to_owned()));
        }
        if !key_methods.iter().any(|method| method == SECRET_AUTH) {
            self.send_error(None, "unsupported_auth", "only p256 key proof is supported")?;
            return Err(OsppError::Protocol("unsupported key method".to_owned()));
        }

        let mut server_nonce = [0u8; 32];
        random_bytes(&mut server_nonce)?;
        let mut salt_bytes = [0u8; 16];
        random_bytes(&mut salt_bytes)?;
        let salt = hex::encode(salt_bytes);
        let challenge = altcha_hash(&salt, 7);
        self.send(&ServerFrame::PairChallenge {
            method: SECRET_AUTH.to_owned(),
            altcha: serde_json::json!({
                "algorithm": "SHA-256",
                "challenge": challenge,
                "salt": salt,
                "maxnumber": 64,
            }),
            challenge: Challenge {
                nonce: hex::encode(server_nonce),
            },
        })?;

        let (altcha, hostname, response) = match self.read()? {
            Some(ClientFrame::PairBegin {
                altcha,
                hostname,
                response,
            }) => (altcha, hostname, response),
            _ => {
                self.send_error(None, "bad_frame", "expected pair_begin")?;
                return Err(OsppError::Protocol("expected pair_begin".to_owned()));
            }
        };
        self.record("pair_begin");
        if !altcha_solution_matches(&altcha, &salt, &challenge) {
            self.send_error(None, "bad_proof", "altcha proof was not valid")?;
            return Err(OsppError::Protocol("bad altcha proof".to_owned()));
        }
        self.authenticate_response(&server_nonce, &response, true)?;

        let mut handle_bytes = [0u8; 32];
        random_bytes(&mut handle_bytes)?;
        let handle = hex::encode(handle_bytes);
        self.send(&ServerFrame::PairHandle {
            url: format!("http://{}/pair", self.shared.http_addr),
            handle: handle.clone(),
        })?;
        lock(&self.shared.state).pairings.push(PairingRecord {
            handle: handle.clone(),
            hostname,
            public_key: response.public_key,
            approved: false,
        });

        let deadline = Instant::now() + IO_TIMEOUT;
        loop {
            if self.shared.stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            if lock(&self.shared.decisions).remove(&handle) {
                self.send(&ServerFrame::PairApproved {
                    account: "alice".to_owned(),
                    machine: "dev-laptop".to_owned(),
                })?;
                if let Some(record) = lock(&self.shared.state)
                    .pairings
                    .iter_mut()
                    .find(|record| record.handle == handle)
                {
                    record.approved = true;
                }
                return Ok(());
            }
            if Instant::now() >= deadline {
                self.send_error(None, "throttled", "pairing verdict timed out")?;
                return Err(OsppError::Timeout);
            }
            match self.read() {
                Ok(None) => {}
                Ok(Some(_)) => {}
                Err(_) => return Ok(()),
            }
        }
    }

    fn read(&mut self) -> Result<Option<ClientFrame>, OsppError> {
        loop {
            match self.socket.read() {
                Ok(tungstenite::Message::Text(text)) => {
                    return serde_json::from_str::<ClientFrame>(text.as_str())
                        .map(Some)
                        .map_err(|error| OsppError::Protocol(error.to_string()));
                }
                Ok(tungstenite::Message::Binary(_)) => {
                    return Err(OsppError::Protocol(
                        "binary WebSocket frames are not part of ospp-v1".to_owned(),
                    ));
                }
                Ok(tungstenite::Message::Ping(payload)) => {
                    self.socket.send(tungstenite::Message::Pong(payload))?;
                }
                Ok(tungstenite::Message::Pong(_)) | Ok(tungstenite::Message::Frame(_)) => {}
                Ok(tungstenite::Message::Close(_)) => return Err(OsppError::Closed),
                Err(tungstenite::Error::Io(error))
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::TimedOut =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn send(&mut self, frame: &ServerFrame) -> Result<(), OsppError> {
        self.socket
            .send(tungstenite::Message::Text(frame.to_json()?.into()))?;
        Ok(())
    }

    fn send_error(&mut self, id: Option<&str>, code: &str, message: &str) -> Result<(), OsppError> {
        self.send(&ServerFrame::Error {
            id: id.map(str::to_owned),
            code: code.to_owned(),
            message: message.to_owned(),
        })
    }

    fn record(&mut self, name: &str) {
        lock(&self.shared.state).frames.push(name.to_owned());
    }
}

fn auth_bytes(server_nonce: &[u8], client_nonce: &[u8]) -> Vec<u8> {
    let mut transcript = b"ospp-v1".to_vec();
    transcript.extend_from_slice(server_nonce);
    transcript.extend_from_slice(client_nonce);
    transcript
}

fn pairing_bytes(server_nonce: &[u8], client_nonce: &[u8]) -> Vec<u8> {
    let mut transcript = b"ospp-v1-pair".to_vec();
    transcript.extend_from_slice(server_nonce);
    transcript.extend_from_slice(client_nonce);
    transcript
}

fn verify_signature(public_key_hex: &str, message: &[u8], signature_hex: &str) -> bool {
    let Ok(public_key) = hex::decode(public_key_hex) else {
        return false;
    };
    let Ok(verifying) = VerifyingKey::from_sec1_bytes(&public_key) else {
        return false;
    };
    let Ok(raw) = hex::decode(signature_hex) else {
        return false;
    };
    let Ok(bytes) = <[u8; 64]>::try_from(raw) else {
        return false;
    };
    let Ok(signature) = Signature::from_slice(&bytes) else {
        return false;
    };
    verifying.verify(message, &signature).is_ok()
}

fn altcha_hash(salt: &str, number: u64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(number.to_string().as_bytes());
    hex::encode(hasher.finalize())
}

fn altcha_solution_matches(solution: &str, salt: &str, challenge: &str) -> bool {
    let Ok(bytes) = STANDARD.decode(solution) else {
        return false;
    };
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    if payload["algorithm"] != "SHA-256"
        || payload["salt"] != salt
        || payload["challenge"] != challenge
    {
        return false;
    }
    let Some(number) = payload["number"].as_u64() else {
        return false;
    };
    altcha_hash(salt, number) == challenge
}

type RecipientPublic = <DhP256HkdfSha256 as Kem>::PublicKey;

fn recipient_public(public_key_hex: &str) -> Result<RecipientPublic, OsppError> {
    let bytes = hex::decode(public_key_hex)
        .map_err(|_| OsppError::InvalidKey("hpke public key is not hex".to_owned()))?;
    RecipientPublic::from_bytes(&bytes)
        .map_err(|_| OsppError::InvalidKey("hpke public key is not SEC1".to_owned()))
}

fn seal(
    public_key_hex: &str,
    info: &[u8],
    aad: &str,
    plaintext: &[u8],
) -> Result<Vec<u8>, OsppError> {
    let public_key = recipient_public(public_key_hex)?;
    let mut rng = FilledRng::new(256)?;
    let (enc, mut context) = setup_sender_with_rng::<AesGcm256, HkdfSha256, DhP256HkdfSha256>(
        &OpModeS::Base,
        &public_key,
        info,
        &mut rng,
    )
    .map_err(|error| OsppError::Hpke(error.to_string()))?;
    let ciphertext = context
        .seal(plaintext, aad.as_bytes())
        .map_err(|error| OsppError::Hpke(error.to_string()))?;
    let mut sealed = enc.to_bytes().as_slice().to_vec();
    sealed.extend_from_slice(&ciphertext);
    Ok(sealed)
}

fn seal_secret(
    public_key_hex: &str,
    component: &str,
    env_key: &str,
    plaintext: &str,
) -> Result<String, OsppError> {
    let aad = format!("{component}:{env_key}");
    seal(public_key_hex, SECRET_INFO, &aad, plaintext.as_bytes())
        .map(|sealed| STANDARD.encode(sealed))
}

/// Server-side sealed dump stream: `info = "ospp-v1-dump"`,
/// `aad = "<subscription_id>:<resource>"`, and a mandatory `len = 0` EOF frame.
pub fn seal_dump(
    public_key_hex: &str,
    subscription_id: &str,
    resource: &str,
    plaintext: &[u8],
) -> Result<Vec<u8>, OsppError> {
    let public_key = recipient_public(public_key_hex)?;
    let mut rng = FilledRng::new(256)?;
    let (enc, mut context) = setup_sender_with_rng::<AesGcm256, HkdfSha256, DhP256HkdfSha256>(
        &OpModeS::Base,
        &public_key,
        DUMP_INFO,
        &mut rng,
    )
    .map_err(|error| OsppError::Hpke(error.to_string()))?;
    let aad = format!("{subscription_id}:{resource}");
    let mut out = enc.to_bytes().as_slice().to_vec();
    for chunk in plaintext.chunks(64 * 1024) {
        let sealed = context
            .seal(chunk, aad.as_bytes())
            .map_err(|error| OsppError::Hpke(error.to_string()))?;
        out.extend_from_slice(&(chunk.len() as u32).to_be_bytes());
        out.extend_from_slice(&sealed);
    }
    let eof = context
        .seal(&[], aad.as_bytes())
        .map_err(|error| OsppError::Hpke(error.to_string()))?;
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&eof);
    Ok(out)
}

fn resolve_components(
    config: &ReferenceServerConfig,
    claims: &OsppClaims,
) -> BTreeMap<String, BTreeMap<String, TestValue>> {
    config
        .components
        .iter()
        .filter(|(component, _)| !claims.components.iter().any(|id| id == *component))
        .map(|(component, variables)| {
            let retained = variables
                .iter()
                .filter(|(name, _)| !claims.vars.contains(name) && !claims.secrets.contains(name))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect::<BTreeMap<_, _>>();
            (component.clone(), retained)
        })
        .filter(|(_, variables)| !variables.is_empty())
        .collect()
}

fn seal_components(
    resolved: &BTreeMap<String, BTreeMap<String, TestValue>>,
    public_key_hex: &str,
) -> Result<Components, OsppError> {
    let mut components = BTreeMap::new();
    for (component, variables) in resolved {
        let mut values = BTreeMap::new();
        for (name, value) in variables {
            let value = match value {
                TestValue::Plain(text) => serde_json::Value::String(text.clone()),
                TestValue::Secret(text) => serde_json::json!({
                    "encrypted": seal_secret(public_key_hex, component, name, text)?,
                }),
            };
            values.insert(name.clone(), value);
        }
        components.insert(component.clone(), values);
    }
    Ok(components)
}

fn random_bytes(buffer: &mut [u8]) -> Result<(), OsppError> {
    getrandom::fill(buffer).map_err(|error| OsppError::Entropy(error.to_string()))
}

/// A bounded, already-gathered CSPRNG adapter for `hpke` encapsulation.
struct FilledRng {
    buffer: Vec<u8>,
    offset: usize,
}

impl FilledRng {
    fn new(len: usize) -> Result<Self, OsppError> {
        let mut buffer = vec![0u8; len];
        random_bytes(&mut buffer)?;
        Ok(Self { buffer, offset: 0 })
    }

    fn take(&mut self, len: usize) -> &[u8] {
        assert!(
            self.offset + len <= self.buffer.len(),
            "reference-server hpke RNG buffer exhausted"
        );
        let start = self.offset;
        self.offset += len;
        &self.buffer[start..self.offset]
    }
}

impl TryRng for FilledRng {
    type Error = std::convert::Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let bytes = self.take(4);
        Ok(u32::from_le_bytes(bytes.try_into().expect("4 bytes")))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let bytes = self.take(8);
        Ok(u64::from_le_bytes(bytes.try_into().expect("8 bytes")))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        let len = dst.len();
        dst.copy_from_slice(self.take(len));
        Ok(())
    }
}

impl TryCryptoRng for FilledRng {}

#[cfg(test)]
mod tests {
    use super::*;
    use tungstenite::Message;
    use tungstenite::client::IntoClientRequest;

    #[test]
    fn a_version_one_hello_is_refused_with_unsupported_version() {
        let server = ReferenceOsppServer::start(ReferenceServerConfig::default());
        let mut request = server
            .endpoint()
            .as_str()
            .into_client_request()
            .expect("loopback ws request");
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static(PROTOCOL_SUBPROTOCOL),
        );
        let (mut socket, _) = tungstenite::connect(request).expect("handshake");
        socket
            .send(Message::Text(
                r#"{"type":"hello","version":1,"auth_methods":["p256"]}"#.into(),
            ))
            .expect("send version 1 hello");
        let Message::Text(reply) = socket.read().expect("refusal frame") else {
            panic!("expected a text error frame");
        };
        let json: serde_json::Value = serde_json::from_str(reply.as_str()).expect("error JSON");
        assert_eq!(json["type"], "error");
        assert_eq!(json["code"], "unsupported_version");
        server.shutdown();
    }
}
