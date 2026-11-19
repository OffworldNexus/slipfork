//! Unpublished OSPP v1 test scaffolding.
//!
//! The crate ships the in-tree `ospp-v1` reference server (`reference_server`),
//! server-side HPKE sealing helpers, and deterministic doubles (`signers`). It is
//! `publish = false` and test-only: consumed through `[dev-dependencies]` by
//! `slipfork-ospp` and `slipfork`, never linked into a shipped binary.

#![forbid(unsafe_code)]

pub mod reference_server;
pub mod signers;

pub use reference_server::{
    PairOutcome, ReferenceOsppServer, ReferenceServerConfig, ServerRequest, ServerState,
    SubscriptionRecord, seal_dump, seal_secret,
};
pub use signers::{
    FixedEntropy, MisSigningSigner, SeededEntropy, known_software_key, software_signer,
};
