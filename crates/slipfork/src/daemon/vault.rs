//! The project `0600` vault seam and the `slipfork secret` verbs.

use super::env::validate_secret_key;
use super::service::DaemonService;
use super::{rejected, runtime_error};
use slipfork_edge::{EdgeError, SecretVault};
use slipfork_forkfile::SecretProvider;
use slipfork_world::ProjectSlug;
use std::path::Path;
use std::sync::Arc;

impl DaemonService {
    /// The project's local vault: the narrow read/ensure input to `Forkfile` evaluation.
    pub(super) fn vault_for(
        &self,
        project: &ProjectSlug,
    ) -> Result<Arc<dyn SecretProvider>, EdgeError> {
        self.vaults.provider(project)
    }
}

impl DaemonService {
    /// The project's local vault as the capability-honest `SecretVault` port.
    pub(super) fn vault_store(
        &self,
        project: &ProjectSlug,
    ) -> Result<Arc<dyn SecretVault>, EdgeError> {
        self.vaults.store(project)
    }
}

impl DaemonService {
    pub(super) fn secret_set(
        &self,
        repo_root: &Path,
        key: &str,
        value: &str,
        force: bool,
    ) -> Result<(), EdgeError> {
        validate_secret_key(key)?;
        let repo = self
            .repo_roots
            .resolve(repo_root)
            .map_err(|err| runtime_error(err.to_string()))?;
        let vault = self.vault_store(&repo.project)?;
        let existing = vault.get(&repo.project, key)?;
        if existing.is_some() && !force {
            return Err(rejected(format!(
                "secret '{key}' already exists in {}; pass --force to rotate it",
                self.config.layout.vault_path(&repo.project).display()
            )));
        }
        vault.put(&repo.project, key, value, true)
    }
}

impl DaemonService {
    pub(super) fn secret_unset(&self, repo_root: &Path, key: &str) -> Result<bool, EdgeError> {
        validate_secret_key(key)?;
        let repo = self
            .repo_roots
            .resolve(repo_root)
            .map_err(|err| runtime_error(err.to_string()))?;
        let vault = self.vault_store(&repo.project)?;
        vault.remove(&repo.project, key)
    }
}

impl DaemonService {
    pub(super) fn secret_list(&self, repo_root: &Path) -> Result<Vec<String>, EdgeError> {
        let repo = self
            .repo_roots
            .resolve(repo_root)
            .map_err(|err| runtime_error(err.to_string()))?;
        let vault = self.vault_store(&repo.project)?;
        vault.keys(&repo.project)
    }
}
