//! In-process `ospp-v1` WebSocket server, deterministic clock, and seeded CSPRNG double.
//!
//! The server is deliberately a *test double*, not a production fallback: it speaks the
//! `ADR-0008` §1 frames over a real loopback WebSocket and verifies real P-256 signatures, so a
//! passing test proves the production client against an independent message exchange.

use std::collections::BTreeSet;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use slipfork_ospp::{
    DeviceSignature, KeyId, OsppEndpoint, PairingRejectReason, RejectReason,
    verify_device_signature,
};
use slipfork_snow::{EntropySource, ProviderError};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::HeaderValue;
use tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tungstenite::{Message, WebSocket, accept_hdr};

/// Deterministic wall clock used by the mock server for pairing TTLs.
#[derive(Debug, Default)]
pub struct MockClock {
    now_ms: AtomicU64,
}

impl MockClock {
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

/// Server behaviour knobs used by the rejection-path tests.
#[derive(Debug, Clone, Default)]
pub struct MockServerConfig {
    /// Public keys the server will accept.
    pub keys: Vec<KeyId>,
    /// Force a typed `REJECT` after any `HELLO`.
    pub forced_reject: Option<RejectReason>,
    /// Reuse this exact `nonce_s` on every connection (used to prove replay refusal).
    pub fixed_nonce: Option<[u8; 32]>,
    /// Send `WELCOME` before any `CHALLENGE`.
    pub welcome_before_challenge: bool,
    /// Send a >16 KiB first frame.
    pub oversized_first_frame: bool,
    /// Accept the upgrade and then send no frame at all (deadline proof).
    pub silent_first_frame: bool,
    /// Pairing handles this server will honour.
    pub handles: Vec<HandleRecord>,
    /// Account returned on a successful pairing.
    pub account: String,
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

/// Observable server state for assertions.
#[derive(Debug, Default)]
pub struct MockState {
    /// Every upgrade request, in order.
    pub requests: Vec<RecordedRequest>,
    /// Every handshake attempt, in order.
    pub attempts: Vec<ConnectAttempt>,
    /// Every pairing handle that reached a `pair_hello`.
    pub handles_seen: Vec<[u8; 32]>,
    used_nonces: BTreeSet<[u8; 32]>,
    redeemed: BTreeSet<[u8; 32]>,
}

impl MockState {
    /// Whether `nonce` was already consumed by an accepted handshake.
    pub fn nonce_used(&self, nonce: &[u8; 32]) -> bool {
        self.used_nonces.contains(nonce)
    }

    /// Whether `handle` was already redeemed.
    pub fn handle_redeemed(&self, handle: &[u8; 32]) -> bool {
        self.redeemed.contains(handle)
    }
}

/// A running in-process WebSocket server.
pub struct MockOsppServer {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    state: Arc<Mutex<MockState>>,
    clock: Arc<MockClock>,
    thread: Option<JoinHandle<()>>,
}

impl MockOsppServer {
    /// Bind `127.0.0.1:0` and start serving.
    pub fn start(config: MockServerConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let addr = listener.local_addr().expect("local addr");
        listener
            .set_nonblocking(true)
            .expect("non-blocking listener");
        let shutdown = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(MockState::default()));
        let clock = Arc::new(MockClock::new(1_000));
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
                            handle_connection(stream, &config, &state, &clock, &nonce_seed);
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
    pub fn clock(&self) -> &MockClock {
        &self.clock
    }

    /// Snapshot the observable state.
    pub fn state(&self) -> MutexGuard<'_, MockState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for MockOsppServer {
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
    config: &Arc<MockServerConfig>,
    state: &Arc<Mutex<MockState>>,
    clock: &Arc<MockClock>,
    nonce_seed: &Arc<AtomicU64>,
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
    let path = recorded
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .unwrap_or_default();
    match path.as_str() {
        "/_ospp/v1/connect" => handle_connect(&mut socket, config, state, nonce_seed),
        "/_ospp/v1/pair" => handle_pair(&mut socket, config, state, clock, nonce_seed),
        _ => {
            let _ = socket.close(None);
        }
    }
    let _ = socket.close(None);
}

fn handle_connect(
    socket: &mut WebSocket<TcpStream>,
    config: &MockServerConfig,
    state: &Mutex<MockState>,
    nonce_seed: &AtomicU64,
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
    let Some(hello) = read_hello(socket) else {
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
    let transcript = slipfork_ospp::connect_transcript(&nonce, &nonce_c, "127.0.0.1");
    if !signature_verifies(&key_id, &transcript, &hello.sig) {
        send_reject(socket, RejectReason::BadSignature);
        return;
    }
    state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .used_nonces
        .insert(nonce);
    send_json(socket, json!({"type": "welcome", "version": 1}));
    drain(socket);
}

fn handle_pair(
    socket: &mut WebSocket<TcpStream>,
    config: &MockServerConfig,
    state: &Mutex<MockState>,
    clock: &MockClock,
    nonce_seed: &AtomicU64,
) {
    let nonce = next_nonce(nonce_seed);
    send_json(
        socket,
        json!({"type": "challenge", "nonce_s": hex::encode(nonce)}),
    );
    let Some(hello) = read_hello(socket) else {
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
    let transcript = slipfork_ospp::pairing_transcript(&nonce, &nonce_c, &handle, "127.0.0.1");
    let Ok(key_id) = KeyId::parse(&hello.key_id) else {
        send_pair_reject(socket, PairingRejectReason::BadSignature);
        return;
    };
    if !signature_verifies(&key_id, &transcript, &hello.sig) {
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

fn read_hello(socket: &mut WebSocket<TcpStream>) -> Option<HelloFrame> {
    let text = read_text(socket)?;
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

fn read_text(socket: &mut WebSocket<TcpStream>) -> Option<String> {
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => return Some(text.to_string()),
            Ok(Message::Ping(_)) => {
                let _ = socket.flush();
            }
            Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
            Ok(Message::Close(_)) | Ok(Message::Binary(_)) | Err(_) => return None,
        }
    }
}

fn drain(socket: &mut WebSocket<TcpStream>) {
    loop {
        match socket.read() {
            Ok(Message::Ping(_)) => {
                let _ = socket.flush();
            }
            Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
            Ok(_) | Err(_) => return,
        }
    }
}

fn send_json(socket: &mut WebSocket<TcpStream>, value: serde_json::Value) {
    let _ = socket.send(Message::Text(value.to_string().into()));
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

fn signature_verifies(key_id: &KeyId, transcript: &[u8], sig_hex: &str) -> bool {
    let Ok(signature) = DeviceSignature::from_hex(sig_hex) else {
        return false;
    };
    verify_device_signature(key_id, transcript, &signature).is_ok()
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
