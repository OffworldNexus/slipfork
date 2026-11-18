//! `slipfork-ospp`: the clean-sheet OSPP v1 synchronous client.
//!
//! `docs/design/ospp.md` is the sole normative wire specification. The crate owns
//! the client-side DI ports only; `slipforkd` wires the adapters at its single
//! Composition Root:
//!
//! - [`client::OsppClient`] + [`client::OsppSession`]: `pair`, `connect` ->
//!   `subscribe`/`dump`/`unsubscribe`/`close`; production
//!   [`client::WsOsppClient`] over `tungstenite` + `rustls` + `webpki-roots`.
//! - [`device_key::DeviceKeySigner`] + [`device_key::DeviceKeyStore`]: the
//!   `0600` machine-authentication P-256 identity (auth and pairing only).
//! - [`keys::SubscriptionKey`]: a per-subscription `hpke-p256` keypair held in
//!   memory only (`info = "ospp-v1-secret"`, `aad = "<component>:<env_key>"`).
//! - [`dump::DumpReader`]: the out-of-band `SetupBaseR` sealed-stream decoder.
//! - [`pairing::BrowserOpener`] + [`pairing::LivePairingUrl`]: the
//!   server-supplied `<url>#handle=<handle>` fragment-only pairing page.
//!
//! Zero `std::env`, zero filesystem access outside [`device_key::FileDeviceKeyStore`]
//! (the injected `0600` machine key), zero `unsafe`, and no global mutable state.

#![forbid(unsafe_code)]

pub mod client;
pub mod device_key;
pub mod dump;
pub mod endpoint;
pub mod error;
pub mod keys;
pub mod model;
pub mod pairing;
pub mod wire;

pub use client::{
    OsppClient, OsppSession, OsppTimeouts, PairingApproval, WsOsppClient, WsOsppSession,
};
pub use device_key::{
    DeviceKeyPath, DeviceKeySigner, DeviceKeyStore, DeviceSignature, FileDeviceKeyStore,
    PUBLIC_KEY_HEX_LEN, SIGNATURE_BYTES, SIGNATURE_HEX_LEN, SoftwareP256Key, SoftwareP256Signer,
    verify_device_signature,
};
pub use dump::{DumpReader, MAX_DUMP_CHUNK};
pub use endpoint::OsppEndpoint;
pub use error::{OsppError, codes};
pub use keys::{
    DUMP_INFO, KEM_ENCODED_KEY_BYTES, SECRET_INFO, SubscriptionKey, dump_aad,
    parse_recipient_public_key, secret_aad,
};
pub use model::{OsppClaims, SubscribeRequest, SubscriptionEnv};
pub use pairing::{BrowserOpener, HANDLE_HEX_LEN, LivePairingUrl, solve_altcha};
pub use wire::{
    AUTH_METHOD_P256, ENCRYPTION_METHOD_HPKE_P256, OSPP_SUBPROTOCOL, PROTOCOL_VERSION,
    auth_transcript, pairing_transcript,
};
