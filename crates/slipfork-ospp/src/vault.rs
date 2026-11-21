//! The offline `SecretVault` port and its `0600` file adapter.
//!
//! `specs/03-under-the-hood.md` names `SecretVault` as the canonical daemon-side
//! port. The adapter keeps cloud-resolved secrets in one caller-injected `0600`
//! JSON record at rest; only [`Secret`] values escape the adapter, so callers can
//! never print a value accidentally. `<state_dir>/ospp.json` remains secret-free.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::OsppError;
use crate::model::Secret;

/// The only mode a private state file may have.
pub const PRIVATE_FILE_MODE: u32 = 0o600;

/// Daemon-side port for persisted secrets.
pub trait SecretVault: Send + Sync {
    /// Read one stored value, if present.
    fn load(&self, component: &str, key: &str) -> Result<Option<Secret>, OsppError>;

    /// Atomically store one value in a `0600` record.
    fn store(&self, component: &str, key: &str, value: &Secret) -> Result<(), OsppError>;
}

/// `0600` JSON-file [`SecretVault`] at a caller-injected path.
pub struct FileSecretVault {
    path: PathBuf,
}

impl FileSecretVault {
    /// Bind the vault to the injected file path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The injected path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn entries(&self) -> Result<BTreeMap<String, BTreeMap<String, String>>, OsppError> {
        match read_private(&self.path)? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| state_error(&self.path, error.to_string())),
            None => Ok(BTreeMap::new()),
        }
    }

    fn store_entry(&self, component: &str, key: &str, value: &Secret) -> Result<(), OsppError> {
        let mut entries = self.entries()?;
        entries
            .entry(component.to_owned())
            .or_default()
            .insert(key.to_owned(), value.expose().to_owned());
        let bytes = serde_json::to_vec_pretty(&entries)
            .map_err(|error| state_error(&self.path, error.to_string()))?;
        write_private(&self.path, &bytes)
    }
}

impl fmt::Debug for FileSecretVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileSecretVault")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl SecretVault for FileSecretVault {
    fn load(&self, component: &str, key: &str) -> Result<Option<Secret>, OsppError> {
        Ok(self
            .entries()?
            .get(component)
            .and_then(|values| values.get(key))
            .map(|value| Secret::new(value.clone())))
    }

    fn store(&self, component: &str, key: &str, value: &Secret) -> Result<(), OsppError> {
        self.store_entry(component, key, value)
    }
}

/// Write `bytes` to `path` atomically with mode `0600`.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), OsppError> {
    let parent = path
        .parent()
        .ok_or_else(|| state_error(path, "state path has no parent directory".to_owned()))?;
    fs::create_dir_all(parent).map_err(|error| state_error(parent, error.to_string()))?;
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).map_err(|error| OsppError::Entropy(error.to_string()))?;
    let temporary = parent.join(format!(".slipfork-{}.tmp", hex::encode(suffix)));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(PRIVATE_FILE_MODE)
            .open(&temporary)
            .map_err(|error| state_error(&temporary, error.to_string()))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| state_error(&temporary, error.to_string()))?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(PRIVATE_FILE_MODE))
            .map_err(|error| state_error(&temporary, error.to_string()))?;
        fs::rename(&temporary, path).map_err(|error| state_error(path, error.to_string()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Read a `0600` state file, refusing anything group- or world-accessible.
pub(crate) fn read_private(path: &Path) -> Result<Option<Vec<u8>>, OsppError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(state_error(path, error.to_string())),
    };
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(state_error(path, "state file mode must be 0600".to_owned()));
    }
    fs::read(path)
        .map(Some)
        .map_err(|error| state_error(path, error.to_string()))
}

fn state_error(path: &Path, message: String) -> OsppError {
    OsppError::State {
        path: path.display().to_string(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_round_trips_a_secret_and_refuses_group_readable_file() {
        let directory = tempfile::TempDir::new_in("/var/tmp").expect("disk temp dir");
        let vault = FileSecretVault::new(directory.path().join("vault.json"));
        vault
            .store("api", "SECRET_KEY", &Secret::new("vaulted-value"))
            .expect("store");
        assert_eq!(
            vault
                .load("api", "SECRET_KEY")
                .expect("load")
                .expect("present")
                .expose(),
            "vaulted-value"
        );
        assert!(vault.load("api", "MISSING").expect("load").is_none());
        std::fs::set_permissions(vault.path(), std::fs::Permissions::from_mode(0o644))
            .expect("chmod 0644");
        let error = vault.load("api", "SECRET_KEY").expect_err("refused");
        assert!(error.to_string().contains("0600"), "{error}");
    }
}
