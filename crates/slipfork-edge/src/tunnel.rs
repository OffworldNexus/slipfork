//! `TunnelRegistrar` adapters for `WorldRegistry::share_world`.
//!
//! [`UnavailableTunnelRegistrar`] is the **fail-closed** registrar `slipforkd` wires until
//! `tunnel-weaver` publishes `weaved`'s IPC specification (`H-006`, closed by deferral): every
//! `register` call is refused and `share_world` surfaces [`crate::EdgeError::TunnelUnavailable`]
//! while `unshare_world` stays an idempotent no-op (`ADR-0005` §2). It is deleted in `M2.22` when
//! the concrete `WeavedRegistrar` ships — it is not a compatibility shim and nothing else stands in
//! for a tunnel.

use slipfork_world::{TunnelError, TunnelLease, TunnelRegistrar, TunnelRequest};

/// Reason rendered for every refused registration.
const UNAVAILABLE_REASON: &str = "public sharing needs the weaved tunnel daemon, whose IPC specification is not published yet; use the world's .localhost origin instead";

/// A [`TunnelRegistrar`] that always fails closed (`H-006`).
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableTunnelRegistrar;

impl UnavailableTunnelRegistrar {
    /// Construct the fail-closed registrar.
    pub fn new() -> Self {
        Self
    }
}

impl TunnelRegistrar for UnavailableTunnelRegistrar {
    fn register(&self, _request: &TunnelRequest<'_>) -> Result<Box<dyn TunnelLease>, TunnelError> {
        Err(TunnelError::DaemonUnavailable(
            UNAVAILABLE_REASON.to_owned(),
        ))
    }
}
