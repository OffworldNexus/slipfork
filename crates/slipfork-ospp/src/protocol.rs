//! Typed `ospp-v1` capability frames.
//!
//! `docs/decisions/ADR-0008-ospp-v1-websocket-tpmd-resource-strategy-and-world-ux.md` §2 is the
//! single normative specification for all post-`WELCOME` frame tags, field sets, size caps,
//! `dump.begin..dump.end` binary sequencing, and `capability.error` refusal semantics.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use slipfork_snow::Namespace;

use crate::error::{OsppError, PairingRejectReason, RejectReason};
use crate::resources::{PgDumpFormat, PgDumpMetadata, S3BucketCoordinates};

/// `env.bundle`: `vars::` / `secrets::` / `domains::` capability name.
pub const CAPABILITY_ENV_BUNDLE: &str = "env.bundle";

/// `secrets.ensure`: compare-and-set `!!` secret capability name.
pub const CAPABILITY_SECRETS_ENSURE: &str = "secrets.ensure";

/// `resources.describe`: remote S3 / PostgreSQL-seed capability name.
pub const CAPABILITY_RESOURCES_DESCRIBE: &str = "resources.describe";

/// `dump.begin`: start of the streamed PostgreSQL seed dump.
pub const CAPABILITY_DUMP_BEGIN: &str = "dump.begin";

/// `dump.end`: end of the streamed PostgreSQL seed dump.
pub const CAPABILITY_DUMP_END: &str = "dump.end";

/// Largest accepted `mode` value (a `snow.yml` mode name such as `local`, `test`, or `cloud`).
pub const MAX_MODE_LEN: usize = 32;

/// Largest accepted name for a `vars::`/`secrets::` key or resource id.
pub const MAX_KEY_LEN: usize = 256;

/// One post-handshake request frame.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum CapabilityRequest<'a> {
    /// Fetch `vars`, `secrets`, and (in `cloud` mode) `domains`.
    #[serde(rename = "env.bundle")]
    EnvBundle {
        request_id: u32,
        project: &'a str,
        mode: &'a str,
    },
    /// Compare-and-set one `!!` secret.
    #[serde(rename = "secrets.ensure")]
    SecretsEnsure {
        request_id: u32,
        project: &'a str,
        mode: &'a str,
        key: &'a str,
        proposed: &'a str,
    },
    /// Describe the S3 buckets and PostgreSQL seed dumps this session may use.
    #[serde(rename = "resources.describe")]
    ResourcesDescribe {
        request_id: u32,
        project: &'a str,
        mode: &'a str,
    },
    /// Start streaming one digest-pinned PostgreSQL seed dump.
    #[serde(rename = "dump.begin")]
    DumpBegin {
        request_id: u32,
        project: &'a str,
        mode: &'a str,
        db_id: &'a str,
    },
}

/// One server frame after `CHALLENGE` (handshake, pairing, capability, or refusal).
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ServerFrame {
    /// First frame of every connection: a single-use 32-byte nonce.
    Challenge {
        #[serde(rename = "nonce_s")]
        nonce_s: String,
    },
    /// `HELLO` accepted.
    Welcome { version: u16 },
    /// `pair_hello` accepted.
    PairApproved { account: String },
    /// Typed refusal; the connection was closed by the server.
    Reject { reason: WireReject },
    /// `vars` / `secrets` / `domains` snapshot.
    #[serde(rename = "env.bundle")]
    EnvBundle {
        request_id: u32,
        #[serde(default)]
        vars: BTreeMap<String, String>,
        #[serde(default)]
        secrets: BTreeMap<String, String>,
        #[serde(default)]
        domains: BTreeMap<String, String>,
    },
    /// Authoritative value of one compare-and-set secret.
    #[serde(rename = "secrets.ensure")]
    SecretsEnsure {
        request_id: u32,
        key: String,
        value: String,
    },
    /// Remote S3 / PostgreSQL-seed coordinates.
    #[serde(rename = "resources.describe")]
    ResourcesDescribe {
        request_id: u32,
        #[serde(default)]
        s3_buckets: BTreeMap<String, S3BucketCoordinates>,
        #[serde(default)]
        pg_databases: BTreeMap<String, PgDumpMetadata>,
    },
    /// Seed-dump metadata; binary frames follow until `dump.end`.
    #[serde(rename = "dump.begin")]
    DumpBegin {
        request_id: u32,
        db_id: String,
        sha256_compressed: String,
        compressed_bytes: u64,
        format: PgDumpFormat,
    },
    /// End of the seed-dump binary range.
    #[serde(rename = "dump.end")]
    DumpEnd {
        request_id: u32,
        sha256_compressed: String,
        compressed_bytes: u64,
    },
    /// Typed capability refusal.
    #[serde(rename = "capability.error")]
    CapabilityError {
        request_id: u32,
        capability: String,
        kind: String,
        #[serde(default)]
        message: String,
    },
}

impl ServerFrame {
    /// The frame's wire `type`, for a diagnostic that never echoes server data.
    pub(crate) const fn kind(&self) -> &'static str {
        match self {
            Self::Challenge { .. } => "challenge",
            Self::Welcome { .. } => "welcome",
            Self::PairApproved { .. } => "pair_approved",
            Self::Reject { .. } => "reject",
            Self::EnvBundle { .. } => CAPABILITY_ENV_BUNDLE,
            Self::SecretsEnsure { .. } => CAPABILITY_SECRETS_ENSURE,
            Self::ResourcesDescribe { .. } => CAPABILITY_RESOURCES_DESCRIBE,
            Self::DumpBegin { .. } => CAPABILITY_DUMP_BEGIN,
            Self::DumpEnd { .. } => CAPABILITY_DUMP_END,
            Self::CapabilityError { .. } => "capability.error",
        }
    }

    /// Interpret this frame as the refusal or mismatch of someone else's request.
    ///
    /// The server's free-text `message` is deliberately withheld: a refusal must never become a
    /// channel that echoes a secret value back into a Slipfork diagnostic.
    pub(crate) fn into_refusal(
        self,
        capability: &'static str,
        scope: &str,
        request_id: u32,
    ) -> OsppError {
        match self {
            Self::CapabilityError {
                request_id: seen,
                capability: refused,
                kind,
                message,
            } => {
                let _withheld = message;
                if seen != request_id || refused != capability {
                    return capability_error(
                        capability,
                        scope,
                        "server refused a different request than the one sent",
                    );
                }
                capability_rejected(capability, scope, kind)
            }
            other => OsppError::Capability {
                capability,
                scope: scope.to_owned(),
                message: format!(
                    "expected a `{capability}` response for request {request_id}, got `{}`",
                    other.kind()
                ),
            },
        }
    }
}

impl fmt::Debug for CapabilityRequest<'_> {
    /// Redacts the `!!` proposal: a request is never logged with a secret value.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EnvBundle {
                request_id,
                project,
                mode,
            } => f
                .debug_struct("EnvBundle")
                .field("request_id", request_id)
                .field("project", project)
                .field("mode", mode)
                .finish(),
            Self::SecretsEnsure {
                request_id,
                project,
                mode,
                key,
                proposed: _,
            } => f
                .debug_struct("SecretsEnsure")
                .field("request_id", request_id)
                .field("project", project)
                .field("mode", mode)
                .field("key", key)
                .field("proposed", &"[REDACTED]")
                .finish(),
            Self::ResourcesDescribe {
                request_id,
                project,
                mode,
            } => f
                .debug_struct("ResourcesDescribe")
                .field("request_id", request_id)
                .field("project", project)
                .field("mode", mode)
                .finish(),
            Self::DumpBegin {
                request_id,
                project,
                mode,
                db_id,
            } => f
                .debug_struct("DumpBegin")
                .field("request_id", request_id)
                .field("project", project)
                .field("mode", mode)
                .field("db_id", db_id)
                .finish(),
        }
    }
}

impl fmt::Debug for ServerFrame {
    /// Redacts every secret-bearing field (`env.bundle.secrets`, `secrets.ensure.value`, and the
    /// server's free-text refusal message), so `{frame:?}` can never leak a value.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Challenge { nonce_s } => f
                .debug_struct("Challenge")
                .field("nonce_s", nonce_s)
                .finish(),
            Self::Welcome { version } => {
                f.debug_struct("Welcome").field("version", version).finish()
            }
            Self::PairApproved { account } => f
                .debug_struct("PairApproved")
                .field("account", account)
                .finish(),
            Self::Reject { reason } => f.debug_struct("Reject").field("reason", reason).finish(),
            Self::EnvBundle {
                request_id,
                vars,
                secrets,
                domains,
            } => f
                .debug_struct("EnvBundle")
                .field("request_id", request_id)
                .field("vars", vars)
                .field("secrets", &RedactedMap(secrets))
                .field("domains", domains)
                .finish(),
            Self::SecretsEnsure {
                request_id,
                key,
                value: _,
            } => f
                .debug_struct("SecretsEnsure")
                .field("request_id", request_id)
                .field("key", key)
                .field("value", &"[REDACTED]")
                .finish(),
            Self::ResourcesDescribe {
                request_id,
                s3_buckets,
                pg_databases,
            } => f
                .debug_struct("ResourcesDescribe")
                .field("request_id", request_id)
                .field("s3_buckets", s3_buckets)
                .field("pg_databases", pg_databases)
                .finish(),
            Self::DumpBegin {
                request_id,
                db_id,
                sha256_compressed,
                compressed_bytes,
                format,
            } => f
                .debug_struct("DumpBegin")
                .field("request_id", request_id)
                .field("db_id", db_id)
                .field("sha256_compressed", sha256_compressed)
                .field("compressed_bytes", compressed_bytes)
                .field("format", format)
                .finish(),
            Self::DumpEnd {
                request_id,
                sha256_compressed,
                compressed_bytes,
            } => f
                .debug_struct("DumpEnd")
                .field("request_id", request_id)
                .field("sha256_compressed", sha256_compressed)
                .field("compressed_bytes", compressed_bytes)
                .finish(),
            Self::CapabilityError {
                request_id,
                capability,
                kind,
                message: _,
            } => f
                .debug_struct("CapabilityError")
                .field("request_id", request_id)
                .field("capability", capability)
                .field("kind", kind)
                .field("message", &"[WITHHELD]")
                .finish(),
        }
    }
}

/// The union of `REJECT` reason kinds; each handshake phase accepts only its own subset.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum WireReject {
    UnknownKey,
    BadSignature,
    UnsupportedVersion { min: u16, max: u16 },
    UnknownHandle,
    ExpiredHandle,
    AlreadyRedeemed,
}

impl WireReject {
    /// Interpret this refusal as a `HELLO` refusal, or `None` when it is pairing-only.
    pub(crate) fn into_connect(self) -> Option<RejectReason> {
        match self {
            Self::UnknownKey => Some(RejectReason::UnknownKey),
            Self::BadSignature => Some(RejectReason::BadSignature),
            Self::UnsupportedVersion { min, max } => {
                Some(RejectReason::UnsupportedVersion { min, max })
            }
            Self::UnknownHandle | Self::ExpiredHandle | Self::AlreadyRedeemed => None,
        }
    }

    /// Interpret this refusal as a `pair_hello` refusal, or `None` when it is connect-only.
    pub(crate) fn into_pairing(self) -> Option<PairingRejectReason> {
        match self {
            Self::UnknownHandle => Some(PairingRejectReason::UnknownHandle),
            Self::ExpiredHandle => Some(PairingRejectReason::ExpiredHandle),
            Self::AlreadyRedeemed => Some(PairingRejectReason::AlreadyRedeemed),
            Self::BadSignature => Some(PairingRejectReason::BadSignature),
            Self::UnsupportedVersion { min, max } => {
                Some(PairingRejectReason::UnsupportedVersion { min, max })
            }
            Self::UnknownKey => None,
        }
    }
}

/// `vars` / `secrets` / `domains` snapshot returned by `env.bundle`.
///
/// `Debug` prints secret key names with `[REDACTED]` values; no rendering of this type exposes a
/// secret value.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct EnvBundle {
    vars: BTreeMap<String, String>,
    secrets: BTreeMap<String, String>,
    domains: BTreeMap<String, String>,
}

impl EnvBundle {
    /// Build a bundle from the server's three maps.
    pub fn new(
        vars: BTreeMap<String, String>,
        secrets: BTreeMap<String, String>,
        domains: BTreeMap<String, String>,
    ) -> Self {
        Self {
            vars,
            secrets,
            domains,
        }
    }

    /// Look one key up in the requested namespace.
    pub fn get(&self, namespace: Namespace, key: &str) -> Option<&str> {
        self.map(namespace).get(key).map(String::as_str)
    }

    /// All `vars::` values.
    pub fn vars(&self) -> &BTreeMap<String, String> {
        &self.vars
    }

    /// All `secrets::` values.
    pub fn secrets(&self) -> &BTreeMap<String, String> {
        &self.secrets
    }

    /// All `domains::` values (`cloud` mode only).
    pub fn domains(&self) -> &BTreeMap<String, String> {
        &self.domains
    }

    fn map(&self, namespace: Namespace) -> &BTreeMap<String, String> {
        match namespace {
            Namespace::Vars => &self.vars,
            Namespace::Secrets => &self.secrets,
            Namespace::Domains => &self.domains,
        }
    }
}

impl fmt::Debug for EnvBundle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvBundle")
            .field("vars", &self.vars)
            .field("secrets", &RedactedMap(&self.secrets))
            .field("domains", &self.domains)
            .finish()
    }
}

/// Renders a secret map without its values.
struct RedactedMap<'a>(&'a BTreeMap<String, String>);

impl fmt::Debug for RedactedMap<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = f.debug_map();
        for key in self.0.keys() {
            map.entry(key, &"[REDACTED]");
        }
        map.finish()
    }
}

/// Validate a `snow.yml` mode name before it reaches the wire.
pub(crate) fn validate_mode(mode: &str) -> Result<(), String> {
    if mode.is_empty() {
        return Err("mode must not be empty".to_owned());
    }
    if mode.len() > MAX_MODE_LEN {
        return Err(format!(
            "mode must be at most {MAX_MODE_LEN} bytes, got {}",
            mode.len()
        ));
    }
    if !mode.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
    }) {
        return Err("mode must be lowercase ASCII letters, digits, `-`, or `_`".to_owned());
    }
    Ok(())
}

/// Validate a `vars::`/`secrets::` key or resource id before it reaches the wire.
pub(crate) fn validate_key(field: &'static str, key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if key.len() > MAX_KEY_LEN {
        return Err(format!(
            "{field} must be at most {MAX_KEY_LEN} bytes, got {}",
            key.len()
        ));
    }
    if !key
        .bytes()
        .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
    {
        return Err(format!(
            "{field} must be printable ASCII without quotes or backslashes"
        ));
    }
    Ok(())
}

/// A capability refusal produced by the client before or after the wire exchange.
pub(crate) fn capability_error(
    capability: &'static str,
    scope: &str,
    message: impl Into<String>,
) -> OsppError {
    OsppError::Capability {
        capability,
        scope: scope.to_owned(),
        message: message.into(),
    }
}

/// A server refusal carrying the capability name and scope but never a value.
pub(crate) fn capability_rejected(
    capability: &'static str,
    scope: &str,
    kind: String,
) -> OsppError {
    OsppError::CapabilityRejected {
        capability,
        scope: scope.to_owned(),
        kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KNOWN_SECRET: &str = "sk-live-known-secret-value";

    #[test]
    fn frames_redact_secret_values_in_debug() {
        let request = CapabilityRequest::SecretsEnsure {
            request_id: 7,
            project: "acme",
            mode: "local",
            key: "STRIPE_KEY",
            proposed: KNOWN_SECRET,
        };
        let rendered = format!("{request:?}");
        assert!(!rendered.contains(KNOWN_SECRET), "{rendered}");
        assert!(rendered.contains("[REDACTED]"), "{rendered}");

        let bundle: ServerFrame = serde_json::from_str(&format!(
            r#"{{"type":"env.bundle","request_id":1,"vars":{{}},"secrets":{{"STRIPE_KEY":"{KNOWN_SECRET}"}},"domains":{{}}}}"#
        ))
        .expect("env.bundle frame");
        let rendered = format!("{bundle:?}");
        assert!(!rendered.contains(KNOWN_SECRET), "{rendered}");
        assert!(rendered.contains("[REDACTED]"), "{rendered}");

        let ensured: ServerFrame = serde_json::from_str(&format!(
            r#"{{"type":"secrets.ensure","request_id":2,"key":"STRIPE_KEY","value":"{KNOWN_SECRET}"}}"#
        ))
        .expect("secrets.ensure frame");
        let rendered = format!("{ensured:?}");
        assert!(!rendered.contains(KNOWN_SECRET), "{rendered}");
        assert!(rendered.contains("[REDACTED]"), "{rendered}");

        let refused: ServerFrame = serde_json::from_str(&format!(
            r#"{{"type":"capability.error","request_id":3,"capability":"secrets.ensure","kind":"not_authenticated","message":"{KNOWN_SECRET}"}}"#
        ))
        .expect("capability.error frame");
        let rendered = format!("{refused:?}");
        assert!(!rendered.contains(KNOWN_SECRET), "{rendered}");
        assert!(rendered.contains("[WITHHELD]"), "{rendered}");
    }
}
