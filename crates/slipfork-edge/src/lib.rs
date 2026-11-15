//! `slipfork-edge`: the sole `slipforkd` ingress plus the canonical world registry and daemon
//! control port.
//!
//! This crate owns the edge half of the DI tree assembled by the `slipforkd` Composition Root in
//! `crates/slipfork`, and it is the **only** place in the workspace that speaks HTTP/WebSocket to
//! world backends:
//!
//! ```text
//! slipforkd  --injects-->  dyn DaemonControl   (thin CLI drives this over <runtime_dir>/daemon.sock)
//! slipforkd  --injects-->  WorldRegistry       ((ProjectSlug, WorldTarget) -> WorldOrigin routing)
//! ```
//!
//! - `DaemonControl`: the typed port the thin `slipfork` CLI drives over the `0600`
//!   `<runtime_dir>/daemon.sock` JSON-line transport, including daemon-owned `forward` (no FD
//!   handover, no stream upgrade, no `SCM_RIGHTS`).
//! - `WorldRegistry`: canonical owner of `(ProjectSlug, WorldTarget)` -> `WorldOrigin` routing,
//!   resolving `*.localhost` and `shared_hosts` requests through each world's `IngressRoute`
//!   longest-prefix table into `WorldEngine::dial_world_tcp`, and minting `WorldOrigin::Shared`
//!   through the injected `TunnelRegistrar`.
//!
//! Ambient OS access is confined to [`activation`]: `std::env` (`LISTEN_PID` / `LISTEN_FDS`) lives
//! **only** there, and `listenfd` / `socket2` / `httparse` never leak into domain crates. Every
//! module is synchronous (`std::thread`, zero async runtime).
//!
//! Modules are documented stubs for the scaffolding node `M2.2`; behaviour lands in `M2.8`-`M2.10`
//! (and the `H-006`-gated `M2.22`).

#![forbid(unsafe_code)]

pub mod activation;
pub mod daemon;
pub mod ingress;
pub mod registry;
pub mod tunnel;
pub mod unit;
