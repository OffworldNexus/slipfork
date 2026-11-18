//! `slipfork-world`: the single canonical home for world identity and `.localhost` routing.
//!
//! This crate is intentionally tiny and depends only on [`serde`], [`thiserror`], [`sha2`], and
//! [`hex`]. It contains **zero** OS socket/file-descriptor types so every engine, edge proxy, or
//! test coordinator can depend on the same [`WorldTarget`] / [`LocalHostRoute`] / [`WorldOrigin`]
//! vocabulary without inverting layering:
//!
//! ```text
//! slipfork-world  <--  slipfork-snow (manifest validation & coordinates)
//!        ^
//!        +----------  slipfork-engine (world specs & entry commands)
//!        +----------  slipfork-edge / slipfork-test (M2/M3)
//! ```

#![forbid(unsafe_code)]

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, de};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Canonical validation requirement for a single `.localhost` DNS label.
pub const DNS_LABEL_REASON: &str = "must be 1..=63 lowercase ASCII letters, digits, or '-' (no '.', '_', uppercase, or leading/trailing '-')";

/// Longest valid Tunnel Weaver `service` field (`weaver-proto`'s `SERVICE_NAME_MAX`).
///
/// The combined `<person>-<machine>-<service>` label stays within the 63-byte DNS budget because
/// the relay caps `person` and `machine` at 15 bytes each (`15 + 1 + 15 + 1 + 31 = 63`).
pub const SERVICE_NAME_MAX: usize = 31;

/// Lowercase hex digits of SHA-256 appended to a truncated Tunnel Weaver service label.
const WEAVER_HASH_LEN: usize = 12;

/// Longest verbatim prefix that fits beside the hash: `31 - 1 - 12 = 18` bytes.
const WEAVER_PREFIX_MAX: usize = SERVICE_NAME_MAX - 1 - WEAVER_HASH_LEN;

const WEAVER_SERVICE_NAME_REASON: &str = "must be 1..=31 bytes of lowercase ASCII letters, digits, or single '-' separators (no '.', '_', uppercase, leading/trailing '-', or '--')";

const WEAVER_EMPTY_LABEL_REASON: &str =
    "must contain at least one lowercase ASCII letter or digit after normalization";

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
        "world origin requires the default `.localhost` ingress: route is scoped to snow.yml domains[].id '{domain}'"
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
    /// Fails closed for a route scoped to a `snow.yml` `domains[].id`: named-domain ingresses keep
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

/// Validated, length-capped Tunnel Weaver `service` field (`ADR-0008` §6).
///
/// The only constructor is [`WeaverServiceName::parse`], so every value obeys
/// `^[a-z0-9]+(-[a-z0-9]+)*$` and [`SERVICE_NAME_MAX`] bytes. Deserialization is fail-closed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct WeaverServiceName(String);

impl WeaverServiceName {
    /// Parse and validate a Tunnel Weaver service name.
    pub fn parse(raw: &str) -> Result<Self, WorldError> {
        if is_valid_weaver_service_name(raw) {
            Ok(Self(raw.to_owned()))
        } else {
            Err(WorldError::InvalidWeaverServiceName {
                label: raw.to_owned(),
                reason: WEAVER_SERVICE_NAME_REASON.to_owned(),
            })
        }
    }

    /// Borrow the service name as a plain string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WeaverServiceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for WeaverServiceName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(de::Error::custom)
    }
}

/// Deterministically derive a valid `<= 31`-byte Tunnel Weaver service name from a full world
/// label such as `<project>` or `<project>-<fork>` (`ADR-0008` §6).
///
/// 1. Normalize to lowercase dash-separated `[a-z0-9]` runs: lowercase ASCII is kept, every byte
///    outside `[a-z0-9]` becomes a separator, and separators collapse so the result never has a
///    leading, trailing, or doubled dash. A label with no `[a-z0-9]` byte fails closed. The
///    normalized label **is** the world label, so two raw labels that normalize identically are
///    deliberately the same world.
/// 2. If the normalized label is at most [`SERVICE_NAME_MAX`] bytes **and** is not shaped like a
///    truncated suffix (see step 3), it is used verbatim.
/// 3. Otherwise the label is truncated to an 18-byte prefix (31 - 1 - 12) with trailing dashes
///    stripped, and `<prefix>-<sha256_hex12>` is emitted (or the bare hash if the prefix is empty),
///    where the hash is SHA-256 over the **full normalized** label.
///
/// The function is deterministic and stable across runs and hosts for the same full label. Its
/// input is always a raw full label, never a previous output: re-deriving an already truncated
/// name re-enters the hash path by design (the truncated-suffix guard), so `f(f(x)) != f(x)`.
///
/// Truncated outputs always end in `-<12 lowercase hex>` or are exactly that hash. A short label
/// that already has that shape (for example `foo-0123456789ab`) is routed through the hash path
/// instead of being used verbatim, keeping the verbatim and truncated codomains strictly disjoint.
pub fn derive_weaver_service_name(full_label: &str) -> Result<WeaverServiceName, WorldError> {
    let normalized =
        normalize_weaver_label(full_label).ok_or_else(|| WorldError::InvalidWeaverServiceName {
            label: full_label.to_owned(),
            reason: WEAVER_EMPTY_LABEL_REASON.to_owned(),
        })?;

    if normalized.len() <= SERVICE_NAME_MAX && !has_truncated_suffix_shape(&normalized) {
        return WeaverServiceName::parse(&normalized);
    }

    let digest = Sha256::digest(normalized.as_bytes());
    let hash = hex::encode(digest);
    let prefix = normalized[..normalized.len().min(WEAVER_PREFIX_MAX)].trim_end_matches('-');
    WeaverServiceName::parse(&join_weaver_prefix(prefix, &hash[..WEAVER_HASH_LEN]))
}

/// Normalize a full world label to lowercase dash-separated `[a-z0-9]` runs.
fn normalize_weaver_label(raw: &str) -> Option<String> {
    let mut normalized = String::with_capacity(raw.len());
    let mut separator_pending = false;
    for byte in raw.bytes() {
        let lower = byte.to_ascii_lowercase();
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
            if separator_pending {
                normalized.push('-');
                separator_pending = false;
            }
            normalized.push(lower as char);
        } else if !normalized.is_empty() {
            separator_pending = true;
        }
    }
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

/// `true` when `label` is exactly 12 lowercase hex chars or ends with `-<12 lowercase hex>`.
fn has_truncated_suffix_shape(label: &str) -> bool {
    let tail = label.rsplit_once('-').map_or(label, |(_, tail)| tail);
    tail.len() == WEAVER_HASH_LEN
        && tail
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Join a truncated prefix to a hash, dropping the separator when the prefix is empty.
fn join_weaver_prefix(prefix: &str, hash: &str) -> String {
    if prefix.is_empty() {
        hash.to_owned()
    } else {
        format!("{prefix}-{hash}")
    }
}

/// `true` for a strictly valid Tunnel Weaver service field (`^[a-z0-9]+(-[a-z0-9]+)*$`, `<= 31`).
fn is_valid_weaver_service_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= SERVICE_NAME_MAX
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_weaver_join_omits_the_separator_for_an_empty_prefix() {
        // The empty-prefix arm is spec-literal but unreachable from a normalized label
        // (normalization never yields a leading dash), so it is pinned directly here.
        assert_eq!(join_weaver_prefix("", "0123456789ab"), "0123456789ab");
        assert_eq!(
            join_weaver_prefix("acme-feat", "0123456789ab"),
            "acme-feat-0123456789ab"
        );
    }
}
