//! An in-process fake `tpmd` daemon built on the real pinned `tpmd-ipc` framing and the real
//! `tpmd-proto` messages.
//!
//! This is a test double, never a production fallback: it binds a real owner-only Unix socket with
//! [`tpmd_ipc::Listener`], speaks the same Postcard frames the daemon speaks, and signs with a real
//! P-256 key. `TpmdDeviceKeySigner` therefore exercises its production probe, `ResumeOrCreate`,
//! sealed-policy handshake, request loop, and signature verification without `/run/tpmd`.
//!
//! The pinned release exposes `tpmd-proto` as a separate crate (the real daemon's wire contract),
//! so this module links it directly from `[dev-dependencies]`; nothing about the framing is
//! hand-rolled.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use p256::ecdsa::SigningKey;
use p256::ecdsa::signature::hazmat::PrehashSigner;
use sha2::{Digest, Sha256};
use tpmd_ipc::{Endpoint, Listener, SocketAccess, read_message, write_message};
use tpmd_proto::{
    Algorithm, BackendSelection, Capabilities, Closed, ErrorCode, ErrorReply, KeyId as TpmdKeyId,
    KeyInfo, KeyOutcome, KeyRequest, Message, Op, Reject, RejectCode, Reply, Request, Response,
    SecurityLevel, Signature, Status, StoreState, Welcome,
};

use slipfork_ospp::KeyStorePaths;

/// How the fake daemon answers a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TpmdBehavior {
    /// Serve a working sealed key.
    Sealed,
    /// Answer `CLOSED`, wait for `AWAIT`, answer `OPENED`, then serve a sealed key.
    StoreLockedThenOpen,
    /// Refuse the handshake with a typed rejection (for example a floor mismatch).
    Reject {
        /// The refusal class.
        code: RejectCode,
        /// The daemon's explanation.
        message: String,
    },
    /// Announce that this machine's backend dependency is gone.
    BackendUnavailable {
        /// The daemon's explanation.
        message: String,
    },
    /// Serve a key but return an invalid raw signature (the adapter must verify it).
    BadSignature,
}

/// Everything the fake daemon observed, for assertions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TpmdObservation {
    /// Completed `HELLO`s.
    pub handshakes: usize,
    /// Namespaces declared by clients, in order.
    pub namespaces: Vec<String>,
    /// Key requests, as `create` / `resume_or_create` / `none`.
    pub key_requests: Vec<&'static str>,
    /// `AWAIT` requests answered.
    pub awaited: usize,
    /// `SIGN` requests answered.
    pub signed: usize,
}

/// A running fake daemon on a scratch Unix socket.
pub struct FakeTpmd {
    dir: tempfile::TempDir,
    socket_path: PathBuf,
    key_id: slipfork_ospp::KeyId,
    observation: Arc<Mutex<TpmdObservation>>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeTpmd {
    /// Bind a scratch socket and serve `behavior` until dropped.
    pub fn start(behavior: TpmdBehavior) -> Self {
        let dir = tempfile::tempdir().expect("scratch tpmd dir");
        let socket_path = dir.path().join("tpmd.sock");
        Self::start_inner(dir, socket_path, behavior)
    }

    /// Bind `socket_path` (creating its parent) and serve `behavior` until dropped.
    ///
    /// Used by downstream composition-root tests that must place the fake daemon exactly where
    /// `KeyStorePaths::user_then_system($XDG_RUNTIME_DIR)` probes.
    pub fn start_at(socket_path: PathBuf, behavior: TpmdBehavior) -> Self {
        let dir = tempfile::tempdir().expect("scratch tpmd dir");
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent).expect("create fake tpmd socket parent");
        }
        Self::start_inner(dir, socket_path, behavior)
    }

    fn start_inner(dir: tempfile::TempDir, socket_path: PathBuf, behavior: TpmdBehavior) -> Self {
        let signing_key = SigningKey::from_slice(&[0x42u8; 32]).expect("fixed P-256 scalar");
        let public = signing_key.verifying_key().to_sec1_point(true);
        let key_id = slipfork_ospp::KeyId::from_public_key_bytes(public.as_bytes())
            .expect("SEC1-compressed public key");
        let tpmd_key_id = TpmdKeyId::P256(*key_id.public_key_bytes());
        let observation = Arc::new(Mutex::new(TpmdObservation::default()));
        let shutdown = Arc::new(AtomicBool::new(false));

        let thread = {
            let socket_path = socket_path.clone();
            let observation = Arc::clone(&observation);
            let shutdown = Arc::clone(&shutdown);
            thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .build()
                    .expect("fake tpmd runtime");
                runtime.block_on(async move {
                    let mut listener =
                        Listener::bind(Endpoint::unix_socket(&socket_path), SocketAccess::Owner)
                            .await
                            .expect("bind fake tpmd socket");
                    while !shutdown.load(Ordering::SeqCst) {
                        match tokio::time::timeout(Duration::from_millis(20), listener.accept())
                            .await
                        {
                            Ok(Ok(connection)) => {
                                serve(
                                    connection.stream,
                                    &behavior,
                                    &signing_key,
                                    tpmd_key_id,
                                    &observation,
                                    &shutdown,
                                )
                                .await;
                            }
                            Ok(Err(_)) => break,
                            Err(_) => {}
                        }
                    }
                });
            })
        };
        // The listener is bound on the spawned thread; wait until the socket is reachable so a
        // caller can connect immediately without a start-up race.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !socket_path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "fake tpmd socket never appeared at {}",
                socket_path.display()
            );
            thread::sleep(Duration::from_millis(1));
        }
        Self {
            dir,
            socket_path,
            key_id,
            observation,
            shutdown,
            thread: Some(thread),
        }
    }

    /// The scratch socket path.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// The scratch directory holding the socket.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// A one-candidate [`KeyStorePaths`] pointing at this daemon.
    pub fn paths(&self) -> KeyStorePaths {
        KeyStorePaths::new([self.socket_path.clone()])
    }

    /// The fingerprint this daemon signs with.
    pub const fn key_id(&self) -> &slipfork_ospp::KeyId {
        &self.key_id
    }

    /// Snapshot the observation record.
    pub fn observation(&self) -> MutexGuard<'_, TpmdObservation> {
        self.observation
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for FakeTpmd {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn serve(
    mut stream: tpmd_ipc::LocalStream,
    behavior: &TpmdBehavior,
    signing_key: &SigningKey,
    key_id: TpmdKeyId,
    observation: &Mutex<TpmdObservation>,
    shutdown: &AtomicBool,
) {
    let mut opened = false;
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        let read =
            tokio::time::timeout(Duration::from_millis(100), read_message(&mut stream)).await;
        let message = match read {
            Ok(Ok(message)) => message,
            Ok(Err(_)) => return,
            // Idle between capability calls: keep the authenticated connection open.
            Err(_) => continue,
        };
        match message {
            Message::Hello(hello) => {
                if let TpmdBehavior::BackendUnavailable { message } = behavior {
                    let reject = Reject {
                        code: RejectCode::BackendUnavailable,
                        message: message.clone(),
                        detail: None,
                    };
                    let _ = write_message(&mut stream, &Message::Reject(reject)).await;
                    return;
                }
                if let TpmdBehavior::Reject { code, message } = behavior {
                    let reject = Reject {
                        code: *code,
                        message: message.clone(),
                        detail: None,
                    };
                    let _ = write_message(&mut stream, &Message::Reject(reject)).await;
                    return;
                }
                if matches!(behavior, TpmdBehavior::StoreLockedThenOpen) && !opened {
                    let closed = Closed {
                        code: ErrorCode::StoreLocked,
                        message: "the user store is locked".to_owned(),
                        detail: None,
                    };
                    if write_message(&mut stream, &Message::Closed(closed))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let Ok(Ok(Message::Request(Request { id, op: Op::Await }))) =
                        tokio::time::timeout(Duration::from_millis(500), read_message(&mut stream))
                            .await
                    else {
                        return;
                    };
                    observation
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .awaited += 1;
                    opened = true;
                    let response = Response {
                        id,
                        result: Ok(Reply::Opened(capabilities())),
                    };
                    if write_message(&mut stream, &Message::Response(response))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
                {
                    let mut observed = observation.lock().unwrap_or_else(PoisonError::into_inner);
                    observed.handshakes += 1;
                    observed.namespaces.push(hello.namespace.clone());
                    observed.key_requests.push(key_request_label(&hello.key));
                }
                let info = KeyInfo {
                    key_id,
                    algorithm: Algorithm::EcdsaP256Sha256,
                    security_level: SecurityLevel::Sealed,
                    created_unix: 1_700_000_000,
                };
                let key = match &hello.key {
                    KeyRequest::None => KeyOutcome::None,
                    KeyRequest::Create(_) => KeyOutcome::Created(info),
                    KeyRequest::ResumeOrCreate { key_id: wanted, .. } => {
                        if *wanted == key_id {
                            KeyOutcome::Resumed(info)
                        } else {
                            // The daemon minted a new identity: the adapter must fail closed.
                            KeyOutcome::Created(info)
                        }
                    }
                };
                let welcome = Welcome {
                    version: tpmd_proto::PROTOCOL_VERSION,
                    principal: format!("uid:{}", std::process::id()),
                    capabilities: capabilities(),
                    limits: tpmd_proto::default_limits(),
                    key,
                };
                if write_message(&mut stream, &Message::Welcome(welcome))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Message::Request(Request { id, op }) => {
                let response = match op {
                    Op::Sign {
                        key_id: requested,
                        message,
                    } => {
                        if requested != key_id {
                            Err(ErrorReply {
                                code: ErrorCode::UnknownKey,
                                message: "unknown key".to_owned(),
                                detail: None,
                            })
                        } else {
                            observation
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .signed += 1;
                            if matches!(behavior, TpmdBehavior::BadSignature) {
                                Ok(Reply::Signature(Signature::P256([0u8; 64])))
                            } else {
                                let digest = Sha256::digest(&message);
                                let signature: p256::ecdsa::Signature =
                                    signing_key.sign_prehash(&digest).expect("P-256 signing");
                                let bytes: [u8; 64] = signature
                                    .to_bytes()
                                    .as_slice()
                                    .try_into()
                                    .expect("64-byte raw signature");
                                Ok(Reply::Signature(Signature::P256(bytes)))
                            }
                        }
                    }
                    Op::Status => Ok(Reply::Status(status())),
                    Op::Describe { key_id: requested } if requested == key_id => {
                        Ok(Reply::KeyInfo(KeyInfo {
                            key_id,
                            algorithm: Algorithm::EcdsaP256Sha256,
                            security_level: SecurityLevel::Sealed,
                            created_unix: 1_700_000_000,
                        }))
                    }
                    Op::Await => {
                        observation
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .awaited += 1;
                        Ok(Reply::Opened(capabilities()))
                    }
                    other => Err(ErrorReply {
                        code: ErrorCode::InvalidRequest,
                        message: format!("unsupported operation: {other:?}"),
                        detail: None,
                    }),
                };
                let response = Response {
                    id,
                    result: response,
                };
                if write_message(&mut stream, &Message::Response(response))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            _ => return,
        }
    }
}

fn key_request_label(request: &KeyRequest) -> &'static str {
    match request {
        KeyRequest::None => "none",
        KeyRequest::Create(_) => "create",
        KeyRequest::ResumeOrCreate { .. } => "resume_or_create",
    }
}

fn capabilities() -> Capabilities {
    Capabilities {
        daemon: "fake-tpmd 0.0.0".to_owned(),
        backend: "test".to_owned(),
        algorithms: vec![Algorithm::EcdsaP256Sha256],
        levels: vec![
            SecurityLevel::File,
            SecurityLevel::Keyring,
            SecurityLevel::Sealed,
        ],
        default_level: SecurityLevel::Sealed,
        key_id_encoding: "sec1-compressed".to_owned(),
        signature_encoding: "raw-r-s".to_owned(),
        store: StoreState::Open,
    }
}

fn status() -> Status {
    Status {
        daemon: "fake-tpmd 0.0.0".to_owned(),
        backend: "test".to_owned(),
        principal: format!("uid:{}", std::process::id()),
        namespace: "slipfork".to_owned(),
        levels: vec![SecurityLevel::Sealed],
        default_level: SecurityLevel::Sealed,
        default_min_security: SecurityLevel::Sealed,
        store: StoreState::Open,
        selection: BackendSelection {
            backend: "test".to_owned(),
            chosen_unix: Some(1_700_000_000),
            reason: Some("test double".to_owned()),
            stronger_available: None,
        },
    }
}
