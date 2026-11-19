//! Claimed (`OSPP`) evaluation: a local world resolves only the `${{ ... }}` expressions it owns
//! and forwards every other expression verbatim to the server.

use super::{Policy, render};
use crate::model::{ResolvedEnvMap, ResourceCoordinates, SnowError, SnowFile};
use crate::provider::{EntropySource, SecretProvider, SecretValue};

/// Resolves the expressions one local world claims, over one injected provider/coordinate set.
///
/// A claimed expression is evaluated exactly like the exhaustive resolver (`??` fallback, write-once
/// `!!`, secret taint); an unclaimed one is forwarded **verbatim** inside its normalized `${{ ` /
/// ` }}` framing, because the remote resolver's reply is template-free and must receive the
/// expression the author wrote.
///
/// A claimed `secrets::*` reference taints the whole value ([`SecretValue::secret`]), exactly as the
/// exhaustive resolver does, so a redaction decision can never depend on which entry point produced
/// the value.
pub struct ClaimedEvaluator<'a> {
    provider: &'a dyn SecretProvider,
    resources: &'a ResourceCoordinates,
    entropy: &'a dyn EntropySource,
    claims: &'a dyn Fn(&str, &[String]) -> bool,
}

impl<'a> ClaimedEvaluator<'a> {
    /// Build the evaluator for one world boot; `claims` decides which expressions resolve locally.
    pub fn new(
        provider: &'a dyn SecretProvider,
        resources: &'a ResourceCoordinates,
        entropy: &'a dyn EntropySource,
        claims: &'a dyn Fn(&str, &[String]) -> bool,
    ) -> Self {
        Self {
            provider,
            resources,
            entropy,
            claims,
        }
    }

    /// Render one template: claimed expressions resolve, unclaimed ones stay verbatim.
    pub fn render(&self, template: &str) -> Result<SecretValue, SnowError> {
        let mut policy = Policy::Claimed {
            claims: self.claims,
        };
        Ok(render(
            template,
            self.provider,
            self.resources,
            self.entropy,
            &mut policy,
        )?
        .value)
    }

    /// Resolve the mode's env groups, evaluating only the expressions `claims` accepts.
    ///
    /// A missing **unclaimed** value never fails the boot; a missing **claimed** value fails closed
    /// with [`SnowError::ClaimedValueMissing`]. `component_id` narrows the sweep to that component's
    /// own `envs` groups and fails with [`SnowError::UnknownComponent`] when the id is undeclared;
    /// `None` resolves every group of the mode.
    pub fn mode(
        &self,
        snow: &SnowFile,
        mode: &str,
        component_id: Option<&str>,
    ) -> Result<ResolvedEnvMap, SnowError> {
        let groups = snow.effective_env_groups(mode)?;
        let selected_groups: Vec<String> = match component_id {
            Some(id) => snow
                .components
                .iter()
                .find(|candidate| candidate.id == id)
                .ok_or_else(|| SnowError::UnknownComponent(id.to_owned()))?
                .envs
                .clone(),
            None => groups.keys().cloned().collect(),
        };

        let mut out = ResolvedEnvMap::new();
        for group_id in selected_groups {
            let Some(vars) = groups.get(&group_id) else {
                continue;
            };
            for (var_name, template) in vars {
                out.insert(var_name.clone(), self.render(template)?);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::provider::{FileVaultProvider, Namespace, OsCsprngEntropy, disk_tempdir};

    fn coords() -> ResourceCoordinates {
        ResourceCoordinates {
            app_url: "http://acme.localhost".to_owned(),
            app_domain: "acme.localhost".to_owned(),
            env_name: "local".to_owned(),
            is_production: false,
            pg_urls: BTreeMap::new(),
            redis_urls: BTreeMap::new(),
            s3_endpoints: BTreeMap::new(),
            component_urls: BTreeMap::new(),
        }
    }

    fn vault() -> (tempfile::TempDir, FileVaultProvider) {
        let dir = disk_tempdir("snow-claimed");
        let provider = FileVaultProvider::open(dir.path().join("vault.json")).expect("open vault");
        (dir, provider)
    }

    #[test]
    fn a_claimed_secret_taints_the_whole_value() {
        let (_dir, provider) = vault();
        provider
            .ensure_secret("SECRET_KEY", "s3cr3t-value")
            .expect("ensure secret");
        let claims = |namespace: &str, path: &[String]| {
            namespace == "secrets" && path.len() == 1 && path[0] == "SECRET_KEY"
        };
        let value = ClaimedEvaluator::new(&provider, &coords(), &OsCsprngEntropy, &claims)
            .render("prefix-${{ secrets::SECRET_KEY }}-suffix")
            .expect("claimed render");
        assert_eq!(value.expose(), "prefix-s3cr3t-value-suffix");
        assert!(value.is_secret());
        assert_eq!(format!("{value:?}"), "SecretValue([REDACTED])");
        assert!(!format!("{value:?}").contains("s3cr3t-value"));
    }

    #[test]
    fn a_second_bang_render_returns_the_stored_secret() {
        let (_dir, provider) = vault();
        let claims = |namespace: &str, path: &[String]| {
            namespace == "secrets" && path.len() == 1 && path[0] == "GENERATED"
        };
        let coords = coords();
        let evaluator = ClaimedEvaluator::new(&provider, &coords, &OsCsprngEntropy, &claims);
        let first = evaluator
            .render("${{ secrets::GENERATED !! random_string(12) }}")
            .expect("first ensure");
        let second = evaluator
            .render("${{ secrets::GENERATED !! random_string(12) }}")
            .expect("second ensure");
        assert_eq!(first.expose().len(), 12);
        assert_eq!(
            second.expose(),
            first.expose(),
            "a later !! must return the stored value, never regenerate"
        );
        assert!(first.is_secret() && second.is_secret());
        assert_eq!(
            provider.get(Namespace::Secrets, "GENERATED").expect("read"),
            Some(first.expose().to_owned())
        );
    }
}
