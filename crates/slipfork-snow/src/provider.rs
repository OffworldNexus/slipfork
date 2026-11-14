//! `SecretProvider` and `EntropySource` DI traits, `SecretValue` redaction wrapper,
//! and concrete local/memory/OSPP-bundle adapters.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Logical namespaces resolved through [`SecretProvider`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Namespace {
    /// Encrypted/redacted secrets (`secrets::<KEY>`).
    Secrets,
    /// Non-secret configuration variables (`vars::<KEY>`).
    Vars,
    /// Configured domain names (`domains::<ID>`).
    Domains,
}

impl Namespace {
    /// Parse one provider-backed namespace identifier.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "secrets" => Some(Self::Secrets),
            "vars" => Some(Self::Vars),
            "domains" => Some(Self::Domains),
            _ => None,
        }
    }

    /// Return the canonical `snow.yml` namespace prefix.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Secrets => "secrets",
            Self::Vars => "vars",
            Self::Domains => "domains",
        }
    }
}

/// Errors produced by a [`SecretProvider`] or [`EntropySource`].
#[derive(Debug, Error)]
pub enum ProviderError {
    /// Underlying storage or network I/O failed.
    #[error("secret provider I/O failed at {path}: {message}")]
    Io { path: String, message: String },
    /// Stored vault JSON was malformed.
    #[error("corrupt vault JSON at {path}: {message}")]
    CorruptVault { path: String, message: String },
    /// OS CSPRNG failed to produce random bytes.
    #[error("entropy source failed: {0}")]
    Entropy(String),
}

/// A resolved configuration value that redacts itself in `Debug` and `Display`
/// whenever any contributing token originated from `secrets::*`.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue {
    raw: String,
    secret: bool,
}

impl SecretValue {
    /// Construct a plain (non-secret) resolved value.
    pub fn plain(value: impl Into<String>) -> Self {
        Self {
            raw: value.into(),
            secret: false,
        }
    }

    /// Construct a secret-contaminated resolved value.
    pub fn secret(value: impl Into<String>) -> Self {
        Self {
            raw: value.into(),
            secret: true,
        }
    }

    /// Return `true` when this value incorporates any `secrets::*` token.
    pub const fn is_secret(&self) -> bool {
        self.secret
    }

    /// Explicitly expose the underlying plaintext at an execution/adapter boundary.
    pub fn expose(&self) -> &str {
        &self.raw
    }

    /// Return `[REDACTED]` for secret values and the raw text for non-secret values.
    pub fn redacted(&self) -> &str {
        if self.secret { "[REDACTED]" } else { &self.raw }
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretValue")
            .field("value", &self.redacted())
            .field("secret", &self.secret)
            .finish()
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.redacted())
    }
}

/// Port for reading variables/secrets/domains and atomically persisting `!!` generated secrets.
pub trait SecretProvider {
    /// Look up `namespace::key`, returning `Ok(None)` when absent.
    fn get(&self, namespace: Namespace, key: &str) -> Result<Option<String>, ProviderError>;

    /// Persist `proposed` under `secrets::<key>` if and only if `key` is currently absent,
    /// returning the authoritative stored value.
    fn ensure_secret(&mut self, key: &str, proposed: &str) -> Result<String, ProviderError>;
}

/// Port for cryptographic byte generation (injected so unit tests stay 100% deterministic).
pub trait EntropySource {
    /// Fill `buf` with cryptographically random bytes.
    fn fill_bytes(&mut self, buf: &mut [u8]) -> Result<(), ProviderError>;
}

/// Production CSPRNG adapter backed by the OS kernel (`getrandom`).
#[derive(Debug, Default, Clone, Copy)]
pub struct OsCsprngEntropy;

impl EntropySource for OsCsprngEntropy {
    fn fill_bytes(&mut self, buf: &mut [u8]) -> Result<(), ProviderError> {
        getrandom::fill(buf).map_err(|e| ProviderError::Entropy(e.to_string()))
    }
}

/// Wire payload of `GET /v1/projects/{project}/modes/{mode}/bundle` in OSPP v1.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OsppBundle {
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
    #[serde(default)]
    pub domains: BTreeMap<String, String>,
}

/// In-memory [`SecretProvider`] for tests and static OSPP bundle evaluation.
#[derive(Debug, Default, Clone)]
pub struct MemorySecretProvider {
    bundle: OsppBundle,
}

impl MemorySecretProvider {
    /// Wrap an existing [`OsppBundle`].
    pub fn from_bundle(bundle: OsppBundle) -> Self {
        Self { bundle }
    }

    /// Set a value in the given namespace.
    pub fn insert(&mut self, namespace: Namespace, key: impl Into<String>, val: impl Into<String>) {
        let map = match namespace {
            Namespace::Secrets => &mut self.bundle.secrets,
            Namespace::Vars => &mut self.bundle.vars,
            Namespace::Domains => &mut self.bundle.domains,
        };
        map.insert(key.into(), val.into());
    }
}

impl SecretProvider for MemorySecretProvider {
    fn get(&self, namespace: Namespace, key: &str) -> Result<Option<String>, ProviderError> {
        let map = match namespace {
            Namespace::Secrets => &self.bundle.secrets,
            Namespace::Vars => &self.bundle.vars,
            Namespace::Domains => &self.bundle.domains,
        };
        Ok(map.get(key).cloned())
    }

    fn ensure_secret(&mut self, key: &str, proposed: &str) -> Result<String, ProviderError> {
        let entry = self
            .bundle
            .secrets
            .entry(key.to_owned())
            .or_insert_with(|| proposed.to_owned());
        Ok(entry.clone())
    }
}

/// Local `0600` JSON vault adapter (`~/.local/share/slipfork/vaults/<project>.json`).
#[derive(Debug, Clone)]
pub struct FileVaultProvider {
    path: PathBuf,
    bundle: OsppBundle,
}

impl FileVaultProvider {
    /// Open an existing vault JSON file or initialize an empty vault at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ProviderError> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Ok(Self {
                path,
                bundle: OsppBundle::default(),
            });
        }
        let raw = fs::read_to_string(&path).map_err(|e| ProviderError::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        let bundle: OsppBundle =
            serde_json::from_str(&raw).map_err(|e| ProviderError::CorruptVault {
                path: path.display().to_string(),
                message: e.to_string(),
            })?;
        Ok(Self { path, bundle })
    }

    /// Atomically flush the vault to disk with `0600` Unix permissions.
    pub fn save(&self) -> Result<(), ProviderError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| ProviderError::Io {
                path: parent.display().to_string(),
                message: e.to_string(),
            })?;
        }
        let tmp_path = self.path.with_extension("json.tmp");
        let serialized =
            serde_json::to_vec_pretty(&self.bundle).map_err(|e| ProviderError::CorruptVault {
                path: self.path.display().to_string(),
                message: e.to_string(),
            })?;
        {
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp_path).map_err(|e| ProviderError::Io {
                path: tmp_path.display().to_string(),
                message: e.to_string(),
            })?;
            f.write_all(&serialized)
                .and_then(|()| f.sync_all())
                .map_err(|e| ProviderError::Io {
                    path: tmp_path.display().to_string(),
                    message: e.to_string(),
                })?;
        }
        fs::rename(&tmp_path, &self.path).map_err(|e| ProviderError::Io {
            path: self.path.display().to_string(),
            message: e.to_string(),
        })
    }

    /// Store or overwrite an explicit secret or variable and flush immediately.
    pub fn put(
        &mut self,
        namespace: Namespace,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), ProviderError> {
        match namespace {
            Namespace::Secrets => self.bundle.secrets.insert(key.into(), value.into()),
            Namespace::Vars => self.bundle.vars.insert(key.into(), value.into()),
            Namespace::Domains => self.bundle.domains.insert(key.into(), value.into()),
        };
        self.save()
    }
}

impl SecretProvider for FileVaultProvider {
    fn get(&self, namespace: Namespace, key: &str) -> Result<Option<String>, ProviderError> {
        let map = match namespace {
            Namespace::Secrets => &self.bundle.secrets,
            Namespace::Vars => &self.bundle.vars,
            Namespace::Domains => &self.bundle.domains,
        };
        Ok(map.get(key).cloned())
    }

    fn ensure_secret(&mut self, key: &str, proposed: &str) -> Result<String, ProviderError> {
        if let Some(existing) = self.bundle.secrets.get(key) {
            return Ok(existing.clone());
        }
        self.bundle
            .secrets
            .insert(key.to_owned(), proposed.to_owned());
        self.save()?;
        Ok(proposed.to_owned())
    }
}

/// Layered [`SecretProvider`]: reads `local_sink` first, then `remote_read`,
/// and persists `!!` generated secrets exclusively into `local_sink` (`ADR-0001`).
#[derive(Debug)]
pub struct LayeredSecretProvider<R, W> {
    remote_read: R,
    local_sink: W,
}

impl<R: SecretProvider, W: SecretProvider> LayeredSecretProvider<R, W> {
    /// Combine a read-only upstream provider (`remote_read`) with a writable local vault (`local_sink`).
    pub fn new(remote_read: R, local_sink: W) -> Self {
        Self {
            remote_read,
            local_sink,
        }
    }
}

impl<R: SecretProvider, W: SecretProvider> SecretProvider for LayeredSecretProvider<R, W> {
    fn get(&self, namespace: Namespace, key: &str) -> Result<Option<String>, ProviderError> {
        if let Some(val) = self.local_sink.get(namespace, key)? {
            return Ok(Some(val));
        }
        self.remote_read.get(namespace, key)
    }

    fn ensure_secret(&mut self, key: &str, proposed: &str) -> Result<String, ProviderError> {
        if let Some(existing) = self.get(Namespace::Secrets, key)? {
            return Ok(existing);
        }
        self.local_sink.ensure_secret(key, proposed)
    }
}
