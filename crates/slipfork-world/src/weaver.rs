//! The validated, length-capped Tunnel Weaver `service` label (`ADR-0008` §6).

#![forbid(unsafe_code)]

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, de};
use sha2::{Digest, Sha256};

use crate::WorldError;

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

#[cfg(test)]
mod tests {
    use super::join_weaver_prefix;

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
