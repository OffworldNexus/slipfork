//! The OSPP v1 client ports and the synchronous WebSocket adapter.
//!
//! `WsOsppClient` performs `hello -> auth_challenge -> auth_response -> auth_ok`
//! and `pair_hello -> pair_challenge -> pair_begin -> pair_handle -> pair_approved`
//! over a real `tungstenite` WebSocket (`Sec-WebSocket-Protocol: ospp-v1`), then
//! hands the authenticated socket to a single session thread that owns the
//! per-subscription ephemeral HPKE keys.

mod download;
mod frames;
mod session;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use download::{DumpTicket, MAX_DUMP_HEADER_BYTES, open_dump_stream};
pub use session::WsOsppSession;

use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tungstenite::{WebSocket, client, stream::MaybeTlsStream};

use crate::device_key::DeviceKeySigner;
use crate::endpoint::OsppEndpoint;
use crate::error::OsppError;
use crate::model::SubscriptionEnv;
use crate::pairing::{LivePairingUrl, PairPhase, PairingState, PairingStore, solve_altcha};
use crate::wire::{
    AUTH_METHOD_P256, AuthResponse, ClientFrame, ENCRYPTION_METHOD_HPKE_P256, OSPP_SUBPROTOCOL,
    PROTOCOL_VERSION, ServerFrame, auth_transcript, pairing_transcript,
};

use frames::{POLL_SLICE, parse_nonce, recv_server_frame, send_frame, unexpected};

/// Injected client timeouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OsppTimeouts {
    /// Handshake read/write bound.
    pub handshake: Duration,
    /// Per-request and push-read bound.
    pub io: Duration,
    /// How long the browser has to approve a pairing before the window closes.
    pub pairing_approval: Duration,
}

impl Default for OsppTimeouts {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(10),
            io: Duration::from_secs(30),
            pairing_approval: Duration::from_secs(120),
        }
    }
}

/// Paired machine identity returned by `pair_approved`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingApproval {
    /// Enrolled compressed public key hex.
    pub public_key: String,
    /// Authenticated account name.
    pub account: String,
    /// Approved machine name.
    pub machine: String,
}

/// Single client-side OSPP v1 adapter port injected into the daemon.
pub trait OsppClient: Send + Sync {
    /// The single boot-injected server identity.
    fn endpoint(&self) -> &OsppEndpoint;

    /// Execute `pair_hello -> pair_challenge -> pair_begin -> pair_handle -> pair_approved`,
    /// reporting [`PairPhase`] progress and delivering the server-supplied
    /// `<url>#handle=<handle>` to `on_handle` while the socket stays open.
    fn pair(
        &self,
        hostname: &str,
        phase: &mut dyn FnMut(PairPhase),
        on_handle: &mut dyn FnMut(&LivePairingUrl) -> Result<(), OsppError>,
    ) -> Result<PairingApproval, OsppError>;

    /// Open an authenticated persistent session, dispatching pushed `env_updated`
    /// frames (already HPKE-decrypted in memory) to `on_update`.
    fn connect(
        &self,
        on_update: Box<dyn Fn(SubscriptionEnv) + Send + Sync>,
    ) -> Result<WsOsppSession, OsppError>;
}

/// Production synchronous `OsppClient`.
pub struct WsOsppClient {
    endpoint: OsppEndpoint,
    signer: Arc<dyn DeviceKeySigner>,
    pairing: Option<PathBuf>,
    timeouts: OsppTimeouts,
}

impl WsOsppClient {
    /// Bind the client to the injected endpoint, machine key signer, optional
    /// `<state_dir>/ospp.json` path, and timeouts.
    pub fn new(
        endpoint: OsppEndpoint,
        signer: Arc<dyn DeviceKeySigner>,
        pairing: Option<PathBuf>,
        timeouts: OsppTimeouts,
    ) -> Self {
        Self {
            endpoint,
            signer,
            pairing,
            timeouts,
        }
    }

    fn dial(&self) -> Result<WebSocket<MaybeTlsStream<std::net::TcpStream>>, OsppError> {
        let mut request = self
            .endpoint
            .as_str()
            .into_client_request()
            .map_err(|error| OsppError::Transport {
                kind: "socket",
                message: error.to_string(),
            })?;
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static(OSPP_SUBPROTOCOL),
        );
        let (socket, _response) = client::connect(request)?;
        frames::set_socket_timeout(&socket, Some(self.timeouts.handshake))?;
        Ok(socket)
    }

    fn random_nonce(&self) -> Result<[u8; 32], OsppError> {
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce).map_err(|error| OsppError::Entropy(error.to_string()))?;
        Ok(nonce)
    }

    fn response(
        &self,
        server_nonce: &[u8],
        client_nonce: &[u8],
        pair: bool,
    ) -> Result<AuthResponse, OsppError> {
        let transcript = if pair {
            pairing_transcript(server_nonce, client_nonce)
        } else {
            auth_transcript(server_nonce, client_nonce)
        };
        let signature = self.signer.sign(&transcript)?;
        Ok(AuthResponse {
            public_key: self.signer.public_key_hex().to_owned(),
            client_nonce: hex::encode(client_nonce),
            signature: signature.to_hex(),
        })
    }

    fn authenticate(
        &self,
        socket: &mut WebSocket<MaybeTlsStream<std::net::TcpStream>>,
    ) -> Result<(), OsppError> {
        send_frame(
            socket,
            &ClientFrame::Hello {
                version: PROTOCOL_VERSION,
                auth_methods: vec![AUTH_METHOD_P256.to_owned()],
            },
        )?;
        let ServerFrame::AuthChallenge { method, challenge } =
            recv_server_frame(socket, self.timeouts.handshake)?
        else {
            return Err(OsppError::Protocol(
                "expected auth_challenge after hello".to_owned(),
            ));
        };
        if method != AUTH_METHOD_P256 {
            return Err(OsppError::Protocol(format!(
                "server selected unsupported auth method {method}"
            )));
        }
        let server_nonce = parse_nonce(&challenge.nonce)?;
        let client_nonce = self.random_nonce()?;
        let response = self.response(&server_nonce, &client_nonce, false)?;
        send_frame(socket, &ClientFrame::AuthResponse { response })?;
        match recv_server_frame(socket, self.timeouts.handshake)? {
            ServerFrame::AuthOk { encryption_methods } => {
                if !encryption_methods
                    .iter()
                    .any(|method| method == ENCRYPTION_METHOD_HPKE_P256)
                {
                    return Err(OsppError::Protocol(
                        "server supports no hpke-p256 encryption".to_owned(),
                    ));
                }
                Ok(())
            }
            ServerFrame::Error { id, code, message } => Err(OsppError::server(id, code, message)),
            other => Err(unexpected("auth_ok", &other)),
        }
    }

    fn await_approval(
        &self,
        socket: &mut WebSocket<MaybeTlsStream<std::net::TcpStream>>,
    ) -> Result<(String, String), OsppError> {
        let deadline = Instant::now() + self.timeouts.pairing_approval;
        loop {
            if Instant::now() >= deadline {
                return Err(OsppError::PairingWindowClosed);
            }
            match recv_server_frame(socket, POLL_SLICE) {
                Ok(ServerFrame::PairApproved { account, machine }) => {
                    return Ok((account, machine));
                }
                Ok(ServerFrame::Error { id, code, message }) => {
                    return Err(OsppError::server(id, code, message));
                }
                Ok(other) => return Err(unexpected("pair_approved", &other)),
                Err(OsppError::Timeout) => {}
                Err(error) => return Err(error),
            }
        }
    }

    fn persist_pairing(&self, account: &str, machine: &str) -> Result<(), OsppError> {
        let Some(path) = &self.pairing else {
            return Ok(());
        };
        PairingStore::at(path.clone()).save(&PairingState {
            endpoint: self.endpoint.to_string(),
            account: account.to_owned(),
            machine: machine.to_owned(),
        })
    }
}

impl OsppClient for WsOsppClient {
    fn endpoint(&self) -> &OsppEndpoint {
        &self.endpoint
    }

    fn pair(
        &self,
        hostname: &str,
        phase: &mut dyn FnMut(PairPhase),
        on_handle: &mut dyn FnMut(&LivePairingUrl) -> Result<(), OsppError>,
    ) -> Result<PairingApproval, OsppError> {
        let mut socket = self.dial()?;
        send_frame(
            &mut socket,
            &ClientFrame::PairHello {
                version: PROTOCOL_VERSION,
                key_methods: vec![AUTH_METHOD_P256.to_owned()],
            },
        )?;
        let ServerFrame::PairChallenge {
            method,
            altcha,
            challenge,
        } = recv_server_frame(&mut socket, self.timeouts.handshake)?
        else {
            return Err(OsppError::Protocol(
                "expected pair_challenge after pair_hello".to_owned(),
            ));
        };
        if method != AUTH_METHOD_P256 {
            return Err(OsppError::Protocol(format!(
                "server selected unsupported key method {method}"
            )));
        }
        phase(PairPhase::SolvingAltcha);
        let altcha = solve_altcha(&altcha)?;
        let server_nonce = parse_nonce(&challenge.nonce)?;
        let client_nonce = self.random_nonce()?;
        let response = self.response(&server_nonce, &client_nonce, true)?;
        send_frame(
            &mut socket,
            &ClientFrame::PairBegin {
                altcha,
                hostname: hostname.to_owned(),
                response,
            },
        )?;

        let (url, handle) = match recv_server_frame(&mut socket, self.timeouts.handshake)? {
            ServerFrame::PairHandle { url, handle } => (url, handle),
            ServerFrame::Error { id, code, message } => {
                return Err(OsppError::server(id, code, message));
            }
            other => return Err(unexpected("pair_handle", &other)),
        };
        let live = LivePairingUrl::with_handle(&url, &handle)?;
        phase(PairPhase::AwaitingApproval);
        on_handle(&live)?;
        drop(live);

        let (account, machine) = self.await_approval(&mut socket)?;
        phase(PairPhase::Approved);
        self.persist_pairing(&account, &machine)?;
        Ok(PairingApproval {
            public_key: self.signer.public_key_hex().to_owned(),
            account,
            machine,
        })
    }

    fn connect(
        &self,
        on_update: Box<dyn Fn(SubscriptionEnv) + Send + Sync>,
    ) -> Result<WsOsppSession, OsppError> {
        let mut socket = self.dial()?;
        self.authenticate(&mut socket)?;
        frames::set_socket_timeout(&socket, Some(self.timeouts.io))?;
        WsOsppSession::spawn(socket, on_update, self.timeouts.io)
    }
}
