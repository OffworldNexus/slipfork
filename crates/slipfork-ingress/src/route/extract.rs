//! Canonical `Forkfile` -> ingress-table extraction.
//!
//! Declared authorities and service routes are read from the evaluated AST once through the
//! `slipfork-forkfile` visitors; no call site re-parses route syntax or guesses an authority from
//! raw text.

use slipfork_forkfile::ast::Forkfile;

use super::{IngressRoute, IngressTable, TableError};

impl IngressTable {
    /// Builds a world's table directly from an evaluated `Forkfile` document.
    ///
    /// Every `domain <id>`, `s3` bucket, and `cdn <id>` becomes a declared authority; each service's
    /// `port` plus `route` items become one [`IngressRoute`]. `on` blocks are flattened by the
    /// document visitors.
    ///
    /// # Errors
    ///
    /// Returns [`TableError`] for an ambiguous, duplicate, or undeclared-domain table.
    pub fn from_forkfile(world: impl Into<String>, doc: &Forkfile) -> Result<Self, TableError> {
        let declared = doc.domains().into_iter().map(str::to_owned);
        let mut routes = Vec::new();
        for service in doc.services() {
            let Some(port) = service.port else {
                continue;
            };
            for route in service.routes {
                routes.push(IngressRoute::new(port, route.clone()));
            }
        }
        Self::new(world, declared, routes)
    }
}
