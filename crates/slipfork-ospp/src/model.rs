//! OSPP v1 domain model: uniform ID-only claims, one subscription request, and the
//! decrypted per-component environment.
//!
//! `specs/05-ospp-v1.md` is the sole normative wire specification. This module
//! carries the DI-facing types and never serializes a secret value.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// One resolved environment value held in memory for one subscription.
///
/// `Debug` prints `[REDACTED]` and there is deliberately no `Display`, so a
/// secret can only reach a child process through an explicit [`Secret::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wrap an already-resolved value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the plaintext for an explicit consumer (a service environment).
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

/// Uniform ID-only claim set across every `Forkfile` namespace.
///
/// Any claimed entry tells the server not to resolve it; everything unclaimed is
/// resolved server-side. Only identifiers cross the wire — never a value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OsppClaims {
    /// Claimed `vars::<KEY>` names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vars: Vec<String>,
    /// Claimed `secrets::<KEY>` names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
    /// Claimed `resources::<kind>.<id>` ids by resource kind.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resources: BTreeMap<String, Vec<String>>,
    /// Claimed `domains::<ID>` ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<String>,
    /// Claimed `components::<ID>` ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
}

/// One `subscribe` request built by the composition root.
///
/// Project identity is the verbatim Git remote string and only that string. The
/// `forkfile` payload is the already feature-evaluated `Forkfile` text produced by
/// the canonical `slipfork-forkfile` compiler at the composition root; OSPP never
/// re-parses it, re-scans `$` tokens, or derives service names from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscribeRequest {
    /// Stable per-project subscription id, reused on a claims or config update.
    pub id: String,
    /// Verbatim Git remote string, with zero client-side canonicalization.
    pub remote: String,
    /// Feature-evaluated `Forkfile` payload as produced by `slipfork-forkfile`.
    pub forkfile: String,
    /// ID-only local claims.
    pub claims: OsppClaims,
}

/// Decrypted per-component environment delivered by `env_resolved` or a pushed
/// `env_updated`.
///
/// `{ "encrypted": "<base64>" }` values have already been opened in memory with
/// the subscription's ephemeral HPKE key. `Debug` redacts every value.
#[derive(Clone, PartialEq, Eq)]
pub struct SubscriptionEnv {
    /// Subscription id the server resolved this environment for.
    pub id: String,
    /// Component id → variable name → resolved value.
    pub components: BTreeMap<String, BTreeMap<String, Secret>>,
}

impl SubscriptionEnv {
    /// Build from already-decrypted components.
    pub fn new(
        id: impl Into<String>,
        components: BTreeMap<String, BTreeMap<String, Secret>>,
    ) -> Self {
        Self {
            id: id.into(),
            components,
        }
    }

    /// One component's resolved variables.
    pub fn component(&self, id: &str) -> Option<&BTreeMap<String, Secret>> {
        self.components.get(id)
    }
}

impl fmt::Debug for SubscriptionEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let components: BTreeMap<&str, Vec<&str>> = self
            .components
            .iter()
            .map(|(component, vars)| {
                (
                    component.as_str(),
                    vars.keys().map(String::as_str).collect::<Vec<_>>(),
                )
            })
            .collect();
        f.debug_struct("SubscriptionEnv")
            .field("id", &self.id)
            .field("components", &components)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_serialize_ids_only_and_omit_empty_namespaces() {
        let claims = OsppClaims {
            secrets: vec!["LOCAL_DEV_KEY".to_owned()],
            resources: BTreeMap::from([("pg_databases".to_owned(), vec!["main".to_owned()])]),
            ..OsppClaims::default()
        };
        let json = serde_json::to_value(&claims).expect("claims serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "secrets": ["LOCAL_DEV_KEY"],
                "resources": { "pg_databases": ["main"] },
            })
        );
    }

    #[test]
    fn secrets_are_redacted_in_debug_output() {
        let secret = Secret::new("s3cr3t-value");
        assert_eq!(secret.expose(), "s3cr3t-value");
        assert_eq!(format!("{secret:?}"), "Secret([REDACTED])");
        let env = SubscriptionEnv::new(
            "sub-1",
            BTreeMap::from([(
                "api".to_owned(),
                BTreeMap::from([("SECRET_KEY".to_owned(), secret)]),
            )]),
        );
        assert!(!format!("{env:?}").contains("s3cr3t-value"));
    }
}
