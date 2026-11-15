//! `TunnelRegistrar` adapters owned by the edge crate: `UnavailableTunnelRegistrar`, which fails
//! closed with `EdgeError::TunnelUnavailable`, and the isolated `WeavedRegistrar` wire adapter that
//! stays gated on `H-006`.
