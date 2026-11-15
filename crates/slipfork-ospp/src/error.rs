//! The single error type for the OSPP v1 client, device-key signers, and pairing.
//!
//! Every variant is typed and fail-closed: there is no "retry with a weaker
//! encoding", no in-memory production fallback, and no variant that carries
//! private key material.

use std::fmt;

use slipfork_snow::ProviderError;
use thiserror::Error;

/// Why the server refused an `ospp-v1` `HELLO` (`ADR-0008` §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// The `key_id` is not registered for the authenticated account.
    UnknownKey,
    /// The signature did not verify over the reconstructed transcript.
    BadSignature,
    /// The server and client share no protocol version.
    UnsupportedVersion {
        /// Lowest version the server speaks.
        min: u16,
        /// Highest version the server speaks.
        max: u16,
    },
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKey => f.write_str("unknown_key"),
            Self::BadSignature => f.write_str("bad_signature"),
            Self::UnsupportedVersion { min, max } => {
                write!(f, "unsupported_version (server speaks {min}..={max})")
            }
        }
    }
}

/// Why the server refused a pairing `pair_hello` (`ADR-0008` §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingRejectReason {
    /// The handle was never issued by this server.
    UnknownHandle,
    /// The five-minute pairing window elapsed.
    ExpiredHandle,
    /// Another redemption already consumed the handle.
    AlreadyRedeemed,
    /// The signature did not verify over the pairing transcript.
    BadSignature,
    /// The server and client share no protocol version.
    UnsupportedVersion {
        /// Lowest version the server speaks.
        min: u16,
        /// Highest version the server speaks.
        max: u16,
    },
}

impl fmt::Display for PairingRejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownHandle => f.write_str("unknown_handle"),
            Self::ExpiredHandle => f.write_str("expired_handle"),
            Self::AlreadyRedeemed => f.write_str("already_redeemed"),
            Self::BadSignature => f.write_str("bad_signature"),
            Self::UnsupportedVersion { min, max } => {
                write!(f, "unsupported_version (server speaks {min}..={max})")
            }
        }
    }
}

/// Errors produced by the OSPP v1 client, device-key adapters, and pairing.
#[derive(Debug, Error)]
pub enum OsppError {
    /// The injected `EntropySource` or a secret-provider adapter failed.
    #[error(transparent)]
    Provider(#[from] ProviderError),

    /// A `KeyId` was not `p256:<66 lowercase hex>` naming a valid SEC1-compressed point.
    #[error("invalid device key id: {reason}")]
    InvalidKeyId {
        /// What failed validation (never key material).
        reason: String,
    },

    /// A `DeviceSignature` was not the raw fixed-width 64-byte `r || s` form.
    #[error("invalid device signature encoding: {reason}")]
    InvalidSignature {
        /// What failed validation (never signature material).
        reason: String,
    },

    /// A signer returned a signature that does not verify against its own `key_id`.
    #[error("signature from {backend} did not verify against {key_id}")]
    SignatureVerification {
        /// Adapter label that produced the signature.
        backend: &'static str,
        /// Public key id the signature claimed to use.
        key_id: String,
    },

    /// The injected device-key store could not load or create a key.
    #[error("device key store {path} failed: {message}")]
    DeviceKeyStore {
        /// Injected store path, for the operator.
        path: String,
        /// What the store reported (never key material).
        message: String,
    },

    /// No `tpmd` socket answered across every injected candidate.
    ///
    /// This is the only signer failure that permits selecting the software fallback.
    #[error("no tpmd endpoint answered (probed: {probed})")]
    TpmdUnavailable {
        /// Comma-separated, in probe order.
        probed: String,
    },

    /// `tpmd` refused the handshake (floor policy, unsupported version, …).
    #[error("tpmd at {endpoint} refused the handshake ({code}): {message}")]
    TpmdRejected {
        /// Socket the daemon answered on.
        endpoint: String,
        /// The daemon's refusal class.
        code: String,
        /// The daemon's own explanation.
        message: String,
    },

    /// `tpmd` cannot serve this machine: its backend dependency is gone.
    #[error("tpmd at {endpoint} cannot serve this machine: {message}")]
    TpmdBackendUnavailable {
        /// Socket the daemon answered on.
        endpoint: String,
        /// The daemon's own explanation.
        message: String,
    },

    /// `tpmd`'s key store stayed locked for the whole wait budget.
    #[error("tpmd store stayed locked at {endpoint}: {message}")]
    TpmdStoreLocked {
        /// Socket the daemon answered on.
        endpoint: String,
        /// The daemon's own explanation.
        message: String,
    },

    /// `tpmd` spoke a frame this adapter cannot interpret.
    #[error("tpmd protocol failure: {message}")]
    TpmdProtocol {
        /// What was unexpected.
        message: String,
    },

    /// A `tpmd` request failed for a reason that retrying cannot fix.
    #[error("tpmd request failed ({code}): {message}")]
    TpmdRequest {
        /// The daemon's error class.
        code: String,
        /// The daemon's own explanation.
        message: String,
    },

    /// `tpmd` resumed/created a key different from the remembered identity.
    ///
    /// A silent device-identity change would invalidate the existing pairing, so this
    /// fails closed and asks the user to re-pair instead.
    #[error("tpmd returned key {actual}, but the remembered identity is {expected}")]
    TpmdKeyMismatch {
        /// Fingerprint the daemon reported.
        actual: String,
        /// Fingerprint the caller remembered.
        expected: String,
    },

    /// The WebSocket transport failed.
    #[error("websocket {phase} failure: {message}")]
    Transport {
        /// `connect`, `challenge`, `hello`, or `welcome`.
        phase: &'static str,
        /// Underlying transport detail.
        message: String,
    },

    /// The server violated the `ospp-v1` state machine or sent malformed JSON.
    #[error("ospp-v1 protocol violation during {phase}: {message}")]
    Protocol {
        /// The handshake step that observed the violation.
        phase: &'static str,
        /// What was unexpected.
        message: String,
    },

    /// A control frame exceeded the 16 KiB `ospp-v1` cap.
    #[error("ospp-v1 control frame exceeds the {limit} byte cap")]
    FrameTooLarge {
        /// The cap in bytes.
        limit: usize,
    },

    /// A bounded handshake or capability deadline elapsed.
    #[error("ospp-v1 {phase} deadline of {millis} ms elapsed")]
    Deadline {
        /// The step that timed out.
        phase: &'static str,
        /// The budget actually armed for that phase, in milliseconds.
        millis: u64,
    },

    /// A capability request or response violated `ospp-v1` or carried an unusable coordinate.
    #[error("ospp-v1 capability {capability} failed for {scope}: {message}")]
    Capability {
        /// Capability wire name (`env.bundle`, `secrets.ensure`, …).
        capability: &'static str,
        /// `"<project>:<mode>"`, the world scope the capability was requested for.
        scope: String,
        /// What was wrong; never a secret value.
        message: String,
    },

    /// The server answered a capability request with a typed refusal.
    #[error("ospp-v1 capability {capability} rejected for {scope}: {kind}")]
    CapabilityRejected {
        /// Capability wire name (`env.bundle`, `secrets.ensure`, …).
        capability: &'static str,
        /// `"<project>:<mode>"`, the world scope the capability was requested for.
        scope: String,
        /// The server's refusal class.
        kind: String,
    },

    /// A streamed seed dump was truncated, mis-hashed, or not a valid archive.
    #[error("ospp-v1 {capability} failed for {scope} ({db_id}): {message}")]
    SeedDump {
        /// Capability wire name (`dump.begin`).
        capability: &'static str,
        /// `"<project>:<mode>"`, the world scope the dump was requested for.
        scope: String,
        /// PostgreSQL database id the dump belongs to.
        db_id: String,
        /// What was wrong; never archive contents.
        message: String,
    },

    /// The local seed-dump cache refused or failed an operation.
    #[error("seed-dump store {path} failed: {message}")]
    SeedDumpStore {
        /// Offending path, for the operator.
        path: String,
        /// What the store reported.
        message: String,
    },

    /// The server answered `REJECT` with a typed reason.
    #[error("server rejected ospp-v1 hello: {reason}")]
    Rejected {
        /// The server's refusal reason.
        reason: RejectReason,
    },

    /// The pairing URL was not a fragment-only handle URL.
    #[error("invalid pairing URL: {reason}")]
    PairingUrl {
        /// What was wrong (never the handle).
        reason: String,
    },

    /// The server answered a pairing request with a typed refusal.
    #[error("pairing refused: {reason}")]
    PairingRejected {
        /// The server's refusal reason.
        reason: PairingRejectReason,
    },
}
