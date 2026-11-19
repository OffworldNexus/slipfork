//! Claim-aware environment resolution over the canonical `Forkfile` evaluator.
//!
//! The reference server owns **no** expression evaluation: it supplies authoritative values through
//! the [`SecretProvider`] port and delegates claim selection, fallback evaluation, taint
//! propagation, and verbatim passthrough to `slipfork-forkfile`.

use std::collections::BTreeMap;

use slipfork_forkfile::{
    EvaluatedFork, Namespace, OsCsprngEntropy, ProviderError, Resolvers, SecretProvider,
    TaintedString,
};
use slipfork_ospp::{OsppClaims, codes};

use super::ReferenceServerConfig;

/// A read-only `SecretProvider` over the reference server's authoritative values.
struct ConfigVault<'a> {
    config: &'a ReferenceServerConfig,
}

impl SecretProvider for ConfigVault<'_> {
    fn get(&self, namespace: Namespace, key: &str) -> Result<Option<String>, ProviderError> {
        let map = match namespace {
            Namespace::Secrets => &self.config.secrets,
            Namespace::Vars => &self.config.vars,
            Namespace::Domains => &self.config.domains,
        };
        Ok(map.get(key).cloned())
    }

    fn ensure_secret(&self, _key: &str, _proposed: &str) -> Result<String, ProviderError> {
        // The reference server has no authority to persist generated secrets; a manifest that
        // needs write-once generation fails closed instead of inventing a local value.
        Err(ProviderError::Io {
            path: "<reference-server>".to_owned(),
            message: "write-once generators are not available in the reference server".to_owned(),
        })
    }
}

/// Resolve every unclaimed service environment from the already feature-evaluated manifest.
///
/// A claimed expression comes back as canonical `Forkfile` source (never evaluated here); every
/// other expression goes through `resolve_env_with_claims` and the reference server's values.
///
/// Resource coordinates come from the manifest projection (`EvaluatedFork::world_facts`), so the
/// reference server never invents an alternative world view.
pub(super) fn resolve_components(
    config: &ReferenceServerConfig,
    claims: &OsppClaims,
    forkfile: &EvaluatedFork,
) -> Result<BTreeMap<String, BTreeMap<String, TaintedString>>, (String, String)> {
    let vault = ConfigVault { config };
    let entropy = OsCsprngEntropy;
    let resolvers = Resolvers::new(&vault, &entropy);
    let owns = |namespace: &str, path: &[String]| claims.owns(namespace, path);
    let is_claimed = |expr: &slipfork_forkfile::Expr| forkfile.expr_is_claimed(expr, &owns);
    let mut components = BTreeMap::new();
    for service in &forkfile.services {
        if claims.components.iter().any(|id| id == &service.id) {
            // A locally owned component is neither resolved nor returned.
            continue;
        }
        let env = forkfile
            .resolve_env_with_claims(&service.id, &resolvers, &is_claimed)
            .map_err(|diagnostics| {
                (
                    codes::UNRESOLVED.to_owned(),
                    diagnostics
                        .into_iter()
                        .map(|diagnostic| diagnostic.message)
                        .collect::<Vec<_>>()
                        .join("; "),
                )
            })?;
        components.insert(service.id.clone(), env);
    }
    Ok(components)
}

#[cfg(test)]
mod tests {
    use super::*;
    use slipfork_forkfile::{FeatureSet, parse_forkfile};

    fn evaluate(source: &str) -> EvaluatedFork {
        parse_forkfile(source)
            .expect("valid Forkfile")
            .evaluate(&FeatureSet::default())
            .expect("evaluates")
    }

    #[test]
    fn claimed_expressions_survive_unresolved() {
        let config = ReferenceServerConfig {
            vars: BTreeMap::from([("DEBUG".to_owned(), "server".to_owned())]),
            ..ReferenceServerConfig::default()
        };
        let claims = OsppClaims {
            vars: vec!["DEBUG".to_owned()],
            ..OsppClaims::default()
        };
        let forkfile = evaluate(
            r#"
env app {
    DEBUG     = var(DEBUG) ?? "x"
    LOG_LEVEL = "info"
}

service api ./api {
    use env app
    run /bin/true
}
"#,
        );
        let components = resolve_components(&config, &claims, &forkfile).expect("resolves");
        assert_eq!(components["api"]["DEBUG"].expose(), "var(DEBUG) ?? \"x\"");
        assert!(!components["api"]["DEBUG"].is_tainted());
        assert_eq!(components["api"]["LOG_LEVEL"].expose(), "info");
    }

    #[test]
    fn canonical_taint_rules_drive_secrets_and_fallbacks() {
        let config = ReferenceServerConfig {
            secrets: BTreeMap::from([("PRESENT".to_owned(), "s3cr3t".to_owned())]),
            ..ReferenceServerConfig::default()
        };
        let claims = OsppClaims {
            secrets: vec!["LOCAL".to_owned()],
            ..OsppClaims::default()
        };
        let forkfile = evaluate(
            r#"
service api ./api {
    env.PRESENT = secret(PRESENT)
    env.FALLBACK = secret(MISSING) ?? "literal"
    env.CLAIMED = secret(LOCAL) ?? "fallback"
    run /bin/true
}
"#,
        );
        let components = resolve_components(&config, &claims, &forkfile).expect("resolves");
        let api = &components["api"];
        assert!(api["PRESENT"].is_tainted(), "a resolved secret is tainted");
        assert_eq!(api["PRESENT"].expose(), "s3cr3t");
        assert!(
            !api["FALLBACK"].is_tainted(),
            "the canonical rule taints only a secret that actually resolved"
        );
        assert_eq!(api["FALLBACK"].expose(), "literal");
        assert_eq!(api["CLAIMED"].expose(), "secret(LOCAL) ?? \"fallback\"");
        assert!(
            !api["CLAIMED"].is_tainted(),
            "a claimed expression stays literal"
        );
    }
}
