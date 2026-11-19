//! One attached world's ingress table: the route grammar, the `<label>.*` host family, and
//! longest-prefix path resolution.
//!
//! The `Host`/DNS-label grammar is owned by `slipfork-world` ([`is_valid_dns_label`] and
//! [`slipfork_world::normalize_host`]); this module only decides which declared route covers one
//! already-normalized `Host` and request path, so the edge never hand-encodes a label rule twice.

use slipfork_world::is_valid_dns_label;

use crate::EdgeError;

use super::IngressRoute;

/// The declared ingress table of one attached world: `Forkfile` prefixes plus the empty-prefix
/// catch-all built from the first component `internal_ports[0]`.
///
/// Two spellings live in one table (`IngressRoute`), so one owner must classify them:
///
/// - a path prefix (`"/api"`, `"/"`, or `""`) is matched longest-prefix-first; or
/// - a `<label>.*` entry (`"mail.*"`) owns the `label.<world>.localhost` host family and never
///   participates in path matching.
#[derive(Debug, Default)]
pub(super) struct IngressTable {
    routes: Vec<IngressRoute>,
}

impl IngressTable {
    /// Wrap the caller's declared routes (the `internal_ports[0]` catch-all included).
    pub(super) fn new(routes: Vec<IngressRoute>) -> Self {
        Self { routes }
    }

    /// Fail closed on a table that cannot be routed deterministically.
    pub(super) fn validate(&self, world: &str) -> Result<(), EdgeError> {
        if self.routes.is_empty() {
            return Err(EdgeError::InvalidIngressRoute {
                world: world.to_owned(),
                reason: "at least one route (the internal_ports[0] catch-all) is required"
                    .to_owned(),
            });
        }
        for (index, route) in self.routes.iter().enumerate() {
            if route.port == 0 {
                return Err(EdgeError::InvalidIngressRoute {
                    world: world.to_owned(),
                    reason: format!("prefix '{}' maps to port 0", route.prefix),
                });
            }
            let subdomain = subdomain_label(&route.prefix).is_some();
            if !subdomain
                && !route.prefix.is_empty()
                && (!route.prefix.starts_with('/')
                    || route.prefix.contains(char::is_whitespace)
                    || route.prefix.contains('?')
                    || route.prefix.contains('#'))
            {
                return Err(EdgeError::InvalidIngressRoute {
                    world: world.to_owned(),
                    reason: format!(
                        "prefix '{}' must be empty or start with '/', or be one lowercase \
                         `<label>.*` subdomain route, and contain no whitespace, '?', or '#'",
                        route.prefix
                    ),
                });
            }
            if self.routes[..index]
                .iter()
                .any(|previous| previous.prefix == route.prefix)
            {
                return Err(EdgeError::InvalidIngressRoute {
                    world: world.to_owned(),
                    reason: format!("duplicate ingress prefix '{}'", route.prefix),
                });
            }
        }
        Ok(())
    }

    /// Longest path prefix wins; `<label>.*` host routes never participate.
    pub(super) fn path_port(&self, path: &str) -> Option<u16> {
        self.routes
            .iter()
            .filter(|route| subdomain_label(&route.prefix).is_none())
            .filter(|route| prefix_matches(&route.prefix, path))
            .max_by_key(|route| route.prefix.len())
            .map(|route| route.port)
    }

    /// The first subdomain route whose label prefixes `host`, with the remaining host labels.
    pub(super) fn subdomain_route<'a>(&self, host: &'a str) -> Option<(u16, &'a str)> {
        self.routes.iter().find_map(|route| {
            let label = subdomain_label(&route.prefix)?;
            let remainder = host.strip_prefix(label)?.strip_prefix('.')?;
            (!remainder.is_empty()).then_some((route.port, remainder))
        })
    }
}

/// `true` when `prefix` covers `path` as a URL path segment boundary.
///
/// The empty prefix (the `internal_ports[0]` catch-all) covers everything; `"/api"` covers
/// `"/api"`, `"/api/users"`, and `"/api?x=1"`, but never `"/apifoo"`.
fn prefix_matches(prefix: &str, path: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    let Some(rest) = path.strip_prefix(prefix) else {
        return false;
    };
    prefix.ends_with('/') || rest.is_empty() || rest.starts_with('/') || rest.starts_with('?')
}

/// A `<label>.*` route entry: exactly one canonical lowercase DNS label plus `.*`.
fn subdomain_label(prefix: &str) -> Option<&str> {
    let label = prefix.strip_suffix(".*")?;
    is_valid_dns_label(label).then_some(label)
}

#[cfg(test)]
mod tests {
    use super::{IngressTable, subdomain_label};
    use crate::IngressRoute;

    fn table(entries: &[(&str, u16)]) -> IngressTable {
        IngressTable::new(
            entries
                .iter()
                .map(|(prefix, port)| IngressRoute {
                    prefix: (*prefix).to_owned(),
                    port: *port,
                })
                .collect(),
        )
    }

    #[test]
    fn a_subdomain_route_owns_its_host_family() {
        let routes = table(&[("mail.*", 81), ("", 80)]);
        assert_eq!(
            routes.subdomain_route("mail.demo.localhost"),
            Some((81, "demo.localhost"))
        );
        assert_eq!(
            routes.subdomain_route("mail.feat.demo.localhost"),
            Some((81, "feat.demo.localhost"))
        );
        assert_eq!(routes.subdomain_route("demo.localhost"), None);
        assert_eq!(routes.subdomain_route("www.demo.localhost"), None);
        assert_eq!(subdomain_label("a.b.*"), None);
        assert!(routes.validate("demo").is_ok());
        assert!(table(&[("MAIL.*", 81)]).validate("demo").is_err());
        // The canonical label grammar: `<= 63` bytes, no edge `-`, lowercase only.
        assert!(table(&[("mail_x.*", 81)]).validate("demo").is_err());
        assert!(table(&[("-mail.*", 81)]).validate("demo").is_err());
        assert!(table(&[("mail-.*", 81)]).validate("demo").is_err());
    }

    #[test]
    fn the_longest_path_prefix_wins() {
        let routes = table(&[("/", 2), ("/api", 1), ("mail.*", 3)]);
        assert_eq!(routes.path_port("/api/users?page=2"), Some(1));
        assert_eq!(routes.path_port("/apifoo"), Some(2));
        assert_eq!(
            table(&[("/api", 1)]).path_port("/other"),
            None,
            "no catch-all leaves the path unroutable"
        );
    }

    #[test]
    fn an_unroutable_table_is_refused() {
        assert!(table(&[]).validate("demo").is_err());
        assert!(table(&[("/api", 0)]).validate("demo").is_err());
        assert!(table(&[("api", 1)]).validate("demo").is_err());
        assert!(table(&[("/bad path", 1)]).validate("demo").is_err());
        assert!(table(&[("/bad?query", 1)]).validate("demo").is_err());
        assert!(table(&[("", 1), ("", 2)]).validate("demo").is_err());
    }
}
