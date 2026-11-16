//! `TpmdDeviceKeySigner`: the hardware/sealed-key `DeviceKeySigner` adapter built directly on the
//! pinned upstream `tpmd-client` / `tpmd-ipc` crates (`H-010` #3).
//!
//! No frame, handshake, or key-request byte is hand-rolled here: the upstream client owns the
//! transport and framing. A private, instance-scoped `current_thread` tokio runtime drives that
//! async client, so no global runtime, async trait, or executor leaks into the synchronous domain
//! tree. The injected [`KeyStorePaths`] are probed in order under namespace `"slipfork"`, with the
//! `sealed` security floor and `allow_below_floor: false`, and every returned signature is verified
//! against the signer's `key_id` before it is released.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use tpmd_client::{
    Client, ClientError, KeyId as TpmdKeyId, KeyOutcome, KeyRequest, ProvisionRequest,
    SecurityLevel, Signature,
};

use tokio::runtime::Builder as TokioRt;

use crate::device_key::{DeviceKeySigner, DeviceSignature, KeyId, verify_device_signature};
use crate::error::OsppError;

/// Application namespace under which Slipfork's `tpmd` keys live.
pub const TPMD_NAMESPACE: &str = "slipfork";

/// Security floor requested for every `tpmd` key (`ADR-0008` §1).
pub const TPMD_SECURITY_FLOOR: SecurityLevel = SecurityLevel::Sealed;

/// How long a locked `tpmd` store is waited out before the adapter fails closed.
pub const TPMD_STORE_WAIT: std::time::Duration = tpmd_client::DEFAULT_STORE_WAIT;

/// Candidate `tpmd` sockets, probed in exactly this order.
///
/// The Composition Root resolves `$XDG_RUNTIME_DIR` once and passes the resulting paths; this crate
/// never reads the environment. [`Self::user_then_system`] builds the normative order
/// (`$XDG_RUNTIME_DIR/tpmd/tpmd.sock`, then `/run/tpmd/tpmd.sock`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyStorePaths {
    candidates: Vec<PathBuf>,
}

impl KeyStorePaths {
    /// Use an explicit candidate list, preserving order.
    pub fn new(candidates: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            candidates: candidates.into_iter().collect(),
        }
    }

    /// Build the normative order from an already-resolved runtime directory.
    ///
    /// A duplicate system path (for example when the runtime dir is literally `/run`) is collapsed.
    pub fn user_then_system(runtime_dir: Option<&Path>) -> Self {
        let mut candidates = Vec::with_capacity(2);
        if let Some(runtime_dir) = runtime_dir {
            candidates.push(runtime_dir.join("tpmd").join("tpmd.sock"));
        }
        let system = PathBuf::from("/run/tpmd/tpmd.sock");
        if !candidates.contains(&system) {
            candidates.push(system);
        }
        Self { candidates }
    }

    /// The ordered candidates.
    pub fn candidates(&self) -> &[PathBuf] {
        &self.candidates
    }
}

/// A `DeviceKeySigner` whose private key never leaves the `tpmd` daemon.
pub struct TpmdDeviceKeySigner {
    key_id: KeyId,
    label: &'static str,
    endpoint: String,
    runtime: tokio::runtime::Runtime,
    client: Mutex<Client>,
}

impl TpmdDeviceKeySigner {
    /// Probe every candidate in order and complete one atomic `ResumeOrCreate` handshake.
    ///
    /// `remembered` is the fingerprint from a previous session; when present the daemon is asked to
    /// resume it, and a different key coming back fails closed (a silent identity change would
    /// invalidate the user's pairing). When absent, a fresh key is created atomically.
    pub fn connect(paths: &KeyStorePaths, remembered: Option<&KeyId>) -> Result<Self, OsppError> {
        if paths.candidates.is_empty() {
            return Err(OsppError::TpmdUnavailable {
                probed: "<no candidates>".to_owned(),
            });
        }
        let runtime = TokioRt::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|error| OsppError::TpmdProtocol {
                message: format!("could not build the private tpmd runtime: {error}"),
            })?;
        let request = key_request(remembered);
        let mut probed = Vec::with_capacity(paths.candidates.len());
        for candidate in &paths.candidates {
            probed.push(candidate.display().to_string());
            let endpoint = tpmd_ipc::Endpoint::unix_socket(candidate.clone());
            let connected = runtime.block_on(Client::connect_within(
                &endpoint,
                TPMD_NAMESPACE,
                request.clone(),
                TPMD_STORE_WAIT,
            ));
            match connected {
                Ok(client) => {
                    return Self::finish(runtime, client, candidate, remembered);
                }
                // Absent daemon: the one condition that may try the next candidate.
                Err(ClientError::Unavailable { .. }) => continue,
                Err(error) => return Err(map_client_error(error, candidate)),
            }
        }
        Err(OsppError::TpmdUnavailable {
            probed: probed.join(", "),
        })
    }

    fn finish(
        runtime: tokio::runtime::Runtime,
        client: Client,
        candidate: &Path,
        remembered: Option<&KeyId>,
    ) -> Result<Self, OsppError> {
        let info = match client.key_outcome() {
            KeyOutcome::Created(info) | KeyOutcome::Resumed(info) => info.clone(),
            KeyOutcome::None => {
                return Err(OsppError::TpmdProtocol {
                    message: "tpmd answered WELCOME without creating or resuming a key".to_owned(),
                });
            }
        };
        if info.algorithm != tpmd_client::Algorithm::EcdsaP256Sha256 {
            return Err(OsppError::TpmdProtocol {
                message: format!("tpmd returned unsupported algorithm {}", info.algorithm),
            });
        }
        let key_id = KeyId::from_public_key_bytes(info.key_id.as_bytes())?;
        if let Some(remembered) = remembered
            && remembered != &key_id
        {
            return Err(OsppError::TpmdKeyMismatch {
                actual: key_id.to_string(),
                expected: remembered.to_string(),
            });
        }
        Ok(Self {
            key_id,
            label: label_for(info.security_level),
            endpoint: candidate.display().to_string(),
            runtime,
            client: Mutex::new(client),
        })
    }

    fn client(&self) -> MutexGuard<'_, Client> {
        // Poisoning only means another thread panicked while holding the connection; the client
        // carries no invariant a panic can corrupt, so recover rather than fail every later call.
        self.client
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl fmt::Debug for TpmdDeviceKeySigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TpmdDeviceKeySigner")
            .field("key_id", &self.key_id)
            .field("backend_label", &self.label)
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

impl DeviceKeySigner for TpmdDeviceKeySigner {
    fn key_id(&self) -> &KeyId {
        &self.key_id
    }

    fn backend_label(&self) -> &'static str {
        self.label
    }

    fn sign(&self, message: &[u8]) -> Result<DeviceSignature, OsppError> {
        let tpmd_key = to_tpmd_key_id(&self.key_id);
        let mut client = self.client();
        let mut waited = false;
        loop {
            match self.runtime.block_on(client.sign(tpmd_key, message)) {
                Ok(Signature::P256(bytes)) => {
                    let signature = DeviceSignature::from_raw(bytes);
                    verify_device_signature(&self.key_id, message, &signature).map_err(|_| {
                        OsppError::SignatureVerification {
                            backend: self.label,
                            key_id: self.key_id.to_string(),
                        }
                    })?;
                    return Ok(signature);
                }
                Err(ClientError::StoreClosed { .. }) if !waited => {
                    waited = true;
                    self.runtime
                        .block_on(client.await_store_open(TPMD_STORE_WAIT))
                        .map_err(|error| map_client_error(error, Path::new(&self.endpoint)))?;
                }
                Err(error) => return Err(map_client_error(error, Path::new(&self.endpoint))),
            }
        }
    }
}

/// The software fallback selected only when `tpmd` is absent across every candidate.
///
/// Construction takes the primary adapter's `Result`: a `TpmdUnavailable` selects `F` and records
/// exactly one diagnostic line; every other failure (floor mismatch, denied permission, protocol
/// error, verification failure) propagates and fails closed. There is no silent downgrade.
pub struct FallbackDeviceKeySigner<P: DeviceKeySigner, F: DeviceKeySigner> {
    active: ActiveSigner<P, F>,
    diagnostic: Option<FallbackDiagnostic>,
}

/// Exactly one signer is live; the software key is never constructed unless `tpmd` is unavailable.
enum ActiveSigner<P, F> {
    Primary(P),
    Fallback(F),
}

/// The one explicit line explaining why the software key is in use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackDiagnostic {
    reason: String,
    label: &'static str,
}

impl FallbackDiagnostic {
    /// Why `tpmd` was not used.
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// The selected adapter label (`software:file-0600`).
    pub const fn label(&self) -> &'static str {
        self.label
    }

    /// The complete diagnostic line the Composition Root must surface to the user.
    pub fn line(&self) -> String {
        format!(
            "{}: using {}, below tpmd sealed floor",
            self.reason, self.label
        )
    }
}

impl fmt::Display for FallbackDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.line())
    }
}

impl<P: DeviceKeySigner, F: DeviceKeySigner> FallbackDeviceKeySigner<P, F> {
    /// Combine a primary attempt with a lazily constructed software fallback.
    ///
    /// `fallback` runs **only** when `primary` is `TpmdUnavailable`, so a tpmd-capable machine never
    /// mints the `0600` software key file. Any other primary failure (floor mismatch, denied
    /// permission, protocol error, verification failure) propagates and fails closed.
    pub fn new(
        primary: Result<P, OsppError>,
        fallback: impl FnOnce() -> Result<F, OsppError>,
    ) -> Result<Self, OsppError> {
        match primary {
            Ok(primary) => Ok(Self {
                active: ActiveSigner::Primary(primary),
                diagnostic: None,
            }),
            Err(error @ OsppError::TpmdUnavailable { .. }) => {
                let fallback = fallback()?;
                let diagnostic = FallbackDiagnostic {
                    reason: error.to_string(),
                    label: fallback.backend_label(),
                };
                Ok(Self {
                    active: ActiveSigner::Fallback(fallback),
                    diagnostic: Some(diagnostic),
                })
            }
            Err(other) => Err(other),
        }
    }

    /// The explicit diagnostic when the fallback is active, or `None` when `tpmd` serves.
    pub fn diagnostic(&self) -> Option<&FallbackDiagnostic> {
        self.diagnostic.as_ref()
    }

    /// Whether the software fallback is the active signer.
    pub const fn is_fallback(&self) -> bool {
        matches!(self.active, ActiveSigner::Fallback(_))
    }
}

impl<P: DeviceKeySigner, F: DeviceKeySigner> fmt::Debug for FallbackDeviceKeySigner<P, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FallbackDeviceKeySigner")
            .field("key_id", &self.key_id())
            .field("backend_label", &self.backend_label())
            .field("fallback", &self.is_fallback())
            .finish()
    }
}

impl<P: DeviceKeySigner, F: DeviceKeySigner> DeviceKeySigner for FallbackDeviceKeySigner<P, F> {
    fn key_id(&self) -> &KeyId {
        match &self.active {
            ActiveSigner::Primary(primary) => primary.key_id(),
            ActiveSigner::Fallback(fallback) => fallback.key_id(),
        }
    }

    fn backend_label(&self) -> &'static str {
        match &self.active {
            ActiveSigner::Primary(primary) => primary.backend_label(),
            ActiveSigner::Fallback(fallback) => fallback.backend_label(),
        }
    }

    fn sign(&self, message: &[u8]) -> Result<DeviceSignature, OsppError> {
        match &self.active {
            ActiveSigner::Primary(primary) => primary.sign(message),
            ActiveSigner::Fallback(fallback) => fallback.sign(message),
        }
    }
}

fn key_request(remembered: Option<&KeyId>) -> KeyRequest {
    let provision = ProvisionRequest {
        algorithm: tpmd_client::Algorithm::EcdsaP256Sha256,
        min_security: TPMD_SECURITY_FLOOR,
        allow_below_floor: false,
    };
    match remembered {
        Some(key_id) => KeyRequest::ResumeOrCreate {
            key_id: to_tpmd_key_id(key_id),
            provision,
        },
        None => KeyRequest::Create(provision),
    }
}

fn to_tpmd_key_id(key_id: &KeyId) -> TpmdKeyId {
    TpmdKeyId::P256(*key_id.public_key_bytes())
}

const fn label_for(level: SecurityLevel) -> &'static str {
    match level {
        SecurityLevel::File => "tpmd:file",
        SecurityLevel::Keyring => "tpmd:keyring",
        SecurityLevel::Sealed => "tpmd:sealed",
        SecurityLevel::Hardware => "tpmd:hardware",
    }
}

fn map_client_error(error: ClientError, endpoint: &Path) -> OsppError {
    let endpoint = endpoint.display().to_string();
    match error {
        ClientError::Unavailable { .. } => OsppError::TpmdUnavailable { probed: endpoint },
        ClientError::StoreClosed { message, .. } => {
            OsppError::TpmdStoreLocked { endpoint, message }
        }
        ClientError::BackendUnavailable { message } => {
            OsppError::TpmdBackendUnavailable { endpoint, message }
        }
        ClientError::Rejected { code, message, .. } => OsppError::TpmdRejected {
            endpoint,
            code: format!("{code:?}"),
            message,
        },
        ClientError::Request { code, message, .. } => OsppError::TpmdRequest {
            code: format!("{code:?}"),
            message,
        },
        ClientError::Protocol(message) => OsppError::TpmdProtocol { message },
    }
}
