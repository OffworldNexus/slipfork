//! Fragment-only browser pairing: [`PairingUrl`] accepts a handle **only** in
//! `#handle=<64 lowercase hex>`, never in a query or path; [`PairingHandle`] zeroizes on `Drop`;
//! and the pairing client signs the `"ospp-v1-pair"` transcript before sending the handle once.
//!
//! `ADR-0008` §1 is normative. Reading the URL from `stdin` is the CLI's job; this module only
//! guarantees that the secret never becomes part of a log line, a `Debug` rendering, or a
//! serialized (`--json`) representation.

use std::fmt;

use serde::{Serialize, Serializer};
use slipfork_snow::EntropySource;

use crate::client::{
    OSPP_SUBPROTOCOL, OsppConnection, OsppEndpoint, OsppTimeouts, PROTOCOL_VERSION, PairingHello,
};
use crate::device_key::DeviceKeySigner;
use crate::error::OsppError;
use crate::hex::{encode_hex_into, hex_fixed, hex_str, is_lower_hex_digit};
use crate::protocol::ServerFrame;

/// Byte length of a single-use pairing handle.
pub const HANDLE_BYTES: usize = 32;

/// Hex character count of a pairing handle.
pub const HANDLE_HEX_LEN: usize = HANDLE_BYTES * 2;

/// Text substituted for a pairing handle wherever a URL is rendered or logged.
pub const HANDLE_REDACTION: &str = "[REDACTED]";

/// A single-use pairing capability, held as raw bytes and zeroized on `Drop`.
///
/// It has no public accessor that returns an owned `String`: the only way to read it is a
/// caller-provided stack buffer, so the secret does not escape into an unzeroized allocation.
pub struct PairingHandle([u8; HANDLE_BYTES]);

impl PairingHandle {
    fn from_hex(raw: &str) -> Result<Self, OsppError> {
        if raw.len() != HANDLE_HEX_LEN {
            return Err(invalid_url(format!(
                "expected a {HANDLE_HEX_LEN}-character handle, got {} characters",
                raw.len()
            )));
        }
        if !raw.bytes().all(is_lower_hex_digit) {
            return Err(invalid_url("expected a lowercase hex handle"));
        }
        let mut bytes = [0u8; HANDLE_BYTES];
        let _ = hex::decode_to_slice(raw, &mut bytes);
        Ok(Self(bytes))
    }

    /// The raw 32 capability bytes, for the pairing transcript and JSON field.
    pub const fn as_bytes(&self) -> &[u8; HANDLE_BYTES] {
        &self.0
    }

    /// Encode the handle into a caller-owned stack buffer (64 lowercase hex bytes).
    ///
    /// The caller is responsible for zeroizing the buffer after the single use. An owned `String`
    /// is deliberately not offered, because it cannot be reliably wiped.
    pub fn hex_into(&self, out: &mut [u8; HANDLE_HEX_LEN]) -> Result<(), OsppError> {
        encode_hex_into(&self.0, out, "pair_hello")
    }
}

impl Drop for PairingHandle {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl fmt::Debug for PairingHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingHandle([REDACTED])")
    }
}

/// The transport scheme of a pairing URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingScheme {
    /// `https://` — the pairing socket is `wss://`.
    Https,
    /// `http://` — permitted only for a loopback test harness.
    Http,
}

impl PairingScheme {
    /// Whether the derived OSPP socket uses TLS.
    pub const fn uses_tls(self) -> bool {
        matches!(self, Self::Https)
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Https => "https",
            Self::Http => "http",
        }
    }
}

/// A browser pairing URL whose handle exists only in the fragment.
///
/// `Display`, `Debug`, and `Serialize` all emit `#handle=[REDACTED]`; [`Self::browser_url`] is the
/// one explicit escape hatch, for an injected [`BrowserOpener`] only.
pub struct PairingUrl {
    scheme: PairingScheme,
    host: String,
    port: u16,
    path: String,
    handle: PairingHandle,
}

/// The live pairing URL, carrying the single-use capability.
///
/// Created only by [`PairingUrl::browser_url`] and consumed by an injected [`BrowserOpener`]. It
/// implements neither `Display` nor `Debug`, and its buffer is overwritten on `Drop`, so the handle
/// cannot land in a log line or an accidental `format!`.
pub struct LivePairingUrl(String);

impl LivePairingUrl {
    /// Borrow the URL, exclusively to hand it to the OS browser opener.
    ///
    /// Never log, serialize, or copy this value into argv by hand.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Drop for LivePairingUrl {
    fn drop(&mut self) {
        let len = self.0.len();
        if len > 0 {
            let blank = " ".repeat(len);
            self.0.replace_range(.., &blank);
            self.0.clear();
        }
    }
}

impl PairingUrl {
    /// Parse `https://<host>[:<port>]/<path>#handle=<64 lowercase hex>`.
    ///
    /// A URL without a fragment fails closed naming the missing handle; a handle in the query or
    /// path is rejected rather than silently promoted.
    pub fn parse(raw: &str) -> Result<Self, OsppError> {
        if raw.trim() != raw {
            return Err(invalid_url("surrounding whitespace is not allowed"));
        }
        let (before_fragment, fragment) = raw
            .split_once('#')
            .ok_or_else(|| invalid_url("missing `#handle=` fragment"))?;
        if fragment.contains('#') {
            return Err(invalid_url("expected exactly one URL fragment"));
        }
        let handle_hex = fragment
            .strip_prefix("handle=")
            .ok_or_else(|| invalid_url("fragment must be exactly `handle=<64 lowercase hex>`"))?;
        if handle_hex.contains('&') {
            return Err(invalid_url("fragment must contain only the pairing handle"));
        }
        let handle = PairingHandle::from_hex(handle_hex)?;

        let (scheme, rest) = if let Some(rest) = before_fragment.strip_prefix("https://") {
            (PairingScheme::Https, rest)
        } else if let Some(rest) = before_fragment.strip_prefix("http://") {
            (PairingScheme::Http, rest)
        } else {
            return Err(invalid_url("expected an `https://` or `http://` URL"));
        };
        if rest.contains('?') {
            return Err(invalid_url("pairing URLs must not carry a query string"));
        }
        let (authority, path) = match rest.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (rest, "/".to_owned()),
        };
        if authority.contains('@') {
            return Err(invalid_url("userinfo is not allowed"));
        }
        if path.to_ascii_lowercase().contains("handle=") {
            return Err(invalid_url(
                "the pairing handle belongs only in the fragment",
            ));
        }
        let (host, port) = parse_authority(authority, scheme)?;
        Ok(Self {
            scheme,
            host,
            port,
            path,
            handle,
        })
    }

    /// The scheme (`https` in production, `http` only for a loopback harness).
    pub const fn scheme(&self) -> PairingScheme {
        self.scheme
    }

    /// The lowercased host, without a port.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The effective port (`443`/`80` when the URL omitted one).
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// The URL path (never contains the handle).
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The redacted rendering safe for logs, errors, and `--json` output.
    pub fn redacted(&self) -> String {
        format!(
            "{}://{}{}{}#handle={HANDLE_REDACTION}",
            self.scheme.as_str(),
            display_host(&self.host),
            if self.omits_default_port() {
                String::new()
            } else {
                format!(":{}", self.port)
            },
            self.path
        )
    }

    /// Materialise the live browser URL for an injected [`BrowserOpener`] only.
    ///
    /// The capability is handed over in a [`LivePairingUrl`], which has no `Display`/`Debug` and
    /// zeroizes on `Drop`, so it cannot be logged or formatted by accident.
    pub fn browser_url(&self) -> Result<LivePairingUrl, OsppError> {
        let mut handle_hex = [0u8; HANDLE_HEX_LEN];
        self.handle.hex_into(&mut handle_hex)?;
        let handle = crate::hex::hex_str(&handle_hex, "pairing_url")?;
        let rendered = format!(
            "{}://{}{}{}#handle={handle}",
            self.scheme.as_str(),
            display_host(&self.host),
            if self.omits_default_port() {
                String::new()
            } else {
                format!(":{}", self.port)
            },
            self.path
        );
        handle_hex.fill(0);
        Ok(LivePairingUrl(rendered))
    }

    /// The pairing WebSocket endpoint derived from this browser URL.
    pub fn ospp_endpoint(&self) -> Result<OsppEndpoint, OsppError> {
        OsppEndpoint::pair(&self.host, self.port, self.scheme.uses_tls())
    }

    /// Consume the URL, returning the endpoint and the single-use handle.
    fn into_parts(self) -> Result<(OsppEndpoint, String, PairingHandle), OsppError> {
        let endpoint = OsppEndpoint::pair(&self.host, self.port, self.scheme.uses_tls())?;
        Ok((endpoint, self.host, self.handle))
    }

    fn omits_default_port(&self) -> bool {
        matches!(
            (self.scheme, self.port),
            (PairingScheme::Https, 443) | (PairingScheme::Http, 80)
        )
    }
}

impl fmt::Debug for PairingUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PairingUrl({})", self.redacted())
    }
}

impl fmt::Display for PairingUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.redacted())
    }
}

impl Serialize for PairingUrl {
    /// Serialize the redacted URL so a `--json` rendering can never carry the capability.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.redacted())
    }
}

/// The result of a successful pairing redemption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingApproval {
    /// The account the pairing handle was minted for.
    pub account: String,
}

/// The injected browser-launch port; `slipforkd`/the CLI implements it without leaking the handle
/// into argv (the live URL is handed to the opener in-process and zeroized after use).
pub trait BrowserOpener: Send + Sync {
    /// Open `url` in the developer's browser. The live capability is consumed here.
    fn open_pairing_url(&self, url: LivePairingUrl) -> Result<(), OsppError>;
}

/// Redeem `url` once over `/_ospp/v1/pair` (`ospp-v1` subprotocol).
///
/// The handle is sent exactly once, in the signed `pair_hello`, and its stack encoding is zeroized
/// immediately afterwards. A failure never retries with a different signature encoding.
pub fn redeem_pairing_url(
    url: PairingUrl,
    signer: &dyn DeviceKeySigner,
    entropy: &dyn EntropySource,
    timeouts: OsppTimeouts,
) -> Result<PairingApproval, OsppError> {
    let (endpoint, server_name, handle) = url.into_parts()?;
    let mut connection = OsppConnection::open(&endpoint, OSPP_SUBPROTOCOL, timeouts)?;
    let nonce_s = connection.expect_challenge("challenge")?;
    let nonce_c = crate::client::random_nonce(entropy)?;
    let transcript = crate::client::pairing_transcript(
        &nonce_s,
        &nonce_c,
        handle.as_bytes(),
        server_name.as_str(),
    );
    let signature = signer.sign(&transcript)?;

    let mut handle_hex = [0u8; HANDLE_HEX_LEN];
    handle.hex_into(&mut handle_hex)?;
    let nonce_hex = hex_fixed::<64>(&nonce_c, "pair_hello")?;
    let sig_hex = hex_fixed::<128>(signature.as_bytes(), "pair_hello")?;

    let hello = PairingHello {
        kind: PairingHello::KIND,
        version: PROTOCOL_VERSION,
        key_id: signer.key_id().as_str(),
        nonce_c: hex_str(&nonce_hex, "pair_hello")?,
        handle: hex_str(&handle_hex, "pair_hello")?,
        sig: hex_str(&sig_hex, "pair_hello")?,
    };
    let sent = connection.send_json("pair_hello", &hello);
    handle_hex.fill(0);
    sent?;
    drop(handle);

    match connection.read_frame("pair_approved")? {
        ServerFrame::PairApproved { account } => Ok(PairingApproval { account }),
        ServerFrame::Reject { reason } => Err(match reason.into_pairing() {
            Some(reason) => OsppError::PairingRejected { reason },
            None => invalid_url("server sent a connect-only refusal during pair_hello"),
        }),
        other => Err(OsppError::Protocol {
            phase: "pair_approved",
            message: format!("expected pair_approved, got `{}`", other.kind()),
        }),
    }
}

fn parse_authority(authority: &str, scheme: PairingScheme) -> Result<(String, u16), OsppError> {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, rest) = rest
            .split_once(']')
            .ok_or_else(|| invalid_url("unterminated IPv6 host"))?;
        let port = match rest {
            "" => None,
            _ => Some(
                rest.strip_prefix(':')
                    .ok_or_else(|| invalid_url("expected `:<port>` after the IPv6 host"))?,
            ),
        };
        (host, port)
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        (host, Some(port))
    } else {
        (authority, None)
    };
    if host.is_empty() {
        return Err(invalid_url("missing host"));
    }
    if !host.is_ascii() || host.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(invalid_url(
            "host must be non-empty ASCII without whitespace",
        ));
    }
    let host = host.to_ascii_lowercase();
    let port = match port {
        Some(raw) => raw
            .parse::<u16>()
            .map_err(|_| invalid_url("invalid port"))?,
        None => match scheme {
            PairingScheme::Https => 443,
            PairingScheme::Http => 80,
        },
    };
    if port == 0 {
        return Err(invalid_url("port must be non-zero"));
    }
    Ok((host, port))
}

fn display_host(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

fn invalid_url(reason: impl Into<String>) -> OsppError {
    OsppError::PairingUrl {
        reason: reason.into(),
    }
}
