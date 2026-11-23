//! Typed failures for building a route table and for resolving one request.

/// A table that would route a request two ways, or never route it at all.
///
/// These are configuration failures raised while the daemon attaches a world; they never reach a
/// live request path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TableError {
    /// World ids double as authorities, so they must be host labels.
    #[error("world `{world}` is not a valid lowercase authority label")]
    InvalidWorld {
        /// The offending world id.
        world: String,
    },
    /// A registry holds at most one table per world.
    #[error("world `{world}` is already registered")]
    DuplicateWorld {
        /// The duplicated world id.
        world: String,
    },
    /// A zero port never reaches a service.
    #[error("world `{world}` route maps to port 0")]
    PortZero {
        /// The world being validated.
        world: String,
    },
    /// `domain <id>` must name a declared domain.
    #[error("world `{world}` route domain `{domain}` is not declared")]
    UndeclaredDomain {
        /// The world being validated.
        world: String,
        /// The undeclared domain id.
        domain: String,
    },
    /// Two routes claim the same authority and path.
    #[error("world `{world}` has a duplicate route `{route}`")]
    Duplicate {
        /// The world being validated.
        world: String,
        /// The duplicated route.
        route: String,
    },
    /// Two routes cover the same request with no deterministic winner.
    #[error("world `{world}` has ambiguous overlapping routes `{left}` and `{right}`")]
    Ambiguous {
        /// The world being validated.
        world: String,
        /// The first overlapping route.
        left: String,
        /// The second overlapping route.
        right: String,
    },
}

/// A request that matches no registered authority or path, or an unsafe request target.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RouteError {
    /// Only `.localhost` authorities are served.
    #[error("host `{host}` is outside `.localhost`")]
    HostOutsideLocalhost {
        /// The rejected `Host` header.
        host: String,
    },
    /// A `.localhost` authority that no attached world owns.
    #[error("no world is attached for host `{host}`")]
    UnknownHost {
        /// The unmatched `Host` header.
        host: String,
    },
    /// The world exists but has no route for this path and authority.
    #[error("world `{world}` has no route for `{path}` (host `{host}`)")]
    UnknownPath {
        /// The matched world.
        world: String,
        /// The authority host.
        host: String,
        /// The unmatched request target.
        path: String,
    },
    /// The proxied request target must stay origin-form.
    #[error("request target `{target}` is not an origin-form path")]
    NotOriginForm {
        /// The rejected target.
        target: String,
    },
    /// A `..` segment would escape the matched prefix.
    #[error("request target `{target}` escapes its routed prefix")]
    PathTraversal {
        /// The rejected target.
        target: String,
    },
}
