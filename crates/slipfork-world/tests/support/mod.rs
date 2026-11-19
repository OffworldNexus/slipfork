//! Dev-only support for `slipfork-world` integration tests.
//!
//! This module is compiled only into `tests/` targets, never into the shipped library: the single
//! route builder every integration test shares.

use slipfork_world::{LocalHostRoute, ProjectSlug, WorldTarget};

/// Single route builder shared by every `slipfork-world` integration test.
pub fn route(project: &str, target: WorldTarget, snow_domain: Option<&str>) -> LocalHostRoute {
    LocalHostRoute::new(
        ProjectSlug::parse(project).expect("valid project slug"),
        target,
        snow_domain,
        &["admin", "docs"],
    )
    .expect("valid route")
}

/// The canonical `acme.localhost` route used by most fixtures.
pub fn canonical_route() -> LocalHostRoute {
    route("acme", WorldTarget::Canonical, None)
}
