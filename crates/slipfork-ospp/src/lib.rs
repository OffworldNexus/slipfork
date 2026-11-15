//! `slipfork-ospp`: Slipfork-owned OSPP v1 client, P-256 device keys, and streaming seed dumps.
//!
//! The crate owns the following synchronous DI ports; `slipforkd` injects every adapter and holds
//! the persistent session:
//!
//! - [`device_key::DeviceKeySigner`] (`KeyId` = `p256:<66 lowercase hex>`, `DeviceSignature` = raw
//!   64-byte `r || s`): [`tpmd::TpmdDeviceKeySigner`] over the pinned upstream `tpmd-client` /
//!   `tpmd-ipc` crates, [`software_key::SoftwareP256Signer`] over an injected
//!   [`device_key::DeviceKeyStore`] + `EntropySource`, and the generic
//!   [`tpmd::FallbackDeviceKeySigner`] composing both.
//! - [`client::OsppClient`]: the OSPP v1 server-speaks-first WebSocket session (`ospp-v1`
//!   subprotocol over `tungstenite` + `rustls` + `webpki-roots`) and the typed capability dispatch
//!   `env.bundle`, `secrets.ensure`, `resources.describe`, and `dump.begin`/`dump.end`
//!   (`ADR-0008` §2). [`client::OsppClient::resource_offer`] is the sole bundle-to-offer mapper.
//! - [`secret_provider::OsppSecretProvider`]: `Send + Sync`, `&self` [`slipfork_snow::SecretProvider`]
//!   with compare-and-set `!!` semantics and zero filesystem writes.
//! - [`dump::DumpStreamer`] + [`dump::SeedDumpStore`]: a 64 KiB-capped binary stream with
//!   incremental SHA-256 into a digest-pinned cache ([`DiskSeedDumpStore`], non-tmpfs); the
//!   streamer performs zero filesystem access, because publishing, verifying, opening, and
//!   discarding the artifact are [`dump::SeedDumpStore`] capabilities.
//! - [`pairing::PairingUrl`] / [`pairing::redeem_pairing_url`]: fragment-only browser pairing with
//!   a single-use handle that zeroizes after the one `pair_hello`.
//!
//! `tokio` is private to [`tpmd`]'s instance-scoped `current_thread` runtime: no async trait and no
//! global runtime appears here, and the crate reads no environment variables (the Composition Root
//! resolves `$XDG_RUNTIME_DIR` into [`tpmd::KeyStorePaths`]).

#![forbid(unsafe_code)]

pub mod client;
pub mod device_key;
pub mod dump;
pub mod error;
pub mod pairing;
pub mod resources;
pub mod secret_provider;
pub mod software_key;
pub mod tpmd;

mod disk_dump;
mod hex;
mod protocol;

pub use client::{
    CONNECT_PATH, CONNECT_TRANSCRIPT_PREFIX, MAX_CONTROL_FRAME, MAX_DUMP_FRAME, NONCE_BYTES,
    OSPP_SUBPROTOCOL, OsppClient, OsppEndpoint, OsppTimeouts, PAIR_PATH, PAIRING_TRANSCRIPT_PREFIX,
    PROTOCOL_VERSION, connect_transcript, pairing_transcript, parse_nonce, random_nonce,
};
pub use device_key::{
    DeviceKeySigner, DeviceKeyStore, DeviceSignature, KEY_ID_BYTES, KEY_ID_HEX_LEN, KEY_ID_PREFIX,
    KeyId, SIGNATURE_BYTES, SIGNATURE_HEX_LEN, verify_device_signature,
};
pub use disk_dump::{DUMP_SUFFIX, DiskSeedDumpStore, PART_SUFFIX};
pub use dump::{DumpStream, DumpStreamer, MAX_DUMP_WINDOW_LOG, SeedDumpStore, VerifiedDump};
pub use error::{OsppError, PairingRejectReason, RejectReason};
pub use pairing::{
    BrowserOpener, HANDLE_BYTES, HANDLE_HEX_LEN, LivePairingUrl, PairingApproval, PairingHandle,
    PairingScheme, PairingUrl, redeem_pairing_url,
};
pub use protocol::{
    CAPABILITY_DUMP_BEGIN, CAPABILITY_DUMP_END, CAPABILITY_ENV_BUNDLE,
    CAPABILITY_RESOURCES_DESCRIBE, CAPABILITY_SECRETS_ENSURE, EnvBundle, MAX_KEY_LEN, MAX_MODE_LEN,
};
pub use resources::{PgDumpFormat, PgDumpMetadata, RemoteResourceBundle, S3BucketCoordinates};
pub use secret_provider::OsppSecretProvider;
pub use software_key::{FileDeviceKeyStore, SoftwareP256Key, SoftwareP256Signer};
pub use tpmd::{
    FallbackDeviceKeySigner, FallbackDiagnostic, KeyStorePaths, TPMD_NAMESPACE,
    TPMD_SECURITY_FLOOR, TPMD_STORE_WAIT, TpmdDeviceKeySigner,
};
