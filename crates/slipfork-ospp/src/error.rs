//! The single typed error for the OSPP v1 client.
//!
//! Every variant is fail-closed and nothing carries a bearer token or private
//! key material: transport and URL failures report a scheme, host, or status —
//! never the full dump URL.

use std::io;

use thiserror::Error;

/// The typed `error.code` strings the OSPP v1 spec names explicitly
/// (`specs/05-ospp-v1.md` §9).
pub mod codes {
    /// The client and server share no protocol version.
    pub const UNSUPPORTED_VERSION: &str = "unsupported_version";
    /// No `auth_methods` / `key_methods` entry is supported by the server.
    pub const UNSUPPORTED_AUTH: &str = "unsupported_auth";
    /// No `encryption_methods` entry is supported by the client.
    pub const UNSUPPORTED_ENCRYPTION: &str = "unsupported_encryption";
    /// The authenticated key may not act on the project.
    pub const UNAUTHORIZED_KEY: &str = "unauthorized_key";
    /// The verbatim Git remote is unknown to the server.
    pub const UNKNOWN_REMOTE: &str = "unknown_remote";
    /// The proof-of-possession signature did not verify.
    pub const BAD_PROOF: &str = "bad_proof";
    /// The browser rejected the pairing request.
    pub const REJECTED: &str = "rejected";
    /// The machine key is already enrolled elsewhere.
    pub const ALREADY_PAIRED: &str = "already_paired";
    /// The unauthenticated pairing attempt was rate-limited.
    pub const THROTTLED: &str = "throttled";
    /// An unclaimed `${{ ... }}` entry could not resolve.
    pub const UNRESOLVED: &str = "unresolved";
    /// The requested resource dump is unavailable.
    pub const DUMP_UNAVAILABLE: &str = "dump_unavailable";
    /// A frame, field, or value violated the wire specification.
    pub const BAD_FRAME: &str = "bad_frame";
}

/// An `ospp-v1` client failure.
#[derive(Debug, Error)]
pub enum OsppError {
    /// The endpoint URL is not a valid `ws(s)://` identity (host only, never secrets).
    #[error("ospp-v1 endpoint {url_host} is invalid: {reason}")]
    InvalidEndpoint {
        /// Host (or `<unparseable>`) reported by the URL, never userinfo.
        url_host: String,
        /// Why the endpoint was refused.
        reason: String,
    },
    /// The out-of-band dump endpoint refused the request with an HTTP status.
    #[error("ospp-v1 dump endpoint returned HTTP {status}")]
    DumpHttp {
        /// HTTP status code.
        status: u16,
    },
    /// A transport failed; `kind` names the layer without echoing the URL.
    #[error("ospp-v1 {kind} transport failed: {message}")]
    Transport {
        /// Transport layer (`socket`, `websocket`, `dump-http`, `dump-url`, `io`).
        kind: &'static str,
        /// Failure description with no bearer token.
        message: String,
    },
    /// A received frame is not part of the closed `ospp-v1` frame set.
    #[error("ospp-v1 protocol violation: {0}")]
    Protocol(String),
    /// The server answered with an `error` frame.
    #[error("ospp-v1 server error ({code}): {message}")]
    Server {
        /// Correlation id when the error answers a specific request.
        id: Option<String>,
        /// Typed code from the server.
        code: String,
        /// Human-readable explanation.
        message: String,
    },
    /// An ECDSA P-256 signature did not verify over the reconstructed transcript.
    #[error("ospp-v1 signature verification failed")]
    InvalidSignature,
    /// Hex, SEC1, or scalar key material was malformed.
    #[error("ospp-v1 key material is invalid: {0}")]
    InvalidKey(String),
    /// An RFC 9180 HPKE operation failed.
    #[error("ospp-v1 hpke operation failed: {0}")]
    Hpke(String),
    /// The session did not answer within the injected I/O bound.
    #[error("ospp-v1 session timed out")]
    Timeout,
    /// The session thread is gone or was closed by the caller.
    #[error("ospp-v1 session is closed")]
    Closed,
    /// A sealed dump stream ended before its mandatory `len = 0` EOF frame.
    #[error("ospp-v1 dump stream is truncated")]
    TruncatedDump,
    /// The injected machine-key file could not be read or created.
    #[error("ospp-v1 device key I/O failed at {path}: {message}")]
    KeyStore {
        /// Injected key path.
        path: String,
        /// Underlying failure.
        message: String,
    },
    /// The injected pairing-state or vault file could not be read or created.
    #[error("ospp-v1 state I/O failed at {path}: {message}")]
    State {
        /// Injected state path.
        path: String,
        /// Underlying failure.
        message: String,
    },
    /// A pairing handle or server-supplied URL is malformed.
    #[error("ospp-v1 pairing URL is invalid: {0}")]
    InvalidPairingUrl(String),
    /// Nobody approved the pairing before the injected window elapsed.
    #[error("ospp-v1 pairing window closed\nnext: open the pairing URL again")]
    PairingWindowClosed,
    /// The operating-system random source failed.
    #[error("ospp-v1 entropy source failed: {0}")]
    Entropy(String),
}

impl OsppError {
    /// The typed server code when this error came from an `error` frame.
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Server { code, .. } => Some(code),
            _ => None,
        }
    }

    /// Build a server-error variant from a received frame.
    pub fn server(id: Option<String>, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Server {
            id,
            code: code.into(),
            message: message.into(),
        }
    }
}

impl From<io::Error> for OsppError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Self::Timeout,
            io::ErrorKind::UnexpectedEof => Self::TruncatedDump,
            _ => Self::Transport {
                kind: "io",
                message: error.to_string(),
            },
        }
    }
}

impl From<tungstenite::Error> for OsppError {
    fn from(error: tungstenite::Error) -> Self {
        match error {
            tungstenite::Error::Io(error) => error.into(),
            tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => {
                Self::Closed
            }
            tungstenite::Error::Utf8(error) => {
                Self::Protocol(format!("frame is not UTF-8: {error}"))
            }
            other => Self::Transport {
                kind: "websocket",
                message: other.to_string(),
            },
        }
    }
}
