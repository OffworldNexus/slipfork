//! Pure `.localhost` world identity: validated project slugs, world targets, routes, origins, and
//! the one DNS-label grammar every manifest and fork name shares.

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
    #[error("invalid world origin url '{url}': {reason}")]
    InvalidOriginUrl { url: String, reason: String },
    #[error(
        "world origin requires the default `.localhost` ingress: route is scoped to Forkfile domain '{domain}'"
    )]
    OriginNotDefaultIngress { domain: String },
    #[error(
        "invalid world origin ingress port 0: `slipforkd` must report the port it actually bound (1..=65535)"
    )]
    OriginPortZero,
    #[error("invalid Tunnel Weaver service name '{label}': {reason}")]
    InvalidWeaverServiceName { label: String, reason: String },
}

/// Validated canonical project slug (one lowercase DNS label).
///
/// Construction is fallible and always goes through [`ProjectSlug::parse`], so no unvalidated
/// string can reach a `.localhost` hostname. Deserialization is fail-closed for the same reason:
/// an invalid `project` value in `Forkfile` is a hard parse error.
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
                        reason: "domain is not declared in the Forkfile".to_owned(),
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

    /// Target `Forkfile` `domain` ids (`None` means the default ingress domain).
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
                let (target, snow_domain) = if let Some(slot) = parse_test_slot(single) {
                    (WorldTarget::TestSlot(slot), None)
                } else if snow_domains.contains(single) {
                    (WorldTarget::Canonical, Some(*single))
                } else {
                    (WorldTarget::UserFork((*single).to_owned()), None)
                };
                Self::new(project.clone(), target, snow_domain, snow_domains).ok()
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
}

/// The single public origin of one world (`ADR-0005` §1, `H-010` #2).
///
/// A world has exactly one public origin: its `.localhost` URL served by `slipforkd`'s HTTP
/// ingress. [`WorldOrigin::local`] is the sole minting path: it takes the world's
/// [`LocalHostRoute`] plus `slipforkd`'s bound HTTP ingress port, and renders `http://<host>` at
/// port 80 and `http://<host>:<port>` otherwise, so `--listen 127.0.0.1:0` deployments hand out a
/// reachable port-qualified origin.
///
/// The payload is private, so a struct literal cannot mint an unvalidated origin: `local` is the
/// only minting path. Deserialization goes through the same validation and fails closed on an
/// empty or non-`http(s)` URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Wire")]
pub struct WorldOrigin {
    url: String,
}

impl WorldOrigin {
    /// Mint the sole origin for a default-ingress `.localhost` route.
    ///
    /// This is the **only** renderer of a world origin URL: port 80 renders port-less
    /// (`http://acme.localhost`) because `slipforkd`'s systemd socket binds `:80`, every other
    /// `1..=65535` renders `:<port>` so an ephemeral `--listen 127.0.0.1:0` deployment hands out a
    /// reachable URL, and port 0 fails closed — the daemon reports the port it actually bound.
    ///
    /// Fails closed for a route scoped to a `Forkfile` `domain` ids: named-domain ingresses keep
    /// their own host and are never the world's single public origin.
    pub fn local(route: &LocalHostRoute, ingress_port: u16) -> Result<Self, WorldError> {
        if let Some(domain) = route.snow_domain() {
            return Err(WorldError::OriginNotDefaultIngress {
                domain: domain.to_owned(),
            });
        }
        if ingress_port == 0 {
            return Err(WorldError::OriginPortZero);
        }
        let port = if ingress_port == 80 {
            String::new()
        } else {
            format!(":{ingress_port}")
        };
        Ok(Self {
            url: format!("http://{}{port}", route.host()),
        })
    }

    /// The rendered origin URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Host authority of the origin URL: the **sole** host accessor (no scheme or port).
    pub fn host(&self) -> &str {
        let url = self.url();
        let authority = url.split_once("://").map_or(url, |(_, rest)| rest);
        let authority = authority.split(['/', '?', '#']).next().unwrap_or(authority);
        authority
            .split_once(':')
            .map_or(authority, |(host, _)| host)
    }
}

/// Wire shape of [`WorldOrigin`]; the field schema is declared exactly once here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    url: String,
}

impl TryFrom<Wire> for WorldOrigin {
    type Error = WorldError;

    fn try_from(wire: Wire) -> Result<Self, WorldError> {
        validate_origin_url(&wire.url)?;
        Ok(Self { url: wire.url })
    }
}

/// Validate an absolute `http(s)` origin URL (empty, schemeless, and host-less URLs fail closed).
fn validate_origin_url(url: &str) -> Result<(), WorldError> {
    let reason = if url.is_empty() {
        Some("must not be empty")
    } else if let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    {
        if rest.is_empty() {
            Some("must include a host after the scheme")
        } else if rest.contains(char::is_whitespace) {
            Some("must not contain whitespace")
        } else {
            None
        }
    } else {
        Some("must be an absolute http:// or https:// URL")
    };
    match reason {
        None => Ok(()),
        Some(reason) => Err(WorldError::InvalidOriginUrl {
            url: url.to_owned(),
            reason: reason.to_owned(),
        }),
    }
}

/// Validate a user/agent fork name against the disjoint `.localhost` namespace.
///
/// Fork names must be single lowercase DNS labels, must not start with the reserved `test-`
/// prefix, and must not collide with any `Forkfile` `domain` ids.
pub fn validate_fork_name(fork: &str, snow_domains: &[&str]) -> Result<(), WorldError> {
    validate_unreserved_dns_label(fork).map_err(|reason| WorldError::InvalidForkName {
        fork: fork.to_owned(),
        reason: reason.to_owned(),
    })?;
    if snow_domains.contains(&fork) {
        return Err(WorldError::InvalidForkName {
            fork: fork.to_owned(),
            reason: format!("collides with Forkfile domain '{fork}'"),
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
///
/// This is the one owner of the `.localhost` label grammar: project slugs, `domains[].id`, fork
/// names, and `slipfork-vcs`'s branch-to-fork-name mapping all ask this predicate instead of
/// re-encoding the alphabet. It says nothing about reserved names — `test-*` and
/// `domains[].id` collisions belong to [`validate_unreserved_dns_label`] and
/// [`validate_fork_name`].
pub fn is_valid_dns_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `true` for labels reserved exclusively for ephemeral `slipfork test` world slots.
fn is_reserved_test_slot_label(label: &str) -> bool {
    label.starts_with("test-")
}

/// Parse a `test-<slot>` label into its numeric slot, rejecting non-numeric or overflowing slots.
fn parse_test_slot(label: &str) -> Option<u32> {
    label.strip_prefix("test-")?.parse().ok()
}

/// Normalize one `Host` header: strip any numeric `:<port>`, lowercase it, and drop one trailing
/// dot, so `Demo.localhost.:8080` and `demo.localhost` name the same host.
///
/// The single owner of the `Host` port-strip grammar: [`LocalHostRoute::parse`] and every other
/// consumer (the `slipfork-edge` ingress included) call this instead of re-splitting on `:`.
/// Returns `None` for an empty host, an IPv6 literal, or a repeated/non-numeric port.
pub fn normalize_host(header: &str) -> Option<String> {
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
