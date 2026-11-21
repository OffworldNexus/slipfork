//! HTTP/WebSocket framing and `.localhost` route matching over an injected listener.

/// Entry marker for the route matcher owned by this crate.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IngressRouter;

impl IngressRouter {
    /// Builds the router marker.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}
