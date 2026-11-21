//! `slipfork-ospp`: the clean-sheet OSPP v1 client.
//!
//! `specs/05-ospp-v1.md` is the sole normative wire specification. The crate owns
//! the client-side ports only; the daemon wires the adapters at its single
//! Composition Root, which is also the canonical `Forkfile` owner:
//!
//! - [`client::OsppClient`] + [`client::WsOsppClient`]: `pair`, `connect` ->
//!   `subscribe`/`dump`/`unsubscribe`/`close` over `tungstenite` + `rustls`.
//! - [`device_key::DeviceKeySigner`] + [`device_key::FileDeviceKeyStore`]: the
//!   `0600` machine-authentication P-256 identity (auth and pairing only).
//! - [`keys::SubscriptionKey`]: a per-subscription `hpke-p256` keypair held in
//!   memory only (`info = "ospp-v1-secret"`, `aad = "<component>:<env_key>"`).
//! - [`dump::DumpReader`]: the out-of-band `SetupBaseR` sealed-stream decoder.
//! - [`pairing::LivePairingUrl`]: the server-supplied `<url>#handle=<handle>`
//!   fragment-only pairing page, returned to the caller that opens it.
//! - [`pairing::PairingStore`]: the public `<state_dir>/ospp.json` record.
//! - [`vault::SecretVault`] + [`vault::FileSecretVault`]: the `0600` offline vault.
//!
//! Zero `std::env`, zero process-wide caches, zero `unsafe`, and no global
//! mutable state. Key material and random nonces come from the operating-system
//! random source directly; every path is injected by the caller.

#![forbid(unsafe_code)]

pub mod client;
pub mod device_key;
pub mod dump;
pub mod endpoint;
pub mod error;
pub mod keys;
pub mod model;
pub mod pairing;
pub mod vault;
pub mod wire;

pub use client::{
    DumpTicket, MAX_DUMP_HEADER_BYTES, OsppClient, OsppTimeouts, PairingApproval, WsOsppClient,
    WsOsppSession, open_dump_stream,
};
pub use device_key::{
    DeviceKeySigner, DeviceSignature, FileDeviceKeyStore, PUBLIC_KEY_HEX_LEN, SIGNATURE_BYTES,
    SIGNATURE_HEX_LEN, SoftwareP256Key, SoftwareP256Signer,
};
pub use dump::{DumpReader, MAX_DUMP_CHUNK};
pub use endpoint::{OsppEndpoint, is_loopback_host};
pub use error::{OsppError, codes};
pub use keys::{
    DUMP_INFO, KEM_ENCODED_KEY_BYTES, SECRET_INFO, SubscriptionKey, dump_aad, secret_aad,
};
pub use model::{OsppClaims, Secret, SubscribeRequest, SubscriptionEnv};
pub use pairing::{
    HANDLE_HEX_LEN, LivePairingUrl, PairPhase, PairingState, PairingStore, altcha_challenge_hash,
    solve_altcha,
};
pub use vault::{FileSecretVault, PRIVATE_FILE_MODE, SecretVault};
pub use wire::{
    AUTH_METHOD_P256, ENCRYPTION_METHOD_HPKE_P256, OSPP_SUBPROTOCOL, PROTOCOL_VERSION,
    auth_transcript, pairing_transcript,
};
