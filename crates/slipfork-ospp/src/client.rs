//! The Slipfork-owned `OSPP v1` synchronous WebSocket session: server-speaks-first
//! `CHALLENGE -> HELLO -> WELCOME | REJECT`, the `ospp-v1` subprotocol, a 16 KiB control-frame cap,
//! a 64 KiB binary-frame cap, and bounded deadlines (`ADR-0008` §1–§2).
//!
//! The transport is synchronous `tungstenite` over `rustls` + `webpki-roots`. Nonces come from the
//! injected `EntropySource`; signatures come from the injected `DeviceKeySigner`. A `REJECT` is
//! returned to the caller as a typed error and is never retried with a different signature
//! encoding. TLS exporter channel binding is **reserved, not assumed**: `ospp-v1` does not enable
//! it, matching the `weaver-mux-v1` posture in `docs/research/tunnel-weaver-and-systemd-facts.md`
//! §3, and a future binding would bump the protocol version.
//!
//! After `WELCOME` one authenticated session serves every capability ([`EnvBundle`],
//! [`CAPABILITY_ENV_BUNDLE`], [`CAPABILITY_SECRETS_ENSURE`], [`CAPABILITY_RESOURCES_DESCRIBE`]):
//! the daemon-held [`OsppClient`] serializes requests behind one interior mutex, so
//! `SecretProvider`'s `&self` contract is satisfied without a global runtime or connection pool.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::Serialize;
use slipfork_snow::EntropySource;
use slipfork_world::ProjectSlug;
use tungstenite::client::IntoClientRequest;
use tungstenite::http::{HeaderValue, Request, Uri};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Connector, Message, WebSocket, client_tls_with_config};

use crate::device_key::{DeviceKeySigner, KeyId, SIGNATURE_HEX_LEN};
use crate::error::OsppError;
use crate::hex::{hex_fixed, hex_str, is_lower_hex_digit};
use crate::protocol::{
    CAPABILITY_ENV_BUNDLE, CAPABILITY_RESOURCES_DESCRIBE, CAPABILITY_SECRETS_ENSURE,
    CapabilityRequest, EnvBundle, ServerFrame, capability_error, validate_key, validate_mode,
};
use crate::resources::RemoteResourceBundle;

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

/// Hard cap on every seed-dump binary WebSocket frame (64 KiB).
pub const MAX_DUMP_FRAME: usize = 65_536;

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
/// One instance is held by `slipforkd` and shared across capability callers: requests are
/// serialized on the single authenticated WebSocket, and `Send + Sync` comes from the interior
/// mutex rather than from a global runtime or connection pool.
pub struct OsppClient {
    session: Mutex<OsppConnection>,
    key_id: KeyId,
    backend_label: &'static str,
    server_name: String,
    next_request: AtomicU32,
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

        match connection.read_frame("welcome")? {
            ServerFrame::Welcome { version } if version == PROTOCOL_VERSION => {}
            ServerFrame::Welcome { version } => {
                return Err(OsppError::Protocol {
                    phase: "welcome",
                    message: format!(
                        "server welcomed ospp-v{version}, but this client speaks v{PROTOCOL_VERSION}"
                    ),
                });
            }
            ServerFrame::Reject { reason } => {
                return Err(match reason.into_connect() {
                    Some(reason) => OsppError::Rejected { reason },
                    None => OsppError::Protocol {
                        phase: "welcome",
                        message: "server sent a pairing-only refusal during HELLO".to_owned(),
                    },
                });
            }
            other => {
                return Err(OsppError::Protocol {
                    phase: "welcome",
                    message: format!("expected WELCOME, got `{}`", other.kind()),
                });
            }
        }
        connection.set_io_timeout();
        Ok(Self {
            session: Mutex::new(connection),
            key_id: signer.key_id().clone(),
            backend_label: signer.backend_label(),
            server_name: endpoint.server_name().to_owned(),
            next_request: AtomicU32::new(1),
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

    /// Fetch the `vars` / `secrets` / (`cloud`-only) `domains` snapshot for one world scope.
    pub fn env_bundle(&self, project: &ProjectSlug, mode: &str) -> Result<EnvBundle, OsppError> {
        let capability = CAPABILITY_ENV_BUNDLE;
        let scope = scope_label(project, mode);
        validate_mode(mode).map_err(|message| capability_error(capability, &scope, message))?;
        let request_id = self.request_id();
        let mut session = self.lock();
        session.send_json(
            capability,
            &CapabilityRequest::EnvBundle {
                request_id,
                project: project.as_str(),
                mode,
            },
        )?;
        match session.read_frame(capability)? {
            ServerFrame::EnvBundle {
                request_id: seen,
                vars,
                secrets,
                domains,
            } if seen == request_id => Ok(EnvBundle::new(vars, secrets, domains)),
            other => Err(other.into_refusal(capability, &scope, request_id)),
        }
    }

    /// Compare-and-set one `!!` secret (`put_if_absent`), returning the authoritative value.
    pub fn ensure_secret(
        &self,
        project: &ProjectSlug,
        mode: &str,
        key: &str,
        proposed: &str,
    ) -> Result<String, OsppError> {
        let capability = CAPABILITY_SECRETS_ENSURE;
        let scope = scope_label(project, mode);
        validate_scope(project, mode, key)?;
        let request_id = self.request_id();
        let mut session = self.lock();
        session.send_json(
            capability,
            &CapabilityRequest::SecretsEnsure {
                request_id,
                project: project.as_str(),
                mode,
                key,
                proposed,
            },
        )?;
        match session.read_frame(capability)? {
            ServerFrame::SecretsEnsure {
                request_id: seen,
                key: sent_key,
                value,
            } if seen == request_id => {
                if sent_key != key || value.is_empty() {
                    return Err(capability_error(
                        capability,
                        &scope,
                        "server returned a different key or an empty stored value",
                    ));
                }
                Ok(value)
            }
            other => Err(other.into_refusal(capability, &scope, request_id)),
        }
    }

    /// The single conversion from the wire `resources.describe` body to
    /// `slipfork_dag::RemoteResourceOffer` (`ADR-0008` §3).
    ///
    /// `slipforkd` passes the result straight into
    /// `slipfork_dag::ResourcePlan::resolve(spec, mode, dump_policy, offer.as_ref())`; no
    /// `ResourceStrategy` or `DumpPolicy` decision is made here.
    pub fn resource_offer(
        &self,
        project: &ProjectSlug,
        mode: &str,
    ) -> Result<Option<slipfork_dag::RemoteResourceOffer>, OsppError> {
        let bundle = self.resources_describe(project, mode)?;
        if bundle.is_empty() {
            return Ok(None);
        }
        Ok(Some(bundle.into_offer(&scope_label(project, mode))?))
    }

    fn resources_describe(
        &self,
        project: &ProjectSlug,
        mode: &str,
    ) -> Result<RemoteResourceBundle, OsppError> {
        let capability = CAPABILITY_RESOURCES_DESCRIBE;
        let scope = scope_label(project, mode);
        validate_mode(mode).map_err(|message| capability_error(capability, &scope, message))?;
        let request_id = self.request_id();
        let mut session = self.lock();
        session.send_json(
            capability,
            &CapabilityRequest::ResourcesDescribe {
                request_id,
                project: project.as_str(),
                mode,
            },
        )?;
        match session.read_frame(capability)? {
            ServerFrame::ResourcesDescribe {
                request_id: seen,
                s3_buckets,
                pg_databases,
            } if seen == request_id => Ok(RemoteResourceBundle::new(s3_buckets, pg_databases)),
            other => Err(other.into_refusal(capability, &scope, request_id)),
        }
    }

    /// Next client-chosen request id; ids are per session, never global.
    pub(crate) fn request_id(&self) -> u32 {
        self.next_request.fetch_add(1, Ordering::Relaxed)
    }

    /// Borrow the authenticated connection; callers must not interleave frames manually.
    pub(crate) fn session(&self) -> MutexGuard<'_, OsppConnection> {
        self.lock()
    }

    fn lock(&self) -> MutexGuard<'_, OsppConnection> {
        // Poisoning only means a capability caller panicked while holding the socket. The session
        // is then unusable for anything but a clean close, which the next request surfaces as a
        // typed error; recovering here keeps the other worlds' diagnostics flowing.
        self.session.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for OsppClient {
    fn drop(&mut self) {
        // Best-effort clean close; a peer that already vanished is not an error worth surfacing.
        let mut session = self.lock();
        let _ = session.socket.close(None);
        let _ = session.socket.flush();
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

/// One inbound WebSocket message, after liveness frames are handled.
pub(crate) enum Inbound {
    Text(String),
    Binary(Vec<u8>),
    Closed,
}

/// The live WebSocket, kept private to this module; only the state machine above may speak.
pub(crate) struct OsppConnection {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    timeouts: OsppTimeouts,
    /// The read budget actually armed on the socket for the current phase.
    budget: Duration,
    /// Set when a streamed dump was abandoned mid-frame; the session must be re-established.
    poisoned: Option<&'static str>,
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
            .max_message_size(Some(MAX_DUMP_FRAME))
            .max_frame_size(Some(MAX_DUMP_FRAME))
            .write_buffer_size(MAX_DUMP_FRAME);

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
        Ok(Self {
            socket,
            timeouts,
            budget: timeouts.handshake,
            poisoned: None,
        })
    }

    /// Read the first frame and require it to be `CHALLENGE`.
    pub(crate) fn expect_challenge(
        &mut self,
        phase: &'static str,
    ) -> Result<[u8; NONCE_BYTES], OsppError> {
        match self.read_frame(phase)? {
            ServerFrame::Challenge { nonce_s } => parse_nonce(&nonce_s),
            other => Err(OsppError::Protocol {
                phase,
                message: format!("expected a server CHALLENGE, got `{}`", other.kind()),
            }),
        }
    }

    pub(crate) fn send_json<T: Serialize>(
        &mut self,
        phase: &'static str,
        value: &T,
    ) -> Result<(), OsppError> {
        self.ensure_live(phase)?;
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

    /// Read one typed server frame, rejecting binary frames on the control channel.
    pub(crate) fn read_frame(&mut self, phase: &'static str) -> Result<ServerFrame, OsppError> {
        match self.read_inbound(phase)? {
            Inbound::Text(text) => {
                serde_json::from_str(&text).map_err(|error| OsppError::Protocol {
                    phase,
                    message: format!("malformed server frame: {error}"),
                })
            }
            Inbound::Binary(_) => Err(OsppError::Protocol {
                phase,
                message: "binary frame outside dump.begin..dump.end".to_owned(),
            }),
            Inbound::Closed => Err(OsppError::Transport {
                phase,
                message: "server closed the connection".to_owned(),
            }),
        }
    }

    /// Read one inbound message, enforcing both frame caps and liveness handling.
    pub(crate) fn read_inbound(&mut self, phase: &'static str) -> Result<Inbound, OsppError> {
        self.ensure_live(phase)?;
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    if text.len() > MAX_CONTROL_FRAME {
                        return Err(OsppError::Protocol {
                            phase,
                            message: format!(
                                "text frame of {} bytes exceeded the 16 KiB ({MAX_CONTROL_FRAME}-byte) cap",
                                text.len()
                            ),
                        });
                    }
                    return Ok(Inbound::Text(text.to_string()));
                }
                Ok(Message::Binary(bytes)) => {
                    if bytes.len() > MAX_DUMP_FRAME {
                        return Err(OsppError::Protocol {
                            phase,
                            message: format!(
                                "binary frame of {} bytes exceeded the 64 KiB ({MAX_DUMP_FRAME}-byte) cap",
                                bytes.len()
                            ),
                        });
                    }
                    return Ok(Inbound::Binary(bytes.to_vec()));
                }
                Ok(Message::Ping(_)) => {
                    // tungstenite queues the pong; flush it so the peer sees liveness.
                    self.socket
                        .flush()
                        .map_err(|error| transport(phase, error))?;
                }
                Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
                Ok(Message::Close(_)) => return Ok(Inbound::Closed),
                Err(tungstenite::Error::Io(error)) if is_timeout(&error) => {
                    return Err(OsppError::Deadline {
                        phase,
                        millis: self.budget.as_millis() as u64,
                    });
                }
                Err(tungstenite::Error::Capacity(error)) => {
                    return Err(OsppError::Protocol {
                        phase,
                        message: format!(
                            "wire frame exceeded the {MAX_DUMP_FRAME}-byte cap: {error}"
                        ),
                    });
                }
                Err(error) => return Err(transport(phase, error)),
            }
        }
    }

    pub(crate) fn set_io_timeout(&mut self) {
        self.budget = self.timeouts.io;
        let _ = set_stream_read_timeout(&self.socket, Some(self.timeouts.io));
    }

    /// Mark the session unusable; a streamed dump stopped between `dump.begin` and `dump.end`, so
    /// the frame stream can no longer be trusted and `slipforkd` must reconnect.
    pub(crate) fn poison(&mut self, why: &'static str) {
        self.poisoned = Some(why);
    }

    fn ensure_live(&self, phase: &'static str) -> Result<(), OsppError> {
        match self.poisoned {
            None => Ok(()),
            Some(why) => Err(OsppError::Transport {
                phase,
                message: format!("{why}; reconnect the ospp-v1 session"),
            }),
        }
    }
}

/// `"<project>:<mode>"`, the world scope every capability diagnostic names.
pub(crate) fn scope_label(project: &ProjectSlug, mode: &str) -> String {
    format!("{project}:{mode}")
}

/// Validate the caller-owned part of a capability request before it reaches the wire.
pub(crate) fn validate_scope(
    project: &ProjectSlug,
    mode: &str,
    key: &str,
) -> Result<(), OsppError> {
    validate_mode(mode)
        .and_then(|()| validate_key("key", key))
        .map_err(|message| OsppError::Capability {
            capability: CAPABILITY_SECRETS_ENSURE,
            scope: format!("{project}:{mode}"),
            message,
        })
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
