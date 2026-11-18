//! The injected project-vault factory used by `slipforkd` (composition layer).
//!
//! `DaemonService` never constructs a concrete vault adapter: it asks this capability for the
//! project's [`SecretProvider`] (the narrow read/ensure input to `SnowFile` evaluation) and its
//! [`SecretVault`] (the `secret set|unset|list` surface plus the ID-only keys that feed OSPP
//! `claims`). Both open the same project's `0600` vault at the explicit `Layout::vault_path`.
//!
//! The vault is deliberately **local-only** (`docs/design/ospp.md`, `ADR-0001` §3): it persists
//! locally provisioned backing-service credentials, local `!!` secrets, and `slipfork secret`
//! overrides. Remote OSPP values are decrypted in `slipforkd` memory and never reach this file.

use std::sync::Arc;

use slipfork_edge::vault::already_exists;
use slipfork_edge::{DaemonError, DaemonErrorKind, EdgeError, SecretVault};
use slipfork_snow::{
    FileVaultProvider, MutableSecretStore, Namespace, ProviderError, SecretProvider, SnowError,
};
use slipfork_world::ProjectSlug;

use crate::layout::Layout;

/// Project-scoped vault factory: the only way `DaemonService` reaches the local vault.
pub trait ProjectVaults: Send + Sync {
    /// Read/ensure capability for one project's local vault (world environment resolution).
    fn provider(&self, project: &ProjectSlug) -> Result<Arc<dyn SecretProvider>, EdgeError>;

    /// Local mutation + key-enumeration capability for one project's vault.
    fn store(&self, project: &ProjectSlug) -> Result<Arc<dyn SecretVault>, EdgeError>;
}

/// [`ProjectVaults`] over an explicit [`Layout`] and the `0600` [`FileVaultProvider`].
#[derive(Debug, Clone)]
pub struct LayoutProjectVaults {
    layout: Layout,
}

impl LayoutProjectVaults {
    /// Build the factory over the composition root's explicit layout.
    pub fn new(layout: Layout) -> Self {
        Self { layout }
    }

    fn open(&self, project: &ProjectSlug) -> Result<FileVaultProvider, EdgeError> {
        FileVaultProvider::open(self.layout.vault_path(project)).map_err(|err| {
            EdgeError::Control(DaemonError::new(
                DaemonErrorKind::Internal,
                format!(
                    "cannot open the 0600 vault {} for project '{project}': {err}",
                    self.layout.vault_path(project).display()
                ),
            ))
        })
    }
}

impl ProjectVaults for LayoutProjectVaults {
    fn provider(&self, project: &ProjectSlug) -> Result<Arc<dyn SecretProvider>, EdgeError> {
        Ok(Arc::new(self.open(project)?))
    }

    fn store(&self, project: &ProjectSlug) -> Result<Arc<dyn SecretVault>, EdgeError> {
        Ok(Arc::new(FileSecretVault {
            provider: self.open(project)?,
        }))
    }
}

/// [`SecretVault`] over one project's `0600` [`FileVaultProvider`].
struct FileSecretVault {
    provider: FileVaultProvider,
}

impl SecretVault for FileSecretVault {
    fn get(&self, _project: &ProjectSlug, key: &str) -> Result<Option<String>, EdgeError> {
        self.provider
            .get(Namespace::Secrets, key)
            .map_err(provider_error)
    }

    fn put(
        &self,
        project: &ProjectSlug,
        key: &str,
        value: &str,
        force: bool,
    ) -> Result<(), EdgeError> {
        if !force
            && self
                .provider
                .get(Namespace::Secrets, key)
                .map_err(provider_error)?
                .is_some()
        {
            return Err(already_exists(project, key));
        }
        self.provider
            .put(Namespace::Secrets, key, value)
            .map_err(provider_error)
    }

    fn remove(&self, _project: &ProjectSlug, key: &str) -> Result<bool, EdgeError> {
        self.provider.remove(key).map_err(provider_error)
    }

    fn keys(&self, _project: &ProjectSlug) -> Result<Vec<String>, EdgeError> {
        self.provider
            .keys_in(Namespace::Secrets)
            .map_err(provider_error)
    }

    fn vars(&self, _project: &ProjectSlug) -> Result<Vec<String>, EdgeError> {
        self.provider
            .keys_in(Namespace::Vars)
            .map_err(provider_error)
    }
}

fn provider_error(error: ProviderError) -> EdgeError {
    EdgeError::Control(DaemonError::new(
        DaemonErrorKind::Internal,
        format!("local vault: {error}"),
    ))
}

/// Translate a `snow.yml` resolution failure without ever embedding a secret value.
pub fn snow_error(error: SnowError) -> EdgeError {
    EdgeError::Control(DaemonError::new(
        DaemonErrorKind::Internal,
        error.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(dir: &std::path::Path) -> Layout {
        Layout::resolve(None, dir, None, Some(dir))
    }

    #[test]
    fn mutable_and_read_capabilities_share_the_project_vault() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vaults = LayoutProjectVaults::new(layout(dir.path()));
        let project = ProjectSlug::parse("acme").expect("slug");

        let store = vaults.store(&project).expect("vault store");
        store.put(&project, "ALPHA", "one", false).expect("put");
        assert_eq!(
            store.keys(&project).expect("keys"),
            vec!["ALPHA".to_owned()]
        );
        assert!(matches!(
            store.put(&project, "ALPHA", "two", false),
            Err(EdgeError::Control(DaemonError {
                kind: DaemonErrorKind::Rejected,
                ..
            }))
        ));
        store
            .put(&project, "ALPHA", "two", true)
            .expect("forced put");

        let read = vaults.provider(&project).expect("read vault");
        assert_eq!(
            read.get(Namespace::Secrets, "ALPHA").expect("get"),
            Some("two".to_owned())
        );

        assert!(store.remove(&project, "ALPHA").expect("remove"));
        assert_eq!(store.keys(&project).expect("keys"), Vec::<String>::new());
    }

    #[test]
    fn local_vars_are_enumerated_for_claims_without_exposing_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vaults = LayoutProjectVaults::new(layout(dir.path()));
        let project = ProjectSlug::parse("acme").expect("slug");
        let store = vaults.store(&project).expect("vault store");
        vaults
            .open(&project)
            .expect("open")
            .put(Namespace::Vars, "DEBUG", "1")
            .expect("local var");
        assert_eq!(
            store.vars(&project).expect("vars"),
            vec!["DEBUG".to_owned()]
        );
        assert_eq!(store.keys(&project).expect("secrets"), Vec::<String>::new());
    }
}
