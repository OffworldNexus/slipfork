//! `slipfork-ospp`: Slipfork-owned OSPP v1 client, P-256 device keys, and streaming seed dumps.
//!
//! The crate owns the following synchronous DI ports; `slipforkd` injects every adapter and holds
//! the persistent session:
//!
//! - `DeviceKeySigner` (`KeyId` = `p256:<66 lowercase hex>`, `DeviceSignature` = raw 64-byte
//!   `r || s`): `TpmdDeviceKeySigner` over the pinned upstream `tpmd-client` / `tpmd-ipc` crates,
//!   `SoftwareP256Signer` over an injected `DeviceKeyStore` + `EntropySource`, and the generic
//!   `FallbackDeviceKeySigner` composing both.
//! - `OsppClient`: the OSPP v1 server-speaks-first WebSocket session (`ospp-v1` subprotocol over
//!   `tungstenite` + `rustls` + `webpki-roots`), the single `resource_offer()` mapper into
//!   `RemoteResourceOffer`, and the zero-filesystem-write `SecretProvider` implementation.
//! - `SeedDumpStore` / `DumpStreamer`: bounded-window `zstd` streaming of PostgreSQL seed dumps with
//!   incremental SHA-256.
//!
//! `tokio` is private to [`tpmd`]'s instance-scoped `current_thread` runtime: no async trait and no
//! global runtime appears here, and the crate reads **zero** `std::env` (the Composition Root
//! resolves `$XDG_RUNTIME_DIR` into `KeyStorePaths`).
//!
//! Modules are documented stubs for the scaffolding node `M2.2`; behaviour lands in `M2.13`-`M2.14`.

#![forbid(unsafe_code)]

pub mod client;
pub mod device_key;
pub mod dump;
pub mod pairing;
pub mod resources;
pub mod secret_provider;
pub mod software_key;
pub mod tpmd;
