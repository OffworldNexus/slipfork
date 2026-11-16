//! Dev-only support for `slipfork-ospp` integration tests.
//!
//! Compiled only into `tests/` targets, never into the shipped library: a deterministic clock, a
//! seeded CSPRNG double, a fake `tpmd` daemon built on the real pinned crates, and the in-tree
//! `ospp-v1` reference server that serves both the handshake and every capability.

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
