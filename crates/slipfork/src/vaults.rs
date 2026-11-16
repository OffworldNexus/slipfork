//! The injected project-vault factory used by `slipforkd` (composition layer).
//!
//! `DaemonService` never constructs a concrete vault adapter: it asks this capability for the
//! project's [`SecretProvider`] (world environment resolution) and its [`MutableSecretStore`]
//! (the `secret set|unset|list` CLI surface). The single `LayoutProjectVaults` instance is
//! constructed once by the `slipforkd` Composition Root; each call opens the project's `0600`
//! vault at the explicit `Layout::vault_path`.
//!
//! When a paired OSPP session exists, [`ProjectVaults::vault`] returns
//! `LayeredSecretProvider<FileVaultProvider, OsppSecretProvider>`: the `0600` local vault stays the
//! sole `!!` write sink and the remote vault is read second (`ADR-0008` §2, `H-003`).

use std::sync::Arc;

use slipfork_edge::{DaemonError, DaemonErrorKind, EdgeError};
use slipfork_ospp::{OsppClient, OsppSecretProvider};
use slipfork_snow::provider::MutableSecretStore;
use slipfork_snow::{FileVaultProvider, LayeredSecretProvider, SecretProvider};
use slipfork_world::ProjectSlug;

use crate::layout::Layout;

/// Project-scoped vault factory: the only way `DaemonService` reaches a secret backend.
pub trait ProjectVaults: Send + Sync {
    /// Read/ensure capability for one project's vault (world environment resolution).
    ///
    /// `remote` is the daemon's lazily connected OSPP session when this developer paired; it is
    /// read second and never written.
    fn vault(
        &self,
        project: &ProjectSlug,
        remote: Option<Arc<OsppClient>>,
        mode: &str,
    ) -> Result<Arc<dyn SecretProvider>, EdgeError>;

    /// Local mutation capability for one project's vault (`secret set|unset|list`).
    ///
    /// This is deliberately a separate capability: read-only backends can implement
    /// [`ProjectVaults::vault`] without pretending they can remove or enumerate keys.
    fn mutable_vault(
        &self,
        project: &ProjectSlug,
    ) -> Result<Arc<dyn MutableSecretStore>, EdgeError>;
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
        let path = self.layout.vault_path(project);
        FileVaultProvider::open(&path).map_err(|err| {
            EdgeError::Control(DaemonError::new(
                DaemonErrorKind::Internal,
                format!(
                    "cannot open the 0600 vault {} for project '{project}': {err}",
                    path.display()
                ),
            ))
        })
    }
}

impl ProjectVaults for LayoutProjectVaults {
    fn vault(
        &self,
        project: &ProjectSlug,
        remote: Option<Arc<OsppClient>>,
        mode: &str,
    ) -> Result<Arc<dyn SecretProvider>, EdgeError> {
        let local = self.open(project)?;
        match remote {
            Some(client) => {
                let remote =
                    OsppSecretProvider::new(client, project.clone(), mode).map_err(|err| {
                        EdgeError::Control(DaemonError::new(
                            DaemonErrorKind::Internal,
                            format!(
                                "cannot scope the OSPP secret provider to '{project}:{mode}': {err}"
                            ),
                        ))
                    })?;
                Ok(Arc::new(LayeredSecretProvider::new(local, remote)))
            }
            None => Ok(Arc::new(local)),
        }
    }

    fn mutable_vault(
        &self,
        project: &ProjectSlug,
    ) -> Result<Arc<dyn MutableSecretStore>, EdgeError> {
        Ok(Arc::new(self.open(project)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slipfork_snow::Namespace;

    fn layout(dir: &std::path::Path) -> Layout {
        Layout::resolve(None, None, dir, None, None, Some(dir))
    }

    #[test]
    fn mutable_and_read_capabilities_share_the_project_vault() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vaults = LayoutProjectVaults::new(layout(dir.path()));
        let project = ProjectSlug::parse("acme").expect("slug");

        let mutable = vaults.mutable_vault(&project).expect("mutable vault");
        mutable
            .put(Namespace::Secrets, "ALPHA", "one")
            .expect("put");
        assert_eq!(mutable.keys().expect("keys"), vec!["ALPHA".to_owned()]);

        let read = vaults.vault(&project, None, "local").expect("read vault");
        assert_eq!(
            read.get(Namespace::Secrets, "ALPHA").expect("get"),
            Some("one".to_owned())
        );

        assert!(mutable.remove("ALPHA").expect("remove"));
        let read = vaults.vault(&project, None, "local").expect("read vault");
        assert_eq!(read.get(Namespace::Secrets, "ALPHA").expect("get"), None);
        assert_eq!(mutable.keys().expect("keys"), Vec::<String>::new());
    }
}
