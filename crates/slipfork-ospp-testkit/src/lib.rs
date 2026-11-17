//! Unpublished OSPP v1 test scaffolding: the in-tree `ospp-v1` reference server, a fake `tpmd`
//! daemon, deterministic signers, and a filesystem-free seed-dump store.
//!
//! This crate is `publish = false` and test-only: it is consumed through `[dev-dependencies]` by
//! `slipfork-ospp` and `slipfork`, and is never linked into a shipped binary. The doubles speak the
//! real wire contracts — a loopback WebSocket with real P-256 verification, the pinned `tpmd-ipc`
//! framing with real `tpmd-proto` messages, and real `zstd` archives — so they are never a
//! production fallback.

#![forbid(unsafe_code)]

pub mod memory_store;
pub mod reference_server;
pub mod signers;
pub mod tpmd_server;

pub use memory_store::MemorySeedDumpStore;
pub use reference_server::{
    DumpRecord, HandleRecord, ReferenceOsppServer, ReferenceServerConfig, S3BucketRecord,
    SeededEntropy,
};
pub use signers::{FixedEntropy, MisSigningSigner, known_software_key, software_signer};
pub use tpmd_server::{FakeTpmd, TpmdBehavior};
