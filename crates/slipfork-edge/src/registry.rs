//! Canonical `WorldRegistry`: `(ProjectSlug, WorldTarget)` -> `WorldOrigin` routing, the
//! `IngressRoute` prefix table, and the `shared_hosts` map.
//!
//! Worlds are attached and released in-process (never through a cross-process handle).
//! `share_world` mints `WorldOrigin::Shared` through the injected `TunnelRegistrar` and requires a
//! bound forward listener; `unshare_world` is an idempotent return to `WorldOrigin::Local`.
