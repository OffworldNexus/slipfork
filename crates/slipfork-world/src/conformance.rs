//! Shared conformance suite for every [`TunnelRegistrar`] implementation, plus an in-memory test
//! double of the crate's own trait (never a mock of production code).

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::{
    LocalHostRoute, ProjectSlug, TunnelError, TunnelLease, TunnelRegistrar, TunnelRequest,
    WorldTarget,
};

/// Deterministic in-memory [`TunnelRegistrar`] used by tests and conformance runs.
///
/// Public URLs use the RFC 2606 reserved `.invalid` TLD so they can never be mistaken for a live
/// tunnel. This is a test double of Slipfork's own domain trait, not a production fallback.
#[derive(Debug, Default)]
pub struct InMemoryTunnelRegistrar {
    next_lease: AtomicU64,
    active_leases: Arc<Mutex<BTreeSet<String>>>,
}

impl InMemoryTunnelRegistrar {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return `true` while `lease_id` is currently active and has not been dropped/revoked.
    pub fn is_lease_active(&self, lease_id: &str) -> bool {
        self.active_leases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(lease_id)
    }
}

struct InMemoryLease {
    public_url: String,
    lease_id: String,
    active_leases: Arc<Mutex<BTreeSet<String>>>,
}

impl Drop for InMemoryLease {
    fn drop(&mut self) {
        self.active_leases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.lease_id);
    }
}

impl TunnelLease for InMemoryLease {
    fn public_url(&self) -> &str {
        &self.public_url
    }

    fn lease_id(&self) -> &str {
        &self.lease_id
    }
}

impl TunnelRegistrar for InMemoryTunnelRegistrar {
    fn register(&self, request: &TunnelRequest<'_>) -> Result<Box<dyn TunnelLease>, TunnelError> {
        let seq = self.next_lease.fetch_add(1, Ordering::Relaxed) + 1;
        let lease_id = format!("lease-{seq}-{}", request.local_port);
        self.active_leases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(lease_id.clone());
        Ok(Box::new(InMemoryLease {
            public_url: format!("https://tunnel.invalid/{}", request.route.host()),
            lease_id,
            active_leases: Arc::clone(&self.active_leases),
        }))
    }
}

/// Verify the full [`TunnelRegistrar`] contract (`ADR-0005` §2):
/// non-empty URLs/ids, unique lease ids, host-aware public URLs, independent concurrent lease
/// lifetimes (`TestSlot` included), and deterministic `Drop`-triggered tunnel revocation via an
/// adapter-supplied `is_lease_active` probe.
pub fn assert_tunnel_registrar_conformance<R, F>(registrar: &R, mut is_lease_active: F)
where
    R: TunnelRegistrar + ?Sized,
    F: FnMut(&str) -> bool,
{
    assert!(
        !is_lease_active("unregistered-lease-probe"),
        "revocation probe must return false for unknown lease ids"
    );

    let canonical = LocalHostRoute::new(
        ProjectSlug::parse("acme").expect("valid project slug"),
        WorldTarget::Canonical,
        None,
        &[],
    )
    .expect("canonical route");
    let first = registrar
        .register(&TunnelRequest {
            route: &canonical,
            local_port: 80,
        })
        .expect("canonical tunnel registration");
    let first_id = first.lease_id().to_owned();

    assert!(!first.public_url().is_empty(), "public_url must be set");
    assert!(
        first.public_url().contains(&canonical.host()),
        "public_url must identify the routed host"
    );
    assert!(!first_id.is_empty(), "lease_id must be set");
    assert!(
        is_lease_active(&first_id),
        "newly registered canonical lease must be active"
    );

    let slot = LocalHostRoute::new(
        ProjectSlug::parse("acme").expect("valid project slug"),
        WorldTarget::TestSlot(3),
        Some("admin"),
        &["admin"],
    )
    .expect("test-slot route");
    let second = registrar
        .register(&TunnelRequest {
            route: &slot,
            local_port: 8000,
        })
        .expect("test-slot tunnel registration");
    let second_id = second.lease_id().to_owned();

    assert!(
        second.public_url().contains(&slot.host()),
        "test-slot public_url must identify the routed host"
    );
    assert_ne!(
        first_id, second_id,
        "concurrent leases must have distinct ids"
    );
    assert!(
        is_lease_active(&first_id) && is_lease_active(&second_id),
        "both concurrent leases must be active before drop"
    );

    drop(second);
    assert!(
        !is_lease_active(&second_id),
        "dropping a TunnelLease MUST revoke its tunnel"
    );
    assert!(
        is_lease_active(&first_id),
        "dropping one lease must not revoke an independent concurrent lease"
    );
    assert!(
        !first.public_url().is_empty(),
        "dropping one lease must not invalidate another"
    );

    drop(first);
    assert!(
        !is_lease_active(&first_id),
        "dropping the surviving TunnelLease MUST revoke its tunnel"
    );
}
