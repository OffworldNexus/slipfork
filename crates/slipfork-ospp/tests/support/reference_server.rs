//! In-tree `ospp-v1` **reference server**: the independent implementation the protected suite
//! drives with the production client.
//!
//! It speaks the same `ADR-0008` §1–§2 frames over a real loopback WebSocket: server-first
//! `CHALLENGE`, real P-256 signature verification, fragment-handle pairing, and all five
//! capability frames (`env.bundle`, `secrets.ensure`, `resources.describe`, `dump.begin`,
//! `dump.end`). `secrets.ensure` is a real compare-and-set over a mutex-held map, dumps are real
//! `zstd::encode_all` archives streamed in bounded binary frames, and the rejection knobs exist
//! only to prove fail-closed client behaviour.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use p256::ecdsa::VerifyingKey;
use p256::ecdsa::signature::hazmat::PrehashVerifier;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use slipfork_ospp::{KeyId, MAX_DUMP_FRAME, OsppEndpoint, PairingRejectReason, RejectReason};
use slipfork_snow::{EntropySource, ProviderError};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::HeaderValue;
use tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tungstenite::{Message, WebSocket, accept_hdr};

/// Server-side read slice; a stopped server notices within this bound.
const READ_SLICE: Duration = Duration::from_millis(100);

/// Deterministic wall clock used by the reference server for pairing TTLs.
#[derive(Debug, Default)]
pub struct ReferenceClock {
    now_ms: AtomicU64,
}

impl ReferenceClock {
    /// Start the clock at `now_ms`.
    pub fn new(now_ms: u64) -> Self {
        Self {
            now_ms: AtomicU64::new(now_ms),
        }
    }

    /// Current time in milliseconds.
    pub fn now_ms(&self) -> u64 {
        self.now_ms.load(Ordering::SeqCst)
    }

    /// Advance by `millis`.
    pub fn advance(&self, millis: u64) {
        self.now_ms.fetch_add(millis, Ordering::SeqCst);
    }
}

/// Seeded CSPRNG double for `nonce_c` (and any client-side entropy in tests).
#[derive(Debug, Default)]
pub struct SeededEntropy {
    state: AtomicU64,
}

impl SeededEntropy {
    /// Seed the generator.
    pub fn new(seed: u64) -> Self {
        Self {
            state: AtomicU64::new(seed),
        }
    }

    fn next_u64(&self) -> u64 {
        // splitmix64: tiny, deterministic, and good enough for test nonces.
        let mut z = self
            .state
            .fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::SeqCst)
            .wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

impl EntropySource for SeededEntropy {
    fn fill_bytes(&self, buf: &mut [u8]) -> Result<(), ProviderError> {
        for chunk in buf.chunks_mut(8) {
            let bytes = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
        Ok(())
    }
}

/// A pairing handle the server will honour.
#[derive(Debug, Clone)]
pub struct HandleRecord {
    /// Raw 32-byte handle.
    pub handle: [u8; 32],
    /// Clock time at which the five-minute window ends.
    pub expires_at_ms: u64,
}

impl HandleRecord {
    /// A handle valid for the standard five minutes from `now_ms`.
    pub fn new(handle: [u8; 32], now_ms: u64) -> Self {
        Self {
            handle,
            expires_at_ms: now_ms + 5 * 60 * 1000,
        }
    }
}

/// One S3 bucket the reference server offers (`s3_buckets.<id>`).
#[derive(Debug, Clone)]
pub struct S3BucketRecord {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
}

impl S3BucketRecord {
    /// Construct one offered bucket.
    pub fn new(
        endpoint: impl Into<String>,
        region: impl Into<String>,
        bucket: impl Into<String>,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            region: region.into(),
            bucket: bucket.into(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
        }
    }
}

/// One PostgreSQL seed dump the reference server can stream (`pg_databases.<id>`).
#[derive(Debug, Clone)]
pub struct DumpRecord {
    /// Database id the archive belongs to.
    pub db_id: String,
    /// Uncompressed `pg_dump --format=custom` bytes.
    pub archive: Vec<u8>,
}

impl DumpRecord {
    /// Register an archive for `db_id`.
    pub fn new(db_id: impl Into<String>, archive: impl Into<Vec<u8>>) -> Self {
        Self {
            db_id: db_id.into(),
            archive: archive.into(),
        }
    }

    /// The zstd-compressed archive the server streams.
    pub fn compressed(&self) -> Vec<u8> {
        zstd::encode_all(self.archive.as_slice(), 3).expect("zstd compression")
    }

    /// SHA-256 of [`Self::compressed`] as lowercase hex.
    pub fn sha256_compressed(&self) -> String {
        hex::encode(Sha256::digest(self.compressed()))
    }
}

/// Reference-server behaviour knobs.
#[derive(Debug, Clone)]
pub struct ReferenceServerConfig {
    /// Public keys the server will accept.
    pub keys: Vec<KeyId>,
    /// Force a typed `REJECT` after any `HELLO`.
    pub forced_reject: Option<RejectReason>,
    /// Reuse this exact `nonce_s` on every connection (used to prove replay refusal).
    pub fixed_nonce: Option<[u8; 32]>,
    /// Send `WELCOME` before any `CHALLENGE`.
    pub welcome_before_challenge: bool,
    /// Send an `env.bundle` capability frame before any `CHALLENGE`.
    pub capability_before_challenge: bool,
    /// Send a >16 KiB first frame.
    pub oversized_first_frame: bool,
    /// Accept the upgrade and then send no frame at all (deadline proof).
    pub silent_first_frame: bool,
    /// Pairing handles this server will honour.
    pub handles: Vec<HandleRecord>,
    /// Account returned on a successful pairing.
    pub account: String,
    /// Seed `vars` served by `env.bundle`.
    pub vars: BTreeMap<String, String>,
    /// Seed `secrets`; `secrets.ensure` compares-and-sets over this map.
    pub secrets: BTreeMap<String, String>,
    /// Seed `domains`; served only in `cloud` mode.
    pub domains: BTreeMap<String, String>,
    /// S3 buckets offered by `resources.describe`.
    pub s3_buckets: BTreeMap<String, S3BucketRecord>,
    /// PostgreSQL dumps offered by `resources.describe` / `dump.begin`.
    pub pg_databases: BTreeMap<String, DumpRecord>,
    /// Answer every capability with `capability.error { kind: "not_authenticated" }`.
    pub deny_capabilities: bool,
    /// Advertise a dump, send part of it, then close before `dump.end`.
    pub truncate_dump: bool,
    /// Stream bytes that do not hash to the advertised digest.
    pub corrupt_dump: bool,
    /// Send one binary frame larger than the 64 KiB cap.
    pub oversized_dump_frame: bool,
    /// Advertised `dump.begin.format` (defaults to `pg_custom_zstd`).
    pub dump_format: Option<String>,
    /// Binary frame payload size for a normal dump stream.
    pub dump_chunk_bytes: usize,
}

impl Default for ReferenceServerConfig {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            forced_reject: None,
            fixed_nonce: None,
            welcome_before_challenge: false,
            capability_before_challenge: false,
            oversized_first_frame: false,
            silent_first_frame: false,
            handles: Vec::new(),
            account: String::new(),
            vars: BTreeMap::new(),
            secrets: BTreeMap::new(),
            domains: BTreeMap::new(),
            s3_buckets: BTreeMap::new(),
            pg_databases: BTreeMap::new(),
            deny_capabilities: false,
            truncate_dump: false,
            corrupt_dump: false,
            oversized_dump_frame: false,
            dump_format: None,
            dump_chunk_bytes: 4096,
        }
    }
}

/// A recorded HTTP upgrade request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    /// Requested path.
    pub path: String,
    /// Requested subprotocol.
    pub subprotocol: Option<String>,
}

/// A recorded `HELLO`/`pair_hello`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectAttempt {
    /// `type` field.
    pub kind: String,
    /// `version` field.
    pub version: u16,
    /// `key_id` field.
    pub key_id: String,
    /// `nonce_c` field.
    pub nonce_c: String,
}

/// A recorded capability request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCapability {
    /// Capability wire `type`.
    pub capability: String,
    /// Requested project.
    pub project: String,
    /// Requested mode.
    pub mode: String,
    /// Client-chosen request id.
    pub request_id: u32,
}

/// One observed `secrets.ensure` compare-and-set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsureCall {
    /// Secret key.
    pub key: String,
    /// Value the client proposed.
    pub proposed: String,
    /// Authoritative value the server returned.
    pub returned: String,
}

/// Observable server state for assertions.
#[derive(Debug, Default)]
pub struct ReferenceState {
    /// Every upgrade request, in order.
    pub requests: Vec<RecordedRequest>,
    /// Every handshake attempt, in order.
    pub attempts: Vec<ConnectAttempt>,
    /// Every pairing handle that reached a `pair_hello`.
    pub handles_seen: Vec<[u8; 32]>,
    /// Every capability request, in order.
    pub capability_requests: Vec<RecordedCapability>,
    /// Every `secrets.ensure` compare-and-set, in order.
    pub ensure_calls: Vec<EnsureCall>,
    /// Database ids streamed by `dump.begin`.
    pub dumps_served: Vec<String>,
    /// Binary dump frames sent.
    pub dump_frames: usize,
    /// Compressed dump bytes sent.
    pub dump_bytes: u64,
    secrets: BTreeMap<String, String>,
    used_nonces: BTreeSet<[u8; 32]>,
    redeemed: BTreeSet<[u8; 32]>,
}

impl ReferenceState {
    /// Whether `nonce` was already consumed by an accepted handshake.
    pub fn nonce_used(&self, nonce: &[u8; 32]) -> bool {
        self.used_nonces.contains(nonce)
    }

    /// Whether `handle` was already redeemed.
    pub fn handle_redeemed(&self, handle: &[u8; 32]) -> bool {
        self.redeemed.contains(handle)
    }

    /// The authoritative stored secret for `key`, if any.
    pub fn secret_value(&self, key: &str) -> Option<&str> {
        self.secrets.get(key).map(String::as_str)
    }
}

/// A running in-process reference server.
pub struct ReferenceOsppServer {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    state: Arc<Mutex<ReferenceState>>,
    clock: Arc<ReferenceClock>,
    thread: Option<JoinHandle<()>>,
}

impl ReferenceOsppServer {
    /// Bind `127.0.0.1:0` and start serving.
    pub fn start(config: ReferenceServerConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind reference server");
        let addr = listener.local_addr().expect("local addr");
        listener
            .set_nonblocking(true)
            .expect("non-blocking listener");
        let shutdown = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(ReferenceState {
            secrets: config.secrets.clone(),
            ..ReferenceState::default()
        }));
        let clock = Arc::new(ReferenceClock::new(1_000));
        let config = Arc::new(config);
        let nonce_seed = Arc::new(AtomicU64::new(0xa5a5_1234_5678_9abc));

        let thread = {
            let shutdown = Arc::clone(&shutdown);
            let state = Arc::clone(&state);
            let clock = Arc::clone(&clock);
            thread::spawn(move || {
                while !shutdown.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nodelay(true).ok();
                            handle_connection(
                                stream,
                                &config,
                                &state,
                                &clock,
                                &nonce_seed,
                                &shutdown,
                            );
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        Self {
            addr,
            shutdown,
            state,
            clock,
            thread: Some(thread),
        }
    }

    /// A connect endpoint for [`slipfork_ospp::OsppClient`].
    pub fn endpoint(&self) -> OsppEndpoint {
        OsppEndpoint::connect("127.0.0.1", self.addr.port(), false).expect("loopback endpoint")
    }

    /// A browser pairing URL carrying `handle`.
    pub fn browser_pairing_url(&self, handle: [u8; 32]) -> String {
        format!(
            "http://127.0.0.1:{}/pair#handle={}",
            self.addr.port(),
            hex::encode(handle)
        )
    }

    /// The deterministic clock shared with the server.
    pub fn clock(&self) -> &ReferenceClock {
        &self.clock
    }

    /// Snapshot the observable state.
    pub fn state(&self) -> MutexGuard<'_, ReferenceState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for ReferenceOsppServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Wake a blocking accept so the thread observes the flag promptly.
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[allow(clippy::result_large_err)] // tungstenite's `Callback` dictates the large `ErrorResponse`.
fn handle_connection(
    stream: TcpStream,
    config: &Arc<ReferenceServerConfig>,
    state: &Arc<Mutex<ReferenceState>>,
    clock: &Arc<ReferenceClock>,
    nonce_seed: &Arc<AtomicU64>,
    shutdown: &Arc<AtomicBool>,
) {
    let recorded = Arc::new(Mutex::new(None::<String>));
    let recorded_in_callback = Arc::clone(&recorded);
    let state_in_callback = Arc::clone(state);
    let accepted = accept_hdr(stream, move |request: &Request, mut response: Response| {
        let path = request.uri().path().to_owned();
        let subprotocol = request
            .headers()
            .get(SEC_WEBSOCKET_PROTOCOL)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        state_in_callback
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .requests
            .push(RecordedRequest {
                path: path.clone(),
                subprotocol,
            });
        *recorded_in_callback
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(path);
        response
            .headers_mut()
            .insert(SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_static("ospp-v1"));
        Ok::<_, ErrorResponse>(response)
    });
    let Ok(mut socket) = accepted else {
        return;
    };
    let _ = socket.get_ref().set_read_timeout(Some(READ_SLICE));
    let path = recorded
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .unwrap_or_default();
    match path.as_str() {
        "/_ospp/v1/connect" => handle_connect(&mut socket, config, state, nonce_seed, shutdown),
        "/_ospp/v1/pair" => handle_pair(&mut socket, config, state, clock, nonce_seed, shutdown),
        _ => {
            let _ = socket.close(None);
        }
    }
    let _ = socket.close(None);
}

fn handle_connect(
    socket: &mut WebSocket<TcpStream>,
    config: &ReferenceServerConfig,
    state: &Mutex<ReferenceState>,
    nonce_seed: &AtomicU64,
    shutdown: &AtomicBool,
) {
    if config.silent_first_frame {
        // The client's injected handshake deadline must fire before this returns.
        thread::sleep(Duration::from_millis(500));
        return;
    }
    if config.welcome_before_challenge {
        send_json(socket, json!({"type": "welcome", "version": 1}));
        return;
    }
    if config.capability_before_challenge {
        send_json(
            socket,
            json!({
                "type": "env.bundle",
                "request_id": 1,
                "vars": {},
                "secrets": {},
                "domains": {},
            }),
        );
        return;
    }
    if config.oversized_first_frame {
        send_json(
            socket,
            json!({
                "type": "challenge",
                "nonce_s": hex::encode([0u8; 32]),
                "padding": "x".repeat(17_000),
            }),
        );
        return;
    }
    let nonce = config.fixed_nonce.unwrap_or_else(|| next_nonce(nonce_seed));
    send_json(
        socket,
        json!({"type": "challenge", "nonce_s": hex::encode(nonce)}),
    );
    let Some(hello) = read_hello(socket, shutdown) else {
        return;
    };
    state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .attempts
        .push(attempt_from(&hello));
    if let Some(reason) = config.forced_reject {
        send_reject(socket, reason);
        return;
    }
    if hello.version != 1 {
        send_reject(socket, RejectReason::UnsupportedVersion { min: 1, max: 1 });
        return;
    }
    if state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .nonce_used(&nonce)
    {
        send_reject(socket, RejectReason::BadSignature);
        return;
    }
    let Ok(key_id) = KeyId::parse(&hello.key_id) else {
        send_reject(socket, RejectReason::UnknownKey);
        return;
    };
    if !config.keys.contains(&key_id) {
        send_reject(socket, RejectReason::UnknownKey);
        return;
    }
    let Ok(nonce_c) = decode_32(&hello.nonce_c) else {
        send_reject(socket, RejectReason::BadSignature);
        return;
    };
    let Some(verifying_key) = parse_verifying_key(&hello.key_id) else {
        send_reject(socket, RejectReason::UnknownKey);
        return;
    };
    let transcript = connect_transcript(&nonce, &nonce_c, "127.0.0.1");
    if !signature_verifies(&verifying_key, &transcript, &hello.sig) {
        send_reject(socket, RejectReason::BadSignature);
        return;
    }
    state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .used_nonces
        .insert(nonce);
    send_json(socket, json!({"type": "welcome", "version": 1}));
    serve_capabilities(socket, config, state, shutdown);
}

fn handle_pair(
    socket: &mut WebSocket<TcpStream>,
    config: &ReferenceServerConfig,
    state: &Mutex<ReferenceState>,
    clock: &ReferenceClock,
    nonce_seed: &AtomicU64,
    shutdown: &AtomicBool,
) {
    let nonce = next_nonce(nonce_seed);
    send_json(
        socket,
        json!({"type": "challenge", "nonce_s": hex::encode(nonce)}),
    );
    let Some(hello) = read_hello(socket, shutdown) else {
        return;
    };
    state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .attempts
        .push(attempt_from(&hello));
    if hello.kind != "pair_hello" {
        send_pair_reject(socket, PairingRejectReason::BadSignature);
        return;
    }
    let Ok(handle) = decode_32(&hello.handle) else {
        send_pair_reject(socket, PairingRejectReason::UnknownHandle);
        return;
    };
    state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .handles_seen
        .push(handle);
    let Some(record) = config.handles.iter().find(|record| record.handle == handle) else {
        send_pair_reject(socket, PairingRejectReason::UnknownHandle);
        return;
    };
    if clock.now_ms() >= record.expires_at_ms {
        send_pair_reject(socket, PairingRejectReason::ExpiredHandle);
        return;
    }
    if state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .redeemed
        .contains(&handle)
    {
        send_pair_reject(socket, PairingRejectReason::AlreadyRedeemed);
        return;
    }
    let Ok(nonce_c) = decode_32(&hello.nonce_c) else {
        send_pair_reject(socket, PairingRejectReason::BadSignature);
        return;
    };
    if hello.version != 1 {
        send_pair_reject(
            socket,
            PairingRejectReason::UnsupportedVersion { min: 1, max: 1 },
        );
        return;
    }
    // The pairing transcript includes the server's side of the handle; `hello.handle` is the same
    // bytes, already validated against a registered record above.
    let transcript = pairing_transcript(&nonce, &nonce_c, &handle, "127.0.0.1");
    let Some(verifying_key) = parse_verifying_key(&hello.key_id) else {
        send_pair_reject(socket, PairingRejectReason::BadSignature);
        return;
    };
    if !signature_verifies(&verifying_key, &transcript, &hello.sig) {
        send_pair_reject(socket, PairingRejectReason::BadSignature);
        return;
    }
    state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .redeemed
        .insert(handle);
    send_json(
        socket,
        json!({"type": "pair_approved", "account": config.account}),
    );
}

/// One parsed capability request.
#[derive(Debug, Deserialize)]
struct CapabilityWire {
    #[serde(rename = "type")]
    kind: String,
    request_id: u32,
    #[serde(default)]
    project: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    proposed: String,
    #[serde(default)]
    db_id: String,
}

fn serve_capabilities(
    socket: &mut WebSocket<TcpStream>,
    config: &ReferenceServerConfig,
    state: &Mutex<ReferenceState>,
    shutdown: &AtomicBool,
) {
    loop {
        let Some(text) = read_text(socket, shutdown) else {
            return;
        };
        let Ok(request) = serde_json::from_str::<CapabilityWire>(&text) else {
            return;
        };
        state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .capability_requests
            .push(RecordedCapability {
                capability: request.kind.clone(),
                project: request.project.clone(),
                mode: request.mode.clone(),
                request_id: request.request_id,
            });
        if config.deny_capabilities {
            send_capability_error(
                socket,
                request.request_id,
                &request.kind,
                "not_authenticated",
            );
            continue;
        }
        if request.project.is_empty() || request.mode.is_empty() {
            send_capability_error(socket, request.request_id, &request.kind, "invalid_request");
            continue;
        }
        match request.kind.as_str() {
            "env.bundle" => {
                let (vars, secrets, domains) = {
                    let state = state.lock().unwrap_or_else(PoisonError::into_inner);
                    let domains = if request.mode == "cloud" {
                        config.domains.clone()
                    } else {
                        // `domains::` is provider data only in `cloud` mode; local worlds derive
                        // their origin from `WorldOrigin` (`ADR-0005` §1).
                        BTreeMap::new()
                    };
                    (config.vars.clone(), state.secrets.clone(), domains)
                };
                send_json(
                    socket,
                    json!({
                        "type": "env.bundle",
                        "request_id": request.request_id,
                        "vars": vars,
                        "secrets": secrets,
                        "domains": domains,
                    }),
                );
            }
            "secrets.ensure" => {
                let returned = {
                    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
                    let entry = state
                        .secrets
                        .entry(request.key.clone())
                        .or_insert_with(|| request.proposed.clone());
                    let returned = entry.clone();
                    state.ensure_calls.push(EnsureCall {
                        key: request.key.clone(),
                        proposed: request.proposed.clone(),
                        returned: returned.clone(),
                    });
                    returned
                };
                send_json(
                    socket,
                    json!({
                        "type": "secrets.ensure",
                        "request_id": request.request_id,
                        "key": request.key,
                        "value": returned,
                    }),
                );
            }
            "resources.describe" => {
                let s3_buckets: BTreeMap<&String, Value> = config
                    .s3_buckets
                    .iter()
                    .map(|(id, bucket)| {
                        (
                            id,
                            json!({
                                "endpoint": bucket.endpoint,
                                "region": bucket.region,
                                "bucket": bucket.bucket,
                                "access_key": bucket.access_key,
                                "secret_key": bucket.secret_key,
                            }),
                        )
                    })
                    .collect();
                let pg_databases: BTreeMap<&String, Value> = config
                    .pg_databases
                    .iter()
                    .map(|(id, dump)| {
                        (
                            id,
                            json!({
                                "db_id": dump.db_id,
                                "sha256_compressed": dump.sha256_compressed(),
                                "compressed_bytes": dump.compressed().len(),
                                "format": "pg_custom_zstd",
                            }),
                        )
                    })
                    .collect();
                send_json(
                    socket,
                    json!({
                        "type": "resources.describe",
                        "request_id": request.request_id,
                        "s3_buckets": s3_buckets,
                        "pg_databases": pg_databases,
                    }),
                );
            }
            "dump.begin" => {
                let Some(record) = config.pg_databases.get(&request.db_id) else {
                    send_capability_error(
                        socket,
                        request.request_id,
                        "dump.begin",
                        "unknown_resource",
                    );
                    continue;
                };
                if !serve_dump(socket, config, state, request.request_id, record) {
                    return;
                }
            }
            _ => {
                send_capability_error(
                    socket,
                    request.request_id,
                    &request.kind,
                    "unsupported_capability",
                );
            }
        }
    }
}

/// Stream one dump; returns `false` when the socket died.
fn serve_dump(
    socket: &mut WebSocket<TcpStream>,
    config: &ReferenceServerConfig,
    state: &Mutex<ReferenceState>,
    request_id: u32,
    record: &DumpRecord,
) -> bool {
    let compressed = record.compressed();
    let sha256 = hex::encode(Sha256::digest(&compressed));
    let format = config
        .dump_format
        .clone()
        .unwrap_or_else(|| "pg_custom_zstd".to_owned());
    send_json(
        socket,
        json!({
            "type": "dump.begin",
            "request_id": request_id,
            "db_id": record.db_id,
            "sha256_compressed": sha256,
            "compressed_bytes": compressed.len(),
            "format": format,
        }),
    );
    state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .dumps_served
        .push(record.db_id.clone());

    if config.truncate_dump {
        let half = compressed.len() / 2;
        if half > 0 {
            send_binary(socket, &compressed[..half]);
        }
        // Close without `dump.end`: the client must abort and unlink.
        return false;
    }

    let mut payload = compressed.clone();
    if config.corrupt_dump && !payload.is_empty() {
        let index = payload.len() / 2;
        payload[index] ^= 0xff;
    }
    if config.oversized_dump_frame {
        let oversized = vec![0u8; MAX_DUMP_FRAME + 1];
        send_binary(socket, &oversized);
        state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .dump_frames += 1;
    }
    let chunk = config.dump_chunk_bytes.max(1);
    for part in payload.chunks(chunk) {
        if !send_binary(socket, part) {
            return false;
        }
        let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
        state.dump_frames += 1;
        state.dump_bytes += part.len() as u64;
    }
    send_json(
        socket,
        json!({
            "type": "dump.end",
            "request_id": request_id,
            "sha256_compressed": sha256,
            "compressed_bytes": compressed.len(),
        }),
    );
    true
}

#[derive(Debug, Clone, Deserialize)]
struct HelloFrame {
    #[serde(rename = "type")]
    kind: String,
    version: u16,
    key_id: String,
    nonce_c: String,
    #[serde(default)]
    handle: String,
    sig: String,
}

fn read_hello(socket: &mut WebSocket<TcpStream>, shutdown: &AtomicBool) -> Option<HelloFrame> {
    let text = read_text(socket, shutdown)?;
    serde_json::from_str(&text).ok()
}

fn attempt_from(hello: &HelloFrame) -> ConnectAttempt {
    ConnectAttempt {
        kind: hello.kind.clone(),
        version: hello.version,
        key_id: hello.key_id.clone(),
        nonce_c: hello.nonce_c.clone(),
    }
}

fn read_message(socket: &mut WebSocket<TcpStream>, shutdown: &AtomicBool) -> Option<Message> {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return None;
        }
        match socket.read() {
            Ok(message) => return Some(message),
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(_) => return None,
        }
    }
}

fn read_text(socket: &mut WebSocket<TcpStream>, shutdown: &AtomicBool) -> Option<String> {
    loop {
        match read_message(socket, shutdown)? {
            Message::Text(text) => return Some(text.to_string()),
            Message::Ping(_) => {
                let _ = socket.flush();
            }
            Message::Pong(_) | Message::Frame(_) => {}
            Message::Close(_) | Message::Binary(_) => return None,
        }
    }
}

fn send_json(socket: &mut WebSocket<TcpStream>, value: serde_json::Value) {
    let _ = socket.send(Message::Text(value.to_string().into()));
}

fn send_binary(socket: &mut WebSocket<TcpStream>, bytes: &[u8]) -> bool {
    socket.send(Message::Binary(bytes.to_vec().into())).is_ok()
}

fn send_capability_error(
    socket: &mut WebSocket<TcpStream>,
    request_id: u32,
    capability: &str,
    kind: &str,
) {
    send_json(
        socket,
        json!({
            "type": "capability.error",
            "request_id": request_id,
            "capability": capability,
            "kind": kind,
            "message": "reference server refused the capability",
        }),
    );
}

fn send_reject(socket: &mut WebSocket<TcpStream>, reason: RejectReason) {
    let reason = match reason {
        RejectReason::UnknownKey => json!({"kind": "unknown_key"}),
        RejectReason::BadSignature => json!({"kind": "bad_signature"}),
        RejectReason::UnsupportedVersion { min, max } => {
            json!({"kind": "unsupported_version", "min": min, "max": max})
        }
    };
    send_json(socket, json!({"type": "reject", "reason": reason}));
}

fn send_pair_reject(socket: &mut WebSocket<TcpStream>, reason: PairingRejectReason) {
    let reason = match reason {
        PairingRejectReason::UnknownHandle => json!({"kind": "unknown_handle"}),
        PairingRejectReason::ExpiredHandle => json!({"kind": "expired_handle"}),
        PairingRejectReason::AlreadyRedeemed => json!({"kind": "already_redeemed"}),
        PairingRejectReason::BadSignature => json!({"kind": "bad_signature"}),
        PairingRejectReason::UnsupportedVersion { min, max } => {
            json!({"kind": "unsupported_version", "min": min, "max": max})
        }
    };
    send_json(socket, json!({"type": "reject", "reason": reason}));
}

/// Independent transcript/signature verification: the reference server never calls the production
/// transcript builders or the production verifier, so a spec deviation in either side is caught by
/// the live handshake tests.
fn connect_transcript(nonce_s: &[u8; 32], nonce_c: &[u8; 32], server_name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"ospp-v1");
    out.extend_from_slice(nonce_s);
    out.extend_from_slice(nonce_c);
    out.extend_from_slice(server_name.as_bytes());
    out
}

fn pairing_transcript(
    nonce_s: &[u8; 32],
    nonce_c: &[u8; 32],
    handle: &[u8; 32],
    server_name: &str,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"ospp-v1-pair");
    out.extend_from_slice(nonce_s);
    out.extend_from_slice(nonce_c);
    out.extend_from_slice(handle);
    out.extend_from_slice(server_name.as_bytes());
    out
}

fn parse_verifying_key(text: &str) -> Option<VerifyingKey> {
    let hex_part = text.strip_prefix("p256:")?;
    if hex_part.len() != 66 || !hex_part.bytes().all(is_lower_hex_digit) {
        return None;
    }
    let mut bytes = [0u8; 33];
    hex::decode_to_slice(hex_part, &mut bytes).ok()?;
    VerifyingKey::from_sec1_bytes(&bytes).ok()
}

fn signature_verifies(key: &VerifyingKey, transcript: &[u8], sig_hex: &str) -> bool {
    if sig_hex.len() != 128 || !sig_hex.bytes().all(is_lower_hex_digit) {
        return false;
    }
    let mut bytes = [0u8; 64];
    if hex::decode_to_slice(sig_hex, &mut bytes).is_err() {
        return false;
    }
    let Ok(signature) = p256::ecdsa::Signature::from_slice(&bytes) else {
        return false;
    };
    let digest = Sha256::digest(transcript);
    key.verify_prehash(&digest, &signature).is_ok()
}

const fn is_lower_hex_digit(byte: u8) -> bool {
    matches!(byte, b'0'..=b'9' | b'a'..=b'f')
}

fn decode_32(hex_part: &str) -> Result<[u8; 32], ()> {
    if hex_part.len() != 64 || !hex_part.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(());
    }
    let mut out = [0u8; 32];
    hex::decode_to_slice(hex_part, &mut out).map_err(|_| ())?;
    Ok(out)
}

fn next_nonce(seed: &AtomicU64) -> [u8; 32] {
    let mut out = [0u8; 32];
    let first = seed.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::SeqCst);
    out[..8].copy_from_slice(&first.to_le_bytes());
    let second = seed.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::SeqCst);
    out[8..16].copy_from_slice(&second.to_le_bytes());
    out[16..24].copy_from_slice(&(first ^ second).to_le_bytes());
    out[24..].copy_from_slice(&first.wrapping_mul(second).to_le_bytes());
    out
}
