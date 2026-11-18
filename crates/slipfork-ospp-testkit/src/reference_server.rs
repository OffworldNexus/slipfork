//! In-tree `ospp-v1` reference server: an independent implementation of
//! `docs/design/ospp.md` that the protected suite drives with the production client.
//!
//! It speaks real loopback WebSocket frames with real P-256 verification and real
//! RFC 9180 HPKE sealing, performs a single-pass `snow.yml` evaluation against the
//! ID-only `claims`, pushes live `env_updated` frames, and serves a sealed dump
//! stream from an out-of-band HTTP endpoint (plaintext HTTP on loopback only,
//! mirroring the `ws://`-on-loopback endpoint rule; the production client's
//! `https://` path uses `rustls` + `webpki-roots`).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hpke::aead::AesGcm256;
use hpke::kdf::HkdfSha256;
use hpke::kem::DhP256HkdfSha256;
use hpke::rand_core::{CryptoRng, TryCryptoRng, TryRng};
use hpke::{OpModeS, Serializable, setup_sender_with_rng};
use slipfork_ospp::wire::{
    AuthResponse, Challenge, ClientFrame, Encryption, ServerFrame, WireValue, auth_transcript,
    pairing_transcript,
};
use slipfork_ospp::{
    AUTH_METHOD_P256, ENCRYPTION_METHOD_HPKE_P256, OSPP_SUBPROTOCOL, OsppClaims, OsppEndpoint,
    OsppError, PROTOCOL_VERSION, altcha_challenge, altcha_challenge_hash, codes, dump_aad,
    parse_recipient_public_key, secret_aad, verify_altcha, verify_device_signature,
};
use slipfork_snow::faceless::{GeneratorSpec, TailModifier, Token, parse_faceless};
use slipfork_snow::{EntropySource, SnowFile};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::HeaderValue;
use tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tungstenite::{Message, WebSocket, accept_hdr};

use crate::signers::SeededEntropy;

const POLL_SLICE: Duration = Duration::from_millis(25);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HTTP_HEADERS: usize = 16 * 1024;

/// One server-resolved resource (`pg_databases.main`, `s3_buckets.media`, …).
#[derive(Debug, Clone)]
pub struct ResourceRecord {
    /// Non-sensitive coordinate (`url`, `endpoint`, …), returned in plaintext.
    pub url: String,
    /// Sensitive credential (`password`, `secret_key`), sealed when unclaimed.
    pub password: String,
}

/// Browser verdict a pending pairing resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairOutcome {
    /// `pair_approved { account, machine }`.
    Approve {
        /// Account name.
        account: String,
        /// Approved machine name.
        machine: String,
    },
    /// Typed `error` frame with this code.
    Reject {
        /// `rejected`, `already_paired`, `throttled`, or `bad_proof`.
        code: String,
    },
}

impl PairOutcome {
    /// The default approval used by `ReferenceServerConfig::default`.
    pub fn approved() -> Self {
        Self::Approve {
            account: "alice".to_owned(),
            machine: "dev-laptop".to_owned(),
        }
    }
}

/// Reference server configuration: authoritative server-side values.
#[derive(Debug, Clone)]
pub struct ReferenceServerConfig {
    /// Compressed P-256 public keys allowed to open an authenticated session.
    pub accepted_keys: Vec<String>,
    /// `snow.yml` mode the server evaluates (`all` by default).
    pub mode: String,
    /// Resolvable `vars::<KEY>` values.
    pub vars: BTreeMap<String, String>,
    /// Resolvable `secrets::<KEY>` values (always sealed to the client).
    pub secrets: BTreeMap<String, String>,
    /// Resolvable `resources::<kind>.<id>` records.
    pub resources: BTreeMap<String, ResourceRecord>,
    /// Resolvable `domains::<ID>` values.
    pub domains: BTreeMap<String, String>,
    /// Resource dumps by resource path.
    pub dumps: BTreeMap<String, Vec<u8>>,
    /// Verdict for a pending pairing unless the test overrides it.
    pub pair_outcome: PairOutcome,
    /// Approve immediately after `pair_handle` instead of waiting for the test.
    pub auto_approve: bool,
}

impl Default for ReferenceServerConfig {
    fn default() -> Self {
        Self {
            accepted_keys: Vec::new(),
            mode: "all".to_owned(),
            vars: BTreeMap::new(),
            secrets: BTreeMap::new(),
            resources: BTreeMap::new(),
            domains: BTreeMap::new(),
            dumps: BTreeMap::new(),
            pair_outcome: PairOutcome::approved(),
            auto_approve: true,
        }
    }
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
    /// Raw `snow.yml` content.
    pub snow: String,
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
    http_addr: SocketAddr,
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
            decisions: Mutex::new(BTreeMap::new()),
            entropy: SeededEntropy::new(0x5eed_1234_9abc_def0),
            http_addr,
            stop: Arc::clone(&stop),
        });

        let ws_thread = {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name("ospp-reference-server".to_owned())
                .spawn(move || accept_loop(listener, shared))
                .expect("spawn reference ws server")
        };
        let http_thread = {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name("ospp-reference-dumps".to_owned())
                .spawn(move || http_loop(http_listener, shared))
                .expect("spawn reference dump server")
        };

        Self {
            endpoint,
            http_addr,
            shared,
            threads: Mutex::new(vec![ws_thread, http_thread]),
            stop,
        }
    }

    /// The validated `ws://127.0.0.1:<port>/_ospp/v1` endpoint.
    pub fn endpoint(&self) -> OsppEndpoint {
        self.endpoint.clone()
    }

    /// The dump listener address.
    pub const fn dump_addr(&self) -> SocketAddr {
        self.http_addr
    }

    /// Snapshot of everything observed so far.
    pub fn state(&self) -> ServerState {
        lock(&self.shared.state).clone()
    }

    /// Replace one server-side `secrets::<KEY>` value.
    pub fn set_secret(&self, key: &str, value: &str) {
        lock(&self.shared.config)
            .secrets
            .insert(key.to_owned(), value.to_owned());
    }

    /// Replace one server-side `vars::<KEY>` value.
    pub fn set_var(&self, key: &str, value: &str) {
        lock(&self.shared.config)
            .vars
            .insert(key.to_owned(), value.to_owned());
    }

    /// Re-resolve and push `env_updated` for every active subscription.
    pub fn push_update(&self) -> Result<(), OsppError> {
        let shared = Arc::clone(&self.shared);
        let subscriptions: Vec<ActiveSubscription> =
            lock(&shared.subscriptions).values().cloned().collect();
        for subscription in subscriptions {
            let config = lock(&shared.config).clone();
            let resolved = resolve_components(
                &config,
                &subscription.record.claims,
                &subscription.record.snow,
            )
            .map_err(|(code, message)| {
                OsppError::server(Some(subscription.record.id.clone()), code, message)
            })?;
            let components = seal_components(
                &resolved,
                &subscription.record.public_key,
                &mut FilledRng::new(&shared.entropy, 8192)?,
            )?;
            let _ = subscription.outbound.send(ServerFrame::EnvUpdated {
                id: subscription.record.id.clone(),
                components,
            });
        }
        Ok(())
    }

    /// Simulate the browser approving a pending handle.
    pub fn approve_pairing(&self, handle: &str) {
        lock(&self.shared.decisions).insert(handle.to_owned(), PairOutcome::approved());
    }

    /// Simulate the browser rejecting a pending handle with a typed code.
    pub fn reject_pairing(&self, handle: &str, code: &str) {
        lock(&self.shared.decisions).insert(
            handle.to_owned(),
            PairOutcome::Reject {
                code: code.to_owned(),
            },
        );
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
    decisions: Mutex<BTreeMap<String, PairOutcome>>,
    entropy: SeededEntropy,
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

fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    while !shared.stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                let shared = Arc::clone(&shared);
                let _ = thread::Builder::new()
                    .name("ospp-reference-conn".to_owned())
                    .spawn(move || handle_connection(stream, shared));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_SLICE);
            }
            Err(_) => break,
        }
    }
}

#[allow(clippy::result_large_err)] // `tungstenite` fixes the handshake callback's error type
fn handle_connection(stream: TcpStream, shared: Arc<Shared>) {
    let recorder = Arc::clone(&shared);
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
                .any(|value| value.trim() == OSPP_SUBPROTOCOL)
            {
                return Err(tungstenite::http::Response::builder()
                    .status(400)
                    .body(Some("missing Sec-WebSocket-Protocol: ospp-v1".to_owned()))
                    .expect("static error response"));
            }
            response.headers_mut().insert(
                SEC_WEBSOCKET_PROTOCOL,
                HeaderValue::from_static(OSPP_SUBPROTOCOL),
            );
            Ok(response)
        };
    let mut socket = match accept_hdr(stream, callback) {
        Ok(socket) => socket,
        Err(_) => return,
    };
    let _ = socket.get_ref().set_read_timeout(Some(POLL_SLICE));
    let _ = serve(&mut socket, &shared);
    let _ = socket.close(None);
}

fn serve(socket: &mut WebSocket<TcpStream>, shared: &Arc<Shared>) -> Result<(), OsppError> {
    let first = loop {
        if shared.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        match read_client(socket)? {
            Some(frame) => break frame,
            None => continue,
        }
    };
    lock(&shared.state)
        .frames
        .push(frame_type(&first).to_owned());
    match first {
        ClientFrame::Hello {
            version,
            auth_methods,
        } => authenticated_session(socket, shared, version, auth_methods),
        ClientFrame::PairHello {
            version,
            key_methods,
        } => pairing_session(socket, shared, version, key_methods),
        other => {
            let _ = send_error(
                socket,
                None,
                codes::BAD_FRAME,
                "expected hello or pair_hello",
            );
            Err(OsppError::Protocol(format!(
                "unexpected first frame {}",
                frame_type(&other)
            )))
        }
    }
}

fn authenticated_session(
    socket: &mut WebSocket<TcpStream>,
    shared: &Arc<Shared>,
    version: u32,
    auth_methods: Vec<String>,
) -> Result<(), OsppError> {
    if version != PROTOCOL_VERSION {
        let _ = send_error(
            socket,
            None,
            codes::UNSUPPORTED_VERSION,
            "only ospp-v1 is supported",
        );
        return Err(OsppError::Protocol("unsupported version".to_owned()));
    }
    if !auth_methods.iter().any(|method| method == AUTH_METHOD_P256) {
        let _ = send_error(
            socket,
            None,
            codes::UNSUPPORTED_AUTH,
            "only p256 authentication is supported",
        );
        return Err(OsppError::Protocol("unsupported auth method".to_owned()));
    }

    let mut server_nonce = [0u8; 32];
    shared.entropy.fill_bytes(&mut server_nonce)?;
    send_frame(
        socket,
        &ServerFrame::AuthChallenge {
            method: AUTH_METHOD_P256.to_owned(),
            challenge: Challenge {
                nonce: hex::encode(server_nonce),
            },
        },
    )?;
    let response = match read_client(socket)? {
        Some(ClientFrame::AuthResponse { response }) => response,
        _ => {
            let _ = send_error(socket, None, codes::BAD_FRAME, "expected auth_response");
            return Err(OsppError::Protocol("expected auth_response".to_owned()));
        }
    };
    lock(&shared.state).frames.push("auth_response".to_owned());
    authenticate_response(socket, shared, &server_nonce, &response, false)?;
    send_frame(
        socket,
        &ServerFrame::AuthOk {
            encryption_methods: vec![ENCRYPTION_METHOD_HPKE_P256.to_owned()],
        },
    )?;
    session_loop(socket, shared)
}

fn authenticate_response(
    socket: &mut WebSocket<TcpStream>,
    shared: &Arc<Shared>,
    server_nonce: &[u8; 32],
    response: &AuthResponse,
    pairing: bool,
) -> Result<(), OsppError> {
    let client_nonce = hex::decode(&response.client_nonce)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .ok_or_else(|| OsppError::Protocol("client_nonce must be 32 bytes".to_owned()))?;
    if !pairing {
        let accepted = lock(&shared.config).accepted_keys.clone();
        if !accepted.iter().any(|key| key == &response.public_key) {
            let _ = send_error(
                socket,
                None,
                codes::UNAUTHORIZED_KEY,
                "machine key is not enrolled",
            );
            return Err(OsppError::Protocol("unauthorized key".to_owned()));
        }
    }
    let transcript = if pairing {
        pairing_transcript(server_nonce, &client_nonce)
    } else {
        auth_transcript(server_nonce, &client_nonce)
    };
    let signature = slipfork_ospp::DeviceSignature::from_hex(&response.signature)
        .map_err(|_| OsppError::InvalidSignature)?;
    if verify_device_signature(&response.public_key, &transcript, &signature).is_err() {
        let _ = send_error(socket, None, codes::BAD_PROOF, "signature did not verify");
        return Err(OsppError::InvalidSignature);
    }
    Ok(())
}

fn session_loop(socket: &mut WebSocket<TcpStream>, shared: &Arc<Shared>) -> Result<(), OsppError> {
    let (outbound, outbound_rx) = mpsc::channel();
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        while let Ok(frame) = outbound_rx.try_recv() {
            send_frame(socket, &frame)?;
        }
        let frame = match read_client(socket)? {
            Some(frame) => frame,
            None => continue,
        };
        lock(&shared.state)
            .frames
            .push(frame_type(&frame).to_owned());
        match frame {
            ClientFrame::Subscribe {
                id,
                remote,
                encryption,
                snow,
                claims,
            } => subscribe(
                socket, shared, &outbound, id, remote, encryption, snow, claims,
            )?,
            ClientFrame::Unsubscribe { id } => {
                lock(&shared.subscriptions).remove(&id);
                lock(&shared.state).subscriptions.remove(&id);
            }
            ClientFrame::DumpRequest {
                id,
                subscription_id,
                resource,
            } => dump(socket, shared, id, subscription_id, resource)?,
            other => {
                let _ = send_error(
                    socket,
                    None,
                    codes::BAD_FRAME,
                    "frame is not valid in an authenticated session",
                );
                return Err(OsppError::Protocol(format!(
                    "unexpected frame {}",
                    frame_type(&other)
                )));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn subscribe(
    socket: &mut WebSocket<TcpStream>,
    shared: &Arc<Shared>,
    outbound: &Sender<ServerFrame>,
    id: String,
    remote: String,
    encryption: Encryption,
    snow: String,
    claims: OsppClaims,
) -> Result<(), OsppError> {
    if encryption.method != ENCRYPTION_METHOD_HPKE_P256 {
        send_error(
            socket,
            Some(&id),
            codes::UNSUPPORTED_ENCRYPTION,
            "only hpke-p256 is supported",
        )?;
        return Ok(());
    }
    let config = lock(&shared.config).clone();
    let resolved = match resolve_components(&config, &claims, &snow) {
        Ok(resolved) => resolved,
        Err((code, message)) => {
            send_error(socket, Some(&id), &code, &message)?;
            return Ok(());
        }
    };
    let components = seal_components(
        &resolved,
        &encryption.public_key,
        &mut FilledRng::new(&shared.entropy, 8192)?,
    )?;
    send_frame(
        socket,
        &ServerFrame::EnvResolved {
            id: id.clone(),
            components,
        },
    )?;
    let record = SubscriptionRecord {
        id: id.clone(),
        remote,
        snow,
        claims,
        public_key: encryption.public_key,
    };
    lock(&shared.subscriptions).insert(
        id.clone(),
        ActiveSubscription {
            record: record.clone(),
            outbound: outbound.clone(),
        },
    );
    lock(&shared.state).subscriptions.insert(id, record);
    Ok(())
}

fn dump(
    socket: &mut WebSocket<TcpStream>,
    shared: &Arc<Shared>,
    id: String,
    subscription_id: String,
    resource: String,
) -> Result<(), OsppError> {
    let public_key = lock(&shared.subscriptions)
        .get(&subscription_id)
        .map(|subscription| subscription.record.public_key.clone());
    let bytes = lock(&shared.config).dumps.get(&resource).cloned();
    let (Some(public_key), Some(bytes)) = (public_key, bytes) else {
        send_error(
            socket,
            Some(&id),
            codes::DUMP_UNAVAILABLE,
            "no dump is available for that resource",
        )?;
        return Ok(());
    };
    let mut token_bytes = [0u8; 16];
    shared.entropy.fill_bytes(&mut token_bytes)?;
    let token = hex::encode(token_bytes);
    lock(&shared.dumps).insert(
        token.clone(),
        DumpEntry {
            public_key,
            subscription_id,
            resource,
            bytes,
        },
    );
    lock(&shared.state).dump_urls += 1;
    send_frame(
        socket,
        &ServerFrame::DumpReady {
            id,
            url: format!("http://{}/dumps/{}", shared.http_addr, token),
        },
    )
}

fn pairing_session(
    socket: &mut WebSocket<TcpStream>,
    shared: &Arc<Shared>,
    version: u32,
    key_methods: Vec<String>,
) -> Result<(), OsppError> {
    if version != PROTOCOL_VERSION {
        send_error(
            socket,
            None,
            codes::UNSUPPORTED_VERSION,
            "only ospp-v1 is supported",
        )?;
        return Err(OsppError::Protocol("unsupported version".to_owned()));
    }
    if !key_methods.iter().any(|method| method == AUTH_METHOD_P256) {
        send_error(
            socket,
            None,
            codes::UNSUPPORTED_AUTH,
            "only p256 key proof is supported",
        )?;
        return Err(OsppError::Protocol("unsupported key method".to_owned()));
    }

    let mut server_nonce = [0u8; 32];
    shared.entropy.fill_bytes(&mut server_nonce)?;
    let mut salt_bytes = [0u8; 16];
    shared.entropy.fill_bytes(&mut salt_bytes)?;
    let salt = hex::encode(salt_bytes);
    let mut number_bytes = [0u8; 1];
    shared.entropy.fill_bytes(&mut number_bytes)?;
    let number = u64::from(number_bytes[0] % 64);
    let challenge = altcha_challenge_hash(&salt, number);
    send_frame(
        socket,
        &ServerFrame::PairChallenge {
            method: AUTH_METHOD_P256.to_owned(),
            altcha: altcha_challenge(&salt, number, 64),
            challenge: Challenge {
                nonce: hex::encode(server_nonce),
            },
        },
    )?;

    let (altcha, hostname, response) = match read_client(socket)? {
        Some(ClientFrame::PairBegin {
            altcha,
            hostname,
            response,
        }) => (altcha, hostname, response),
        _ => {
            send_error(socket, None, codes::BAD_FRAME, "expected pair_begin")?;
            return Err(OsppError::Protocol("expected pair_begin".to_owned()));
        }
    };
    lock(&shared.state).frames.push("pair_begin".to_owned());
    if !verify_altcha(&altcha, &salt, &challenge) {
        send_error(socket, None, codes::BAD_PROOF, "altcha proof was not valid")?;
        return Err(OsppError::Protocol("bad altcha proof".to_owned()));
    }
    authenticate_response(socket, shared, &server_nonce, &response, true)?;

    let mut handle_bytes = [0u8; 32];
    shared.entropy.fill_bytes(&mut handle_bytes)?;
    let handle = hex::encode(handle_bytes);
    send_frame(
        socket,
        &ServerFrame::PairHandle {
            url: format!("http://{}/pair", shared.http_addr),
            handle: handle.clone(),
        },
    )?;
    lock(&shared.state).pairings.push(PairingRecord {
        handle: handle.clone(),
        hostname,
        public_key: response.public_key,
        approved: false,
    });

    let config = lock(&shared.config).clone();
    if config.auto_approve {
        lock(&shared.decisions).insert(handle.clone(), config.pair_outcome.clone());
    }
    let deadline = Instant::now() + IO_TIMEOUT;
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        if let Some(outcome) = lock(&shared.decisions).remove(&handle) {
            match outcome {
                PairOutcome::Approve { account, machine } => {
                    send_frame(socket, &ServerFrame::PairApproved { account, machine })?;
                    if let Some(record) = lock(&shared.state)
                        .pairings
                        .iter_mut()
                        .find(|record| record.handle == handle)
                    {
                        record.approved = true;
                    }
                    return Ok(());
                }
                PairOutcome::Reject { code } => {
                    send_error(socket, None, &code, "pairing was rejected")?;
                    return Err(OsppError::Protocol("pairing rejected".to_owned()));
                }
            }
        }
        if Instant::now() >= deadline {
            send_error(socket, None, codes::THROTTLED, "pairing verdict timed out")?;
            return Err(OsppError::Timeout);
        }
        let _ = read_client(socket)?;
    }
}

fn read_client(socket: &mut WebSocket<TcpStream>) -> Result<Option<ClientFrame>, OsppError> {
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => {
                return ClientFrame::parse(text.as_str()).map(Some);
            }
            Ok(Message::Binary(_)) => {
                return Err(OsppError::Protocol(
                    "binary WebSocket frames are not part of ospp-v1".to_owned(),
                ));
            }
            Ok(Message::Ping(payload)) => {
                socket.send(Message::Pong(payload))?;
            }
            Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
            Ok(Message::Close(_)) => return Err(OsppError::Closed),
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

fn send_frame(socket: &mut WebSocket<TcpStream>, frame: &ServerFrame) -> Result<(), OsppError> {
    socket.send(Message::Text(frame.to_json()?.into()))?;
    Ok(())
}

fn send_error(
    socket: &mut WebSocket<TcpStream>,
    id: Option<&str>,
    code: &str,
    message: &str,
) -> Result<(), OsppError> {
    send_frame(
        socket,
        &ServerFrame::Error {
            id: id.map(str::to_owned),
            code: code.to_owned(),
            message: message.to_owned(),
        },
    )
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

fn http_loop(listener: TcpListener, shared: Arc<Shared>) {
    while !shared.stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _peer)) => handle_http(stream, &shared),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_SLICE);
            }
            Err(_) => break,
        }
    }
}

fn handle_http(mut stream: TcpStream, shared: &Arc<Shared>) {
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
        let _ = respond(&mut stream, 404, b"not found");
        return;
    };
    let entry = lock(&shared.dumps).remove(token);
    let Some(entry) = entry else {
        let _ = respond(&mut stream, 404, b"unknown dump token");
        return;
    };
    match seal_dump(
        &entry.public_key,
        &entry.subscription_id,
        &entry.resource,
        &entry.bytes,
        &shared.entropy,
    ) {
        Ok(body) => {
            let _ = respond(&mut stream, 200, &body);
        }
        Err(_) => {
            let _ = respond(&mut stream, 500, b"sealing failure");
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

/// Server-side sealed secret: `info = "ospp-v1-secret"`, `aad = "<component>:<env_key>"`.
pub fn seal_secret(
    public_key_hex: &str,
    component: &str,
    env_key: &str,
    plaintext: &str,
    entropy: &dyn EntropySource,
) -> Result<String, OsppError> {
    let mut rng = FilledRng::new(entropy, 128)?;
    seal_secret_with_rng(public_key_hex, component, env_key, plaintext, &mut rng)
}

fn seal_secret_with_rng(
    public_key_hex: &str,
    component: &str,
    env_key: &str,
    plaintext: &str,
    rng: &mut impl CryptoRng,
) -> Result<String, OsppError> {
    let public_key = parse_recipient_public_key(public_key_hex)?;
    let (enc, mut context) = setup_sender_with_rng::<AesGcm256, HkdfSha256, DhP256HkdfSha256>(
        &OpModeS::Base,
        &public_key,
        slipfork_ospp::SECRET_INFO,
        rng,
    )
    .map_err(|error| OsppError::Hpke(error.to_string()))?;
    let ciphertext = context
        .seal(
            plaintext.as_bytes(),
            secret_aad(component, env_key).as_bytes(),
        )
        .map_err(|error| OsppError::Hpke(error.to_string()))?;
    let mut sealed = enc.to_bytes().as_slice().to_vec();
    sealed.extend_from_slice(&ciphertext);
    Ok(STANDARD.encode(sealed))
}

/// Server-side sealed dump stream: `SetupBaseS` with `info = "ospp-v1-dump"`,
/// `aad = "<subscription_id>:<resource>"`, and a mandatory `len = 0` EOF frame.
pub fn seal_dump(
    public_key_hex: &str,
    subscription_id: &str,
    resource: &str,
    plaintext: &[u8],
    entropy: &dyn EntropySource,
) -> Result<Vec<u8>, OsppError> {
    let mut rng = FilledRng::new(entropy, 4096)?;
    seal_dump_with_rng(
        public_key_hex,
        subscription_id,
        resource,
        plaintext,
        &mut rng,
    )
}

fn seal_dump_with_rng(
    public_key_hex: &str,
    subscription_id: &str,
    resource: &str,
    plaintext: &[u8],
    rng: &mut impl CryptoRng,
) -> Result<Vec<u8>, OsppError> {
    let public_key = parse_recipient_public_key(public_key_hex)?;
    let (enc, mut context) = setup_sender_with_rng::<AesGcm256, HkdfSha256, DhP256HkdfSha256>(
        &OpModeS::Base,
        &public_key,
        slipfork_ospp::DUMP_INFO,
        rng,
    )
    .map_err(|error| OsppError::Hpke(error.to_string()))?;
    let aad = dump_aad(subscription_id, resource);
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

fn seal_components(
    resolved: &BTreeMap<String, BTreeMap<String, Resolved>>,
    public_key_hex: &str,
    rng: &mut FilledRng,
) -> Result<BTreeMap<String, BTreeMap<String, WireValue>>, OsppError> {
    let mut components = BTreeMap::new();
    for (component, variables) in resolved {
        let mut values = BTreeMap::new();
        for (name, value) in variables {
            let value = if value.secret {
                WireValue::Encrypted(seal_secret_with_rng(
                    public_key_hex,
                    component,
                    name,
                    &value.text,
                    rng,
                )?)
            } else {
                WireValue::Plain(value.text.clone())
            };
            values.insert(name.clone(), value);
        }
        components.insert(component.clone(), values);
    }
    Ok(components)
}

#[derive(Debug, Clone)]
struct Resolved {
    text: String,
    secret: bool,
}

fn resolve_components(
    config: &ReferenceServerConfig,
    claims: &OsppClaims,
    snow: &str,
) -> Result<BTreeMap<String, BTreeMap<String, Resolved>>, (String, String)> {
    let file = SnowFile::parse_yaml(snow)
        .map_err(|error| (codes::UNRESOLVED.to_owned(), error.to_string()))?;
    let groups = file
        .effective_env_groups(&config.mode)
        .map_err(|error| (codes::UNRESOLVED.to_owned(), error.to_string()))?;
    let mut components = BTreeMap::new();
    for (component, variables) in groups {
        if claims.components.iter().any(|id| id == &component) {
            // A locally owned component is neither resolved nor returned.
            continue;
        }
        let mut resolved = BTreeMap::new();
        for (name, template) in variables {
            let value = resolve_template(config, claims, &template)?;
            resolved.insert(name, value);
        }
        components.insert(component, resolved);
    }
    Ok(components)
}

fn resolve_template(
    config: &ReferenceServerConfig,
    claims: &OsppClaims,
    template: &str,
) -> Result<Resolved, (String, String)> {
    let mut out = String::new();
    let mut secret = false;
    let mut rest = template;
    while let Some(start) = rest.find("${{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        let end = after
            .find("}}")
            .ok_or_else(|| (codes::UNRESOLVED.to_owned(), "unclosed template".to_owned()))?;
        let expression = &rest[start..start + 3 + end + 2];
        let tokens = parse_faceless(expression)
            .map_err(|error| (codes::UNRESOLVED.to_owned(), error.to_string()))?;
        let token = tokens
            .into_iter()
            .find(|token| matches!(token, Token::Expr { .. }))
            .ok_or_else(|| (codes::UNRESOLVED.to_owned(), "empty expression".to_owned()))?;
        let Token::Expr {
            namespace,
            path,
            modifier,
            ..
        } = token
        else {
            unreachable!("filtered above")
        };
        if claimed(claims, &namespace, &path) {
            // Claimed entries stay byte-verbatim; nothing is resolved or refused.
            out.push_str(expression);
        } else {
            let (value, contaminated) = resolve_expression(config, &namespace, &path, &modifier)?;
            secret |= contaminated;
            out.push_str(&value);
        }
        rest = &rest[start + 3 + end + 2..];
    }
    out.push_str(rest);
    Ok(Resolved { text: out, secret })
}

fn claimed(claims: &OsppClaims, namespace: &str, path: &[String]) -> bool {
    match namespace {
        "vars" => path.len() == 1 && claims.vars.iter().any(|key| key == &path[0]),
        "secrets" => path.len() == 1 && claims.secrets.iter().any(|key| key == &path[0]),
        "domains" => path.len() == 1 && claims.domains.iter().any(|id| id == &path[0]),
        "components" => path.len() == 1 && claims.components.iter().any(|id| id == &path[0]),
        "resources" => {
            path.len() >= 2
                && claims
                    .resources
                    .get(&path[0])
                    .is_some_and(|ids| ids.iter().any(|id| id == &path[1]))
        }
        _ => false,
    }
}

fn resolve_expression(
    config: &ReferenceServerConfig,
    namespace: &str,
    path: &[String],
    modifier: &TailModifier,
) -> Result<(String, bool), (String, String)> {
    let missing = || {
        (
            codes::UNRESOLVED.to_owned(),
            format!(
                "unclaimed ${{{{ {namespace}::{} }}}} has no value",
                path.join(".")
            ),
        )
    };
    match namespace {
        "vars" => {
            let key = path.join(".");
            match config.vars.get(&key) {
                Some(value) => Ok((value.clone(), false)),
                None => match modifier {
                    TailModifier::Fallback(value) => Ok((value.clone(), false)),
                    _ => Err(missing()),
                },
            }
        }
        "secrets" => {
            let key = path.join(".");
            match config.secrets.get(&key) {
                Some(value) => Ok((value.clone(), true)),
                None => match modifier {
                    TailModifier::Fallback(value) => Ok((value.clone(), true)),
                    TailModifier::Ensure(generator) => Ok((generate(generator)?, true)),
                    TailModifier::None => Err(missing()),
                },
            }
        }
        "resources" => {
            if path.len() < 2 {
                return Err(missing());
            }
            let id = format!("{}.{}", path[0], path[1]);
            let record = config.resources.get(&id).ok_or_else(missing)?;
            match path.get(2).map(String::as_str) {
                Some("url") => Ok((record.url.clone(), false)),
                Some("password") => Ok((record.password.clone(), true)),
                _ => Err(missing()),
            }
        }
        "domains" => config
            .domains
            .get(&path.join("."))
            .cloned()
            .map(|value| (value, false))
            .ok_or_else(missing),
        _ => Err(missing()),
    }
}

fn generate(generator: &GeneratorSpec) -> Result<String, (String, String)> {
    generator
        .generate(&SeededEntropy::new(0x9e37_79b9_7f4a_7c15))
        .map_err(|error| (codes::UNRESOLVED.to_owned(), error.to_string()))
}

/// A bounded, already-gathered CSPRNG adapter for `hpke` encapsulation.
///
/// P-256 encapsulation consumes a handful of 32-byte scalars; the buffer is
/// sized far above that, and exhaustion is a programming error rather than a
/// silent weak-randomness fallback.
struct FilledRng {
    buffer: Vec<u8>,
    offset: usize,
}

impl FilledRng {
    fn new(entropy: &dyn EntropySource, len: usize) -> Result<Self, OsppError> {
        let mut buffer = vec![0u8; len];
        entropy.fill_bytes(&mut buffer)?;
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

    #[test]
    fn claimed_expressions_survive_byte_verbatim() {
        let config = ReferenceServerConfig {
            vars: BTreeMap::from([("DEBUG".to_owned(), "server".to_owned())]),
            ..ReferenceServerConfig::default()
        };
        let claims = OsppClaims {
            vars: vec!["DEBUG".to_owned()],
            ..OsppClaims::default()
        };
        let snow = r#"
images: []
modes:
  all:
    envs:
      api:
        DEBUG: "  ${{ vars::DEBUG ?? \"x\" }}  "
        LOG_LEVEL: "info"
"#;
        let components = resolve_components(&config, &claims, snow).expect("resolves");
        assert_eq!(
            components["api"]["DEBUG"].text,
            "  ${{ vars::DEBUG ?? \"x\" }}  "
        );
        assert_eq!(components["api"]["LOG_LEVEL"].text, "info");
    }

    #[test]
    fn unclaimed_secrets_contaminate_and_claim_nothing() {
        let config = ReferenceServerConfig {
            secrets: BTreeMap::from([("SECRET_KEY".to_owned(), "s3cr3t".to_owned())]),
            ..ReferenceServerConfig::default()
        };
        let snow = r#"
images: []
modes:
  all:
    envs:
      api:
        SECRET_KEY: "${{ secrets::SECRET_KEY }}"
"#;
        let components =
            resolve_components(&config, &OsppClaims::default(), snow).expect("resolves");
        assert!(components["api"]["SECRET_KEY"].secret);
        assert_eq!(components["api"]["SECRET_KEY"].text, "s3cr3t");
    }
}
