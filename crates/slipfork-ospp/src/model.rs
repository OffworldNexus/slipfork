//! OSPP v1 domain model: uniform ID-only claims, one subscription request, and the
//! decrypted per-component environment.
//!
//! `docs/design/ospp.md` is the sole normative wire specification; this module
//! carries the DI-facing types and never serializes values.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use slipfork_forkfile::{EvaluatedFork, TaintedString};

/// Uniform ID-only claim set across every `snow.yml` namespace.
///
/// Any claimed entry tells the server not to resolve it and to leave its
/// `${{ ... }}` expression untouched; everything unclaimed is resolved by the
/// server. Only identifiers cross the wire — never a variable or secret value.
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

impl OsppClaims {
    /// Whether this claim set owns the `${{ namespace::path }}` reference locally.
    ///
    /// Only identifiers are consulted — never a value — so this is safe to call for a template
    /// the daemon has not resolved yet.
    pub fn owns(&self, namespace: &str, path: &[String]) -> bool {
        match namespace {
            "vars" => path.len() == 1 && self.vars.iter().any(|key| key == &path[0]),
            "secrets" => path.len() == 1 && self.secrets.iter().any(|key| key == &path[0]),
            "domains" => path.len() == 1 && self.domains.iter().any(|id| id == &path[0]),
            "components" => path.len() == 1 && self.components.iter().any(|id| id == &path[0]),
            "resources" => {
                path.len() >= 2
                    && self
                        .resources
                        .get(&path[0])
                        .is_some_and(|ids| ids.iter().any(|id| id == &path[1]))
            }
            _ => false,
        }
    }
}

/// One `subscribe` request: project identity is the verbatim Git remote string
/// and only that string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscribeRequest {
    /// Stable per-project subscription id, reused on a claims/config update.
    pub id: String,
    /// Verbatim Git remote string, with zero client-side canonicalization.
    pub remote: String,
    /// The feature-evaluated manifest `slipforkd` subscribes to.
    pub forkfile: EvaluatedFork,
    /// ID-only local claims.
    pub claims: OsppClaims,
}

/// Decrypted per-component environment delivered by `env_resolved` or a pushed
/// `env_updated`.
///
/// `{ "encrypted": "<base64>" }` values have already been opened in memory with
/// the subscription's ephemeral HPKE key, leaving server-resolved plaintext and
/// claimed `${{ ... }}` templates; `Debug` redacts every value.
#[derive(Clone, PartialEq, Eq)]
pub struct SubscriptionEnv {
    /// Subscription id the server resolved this environment for.
    pub id: String,
    /// Component id → variable name → resolved value.
    pub components: BTreeMap<String, BTreeMap<String, TaintedString>>,
}

impl SubscriptionEnv {
    /// Build from already-decrypted components.
    pub fn new(
        id: impl Into<String>,
        components: BTreeMap<String, BTreeMap<String, TaintedString>>,
    ) -> Self {
        Self {
            id: id.into(),
            components,
        }
    }

    /// One component's resolved variables.
    pub fn component(&self, id: &str) -> Option<&BTreeMap<String, TaintedString>> {
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
}
