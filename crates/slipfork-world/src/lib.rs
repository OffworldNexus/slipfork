//! `slipfork-world`: the single canonical home for world identity and `.localhost` routing.
//!
//! This crate is intentionally tiny and depends only on [`serde`] and [`thiserror`]. It contains
//! **zero** OS socket/file-descriptor types so every engine, edge proxy, or test coordinator can
//! depend on the same [`WorldTarget`] / [`LocalHostRoute`] vocabulary without inverting layering:
//!
//! ```text
//! slipfork-world  <--  slipfork-snow (manifest validation & coordinates)
//!        ^
//!        +----------  slipfork-edge / slipfork-test (M2/M3)
//!        +----------  slipfork-machine adapters (M2)
//! ```
//!
//! Tunnel sharing is expressed purely as [`TunnelRegistrar`] + opaque [`TunnelLease`]; no
//! `UnixStream`, raw FD, JSON wire payload, or socket path lives here. The concrete
//! `WeavedRegistrar` wire adapter is gated on `tunnel-weaver` publishing `weaved`'s real IPC
//! specification.

#![forbid(unsafe_code)]

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, de};
use thiserror::Error;

/// Canonical validation requirement for a single `.localhost` DNS label.
pub const DNS_LABEL_REASON: &str = "must be 1..=63 lowercase ASCII letters, digits, or '-' (no '.', '_', uppercase, or leading/trailing '-')";

const RESERVED_TEST_SLOT_REASON: &str =
    "prefix 'test-' is reserved for ephemeral 'slipfork test' world slots";

/// Errors produced by pure world identity validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorldError {
    #[error("invalid fork name '{fork}': {reason}")]
    InvalidForkName { fork: String, reason: String },
    #[error("invalid project slug '{slug}': {DNS_LABEL_REASON}")]
    InvalidProjectSlug { slug: String },
    #[error("invalid snow domain '{domain}': {reason}")]
    InvalidSnowDomain { domain: String, reason: String },
}

/// Validated canonical project slug (one lowercase DNS label).
///
/// Construction is fallible and always goes through [`ProjectSlug::parse`], so no unvalidated
/// string can reach a `.localhost` hostname. Deserialization is fail-closed for the same reason:
/// an invalid `project` value in `slipfork.toml` is a hard parse error.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ProjectSlug(String);

impl ProjectSlug {
    /// Parse and validate a canonical project slug.
    pub fn parse(raw: &str) -> Result<Self, WorldError> {
        if is_valid_dns_label(raw) {
            Ok(Self(raw.to_owned()))
        } else {
            Err(WorldError::InvalidProjectSlug {
                slug: raw.to_owned(),
            })
        }
    }

    /// Borrow the slug as a plain string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProjectSlug {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(de::Error::custom)
    }
}

/// Typed identity of one routable world.
///
/// `TestSlot` is intentionally distinct from `UserFork("test-3")`: only the test coordinator can
/// mint slots, and `test-*` labels are reserved at manifest validation so a user fork can never
/// shadow a slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldTarget {
    /// Canonical root world (`http://<project>.localhost`).
    Canonical,
    /// User/agent workspace fork (`http://<fork>.<project>.localhost`).
    UserFork(String),
    /// Ephemeral `slipfork test` pool slot (`http://test-<slot>.<project>.localhost`).
    TestSlot(u32),
}

impl WorldTarget {
    /// The single hostname label owned by this target, if any (`Canonical` owns no label).
    fn host_label(&self) -> Option<String> {
        match self {
            Self::Canonical => None,
            Self::UserFork(fork) => Some(fork.clone()),
            Self::TestSlot(slot) => Some(format!("test-{slot}")),
        }
    }

    /// Fail closed unless this target is routable against the manifest's `domains[].id` set.
    pub fn validate(&self, snow_domains: &[&str]) -> Result<(), WorldError> {
        match self {
            Self::Canonical | Self::TestSlot(_) => Ok(()),
            Self::UserFork(fork) => validate_fork_name(fork, snow_domains),
        }
    }
}

/// Disambiguated, validated target of `http://(<snow-domain>.)?(<fork>.)?<project>.localhost`.
///
/// Fields are private so every `LocalHostRoute` is guaranteed routable by construction via
/// [`LocalHostRoute::new`] or [`LocalHostRoute::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalHostRoute {
    project: ProjectSlug,
    target: WorldTarget,
    snow_domain: Option<String>,
}

impl LocalHostRoute {
    /// Construct a validated `.localhost` route against the manifest's declared `domains[].id` set.
    pub fn new(
        project: ProjectSlug,
        target: WorldTarget,
        snow_domain: Option<&str>,
        snow_domains: &[&str],
    ) -> Result<Self, WorldError> {
        target.validate(snow_domains)?;
        let snow_domain = match snow_domain {
            None => None,
            Some(domain) => {
                validate_unreserved_dns_label(domain).map_err(|reason| {
                    WorldError::InvalidSnowDomain {
                        domain: domain.to_owned(),
                        reason: reason.to_owned(),
                    }
                })?;
                if !snow_domains.contains(&domain) {
                    return Err(WorldError::InvalidSnowDomain {
                        domain: domain.to_owned(),
                        reason: "domain is not declared in snow.yml domains[].id".to_owned(),
                    });
                }
                Some(domain.to_owned())
            }
        };
        Ok(Self {
            project,
            target,
            snow_domain,
        })
    }

    /// Canonical project slug (a single lowercase DNS label).
    pub fn project(&self) -> &ProjectSlug {
        &self.project
    }

    /// Target world identity.
    pub fn target(&self) -> &WorldTarget {
        &self.target
    }

    /// Target `snow.yml` `domains[].id` (`None` means the default ingress domain).
    pub fn snow_domain(&self) -> Option<&str> {
        self.snow_domain.as_deref()
    }

    /// Parse a port-80 `Host` header under `(<snow-domain>.)?(<fork>.)?<project>.localhost`.
    ///
    /// Returns `None` for hosts outside the project's `.localhost` namespace **and** for
    /// malformed/ambiguous hosts: IPv6 literals (`[::1]`), non-numeric or repeated ports,
    /// labels that are not lowercase DNS labels, or any target/domain rejected by
    /// [`LocalHostRoute::new`] (reserved `test-*` prefixes such as `test-feat`, or a fork that
    /// collides with a declared `domains[].id`, for example `admin.admin`).
    pub fn parse(project: &ProjectSlug, snow_domains: &[&str], host_header: &str) -> Option<Self> {
        let host = normalize_host(host_header)?;
        let root_host = format!("{project}.localhost");

        if host == root_host {
            return Self::new(project.clone(), WorldTarget::Canonical, None, snow_domains).ok();
        }

        let prefix = host.strip_suffix(&format!(".{root_host}"))?;
        let labels: Vec<&str> = prefix.split('.').collect();
        match labels.as_slice() {
            [single] => {
                if let Some(slot) = parse_test_slot(single) {
                    Self::new(
                        project.clone(),
                        WorldTarget::TestSlot(slot),
                        None,
                        snow_domains,
                    )
                    .ok()
                } else if snow_domains.contains(single) {
                    Self::new(
                        project.clone(),
                        WorldTarget::Canonical,
                        Some(single),
                        snow_domains,
                    )
                    .ok()
                } else {
                    Self::new(
                        project.clone(),
                        WorldTarget::UserFork((*single).to_owned()),
                        None,
                        snow_domains,
                    )
                    .ok()
                }
            }
            [domain, label] => {
                let target = parse_test_slot(label).map_or_else(
                    || WorldTarget::UserFork((*label).to_owned()),
                    WorldTarget::TestSlot,
                );
                Self::new(project.clone(), target, Some(domain), snow_domains).ok()
            }
            _ => None,
        }
    }

    /// Render the canonical hostname (no scheme, no port).
    pub fn host(&self) -> String {
        let mut labels = Vec::with_capacity(3);
        if let Some(domain) = &self.snow_domain {
            labels.push(domain.clone());
        }
        if let Some(label) = self.target.host_label() {
            labels.push(label);
        }
        labels.push(self.project.as_str().to_owned());
        format!("{}.localhost", labels.join("."))
    }

    /// Render the canonical loopback URL `http://<host>`.
    pub fn url(&self) -> String {
        format!("http://{}", self.host())
    }
}

/// Validate a user/agent fork name against the disjoint `.localhost` namespace.
///
/// Fork names must be single lowercase DNS labels, must not start with the reserved `test-`
/// prefix, and must not collide with any `snow.yml` `domains[].id`.
pub fn validate_fork_name(fork: &str, snow_domains: &[&str]) -> Result<(), WorldError> {
    validate_unreserved_dns_label(fork).map_err(|reason| WorldError::InvalidForkName {
        fork: fork.to_owned(),
        reason: reason.to_owned(),
    })?;
    if snow_domains.contains(&fork) {
        return Err(WorldError::InvalidForkName {
            fork: fork.to_owned(),
            reason: format!("collides with snow.yml domains[].id '{fork}'"),
        });
    }
    Ok(())
}

/// Validate a non-reserved `.localhost` subdomain label (`domains[].id` or user fork):
/// must satisfy [`DNS_LABEL_REASON`] and must not claim the reserved `test-*` prefix.
pub fn validate_unreserved_dns_label(label: &str) -> Result<(), &'static str> {
    if !is_valid_dns_label(label) {
        return Err(DNS_LABEL_REASON);
    }
    if is_reserved_test_slot_label(label) {
        return Err(RESERVED_TEST_SLOT_REASON);
    }
    Ok(())
}

/// `true` when `label` is a single lowercase DNS label (`[a-z0-9-]`, 1..=63, no edge `-`).
fn is_valid_dns_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `true` for labels reserved exclusively for ephemeral `slipfork test` world slots.
pub fn is_reserved_test_slot_label(label: &str) -> bool {
    label.starts_with("test-")
}

/// Parse a `test-<slot>` label into its numeric slot, rejecting non-numeric or overflowing slots.
fn parse_test_slot(label: &str) -> Option<u32> {
    label.strip_prefix("test-")?.parse().ok()
}

fn normalize_host(header: &str) -> Option<String> {
    let trimmed = header.trim();
    if trimmed.is_empty() || trimmed.contains('[') || trimmed.contains(']') {
        return None;
    }
    let host = match trimmed.split(':').collect::<Vec<_>>().as_slice() {
        [host] => *host,
        [host, port] if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => *host,
        _ => return None,
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() { None } else { Some(host) }
}

/// One tunnel registration request: a resolved `.localhost` route plus the loopback port.
#[derive(Debug, Clone, Copy)]
pub struct TunnelRequest<'a> {
    pub route: &'a LocalHostRoute,
    pub local_port: u16,
}

/// Opaque RAII tunnel lease.
///
/// Adapters own their transport (a future `WeavedRegistrar` keeps its `UnixStream` private); the
/// domain trait only exposes the lease identity. Dropping the lease MUST revoke the tunnel.
pub trait TunnelLease: Send + Sync {
    /// Public HTTPS URL handed out by the tunnel daemon.
    fn public_url(&self) -> &str;
    /// Opaque daemon-issued lease identifier.
    fn lease_id(&self) -> &str;
}

/// Errors produced by a [`TunnelRegistrar`] adapter.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TunnelError {
    #[error("tunnel daemon unavailable: {0}")]
    DaemonUnavailable(String),
    #[error("tunnel registration failed: {0}")]
    RegistrationFailed(String),
}

/// Port for acquiring a public tunnel lease for a resolved world route.
///
/// There is no no-op fallback registrar: if no daemon is reachable, `register` fails closed with
/// [`TunnelError::DaemonUnavailable`].
pub trait TunnelRegistrar: Send + Sync {
    fn register(&self, request: &TunnelRequest<'_>) -> Result<Box<dyn TunnelLease>, TunnelError>;
}

pub mod conformance;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_host_strips_one_numeric_port_and_trailing_dot() {
        assert_eq!(
            normalize_host("ACME.localhost.:80").as_deref(),
            Some("acme.localhost")
        );
        assert_eq!(normalize_host("[::1]:80"), None);
        assert_eq!(normalize_host("acme.localhost:not-a-port"), None);
        assert_eq!(normalize_host("acme.localhost:80:81"), None);
    }

    #[test]
    fn test_slot_parser_rejects_non_numeric_and_overflow() {
        assert_eq!(parse_test_slot("test-0"), Some(0));
        assert_eq!(parse_test_slot("test-42"), Some(42));
        assert_eq!(parse_test_slot("test-"), None);
        assert_eq!(parse_test_slot("test-feat"), None);
        assert_eq!(parse_test_slot("test-99999999999"), None);
    }
}
