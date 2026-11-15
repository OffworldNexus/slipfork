//! Dev-only support for `slipfork-ospp` integration tests.
//!
//! Compiled only into `tests/` targets, never into the shipped library: a deterministic clock, a
//! seeded CSPRNG double, a fake `tpmd` daemon built on the real pinned crates, and an in-process
//! `ospp-v1` WebSocket server that exercises the production client code path.

pub mod mock_server;
pub mod signers;
pub mod tpmd_server;

pub use mock_server::{HandleRecord, MockOsppServer, MockServerConfig, SeededEntropy};
pub use signers::{FixedEntropy, MisSigningSigner, known_software_key, software_signer};
pub use tpmd_server::{FakeTpmd, TpmdBehavior};
