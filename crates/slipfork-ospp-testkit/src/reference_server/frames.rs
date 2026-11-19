//! Wire I/O and server-side HPKE sealing for the `ospp-v1` reference server.
//!
//! Every frame stays a closed UTF-8 JSON text message; binary frames are a protocol violation. The
//! sealing routines mirror the production client exactly (`info = "ospp-v1-secret"`,
//! `aad = "<component>:<env_key>"`; dumps use `ospp-v1-dump` with a mandatory `len = 0` EOF frame).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hpke::aead::AesGcm256;
use hpke::kdf::HkdfSha256;
use hpke::kem::DhP256HkdfSha256;
use hpke::rand_core::{CryptoRng, TryCryptoRng, TryRng};
use hpke::{OpModeS, Serializable, setup_sender_with_rng};
use slipfork_forkfile::EntropySource;
use slipfork_ospp::wire::{ClientFrame, ServerFrame};
use slipfork_ospp::{
    DUMP_INFO, OsppError, SECRET_INFO, dump_aad, parse_recipient_public_key, secret_aad,
};
use std::net::TcpStream;
use tungstenite::{Message, WebSocket};

/// Server-side sealed secret: `info = "ospp-v1-secret"`, `aad = "<component>:<env_key>"`.
pub fn seal_secret(
    public_key_hex: &str,
    component: &str,
    env_key: &str,
    plaintext: &str,
    entropy: &dyn EntropySource,
) -> Result<String, OsppError> {
    let mut rng = FilledRng::new(entropy, 128)?;
    seal_secret_with_rng(public_key_hex, component, env_key, plaintext, &mut rng)
}

pub(super) fn seal_secret_with_rng(
    public_key_hex: &str,
    component: &str,
    env_key: &str,
    plaintext: &str,
    rng: &mut impl CryptoRng,
) -> Result<String, OsppError> {
    let public_key = parse_recipient_public_key(public_key_hex)?;
    let (enc, mut context) = setup_sender_with_rng::<AesGcm256, HkdfSha256, DhP256HkdfSha256>(
        &OpModeS::Base,
        &public_key,
        SECRET_INFO,
        rng,
    )
    .map_err(|error| OsppError::Hpke(error.to_string()))?;
    let ciphertext = context
        .seal(
            plaintext.as_bytes(),
            secret_aad(component, env_key).as_bytes(),
        )
        .map_err(|error| OsppError::Hpke(error.to_string()))?;
    let mut sealed = enc.to_bytes().as_slice().to_vec();
    sealed.extend_from_slice(&ciphertext);
    Ok(STANDARD.encode(sealed))
}

/// Server-side sealed dump stream: `SetupBaseS` with `info = "ospp-v1-dump"`,
/// `aad = "<subscription_id>:<resource>"`, and a mandatory `len = 0` EOF frame.
pub fn seal_dump(
    public_key_hex: &str,
    subscription_id: &str,
    resource: &str,
    plaintext: &[u8],
    entropy: &dyn EntropySource,
) -> Result<Vec<u8>, OsppError> {
    let mut rng = FilledRng::new(entropy, 4096)?;
    seal_dump_with_rng(
        public_key_hex,
        subscription_id,
        resource,
        plaintext,
        &mut rng,
    )
}

fn seal_dump_with_rng(
    public_key_hex: &str,
    subscription_id: &str,
    resource: &str,
    plaintext: &[u8],
    rng: &mut impl CryptoRng,
) -> Result<Vec<u8>, OsppError> {
    let public_key = parse_recipient_public_key(public_key_hex)?;
    let (enc, mut context) = setup_sender_with_rng::<AesGcm256, HkdfSha256, DhP256HkdfSha256>(
        &OpModeS::Base,
        &public_key,
        DUMP_INFO,
        rng,
    )
    .map_err(|error| OsppError::Hpke(error.to_string()))?;
    let aad = dump_aad(subscription_id, resource);
    let mut out = enc.to_bytes().as_slice().to_vec();
    for chunk in plaintext.chunks(64 * 1024) {
        let sealed = context
            .seal(chunk, aad.as_bytes())
            .map_err(|error| OsppError::Hpke(error.to_string()))?;
        out.extend_from_slice(&(chunk.len() as u32).to_be_bytes());
        out.extend_from_slice(&sealed);
    }
    let eof = context
        .seal(&[], aad.as_bytes())
        .map_err(|error| OsppError::Hpke(error.to_string()))?;
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&eof);
    Ok(out)
}

pub(super) fn read_client(
    socket: &mut WebSocket<TcpStream>,
) -> Result<Option<ClientFrame>, OsppError> {
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => return ClientFrame::parse(text.as_str()).map(Some),
            Ok(Message::Binary(_)) => {
                return Err(OsppError::Protocol(
                    "binary WebSocket frames are not part of ospp-v1".to_owned(),
                ));
            }
            Ok(Message::Ping(payload)) => {
                socket.send(Message::Pong(payload))?;
            }
            Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
            Ok(Message::Close(_)) => return Err(OsppError::Closed),
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

pub(super) fn send_frame(
    socket: &mut WebSocket<TcpStream>,
    frame: &ServerFrame,
) -> Result<(), OsppError> {
    socket.send(Message::Text(frame.to_json()?.into()))?;
    Ok(())
}

pub(super) fn send_error(
    socket: &mut WebSocket<TcpStream>,
    id: Option<&str>,
    code: &str,
    message: &str,
) -> Result<(), OsppError> {
    send_frame(
        socket,
        &ServerFrame::Error {
            id: id.map(str::to_owned),
            code: code.to_owned(),
            message: message.to_owned(),
        },
    )
}

pub(super) fn frame_type(frame: &ClientFrame) -> &'static str {
    match frame {
        ClientFrame::Hello { .. } => "hello",
        ClientFrame::AuthResponse { .. } => "auth_response",
        ClientFrame::PairHello { .. } => "pair_hello",
        ClientFrame::PairBegin { .. } => "pair_begin",
        ClientFrame::Subscribe { .. } => "subscribe",
        ClientFrame::Unsubscribe { .. } => "unsubscribe",
        ClientFrame::DumpRequest { .. } => "dump_request",
    }
}

/// A bounded, already-gathered CSPRNG adapter for `hpke` encapsulation.
///
/// P-256 encapsulation consumes a handful of 32-byte scalars; the buffer is sized far above that,
/// and exhaustion is a programming error rather than a silent weak-randomness fallback.
pub(super) struct FilledRng {
    buffer: Vec<u8>,
    offset: usize,
}

impl FilledRng {
    pub(super) fn new(entropy: &dyn EntropySource, len: usize) -> Result<Self, OsppError> {
        let mut buffer = vec![0u8; len];
        entropy.fill_bytes(&mut buffer)?;
        Ok(Self { buffer, offset: 0 })
    }

    fn take(&mut self, len: usize) -> &[u8] {
        assert!(
            self.offset + len <= self.buffer.len(),
            "reference-server hpke RNG buffer exhausted"
        );
        let start = self.offset;
        self.offset += len;
        &self.buffer[start..self.offset]
    }
}

impl TryRng for FilledRng {
    type Error = std::convert::Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let bytes = self.take(4);
        Ok(u32::from_le_bytes(bytes.try_into().expect("4 bytes")))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let bytes = self.take(8);
        Ok(u64::from_le_bytes(bytes.try_into().expect("8 bytes")))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        let len = dst.len();
        dst.copy_from_slice(self.take(len));
        Ok(())
    }
}

impl TryCryptoRng for FilledRng {}
