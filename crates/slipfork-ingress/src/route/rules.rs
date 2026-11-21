//! Route-entry normalization, path resolution, and the rewrite rules for every route mode.

use std::collections::BTreeSet;

use slipfork_forkfile::{DomainFilter, Route, RouteKind};

use super::error::{RouteError, TableError};
use super::{IngressRoute, RouteMatch};

/// One validated route: its authority, its matching prefix, and the mode that rewrites it.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub(crate) port: u16,
    pub(crate) authority: String,
    pub(crate) matcher: String,
    pub(crate) kind: RouteKind,
}

/// Expand every declared route into one entry per authority it owns.
///
/// A route without a `domain` filter serves the primary `<world>.localhost` authority **and** every
/// declared app domain (`route /api` answers both `demo.localhost` and `main.demo.localhost`); a
/// `domain <id>` route owns exactly that authority; a `domain <prefix>.*` route owns only its
/// subdomain family. Path and target syntax is already validated by the `Forkfile` compiler.
pub(crate) fn entries_for(
    world: &str,
    declared_domains: &BTreeSet<String>,
    routes: impl IntoIterator<Item = IngressRoute>,
) -> Result<Vec<Entry>, TableError> {
    let mut entries = Vec::new();
    for route in routes {
        let IngressRoute {
            port,
            route: declared,
        } = route;
        if port == 0 {
            return Err(TableError::PortZero {
                world: world.to_owned(),
            });
        }
        let authorities = authorities_for(world, declared_domains, declared.domain.as_ref())?;
        let matcher = matcher_for(&declared);
        for authority in authorities {
            entries.push(Entry {
                port,
                authority,
                matcher: matcher.clone(),
                kind: declared.kind.clone(),
            });
        }
    }
    reject_collisions(world, &entries)?;
    Ok(entries)
}

/// Resolve one authority plus target against a validated entry list.
pub(crate) fn resolve(
    world: &str,
    entries: &[Entry],
    authority: &str,
    target: &str,
) -> Result<RouteMatch, RouteError> {
    validate_target(target)?;
    let (path, _query) = split_query(target);
    let entry = entries
        .iter()
        .filter(|entry| entry.authority == authority)
        .filter(|entry| match &entry.kind {
            RouteKind::Exact(_) => path == entry.matcher,
            _ => prefix_matches(&entry.matcher, path),
        })
        .max_by_key(|entry| entry.matcher.len())
        .ok_or_else(|| RouteError::UnknownPath {
            world: world.to_owned(),
            host: authority_host(world, authority),
            path: target.to_owned(),
        })?;
    Ok(RouteMatch {
        world: world.to_owned(),
        authority: authority.to_owned(),
        port: entry.port,
        target: rewrite(entry, target),
    })
}

/// The authorities one route owns.
fn authorities_for(
    world: &str,
    declared_domains: &BTreeSet<String>,
    filter: Option<&DomainFilter>,
) -> Result<Vec<String>, TableError> {
    match filter {
        None => Ok(std::iter::once(String::new())
            .chain(declared_domains.iter().cloned())
            .collect()),
        Some(DomainFilter::Domain { name, .. }) => {
            if !declared_domains.contains(name) {
                return Err(TableError::UndeclaredDomain {
                    world: world.to_owned(),
                    domain: name.clone(),
                });
            }
            Ok(vec![name.clone()])
        }
        Some(DomainFilter::Prefix(prefix)) => Ok(vec![prefix.clone()]),
    }
}

/// The prefix an entry matches, or the exact path for `exact`.
fn matcher_for(route: &Route) -> String {
    match &route.kind {
        RouteKind::Exact(target) => target.clone(),
        RouteKind::Preserve | RouteKind::StripPrefix | RouteKind::Rewrite(_) => {
            route.path.clone().unwrap_or_default()
        }
    }
}

/// Reject duplicate and overlapping routes inside one world.
fn reject_collisions(world: &str, entries: &[Entry]) -> Result<(), TableError> {
    for (index, entry) in entries.iter().enumerate() {
        for previous in &entries[..index] {
            if previous.authority != entry.authority {
                continue;
            }
            if previous.matcher == entry.matcher {
                return Err(TableError::Duplicate {
                    world: world.to_owned(),
                    route: describe(world, entry),
                });
            }
            if is_catch_all(&previous.matcher) && is_catch_all(&entry.matcher) {
                return Err(TableError::Ambiguous {
                    world: world.to_owned(),
                    left: describe(world, previous),
                    right: describe(world, entry),
                });
            }
        }
    }
    Ok(())
}

/// `true` for the two spellings that cover every origin-form path.
fn is_catch_all(matcher: &str) -> bool {
    matcher.is_empty() || matcher == "/"
}

/// Rewrite one matched request target into the exact upstream target.
fn rewrite(entry: &Entry, target: &str) -> String {
    let (path, query) = split_query(target);
    match &entry.kind {
        RouteKind::Preserve => target.to_owned(),
        RouteKind::StripPrefix => {
            let rest = if entry.matcher.is_empty() || entry.matcher == "/" {
                path
            } else {
                path.strip_prefix(entry.matcher.as_str()).unwrap_or(path)
            };
            format!("{}{query}", with_leading_slash(rest))
        }
        RouteKind::Rewrite(destination) => {
            let rest = path
                .strip_prefix(entry.matcher.as_str())
                .unwrap_or_default();
            format!("{}{query}", join_destination(destination, rest))
        }
        RouteKind::Exact(destination) => format!("{destination}{query}"),
    }
}

/// Join a rewrite destination and a stripped remainder without ever doubling or dropping a slash.
fn join_destination(destination: &str, rest: &str) -> String {
    let destination = trim_destination(destination);
    match (destination.is_empty(), rest.is_empty()) {
        (true, true) => "/".to_owned(),
        (true, false) => rest.to_owned(),
        (false, true) => destination.to_owned(),
        (false, false) => format!("{destination}{rest}"),
    }
}

/// A stripped remainder is either `/` or a `/`-prefixed path.
fn with_leading_slash(rest: &str) -> String {
    if rest.is_empty() {
        "/".to_owned()
    } else if rest.starts_with('/') {
        rest.to_owned()
    } else {
        format!("/{rest}")
    }
}

/// `"/"` joins without doubling, and any other destination loses its trailing slash.
fn trim_destination(destination: &str) -> &str {
    if destination == "/" {
        ""
    } else {
        destination.trim_end_matches('/')
    }
}

/// `true` when `prefix` covers `path` on a URL path-segment boundary.
fn prefix_matches(prefix: &str, path: &str) -> bool {
    if prefix.is_empty() || prefix == "/" {
        return true;
    }
    let Some(rest) = path.strip_prefix(prefix) else {
        return false;
    };
    rest.is_empty() || rest.starts_with('/') || rest.starts_with('?')
}

/// Reject targets that are not safe origin-form paths.
pub(crate) fn validate_target(target: &str) -> Result<(), RouteError> {
    if !target.starts_with('/')
        || target
            .bytes()
            .any(|byte| matches!(byte, b'\\' | b' ' | b'\r' | b'\n'))
    {
        return Err(RouteError::NotOriginForm {
            target: target.to_owned(),
        });
    }
    let (path, _query) = split_query(target);
    if path.split('/').any(|segment| segment == "..") {
        return Err(RouteError::PathTraversal {
            target: target.to_owned(),
        });
    }
    Ok(())
}

/// Split an origin-form target into its path and `?`-prefixed query (both verbatim).
pub(crate) fn split_query(target: &str) -> (&str, &str) {
    match target.find('?') {
        Some(index) => (&target[..index], &target[index..]),
        None => (target, ""),
    }
}

/// Render the authoritative host for error messages.
fn authority_host(world: &str, authority: &str) -> String {
    if authority.is_empty() {
        format!("{world}.localhost")
    } else {
        format!("{authority}.{world}.localhost")
    }
}

/// Render one route for diagnostics.
fn describe(world: &str, entry: &Entry) -> String {
    let authority = if entry.authority.is_empty() {
        format!("{world}.localhost")
    } else {
        format!("{}.{world}.localhost", entry.authority)
    };
    let mode = match &entry.kind {
        RouteKind::Preserve => "preserve".to_owned(),
        RouteKind::StripPrefix => "strip-prefix".to_owned(),
        RouteKind::Rewrite(target) => format!("rewrite {target}"),
        RouteKind::Exact(target) => format!("exact {target}"),
    };
    format!("{authority} {} ({mode})", entry.matcher)
}

/// A world or declared-domain authority label.
///
/// Declared domains such as `static_cdn` are valid `.localhost` authorities even though the
/// underscore is outside the DNS host-letter set, so authority ids are deliberately a touch wider
/// than the `domain <prefix>.*` route filters, which stay strict DNS labels.
pub(crate) fn is_authority_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}
