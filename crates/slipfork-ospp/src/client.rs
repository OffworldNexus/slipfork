//! The Slipfork-owned `OSPP v1` synchronous WebSocket session: server-speaks-first
//! `CHALLENGE -> HELLO -> WELCOME | REJECT`, the `ospp-v1` subprotocol, a 16 KiB control-frame cap,
//! and a bounded first-frame deadline (`ADR-0008` §1).
//!
//! The transport is synchronous `tungstenite` over `rustls` + `webpki-roots`. Nonces come from the
//! injected `EntropySource`; signatures come from the injected `DeviceKeySigner`. A `REJECT` is
//! returned to the caller as a typed error and is never retried with a different signature
//! encoding. TLS exporter channel binding is **reserved, not assumed**: `ospp-v1` does not enable
//! it, matching the `weaver-mux-v1` posture in `docs/research/tunnel-weaver-and-systemd-facts.md`
//! §3, and a future binding would bump the protocol version.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use slipfork_snow::EntropySource;
use tungstenite::client::IntoClientRequest;
use tungstenite::http::{HeaderValue, Request, Uri};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Connector, Message, WebSocket, client_tls_with_config};

use crate::device_key::{DeviceKeySigner, KeyId, SIGNATURE_HEX_LEN};
use crate::error::{OsppError, PairingRejectReason, RejectReason};

/// The `ospp-v1` WebSocket subprotocol (`ADR-0008` §1).
pub const OSPP_SUBPROTOCOL: &str = "ospp-v1";

/// Domain-separation prefix signed for the connect handshake.
pub const CONNECT_TRANSCRIPT_PREFIX: &[u8] = b"ospp-v1";

/// Domain-separation prefix signed for the pairing handshake.
pub const PAIRING_TRANSCRIPT_PREFIX: &[u8] = b"ospp-v1-pair";

/// Connect path on the OSPP host.
pub const CONNECT_PATH: &str = "/_ospp/v1/connect";

/// Pairing path on the OSPP host.
pub const PAIR_PATH: &str = "/_ospp/v1/pair";

/// Protocol version this crate speaks (`version: 1` is the first field of `HELLO`).
pub const PROTOCOL_VERSION: u16 = 1;

/// Hard cap on every control/RPC text frame (16 KiB).
pub const MAX_CONTROL_FRAME: usize = 16_384;

/// Length of both `nonce_s` and `nonce_c`.
pub const NONCE_BYTES: usize = 32;

/// Hex character count of a nonce.
pub const NONCE_HEX_LEN: usize = NONCE_BYTES * 2;

/// Default bound on TCP connect and on the first server frame.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Injected transport budgets for one `ospp-v1` session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OsppTimeouts {
    /// Bound on establishing the TCP connection.
    pub connect: Duration,
    /// Bound on the first server frame (`CHALLENGE`, and every handshake frame).
    pub handshake: Duration,
    /// Bound on post-handshake reads.
    pub io: Duration,
}

impl Default for OsppTimeouts {
    fn default() -> Self {
        Self {
            connect: DEFAULT_TIMEOUT,
            handshake: DEFAULT_TIMEOUT,
            io: Duration::from_secs(30),
        }
    }
}

/// A validated OSPP endpoint: `wss://<host>:<port>/_ospp/v1/...`.
///
/// `ws://` is accepted only for a literal loopback host, so a test harness never becomes a
/// production plaintext side-door.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsppEndpoint {
    host: String,
    port: u16,
    tls: bool,
    path: &'static str,
}

impl OsppEndpoint {
    /// The connect endpoint (`/_ospp/v1/connect`).
    pub fn connect(host: &str, port: u16, tls: bool) -> Result<Self, OsppError> {
        Self::new(host, port, tls, CONNECT_PATH)
    }

    /// The pairing endpoint (`/_ospp/v1/pair`).
    pub fn pair(host: &str, port: u16, tls: bool) -> Result<Self, OsppError> {
        Self::new(host, port, tls, PAIR_PATH)
    }

    fn new(host: &str, port: u16, tls: bool, path: &'static str) -> Result<Self, OsppError> {
        if host.is_empty() || !host.is_ascii() || host.bytes().any(|b| b.is_ascii_whitespace()) {
            return Err(OsppError::Protocol {
                phase: "connect",
                message: "host must be non-empty ASCII without whitespace".to_owned(),
            });
        }
        if host.contains('/') || host.contains('@') {
            return Err(OsppError::Protocol {
                phase: "connect",
                message: "host must be a bare hostname or IP address".to_owned(),
            });
        }
        if port == 0 {
            return Err(OsppError::Protocol {
                phase: "connect",
                message: "port must be non-zero".to_owned(),
            });
        }
        let host = host.to_ascii_lowercase();
        if !tls && !is_loopback_host(&host) {
            return Err(OsppError::Protocol {
                phase: "connect",
                message: "plaintext ws:// is permitted only on loopback".to_owned(),
            });
        }
        Ok(Self {
            host,
            port,
            tls,
            path,
        })
    }

    /// The exact hostname string mixed into the signed transcript (no port, lowercase ASCII).
    pub fn server_name(&self) -> &str {
        &self.host
    }

    /// The TCP port.
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Whether the endpoint uses TLS.
    pub const fn uses_tls(&self) -> bool {
        self.tls
    }

    /// The request path.
    pub const fn path(&self) -> &'static str {
        self.path
    }

    fn uri(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let scheme = if self.tls { "wss" } else { "ws" };
        format!("{scheme}://{host}:{}{}", self.port, self.path)
    }
}

/// The decoded `CHALLENGE` nonce.
pub fn parse_nonce(raw: &str) -> Result<[u8; NONCE_BYTES], OsppError> {
    if raw.len() != NONCE_HEX_LEN {
        return Err(OsppError::Protocol {
            phase: "challenge",
            message: format!(
                "expected a {NONCE_HEX_LEN}-character nonce, got {} characters",
                raw.len()
            ),
        });
    }
    if !raw.bytes().all(is_lower_hex_digit) {
        return Err(OsppError::Protocol {
            phase: "challenge",
            message: "nonce must be lowercase hex".to_owned(),
        });
    }
    let mut nonce = [0u8; NONCE_BYTES];
    let _ = hex::decode_to_slice(raw, &mut nonce);
    Ok(nonce)
}

/// Draw a fresh 32-byte client nonce from the injected entropy source.
pub fn random_nonce(entropy: &dyn EntropySource) -> Result<[u8; NONCE_BYTES], OsppError> {
    let mut nonce = [0u8; NONCE_BYTES];
    entropy.fill_bytes(&mut nonce)?;
    Ok(nonce)
}

/// Hex-encode `bytes` into a caller-owned fixed buffer.
pub(crate) fn encode_hex_into(
    bytes: &[u8],
    out: &mut [u8],
    phase: &'static str,
) -> Result<(), OsppError> {
    hex::encode_to_slice(bytes, out).map_err(|error| OsppError::Protocol {
        phase,
        message: format!("could not hex-encode {} bytes: {error}", bytes.len()),
    })
}

/// Hex-encode `bytes` into a fixed-size array.
pub(crate) fn hex_fixed<const N: usize>(
    bytes: &[u8],
    phase: &'static str,
) -> Result<[u8; N], OsppError> {
    let mut out = [0u8; N];
    encode_hex_into(bytes, &mut out, phase)?;
    Ok(out)
}

/// Borrow a hex encoding as `&str`; hex output is always ASCII.
pub(crate) fn hex_str<'a>(bytes: &'a [u8], phase: &'static str) -> Result<&'a str, OsppError> {
    std::str::from_utf8(bytes).map_err(|error| OsppError::Protocol {
        phase,
        message: format!("hex encoding was not UTF-8: {error}"),
    })
}

/// The exact bytes signed by `HELLO`:
/// `"ospp-v1" || nonce_s(32) || nonce_c(32) || server_name`.
pub fn connect_transcript(
    nonce_s: &[u8; NONCE_BYTES],
    nonce_c: &[u8; NONCE_BYTES],
    server_name: &str,
) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(CONNECT_TRANSCRIPT_PREFIX.len() + NONCE_BYTES * 2 + server_name.len());
    out.extend_from_slice(CONNECT_TRANSCRIPT_PREFIX);
    out.extend_from_slice(nonce_s);
    out.extend_from_slice(nonce_c);
    out.extend_from_slice(server_name.as_bytes());
    out
}

/// The exact bytes signed by `pair_hello`:
/// `"ospp-v1-pair" || nonce_s(32) || nonce_c(32) || handle(32) || server_name`.
pub fn pairing_transcript(
    nonce_s: &[u8; NONCE_BYTES],
    nonce_c: &[u8; NONCE_BYTES],
    handle: &[u8; 32],
    server_name: &str,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        PAIRING_TRANSCRIPT_PREFIX.len() + NONCE_BYTES * 2 + handle.len() + server_name.len(),
    );
    out.extend_from_slice(PAIRING_TRANSCRIPT_PREFIX);
    out.extend_from_slice(nonce_s);
    out.extend_from_slice(nonce_c);
    out.extend_from_slice(handle);
    out.extend_from_slice(server_name.as_bytes());
    out
}

/// An authenticated, synchronous `ospp-v1` session.
///
/// M2.14 adds the typed capability dispatch on top of this authenticated connection; the
/// handshake itself is complete here.
pub struct OsppClient {
    connection: OsppConnection,
    key_id: KeyId,
    backend_label: &'static str,
    server_name: String,
}

impl OsppClient {
    /// Dial `endpoint`, perform `CHALLENGE -> HELLO -> WELCOME`, and hold the authenticated session.
    pub fn connect(
        endpoint: &OsppEndpoint,
        signer: &dyn DeviceKeySigner,
        entropy: &dyn EntropySource,
        timeouts: OsppTimeouts,
    ) -> Result<Self, OsppError> {
        if endpoint.path() != CONNECT_PATH {
            return Err(OsppError::Protocol {
                phase: "connect",
                message: "OsppClient::connect requires the /_ospp/v1/connect endpoint".to_owned(),
            });
        }
        let mut connection = OsppConnection::open(endpoint, OSPP_SUBPROTOCOL, timeouts)?;
        let nonce_s = connection.expect_challenge("challenge")?;
        let nonce_c = random_nonce(entropy)?;
        let transcript = connect_transcript(&nonce_s, &nonce_c, endpoint.server_name());
        let signature = signer.sign(&transcript)?;

        let nonce_hex = hex_fixed::<NONCE_HEX_LEN>(&nonce_c, "hello")?;
        let sig_hex = hex_fixed::<SIGNATURE_HEX_LEN>(signature.as_bytes(), "hello")?;

        let hello = ConnectHello {
            kind: "hello",
            version: PROTOCOL_VERSION,
            key_id: signer.key_id().as_str(),
            nonce_c: hex_str(&nonce_hex, "hello")?,
            sig: hex_str(&sig_hex, "hello")?,
        };
        connection.send_json("hello", &hello)?;

        match connection.read_connect_frame("welcome")? {
            ConnectFrame::Welcome { version } if version == PROTOCOL_VERSION => {}
            ConnectFrame::Welcome { version } => {
                return Err(OsppError::Protocol {
                    phase: "welcome",
                    message: format!(
                        "server welcomed ospp-v{version}, but this client speaks v{PROTOCOL_VERSION}"
                    ),
                });
            }
            ConnectFrame::Reject { reason } => {
                return Err(OsppError::Rejected {
                    reason: reason.into(),
                });
            }
            ConnectFrame::Challenge { _nonce_s } => {
                return Err(OsppError::Protocol {
                    phase: "welcome",
                    message: "server sent a second CHALLENGE after HELLO".to_owned(),
                });
            }
        }
        connection.set_io_timeout();
        Ok(Self {
            connection,
            key_id: signer.key_id().clone(),
            backend_label: signer.backend_label(),
            server_name: endpoint.server_name().to_owned(),
        })
    }

    /// The authenticated device fingerprint.
    pub fn key_id(&self) -> &KeyId {
        &self.key_id
    }

    /// The adapter label that produced this session's signatures.
    pub const fn backend_label(&self) -> &'static str {
        self.backend_label
    }

    /// The hostname bound into the signed transcript.
    pub fn server_name(&self) -> &str {
        &self.server_name
    }
}

impl Drop for OsppClient {
    fn drop(&mut self) {
        // Best-effort clean close; a peer that already vanished is not an error worth surfacing.
        let _ = self.connection.socket.close(None);
        let _ = self.connection.socket.flush();
    }
}

/// `HELLO` body (`version` is deliberately the first field, `ADR-0008` §1).
#[derive(Serialize)]
struct ConnectHello<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    version: u16,
    key_id: &'a str,
    nonce_c: &'a str,
    sig: &'a str,
}

/// `pair_hello` body.
#[derive(Serialize)]
pub(crate) struct PairingHello<'a> {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) version: u16,
    pub(crate) key_id: &'a str,
    pub(crate) nonce_c: &'a str,
    pub(crate) handle: &'a str,
    pub(crate) sig: &'a str,
}

impl PairingHello<'_> {
    pub(crate) const KIND: &'static str = "pair_hello";
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ConnectFrame {
    Challenge {
        #[serde(rename = "nonce_s")]
        _nonce_s: String,
    },
    Welcome {
        version: u16,
    },
    Reject {
        reason: WireReject,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum PairingFrame {
    Challenge {
        #[serde(rename = "nonce_s")]
        _nonce_s: String,
    },
    PairApproved {
        account: String,
    },
    Reject {
        reason: WirePairingReject,
    },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WireReject {
    UnknownKey,
    BadSignature,
    UnsupportedVersion { min: u16, max: u16 },
}

impl From<WireReject> for RejectReason {
    fn from(value: WireReject) -> Self {
        match value {
            WireReject::UnknownKey => Self::UnknownKey,
            WireReject::BadSignature => Self::BadSignature,
            WireReject::UnsupportedVersion { min, max } => Self::UnsupportedVersion { min, max },
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum WirePairingReject {
    UnknownHandle,
    ExpiredHandle,
    AlreadyRedeemed,
    BadSignature,
    UnsupportedVersion { min: u16, max: u16 },
}

impl From<WirePairingReject> for PairingRejectReason {
    fn from(value: WirePairingReject) -> Self {
        match value {
            WirePairingReject::UnknownHandle => Self::UnknownHandle,
            WirePairingReject::ExpiredHandle => Self::ExpiredHandle,
            WirePairingReject::AlreadyRedeemed => Self::AlreadyRedeemed,
            WirePairingReject::BadSignature => Self::BadSignature,
            WirePairingReject::UnsupportedVersion { min, max } => {
                Self::UnsupportedVersion { min, max }
            }
        }
    }
}

/// The live WebSocket, kept private to this module; only the state machine above may speak.
pub(crate) struct OsppConnection {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    timeouts: OsppTimeouts,
}

impl OsppConnection {
    pub(crate) fn open(
        endpoint: &OsppEndpoint,
        subprotocol: &'static str,
        timeouts: OsppTimeouts,
    ) -> Result<Self, OsppError> {
        let uri: Uri = endpoint
            .uri()
            .parse()
            .map_err(|error| OsppError::Transport {
                phase: "connect",
                message: format!("invalid endpoint URI: {error}"),
            })?;
        let mut request: Request<()> = uri
            .clone()
            .into_client_request()
            .map_err(|error| transport("connect", error))?;
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            HeaderValue::from_static(subprotocol),
        );

        let stream = tcp_connect(&endpoint.host, endpoint.port, timeouts.connect)?;
        stream
            .set_nodelay(true)
            .map_err(|error| transport("connect", error))?;
        stream
            .set_read_timeout(Some(timeouts.handshake))
            .map_err(|error| transport("connect", error))?;
        stream
            .set_write_timeout(Some(timeouts.io))
            .map_err(|error| transport("connect", error))?;

        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_CONTROL_FRAME))
            .max_frame_size(Some(MAX_CONTROL_FRAME))
            .write_buffer_size(MAX_CONTROL_FRAME);

        let connector = if endpoint.uses_tls() {
            Connector::Rustls(Arc::new(tls_config()?))
        } else {
            Connector::Plain
        };
        let (socket, response) =
            client_tls_with_config(request, stream, Some(config), Some(connector)).map_err(
                |error| match error {
                    tungstenite::HandshakeError::Failure(error) => transport("connect", error),
                    tungstenite::HandshakeError::Interrupted(_) => OsppError::Transport {
                        phase: "connect",
                        message: "TLS/WebSocket handshake would block".to_owned(),
                    },
                },
            )?;
        let selected = response
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|value| value.to_str().ok());
        if selected != Some(subprotocol) {
            return Err(OsppError::Protocol {
                phase: "connect",
                message: format!(
                    "server did not select the `{subprotocol}` subprotocol (got {selected:?})"
                ),
            });
        }
        Ok(Self { socket, timeouts })
    }

    /// Read the first frame and require it to be `CHALLENGE`.
    pub(crate) fn expect_challenge(
        &mut self,
        phase: &'static str,
    ) -> Result<[u8; NONCE_BYTES], OsppError> {
        let text = self.read_frame_text(phase)?;
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum Frame {
            Challenge { nonce_s: String },
        }
        match serde_json::from_str::<Frame>(&text) {
            Ok(Frame::Challenge { nonce_s }) => parse_nonce(&nonce_s),
            Err(error) => Err(OsppError::Protocol {
                phase,
                message: format!("expected a server CHALLENGE: {error}"),
            }),
        }
    }

    pub(crate) fn send_json<T: Serialize>(
        &mut self,
        phase: &'static str,
        value: &T,
    ) -> Result<(), OsppError> {
        let text = serde_json::to_string(value).map_err(|error| OsppError::Protocol {
            phase,
            message: format!("could not encode {phase}: {error}"),
        })?;
        if text.len() > MAX_CONTROL_FRAME {
            return Err(OsppError::FrameTooLarge {
                limit: MAX_CONTROL_FRAME,
            });
        }
        self.socket
            .send(Message::Text(text.into()))
            .map_err(|error| transport(phase, error))
    }

    fn read_connect_frame(&mut self, phase: &'static str) -> Result<ConnectFrame, OsppError> {
        let text = self.read_frame_text(phase)?;
        serde_json::from_str(&text).map_err(|error| OsppError::Protocol {
            phase,
            message: format!("malformed server frame: {error}"),
        })
    }

    pub(crate) fn read_pairing_frame(
        &mut self,
        phase: &'static str,
    ) -> Result<PairingFrame, OsppError> {
        let text = self.read_frame_text(phase)?;
        serde_json::from_str(&text).map_err(|error| OsppError::Protocol {
            phase,
            message: format!("malformed server frame: {error}"),
        })
    }

    pub(crate) fn set_io_timeout(&mut self) {
        let _ = set_stream_read_timeout(&self.socket, Some(self.timeouts.io));
    }

    fn read_frame_text(&mut self, phase: &'static str) -> Result<String, OsppError> {
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    if text.len() > MAX_CONTROL_FRAME {
                        return Err(OsppError::FrameTooLarge {
                            limit: MAX_CONTROL_FRAME,
                        });
                    }
                    return Ok(text.to_string());
                }
                Ok(Message::Ping(_)) => {
                    // tungstenite queues the pong; flush it so the peer sees liveness.
                    self.socket
                        .flush()
                        .map_err(|error| transport(phase, error))?;
                }
                Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
                Ok(Message::Binary(_)) => {
                    return Err(OsppError::Protocol {
                        phase,
                        message: "binary frame on the control channel".to_owned(),
                    });
                }
                Ok(Message::Close(_)) => {
                    return Err(OsppError::Transport {
                        phase,
                        message: "server closed the connection".to_owned(),
                    });
                }
                Err(tungstenite::Error::Io(error)) if is_timeout(&error) => {
                    return Err(OsppError::Deadline {
                        phase,
                        millis: self.timeouts.handshake.as_millis() as u64,
                    });
                }
                Err(tungstenite::Error::Capacity(error)) => {
                    return Err(OsppError::Protocol {
                        phase,
                        message: format!("control frame exceeded the 16 KiB cap: {error}"),
                    });
                }
                Err(error) => return Err(transport(phase, error)),
            }
        }
    }
}

fn tcp_connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, OsppError> {
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| OsppError::Transport {
            phase: "connect",
            message: format!("could not resolve {host}:{port}: {error}"),
        })?;
    let mut last = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => return Ok(stream),
            Err(error) => last = Some(error),
        }
    }
    Err(OsppError::Transport {
        phase: "connect",
        message: match last {
            Some(error) => format!("could not connect to {host}:{port}: {error}"),
            None => format!("{host}:{port} resolved to no addresses"),
        },
    })
}

fn tls_config() -> Result<rustls::ClientConfig, OsppError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Ok(rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth())
}

fn set_stream_read_timeout(
    socket: &WebSocket<MaybeTlsStream<TcpStream>>,
    timeout: Option<Duration>,
) -> io::Result<()> {
    let stream = socket.get_ref();
    match stream {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(timeout),
        MaybeTlsStream::Rustls(stream) => stream.sock.set_read_timeout(timeout),
        _ => Ok(()),
    }
}

fn transport(phase: &'static str, error: impl std::fmt::Display) -> OsppError {
    OsppError::Transport {
        phase,
        message: error.to_string(),
    }
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

const fn is_lower_hex_digit(byte: u8) -> bool {
    matches!(byte, b'0'..=b'9' | b'a'..=b'f')
}
