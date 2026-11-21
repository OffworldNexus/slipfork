//! The single closed-frame reader/writer and socket-timeout plumbing.

use std::time::Duration;

use tungstenite::WebSocket;
use tungstenite::stream::MaybeTlsStream;

use crate::error::OsppError;
use crate::wire::{ClientFrame, ServerFrame};

/// Bound on how long the session thread sleeps between command polls.
pub(crate) const POLL_SLICE: Duration = Duration::from_millis(25);

/// Encode and send one client frame as a UTF-8 JSON text message.
pub(crate) fn send_frame(
    socket: &mut WebSocket<MaybeTlsStream<std::net::TcpStream>>,
    frame: &ClientFrame,
) -> Result<(), OsppError> {
    socket.send(tungstenite::Message::Text(frame.to_json()?.into()))?;
    Ok(())
}

/// The only server-frame reader: bounded read, binary frames fail closed.
pub(crate) fn recv_server_frame(
    socket: &mut WebSocket<MaybeTlsStream<std::net::TcpStream>>,
    timeout: Duration,
) -> Result<ServerFrame, OsppError> {
    set_socket_timeout(socket, Some(timeout))?;
    loop {
        match socket.read() {
            Ok(tungstenite::Message::Text(text)) => return ServerFrame::parse(text.as_str()),
            Ok(tungstenite::Message::Binary(_)) => {
                return Err(OsppError::Protocol(
                    "binary WebSocket frames are not part of ospp-v1".to_owned(),
                ));
            }
            Ok(tungstenite::Message::Ping(payload)) => {
                socket.send(tungstenite::Message::Pong(payload))?;
            }
            Ok(tungstenite::Message::Pong(_)) | Ok(tungstenite::Message::Frame(_)) => {}
            Ok(tungstenite::Message::Close(_)) => return Err(OsppError::Closed),
            Err(error) => return Err(error.into()),
        }
    }
}

pub(crate) fn set_socket_timeout(
    socket: &WebSocket<MaybeTlsStream<std::net::TcpStream>>,
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

pub(crate) fn parse_nonce(value: &str) -> Result<[u8; 32], OsppError> {
    let bytes = hex::decode(value)
        .map_err(|_| OsppError::Protocol("challenge nonce is not hex".to_owned()))?;
    bytes
        .try_into()
        .map_err(|_| OsppError::Protocol("challenge nonce must be 32 bytes".to_owned()))
}

pub(crate) fn unexpected(expected: &str, actual: &ServerFrame) -> OsppError {
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
