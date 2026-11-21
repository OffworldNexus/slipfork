//! The session thread: request/response over one authenticated socket.
//!
//! The thread owns the socket and every ephemeral subscription key. Pushed
//! `env_updated` frames are decrypted in memory and handed to the caller's
//! callback. A `dump_request` never performs network I/O here: it returns a
//! [`DumpTicket`] so the caller opens the out-of-band stream on its own thread.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tungstenite::WebSocket;
use tungstenite::stream::MaybeTlsStream;

use crate::client::download::DumpTicket;
use crate::client::frames::{POLL_SLICE, recv_server_frame, send_frame, unexpected};
use crate::error::OsppError;
use crate::keys::SubscriptionKey;
use crate::model::{Secret, SubscribeRequest, SubscriptionEnv};
use crate::wire::{ClientFrame, ENCRYPTION_METHOD_HPKE_P256, Encryption, ServerFrame, WireValue};

/// Live session handle: request/response over the session thread's channel.
pub struct WsOsppSession {
    commands: Mutex<mpsc::Sender<Command>>,
    thread: Mutex<Option<JoinHandle<Result<(), OsppError>>>>,
    closed: Arc<AtomicBool>,
    io: Duration,
}

impl WsOsppSession {
    pub(crate) fn spawn(
        socket: WebSocket<MaybeTlsStream<std::net::TcpStream>>,
        on_update: Box<dyn Fn(SubscriptionEnv) + Send + Sync>,
        io: Duration,
    ) -> Result<Self, OsppError> {
        let (commands, receiver) = mpsc::channel();
        let closed = Arc::new(AtomicBool::new(false));
        let thread_closed = Arc::clone(&closed);
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
            .map_err(|error| OsppError::Transport {
                kind: "socket",
                message: error.to_string(),
            })?;
        Ok(Self {
            commands: Mutex::new(commands),
            thread: Mutex::new(Some(handle)),
            closed,
            io,
        })
    }

    fn request<T>(
        &self,
        build: impl FnOnce(SyncSender<Result<T, OsppError>>) -> Request,
    ) -> Result<T, OsppError> {
        let sender = self.commands.lock().map_err(|_| OsppError::Closed)?.clone();
        let (reply, receiver) = mpsc::sync_channel(1);
        sender
            .send(Command::Request(build(reply)))
            .map_err(|_| OsppError::Closed)?;
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
        handle.join().map_err(|_| OsppError::Transport {
            kind: "socket",
            message: "session thread panicked".to_owned(),
        })?
    }

    /// Subscribe or re-subscribe with the same id.
    pub fn subscribe(&self, req: &SubscribeRequest) -> Result<SubscriptionEnv, OsppError> {
        let key = SubscriptionKey::generate()?;
        let frame = ClientFrame::Subscribe {
            id: req.id.clone(),
            remote: req.remote.clone(),
            encryption: Encryption {
                method: ENCRYPTION_METHOD_HPKE_P256.to_owned(),
                public_key: key.public_key_hex().to_owned(),
            },
            forkfile: req.forkfile.clone(),
            claims: req.claims.clone(),
        };
        let id = req.id.clone();
        self.request(|reply| Request::Subscribe {
            id,
            frame: Box::new(frame),
            key,
            reply,
        })
    }

    /// Request an out-of-band sealed dump ticket for the caller to stream.
    pub fn dump(&self, subscription_id: &str, resource: &str) -> Result<DumpTicket, OsppError> {
        let id = format!("dump-{subscription_id}-{resource}");
        let subscription_id = subscription_id.to_owned();
        let resource = resource.to_owned();
        self.request(|reply| Request::Dump {
            id,
            subscription_id,
            resource,
            reply,
        })
    }

    /// Release one subscription and zeroize its ephemeral key.
    pub fn unsubscribe(&self, subscription_id: &str) -> Result<(), OsppError> {
        let id = subscription_id.to_owned();
        self.request(|reply| Request::Unsubscribe { id, reply })
    }

    /// Close the WebSocket and join the session thread.
    pub fn close(&self) -> Result<(), OsppError> {
        let sender = self.commands.lock().map_err(|_| OsppError::Closed)?.clone();
        let (reply, receiver) = mpsc::sync_channel(1);
        if sender.send(Command::Close { reply }).is_ok() {
            let _ = receiver.recv_timeout(self.io);
        }
        self.finish()
    }
}

impl Drop for WsOsppSession {
    fn drop(&mut self) {
        if !self.closed.load(Ordering::SeqCst)
            && let Ok(sender) = self.commands.lock()
        {
            let (reply, _receiver) = mpsc::sync_channel(1);
            let _ = sender.send(Command::Close { reply });
        }
        let _ = self.finish();
    }
}

enum Command {
    Request(Request),
    Close { reply: SyncSender<()> },
}

enum Request {
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
        reply: SyncSender<Result<DumpTicket, OsppError>>,
    },
    Unsubscribe {
        id: String,
        reply: SyncSender<Result<(), OsppError>>,
    },
}

/// The session thread owns the socket and every ephemeral subscription key.
struct SessionThread {
    socket: WebSocket<MaybeTlsStream<std::net::TcpStream>>,
    on_update: Box<dyn Fn(SubscriptionEnv) + Send + Sync>,
    subscriptions: BTreeMap<String, Arc<SubscriptionKey>>,
    io: Duration,
}

impl SessionThread {
    fn run(&mut self, receiver: Receiver<Command>) -> Result<(), OsppError> {
        loop {
            match receiver.try_recv() {
                Ok(Command::Close { reply }) => {
                    let _ = self.socket.close(None);
                    let _ = reply.send(());
                    return Ok(());
                }
                Ok(Command::Request(request)) => self.handle(request),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    let _ = self.socket.close(None);
                    return Ok(());
                }
            }
            match recv_server_frame(&mut self.socket, POLL_SLICE) {
                Ok(frame) => self.dispatch_push(frame)?,
                Err(OsppError::Timeout) => {}
                Err(error) => return Err(error),
            }
        }
    }

    fn handle(&mut self, request: Request) {
        match request {
            Request::Subscribe {
                id,
                frame,
                key,
                reply,
            } => {
                let result = self.subscribe(&id, *frame, key);
                let _ = reply.send(result);
            }
            Request::Dump {
                id,
                subscription_id,
                resource,
                reply,
            } => {
                let result = self.dump(&id, &subscription_id, &resource);
                let _ = reply.send(result);
            }
            Request::Unsubscribe { id, reply } => {
                let result = self.unsubscribe(&id);
                let _ = reply.send(result);
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
            return Err(frame_error(frame, "env_resolved"));
        };
        let environment = decrypt_env(&key, &resolved_id, components)?;
        self.subscriptions.insert(id.to_owned(), Arc::new(key));
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
    ) -> Result<DumpTicket, OsppError> {
        let key = self
            .subscriptions
            .get(subscription_id)
            .cloned()
            .ok_or_else(|| {
                OsppError::Protocol(format!("dump for unknown subscription {subscription_id}"))
            })?;
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
            return Err(frame_error(frame, "dump_ready"));
        };
        Ok(DumpTicket {
            url,
            key,
            subscription_id: subscription_id.to_owned(),
            resource: resource.to_owned(),
        })
    }

    /// Read frames until the response correlated to `id` arrives.
    fn await_frame(&mut self, id: &str) -> Result<ServerFrame, OsppError> {
        let deadline = Instant::now() + self.io;
        loop {
            if Instant::now() >= deadline {
                return Err(OsppError::Timeout);
            }
            match recv_server_frame(&mut self.socket, POLL_SLICE) {
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
                WireValue::Plain(text) => Secret::new(text),
                WireValue::Encrypted(ciphertext) => {
                    Secret::new(key.open_secret(&component, &name, &ciphertext)?)
                }
            };
            values.insert(name, value);
        }
        resolved.insert(component, values);
    }
    Ok(SubscriptionEnv::new(id, resolved))
}

/// Map a correlated `error` frame onto the typed server error, otherwise report
/// the unexpected frame.
fn frame_error(frame: ServerFrame, expected: &str) -> OsppError {
    match frame {
        ServerFrame::Error { id, code, message } => OsppError::server(id, code, message),
        other => unexpected(expected, &other),
    }
}
