//! The OSPP v1 capability ports and the synchronous WebSocket adapter.
//!
//! `WsOsppClient` performs `hello -> auth_challenge -> auth_response -> auth_ok`
//! and `pair_hello -> pair_challenge -> pair_begin -> pair_handle -> pair_approved`
//! over a real `tungstenite` WebSocket (`Sec-WebSocket-Protocol: ospp-v1`), then
//! hands the authenticated socket to a single session thread that owns the
//! per-subscription ephemeral HPKE keys. Every value the server resolved from a
//! secret is opened in memory; nothing is written to disk or read from the
//! environment.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use slipfork_snow::{EntropySource, SecretValue};
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tungstenite::{Message, WebSocket, client, stream::MaybeTlsStream};

use crate::device_key::DeviceKeySigner;
use crate::dump::DumpReader;
use crate::endpoint::OsppEndpoint;
use crate::error::OsppError;
use crate::keys::SubscriptionKey;
use crate::model::{SubscribeRequest, SubscriptionEnv};
use crate::pairing::{LivePairingUrl, solve_altcha};
use crate::wire::{
    AUTH_METHOD_P256, AuthResponse, ClientFrame, ENCRYPTION_METHOD_HPKE_P256, Encryption,
    OSPP_SUBPROTOCOL, PROTOCOL_VERSION, ServerFrame, WireValue, auth_transcript,
    pairing_transcript,
};

/// Bound on how long the session thread sleeps between command polls.
const POLL_SLICE: Duration = Duration::from_millis(25);
/// Largest accepted HTTP response header block.
const MAX_HTTP_HEADERS: usize = 16 * 1024;

/// Injected client timeouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OsppTimeouts {
    /// Handshake read/write bound.
    pub handshake: Duration,
    /// Per-request and push-read bound.
    pub io: Duration,
}

impl Default for OsppTimeouts {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(10),
            io: Duration::from_secs(30),
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

/// Single client-side OSPP v1 adapter port injected into `slipforkd`.
pub trait OsppClient: Send + Sync {
    /// The single boot-injected server identity.
    fn endpoint(&self) -> &OsppEndpoint;

    /// Execute `pair_hello -> pair_challenge -> pair_begin -> pair_handle -> pair_approved`,
    /// delivering the server-supplied `<url>#handle=<handle>` to `on_handle` while
    /// keeping the socket open.
    fn pair(
        &self,
        hostname: &str,
        on_handle: &mut dyn FnMut(&LivePairingUrl) -> Result<(), OsppError>,
    ) -> Result<PairingApproval, OsppError>;

    /// Open an authenticated persistent session, dispatching pushed `env_updated`
    /// frames (already HPKE-decrypted in memory) to `on_update`.
    fn connect(
        &self,
        on_update: Box<dyn Fn(SubscriptionEnv) + Send + Sync>,
    ) -> Result<Box<dyn OsppSession>, OsppError>;
}

/// Live authenticated `ospp-v1` session.
pub trait OsppSession: Send + Sync {
    /// Subscribe or re-subscribe with the same id.
    fn subscribe(&self, req: &SubscribeRequest) -> Result<SubscriptionEnv, OsppError>;

    /// Request an out-of-band sealed dump and return its decoder.
    fn dump(
        &self,
        subscription_id: &str,
        resource: &str,
    ) -> Result<Box<dyn Read + Send>, OsppError>;

    /// Release one subscription and zeroize its ephemeral key.
    fn unsubscribe(&self, subscription_id: &str) -> Result<(), OsppError>;

    /// Close the WebSocket and join the session thread.
    fn close(&self) -> Result<(), OsppError>;
}

/// Production synchronous `OsppClient`.
pub struct WsOsppClient {
    endpoint: OsppEndpoint,
    signer: Arc<dyn DeviceKeySigner>,
    entropy: Arc<dyn EntropySource>,
    timeouts: OsppTimeouts,
}

impl WsOsppClient {
    /// Bind the client to the injected endpoint, machine key, and entropy source.
    pub fn new(
        endpoint: OsppEndpoint,
        signer: Arc<dyn DeviceKeySigner>,
        entropy: Arc<dyn EntropySource>,
        timeouts: OsppTimeouts,
    ) -> Self {
        Self {
            endpoint,
            signer,
            entropy,
            timeouts,
        }
    }

    fn dial(&self) -> Result<WebSocket<MaybeTlsStream<TcpStream>>, OsppError> {
        let mut request = self
            .endpoint
            .as_str()
            .into_client_request()
            .map_err(|error| OsppError::Transport(error.to_string()))?;
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static(OSPP_SUBPROTOCOL),
        );
        let (socket, _response) = client::connect(request)?;
        set_socket_timeout(&socket, Some(self.timeouts.handshake))?;
        Ok(socket)
    }

    fn random_nonce(&self) -> Result<[u8; 32], OsppError> {
        let mut nonce = [0u8; 32];
        self.entropy.fill_bytes(&mut nonce)?;
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
        socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    ) -> Result<(), OsppError> {
        send_frame(
            socket,
            &ClientFrame::Hello {
                version: PROTOCOL_VERSION,
                auth_methods: vec![AUTH_METHOD_P256.to_owned()],
            },
        )?;
        let ServerFrame::AuthChallenge { method, challenge } = read_frame(socket)? else {
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
        match read_frame(socket)? {
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
}

impl OsppClient for WsOsppClient {
    fn endpoint(&self) -> &OsppEndpoint {
        &self.endpoint
    }

    fn pair(
        &self,
        hostname: &str,
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
        } = read_frame(&mut socket)?
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

        let (url, handle) = match read_frame(&mut socket)? {
            ServerFrame::PairHandle { url, handle } => (url, handle),
            ServerFrame::Error { id, code, message } => {
                return Err(OsppError::server(id, code, message));
            }
            other => return Err(unexpected("pair_handle", &other)),
        };
        let live = LivePairingUrl::with_handle(&url, &handle)?;
        on_handle(&live)?;
        drop(live);

        set_socket_timeout(&socket, Some(self.timeouts.io))?;
        match read_frame(&mut socket)? {
            ServerFrame::PairApproved { account, machine } => Ok(PairingApproval {
                public_key: self.signer.public_key_hex().to_owned(),
                account,
                machine,
            }),
            ServerFrame::Error { id, code, message } => Err(OsppError::server(id, code, message)),
            other => Err(unexpected("pair_approved", &other)),
        }
    }

    fn connect(
        &self,
        on_update: Box<dyn Fn(SubscriptionEnv) + Send + Sync>,
    ) -> Result<Box<dyn OsppSession>, OsppError> {
        let mut socket = self.dial()?;
        self.authenticate(&mut socket)?;
        set_socket_timeout(&socket, Some(self.timeouts.io))?;

        let (commands, receiver) = mpsc::channel();
        let closed = Arc::new(AtomicBool::new(false));
        let thread_closed = Arc::clone(&closed);
        let io = self.timeouts.io;
        let handle = thread::Builder::new()
            .name("ospp-session".to_owned())
            .spawn(move || {
                let mut session = SessionThread {
                    socket,
                    on_update,
                    subscriptions: BTreeMap::new(),
                    io,
                };
                let result = session.run(receiver);
                thread_closed.store(true, Ordering::SeqCst);
                result
            })
            .map_err(|error| OsppError::Transport(error.to_string()))?;

        Ok(Box::new(WsOsppSession {
            commands: Mutex::new(commands),
            thread: Mutex::new(Some(handle)),
            closed,
            entropy: Arc::clone(&self.entropy),
            io: self.timeouts.io,
        }))
    }
}

/// Live session handle: request/response over the session thread's channel.
pub struct WsOsppSession {
    commands: Mutex<mpsc::Sender<Command>>,
    thread: Mutex<Option<JoinHandle<Result<(), OsppError>>>>,
    closed: Arc<AtomicBool>,
    entropy: Arc<dyn EntropySource>,
    io: Duration,
}

enum Command {
    Subscribe {
        id: String,
        frame: Box<ClientFrame>,
        key: SubscriptionKey,
        reply: SyncSender<Result<SubscriptionEnv, OsppError>>,
    },
    Dump {
        id: String,
        subscription_id: String,
        resource: String,
        reply: SyncSender<Result<Box<dyn Read + Send>, OsppError>>,
    },
    Unsubscribe {
        id: String,
        reply: SyncSender<Result<(), OsppError>>,
    },
    Close {
        reply: SyncSender<Result<(), OsppError>>,
    },
}

impl WsOsppSession {
    fn request<T>(
        &self,
        build: impl FnOnce(SyncSender<Result<T, OsppError>>) -> Command,
    ) -> Result<T, OsppError> {
        let sender = self.commands.lock().map_err(|_| OsppError::Closed)?.clone();
        let (reply, receiver) = mpsc::sync_channel(1);
        sender.send(build(reply)).map_err(|_| OsppError::Closed)?;
        match receiver.recv_timeout(self.io) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(OsppError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(OsppError::Closed),
        }
    }

    fn finish(&self) -> Result<(), OsppError> {
        let Some(handle) = self.thread.lock().map_err(|_| OsppError::Closed)?.take() else {
            return Ok(());
        };
        handle
            .join()
            .map_err(|_| OsppError::Transport("session thread panicked".to_owned()))?
    }
}

impl OsppSession for WsOsppSession {
    fn subscribe(&self, req: &SubscribeRequest) -> Result<SubscriptionEnv, OsppError> {
        let key = SubscriptionKey::generate(&*self.entropy)?;
        let frame = ClientFrame::Subscribe {
            id: req.id.clone(),
            remote: req.remote.clone(),
            encryption: Encryption {
                method: ENCRYPTION_METHOD_HPKE_P256.to_owned(),
                public_key: key.public_key_hex().to_owned(),
            },
            snow: req.snow.clone(),
            claims: req.claims.clone(),
        };
        let id = req.id.clone();
        self.request(|reply| Command::Subscribe {
            id,
            frame: Box::new(frame),
            key,
            reply,
        })
    }

    fn dump(
        &self,
        subscription_id: &str,
        resource: &str,
    ) -> Result<Box<dyn Read + Send>, OsppError> {
        let id = format!("dump-{subscription_id}-{resource}");
        let subscription_id = subscription_id.to_owned();
        let resource = resource.to_owned();
        self.request(|reply| Command::Dump {
            id,
            subscription_id,
            resource,
            reply,
        })
    }

    fn unsubscribe(&self, subscription_id: &str) -> Result<(), OsppError> {
        let id = subscription_id.to_owned();
        self.request(|reply| Command::Unsubscribe { id, reply })
    }

    fn close(&self) -> Result<(), OsppError> {
        let result = self.request(|reply| Command::Close { reply });
        self.finish()?;
        result
    }
}

impl Drop for WsOsppSession {
    fn drop(&mut self) {
        if self.closed.load(Ordering::SeqCst) {
            let _ = self.finish();
            return;
        }
        if let Ok(sender) = self.commands.lock() {
            let (reply, _receiver) = mpsc::sync_channel(1);
            let _ = sender.send(Command::Close { reply });
        }
        let _ = self.finish();
    }
}

/// The session thread owns the socket and every ephemeral subscription key.
struct SessionThread {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    on_update: Box<dyn Fn(SubscriptionEnv) + Send + Sync>,
    subscriptions: BTreeMap<String, SubscriptionKey>,
    io: Duration,
}

impl SessionThread {
    fn run(&mut self, receiver: Receiver<Command>) -> Result<(), OsppError> {
        loop {
            match receiver.try_recv() {
                Ok(Command::Close { reply }) => {
                    let _ = self.socket.close(None);
                    let _ = reply.send(Ok(()));
                    return Ok(());
                }
                Ok(command) => self.handle(command),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    let _ = self.socket.close(None);
                    return Ok(());
                }
            }
            match self.read_frame(POLL_SLICE) {
                Ok(frame) => self.dispatch_push(frame)?,
                Err(OsppError::Timeout) => {}
                Err(error) => return Err(error),
            }
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Subscribe {
                id,
                frame,
                key,
                reply,
            } => {
                let result = self.subscribe(&id, *frame, key);
                let _ = reply.send(result);
            }
            Command::Dump {
                id,
                subscription_id,
                resource,
                reply,
            } => {
                let result = self.dump(&id, &subscription_id, &resource);
                let _ = reply.send(result);
            }
            Command::Unsubscribe { id, reply } => {
                let result = self.unsubscribe(&id);
                let _ = reply.send(result);
            }
            Command::Close { reply } => {
                // Handled in `run`; reaching here means the loop shape changed.
                let _ = reply.send(Ok(()));
            }
        }
    }

    fn subscribe(
        &mut self,
        id: &str,
        frame: ClientFrame,
        key: SubscriptionKey,
    ) -> Result<SubscriptionEnv, OsppError> {
        send_frame(&mut self.socket, &frame)?;
        let frame = self.await_frame(id)?;
        let ServerFrame::EnvResolved {
            id: resolved_id,
            components,
        } = frame
        else {
            return Err(unexpected("env_resolved", &frame));
        };
        let environment = decrypt_env(&key, &resolved_id, components)?;
        self.subscriptions.insert(id.to_owned(), key);
        Ok(environment)
    }

    fn unsubscribe(&mut self, id: &str) -> Result<(), OsppError> {
        send_frame(
            &mut self.socket,
            &ClientFrame::Unsubscribe { id: id.to_owned() },
        )?;
        self.subscriptions.remove(id);
        Ok(())
    }

    fn dump(
        &mut self,
        id: &str,
        subscription_id: &str,
        resource: &str,
    ) -> Result<Box<dyn Read + Send>, OsppError> {
        let key = self.subscriptions.remove(subscription_id).ok_or_else(|| {
            OsppError::Protocol(format!("dump for unknown subscription {subscription_id}"))
        })?;
        let result = (|| {
            send_frame(
                &mut self.socket,
                &ClientFrame::DumpRequest {
                    id: id.to_owned(),
                    subscription_id: subscription_id.to_owned(),
                    resource: resource.to_owned(),
                },
            )?;
            let frame = self.await_frame(id)?;
            let ServerFrame::DumpReady { url, .. } = frame else {
                return Err(unexpected("dump_ready", &frame));
            };
            let source = http_get(&url, self.io)?;
            let dump = DumpReader::open(source, &key, subscription_id, resource)?;
            Ok(Box::new(dump) as Box<dyn Read + Send>)
        })();
        self.subscriptions.insert(subscription_id.to_owned(), key);
        result
    }

    /// Read frames until the response correlated to `id` arrives.
    fn await_frame(&mut self, id: &str) -> Result<ServerFrame, OsppError> {
        let deadline = Instant::now() + self.io;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(OsppError::Timeout);
            }
            match self.read_frame(POLL_SLICE) {
                Ok(frame) => {
                    let correlated = match &frame {
                        ServerFrame::Error { id: error_id, .. } => {
                            error_id.as_deref().is_none_or(|value| value == id)
                        }
                        ServerFrame::EnvResolved { id: frame_id, .. }
                        | ServerFrame::DumpReady { id: frame_id, .. } => frame_id == id,
                        _ => false,
                    };
                    if correlated {
                        return Ok(frame);
                    }
                    if matches!(frame, ServerFrame::EnvUpdated { .. }) {
                        self.dispatch_push(frame)?;
                    } else {
                        return Err(unexpected("a correlated response", &frame));
                    }
                }
                Err(OsppError::Timeout) => {}
                Err(error) => return Err(error),
            }
        }
    }

    fn dispatch_push(&mut self, frame: ServerFrame) -> Result<(), OsppError> {
        match frame {
            ServerFrame::EnvUpdated { id, components } => {
                let key = self.subscriptions.get(&id).ok_or_else(|| {
                    OsppError::Protocol(format!("env_updated for unknown subscription {id}"))
                })?;
                let environment = decrypt_env(key, &id, components)?;
                (self.on_update)(environment);
                Ok(())
            }
            ServerFrame::Error { id, code, message } => Err(OsppError::server(id, code, message)),
            other => Err(unexpected("env_updated", &other)),
        }
    }

    fn read_frame(&mut self, timeout: Duration) -> Result<ServerFrame, OsppError> {
        set_socket_timeout(&self.socket, Some(timeout))?;
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => return ServerFrame::parse(text.as_str()),
                Ok(Message::Binary(_)) => {
                    return Err(OsppError::Protocol(
                        "binary WebSocket frames are not part of ospp-v1".to_owned(),
                    ));
                }
                Ok(Message::Ping(payload)) => {
                    self.socket.send(Message::Pong(payload))?;
                }
                Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
                Ok(Message::Close(_)) => return Err(OsppError::Closed),
                Err(error) => return Err(error.into()),
            }
        }
    }
}

fn decrypt_env(
    key: &SubscriptionKey,
    id: &str,
    components: BTreeMap<String, BTreeMap<String, WireValue>>,
) -> Result<SubscriptionEnv, OsppError> {
    let mut resolved = BTreeMap::new();
    for (component, variables) in components {
        let mut values = BTreeMap::new();
        for (name, value) in variables {
            let value = match value {
                WireValue::Plain(text) => SecretValue::plain(text),
                WireValue::Encrypted(ciphertext) => {
                    SecretValue::secret(key.open_secret(&component, &name, &ciphertext)?)
                }
            };
            values.insert(name, value);
        }
        resolved.insert(component, values);
    }
    Ok(SubscriptionEnv::new(id, resolved))
}

fn send_frame(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    frame: &ClientFrame,
) -> Result<(), OsppError> {
    socket.send(Message::Text(frame.to_json()?.into()))?;
    Ok(())
}

fn read_frame(socket: &mut WebSocket<MaybeTlsStream<TcpStream>>) -> Result<ServerFrame, OsppError> {
    loop {
        match socket.read()? {
            Message::Text(text) => return ServerFrame::parse(text.as_str()),
            Message::Binary(_) => {
                return Err(OsppError::Protocol(
                    "binary WebSocket frames are not part of ospp-v1".to_owned(),
                ));
            }
            Message::Ping(payload) => socket.send(Message::Pong(payload))?,
            Message::Pong(_) | Message::Frame(_) => {}
            Message::Close(_) => return Err(OsppError::Closed),
        }
    }
}

fn set_socket_timeout(
    socket: &WebSocket<MaybeTlsStream<TcpStream>>,
    timeout: Option<Duration>,
) -> Result<(), OsppError> {
    match socket.get_ref() {
        MaybeTlsStream::Plain(stream) => {
            stream.set_read_timeout(timeout)?;
            stream.set_write_timeout(timeout)?;
        }
        MaybeTlsStream::Rustls(stream) => {
            stream.sock.set_read_timeout(timeout)?;
            stream.sock.set_write_timeout(timeout)?;
        }
        _ => {}
    }
    Ok(())
}

fn parse_nonce(value: &str) -> Result<[u8; 32], OsppError> {
    let bytes = hex::decode(value)
        .map_err(|_| OsppError::Protocol("challenge nonce is not hex".to_owned()))?;
    bytes
        .try_into()
        .map_err(|_| OsppError::Protocol("challenge nonce must be 32 bytes".to_owned()))
}

fn unexpected(expected: &str, actual: &ServerFrame) -> OsppError {
    let kind = match actual {
        ServerFrame::AuthChallenge { .. } => "auth_challenge",
        ServerFrame::AuthOk { .. } => "auth_ok",
        ServerFrame::PairChallenge { .. } => "pair_challenge",
        ServerFrame::PairHandle { .. } => "pair_handle",
        ServerFrame::PairApproved { .. } => "pair_approved",
        ServerFrame::EnvResolved { .. } => "env_resolved",
        ServerFrame::EnvUpdated { .. } => "env_updated",
        ServerFrame::DumpReady { .. } => "dump_ready",
        ServerFrame::Error { .. } => "error",
    };
    OsppError::Protocol(format!("expected {expected}, got {kind}"))
}

/// Open an out-of-band dump body: `https://` anywhere, `http://` on loopback only.
fn http_get(url: &str, io: Duration) -> Result<Box<dyn Read + Send>, OsppError> {
    let (secure, host, port, path) = parse_http_url(url)?;
    let address = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|error| OsppError::Transport(error.to_string()))?
        .next()
        .ok_or_else(|| OsppError::Transport(format!("dump host {host} did not resolve")))?;
    let mut stream = TcpStream::connect_timeout(&address, io)?;
    stream.set_read_timeout(Some(io))?;
    stream.set_write_timeout(Some(io))?;

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\naccept-encoding: identity\r\n\r\n"
    );
    let reader: Box<dyn Read + Send> = if secure {
        let mut tls = tls_stream(&host, stream)?;
        tls.write_all(request.as_bytes())?;
        tls.flush()?;
        Box::new(tls)
    } else {
        stream.write_all(request.as_bytes())?;
        stream.flush()?;
        Box::new(stream)
    };
    http_body(reader)
}

fn http_body(reader: Box<dyn Read + Send>) -> Result<Box<dyn Read + Send>, OsppError> {
    let mut reader = BufReader::new(reader);
    let mut header_bytes = 0usize;
    let mut status = String::new();
    reader
        .read_line(&mut status)
        .map_err(|error| OsppError::Transport(error.to_string()))?;
    header_bytes += status.len();
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| OsppError::Transport(error.to_string()))?;
        if read == 0 {
            return Err(OsppError::TruncatedDump);
        }
        header_bytes += read;
        if header_bytes > MAX_HTTP_HEADERS {
            return Err(OsppError::Transport(
                "dump headers are too large".to_owned(),
            ));
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length =
                Some(value.trim().parse().map_err(|_| {
                    OsppError::Transport("dump content-length is invalid".to_owned())
                })?);
        }
    }
    if !status.contains(" 200 ") {
        return Err(OsppError::Transport(format!(
            "dump endpoint returned {}",
            status.trim()
        )));
    }
    let length = content_length
        .ok_or_else(|| OsppError::Transport("dump response has no content-length".to_owned()))?;
    Ok(Box::new(reader.take(length as u64)))
}

fn parse_http_url(url: &str) -> Result<(bool, String, u16, String), OsppError> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| OsppError::Transport(format!("dump URL {url:?} has no scheme")))?;
    let secure = match scheme {
        "https" => true,
        "http" => false,
        _ => {
            return Err(OsppError::Transport(format!(
                "dump URL scheme {scheme} is unsupported"
            )));
        }
    };
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => (
            host.to_owned(),
            port.parse()
                .map_err(|_| OsppError::Transport("dump URL port is invalid".to_owned()))?,
        ),
        _ => (authority.to_owned(), if secure { 443 } else { 80 }),
    };
    if !secure && !matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1") {
        return Err(OsppError::Transport(
            "plaintext dump URLs are only allowed on a loopback host".to_owned(),
        ));
    }
    Ok((secure, host, port, path.to_owned()))
}

fn tls_stream(
    host: &str,
    stream: TcpStream,
) -> Result<rustls::StreamOwned<rustls::ClientConnection, TcpStream>, OsppError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|_| OsppError::Transport(format!("dump host {host} is not a TLS name")))?;
    let connection = rustls::ClientConnection::new(Arc::new(config), server_name)
        .map_err(|error| OsppError::Transport(error.to_string()))?;
    Ok(rustls::StreamOwned::new(connection, stream))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_key::DeviceSignature;

    #[test]
    fn plaintext_dump_urls_are_loopback_only() {
        assert!(parse_http_url("http://127.0.0.1:1234/dumps/x").is_ok());
        assert!(parse_http_url("http://ci.example.com/dumps/x").is_err());
        assert!(parse_http_url("https://ci.example.com/dumps/x").is_ok());
    }

    #[test]
    fn device_signature_hex_is_fixed_width() {
        let signature = DeviceSignature::from_raw([0x11; 64]);
        assert_eq!(signature.to_hex().len(), 128);
    }
}
