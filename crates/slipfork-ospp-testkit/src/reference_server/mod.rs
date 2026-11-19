//! In-tree `ospp-v1` reference server: an independent implementation of
//! `docs/design/ospp.md` that the protected suite drives with the production client.
//!
//! It speaks real loopback WebSocket frames with real P-256 verification and real RFC 9180 HPKE
//! sealing, resolves the already feature-evaluated `Forkfile` IR against the ID-only `claims`
//! through the canonical `slipfork-forkfile` evaluator, pushes live `env_updated` frames, and
//! serves a sealed dump stream from an out-of-band HTTP endpoint (plaintext HTTP on loopback only,
//! mirroring the `ws://`-on-loopback endpoint rule; the production client's `https://` path uses
//! `rustls` + `webpki-roots`).

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use slipfork_forkfile::EvaluatedFork;
use slipfork_ospp::wire::ServerFrame;
use slipfork_ospp::{OSPP_SUBPROTOCOL, OsppClaims, OsppEndpoint, OsppError};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::HeaderValue;
use tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tungstenite::{WebSocket, accept_hdr};

use crate::signers::SeededEntropy;

mod claims;
mod dumps;
mod env;
mod frames;
mod session;

pub use frames::{seal_dump, seal_secret};

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
    /// The feature-evaluated manifest the client subscribed with.
    pub forkfile: EvaluatedFork,
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
                .spawn(move || dumps::http_loop(http_listener, shared))
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

    /// Replace the sealed dump body served for one resource (`pg_databases.main`, …).
    ///
    /// A live `up` asks for the dump only when it has one to offer, and this setter lets one
    /// journey produce the archive from the world's own `pg_dump` and then serve it back without
    /// restarting the daemon or the server.
    pub fn set_dump(&self, resource: &str, bytes: Vec<u8>) {
        lock(&self.shared.config)
            .dumps
            .insert(resource.to_owned(), bytes);
    }

    /// Re-resolve and push `env_updated` for every active subscription.
    pub fn push_update(&self) -> Result<(), OsppError> {
        let shared = Arc::clone(&self.shared);
        let subscriptions: Vec<ActiveSubscription> =
            lock(&shared.subscriptions).values().cloned().collect();
        for subscription in subscriptions {
            let config = lock(&shared.config).clone();
            let resolved = claims::resolve_components(
                &config,
                &subscription.record.claims,
                &subscription.record.forkfile,
            )
            .map_err(|(code, message)| {
                OsppError::server(Some(subscription.record.id.clone()), code, message)
            })?;
            let components = env::seal_components(
                &resolved,
                &subscription.record.public_key,
                &mut frames::FilledRng::new(&shared.entropy, 8192)?,
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
    let mut socket: WebSocket<TcpStream> = match accept_hdr(stream, callback) {
        Ok(socket) => socket,
        Err(_) => return,
    };
    let _ = socket.get_ref().set_read_timeout(Some(POLL_SLICE));
    let _ = session::serve(&mut socket, &shared);
    let _ = socket.close(None);
}
