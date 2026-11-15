//! `OsppSecretProvider`: the zero-filesystem-write `SecretProvider` over the daemon-held OSPP
//! session, including compare-and-set `put_if_absent` for `!!` values.
//!
//! `FileVaultProvider` inside `LayeredSecretProvider` remains the only `0600` local vault writer
//! (`H-003`); this adapter reads from and performs compare-and-set writes to the remote vault only.
//!
//! `slipfork_snow::ProviderError` has no remote variant, so a capability failure is reported as
//! `ProviderError::Io` whose locus names the capability and world scope (`ospp-v1/<capability>/<project>:<mode>`)
//! and whose message is the typed `OsppError` rendering. No secret value ever appears in either.

use std::fmt;
use std::sync::Arc;

use slipfork_snow::{Namespace, ProviderError, SecretProvider};
use slipfork_world::ProjectSlug;

use crate::client::OsppClient;
use crate::error::OsppError;
use crate::protocol::{
    CAPABILITY_ENV_BUNDLE, CAPABILITY_SECRETS_ENSURE, capability_error, validate_mode,
};

/// A `SecretProvider` scoped to one `<project>:<mode>` over one authenticated OSPP session.
///
/// `slipforkd` holds one persistent [`OsppClient`] and constructs one provider per world scope; the
/// shared session serializes requests, so this type is cheap to clone and safe to share.
#[derive(Clone)]
pub struct OsppSecretProvider {
    client: Arc<OsppClient>,
    project: ProjectSlug,
    mode: String,
}

impl OsppSecretProvider {
    /// Scope a shared authenticated session to `project`/`mode`.
    pub fn new(
        client: Arc<OsppClient>,
        project: ProjectSlug,
        mode: impl Into<String>,
    ) -> Result<Self, OsppError> {
        let mode = mode.into();
        let scope = format!("{project}:{mode}");
        validate_mode(&mode)
            .map_err(|message| capability_error(CAPABILITY_ENV_BUNDLE, &scope, message))?;
        Ok(Self {
            client,
            project,
            mode,
        })
    }

    fn scope(&self) -> String {
        format!("{}:{}", self.project, self.mode)
    }
}

impl fmt::Debug for OsppSecretProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OsppSecretProvider")
            .field("scope", &self.scope())
            .finish_non_exhaustive()
    }
}

impl SecretProvider for OsppSecretProvider {
    fn get(&self, namespace: Namespace, key: &str) -> Result<Option<String>, ProviderError> {
        let bundle = self
            .client
            .env_bundle(&self.project, &self.mode)
            .map_err(|error| provider_error(CAPABILITY_ENV_BUNDLE, &self.scope(), &error))?;
        Ok(bundle.get(namespace, key).map(str::to_owned))
    }

    fn ensure_secret(&self, key: &str, proposed: &str) -> Result<String, ProviderError> {
        self.client
            .ensure_secret(&self.project, &self.mode, key, proposed)
            .map_err(|error| provider_error(CAPABILITY_SECRETS_ENSURE, &self.scope(), &error))
    }
}

fn provider_error(capability: &'static str, scope: &str, error: &OsppError) -> ProviderError {
    ProviderError::Io {
        path: format!("ospp-v1/{capability}/{scope}"),
        message: error.to_string(),
    }
}
