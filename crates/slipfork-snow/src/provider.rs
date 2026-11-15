//! `SecretProvider` / `EntropySource` DI traits, the `SecretValue` redaction wrapper, and the
//! local vault / in-memory adapters.
//!
//! Both ports are `Send + Sync` and take `&self`: M2 resolves worlds and secrets from several
//! threads, so interior synchronization belongs to the adapter (the `0600` [`FileVaultProvider`]
//! holds an `flock` for the duration of a compare-and-set) and never to a caller.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// How often one compare-and-set re-acquires the vault lock when a concurrent writer replaced the
/// vault inode between `open` and `flock`. Real contention settles on the second attempt.
const LOCK_ATTEMPTS: usize = 8;

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

/// One vault's `vars` / `secrets` / `domains` maps (the on-disk `0600` JSON shape).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VaultRecord {
    #[serde(default)]
    vars: BTreeMap<String, String>,
    #[serde(default)]
    secrets: BTreeMap<String, String>,
    #[serde(default)]
    domains: BTreeMap<String, String>,
}

impl VaultRecord {
    fn map(&self, namespace: Namespace) -> &BTreeMap<String, String> {
        match namespace {
            Namespace::Secrets => &self.secrets,
            Namespace::Vars => &self.vars,
            Namespace::Domains => &self.domains,
        }
    }

    fn map_mut(&mut self, namespace: Namespace) -> &mut BTreeMap<String, String> {
        match namespace {
            Namespace::Secrets => &mut self.secrets,
            Namespace::Vars => &mut self.vars,
            Namespace::Domains => &mut self.domains,
        }
    }

    /// Parse a vault body. An empty file is the zero-record vault: `FileVaultProvider` mints the
    /// vault file (`O_CREAT`, `0600`) before its first atomic rename so `flock` has an inode.
    fn parse(raw: &str, path: &Path) -> Result<Self, ProviderError> {
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(raw).map_err(|e| ProviderError::CorruptVault {
            path: path.display().to_string(),
            message: e.to_string(),
        })
    }
}

/// Build the `ProviderError::Io` for an operation on `path`.
fn io_error(path: &Path) -> impl FnOnce(io::Error) -> ProviderError + '_ {
    move |e| ProviderError::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    }
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
        write!(f, "SecretValue({})", self.redacted())
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.redacted())
    }
}

/// Port for reading variables/secrets/domains and atomically persisting `!!` generated secrets.
pub trait SecretProvider: Send + Sync {
    /// Look up `namespace::key`, returning `Ok(None)` when absent.
    fn get(&self, namespace: Namespace, key: &str) -> Result<Option<String>, ProviderError>;

    /// Persist `proposed` under `secrets::<key>` if and only if `key` is currently absent,
    /// returning the authoritative stored value.
    fn ensure_secret(&self, key: &str, proposed: &str) -> Result<String, ProviderError>;
}

/// Port for cryptographic byte generation (injected so unit tests stay 100% deterministic).
pub trait EntropySource: Send + Sync {
    /// Fill `buf` with cryptographically random bytes.
    fn fill_bytes(&self, buf: &mut [u8]) -> Result<(), ProviderError>;
}

/// Production CSPRNG adapter backed by the OS kernel (`getrandom`).
#[derive(Debug, Default, Clone, Copy)]
pub struct OsCsprngEntropy;

impl EntropySource for OsCsprngEntropy {
    fn fill_bytes(&self, buf: &mut [u8]) -> Result<(), ProviderError> {
        getrandom::fill(buf).map_err(|e| ProviderError::Entropy(e.to_string()))
    }
}

/// In-memory [`SecretProvider`] for tests and static snapshots.
#[derive(Default)]
pub struct MemorySecretProvider {
    record: RwLock<VaultRecord>,
}

impl fmt::Debug for MemorySecretProvider {
    /// Never print the store: `{:?}` on a provider must not expose plaintext secrets.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MemorySecretProvider(<redacted>)")
    }
}

impl MemorySecretProvider {
    /// Construct an empty in-memory provider.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a value in the given namespace.
    pub fn insert(&self, namespace: Namespace, key: impl Into<String>, val: impl Into<String>) {
        self.write()
            .map_mut(namespace)
            .insert(key.into(), val.into());
    }

    fn read(&self) -> RwLockReadGuard<'_, VaultRecord> {
        // Poisoning only means a panicking thread held the lock; the maps carry no invariant a
        // panic can break, so recover instead of failing every later resolution.
        self.record
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, VaultRecord> {
        self.record
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Clone for MemorySecretProvider {
    fn clone(&self) -> Self {
        Self {
            record: RwLock::new(self.read().clone()),
        }
    }
}

impl SecretProvider for MemorySecretProvider {
    fn get(&self, namespace: Namespace, key: &str) -> Result<Option<String>, ProviderError> {
        Ok(self.read().map(namespace).get(key).cloned())
    }

    fn ensure_secret(&self, key: &str, proposed: &str) -> Result<String, ProviderError> {
        let mut record = self.write();
        let entry = record
            .map_mut(Namespace::Secrets)
            .entry(key.to_owned())
            .or_insert_with(|| proposed.to_owned());
        Ok(entry.clone())
    }
}

/// Local `0600` JSON vault adapter (`~/.local/share/slipfork/vaults/<project>.json`).
///
/// Reads are lock-free because every write lands through an atomic `rename`; a write takes an
/// exclusive `flock` on the vault inode and re-reads the vault under that lock, so
/// [`SecretProvider::ensure_secret`] is a real compare-and-set: exactly one racer writes and every
/// racer returns the same authoritative value.
#[derive(Debug, Clone)]
pub struct FileVaultProvider {
    path: PathBuf,
}

impl FileVaultProvider {
    /// Open an existing vault JSON file or accept the path for a not-yet-created vault at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ProviderError> {
        let path = path.as_ref().to_path_buf();
        if path.exists() {
            let raw = fs::read_to_string(&path).map_err(io_error(&path))?;
            VaultRecord::parse(&raw, &path)?;
        }
        Ok(Self { path })
    }

    /// Store or overwrite an explicit secret or variable and flush immediately.
    pub fn put(
        &self,
        namespace: Namespace,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), ProviderError> {
        let key = key.into();
        let value = value.into();
        self.update(|record| {
            record.map_mut(namespace).insert(key, value);
            ((), true)
        })
    }

    /// Read the whole vault from disk (`Ok(default)` when the vault does not exist yet).
    fn read_vault(&self) -> Result<VaultRecord, ProviderError> {
        match fs::read_to_string(&self.path) {
            Ok(raw) => VaultRecord::parse(&raw, &self.path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(VaultRecord::default()),
            Err(e) => Err(io_error(&self.path)(e)),
        }
    }

    /// Run `mutate` against the on-disk vault while holding the exclusive vault lock.
    ///
    /// The closure returns `(value, changed)`; the vault is atomically rewritten only when
    /// `changed` is `true`, so a losing compare-and-set racer performs zero writes.
    fn update<T>(
        &self,
        mutate: impl FnOnce(&mut VaultRecord) -> (T, bool),
    ) -> Result<T, ProviderError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(io_error(parent))?;
        }
        for _ in 0..LOCK_ATTEMPTS {
            // `create(true)` mints the `0600` vault on first use so `flock` owns a real inode.
            let locked = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(&self.path)
                .map_err(io_error(&self.path))?;
            locked.lock().map_err(io_error(&self.path))?;
            // A writer replaces the vault path with an atomically renamed inode, which retires the
            // lock we just took on the previous one; proceed only once the handle is still `path`.
            if !names_same_file(&locked, &self.path)? {
                continue;
            }
            let mut record = self.read_vault()?;
            let (out, changed) = mutate(&mut record);
            if changed {
                self.write_atomic(&record)?;
            }
            return Ok(out);
        }
        Err(ProviderError::Io {
            path: self.path.display().to_string(),
            message: format!("vault lock did not settle after {LOCK_ATTEMPTS} attempts"),
        })
    }

    /// Write `record` to `0600` `O_EXCL` temp file, `fsync`, then atomically `rename` over the vault.
    fn write_atomic(&self, record: &VaultRecord) -> Result<(), ProviderError> {
        let tmp_path = self.path.with_extension("json.tmp");
        let serialized =
            serde_json::to_vec_pretty(record).map_err(|e| ProviderError::CorruptVault {
                path: self.path.display().to_string(),
                message: e.to_string(),
            })?;
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true).mode(0o600);
        let mut tmp = match opts.open(&tmp_path) {
            Ok(file) => file,
            // Only a crashed writer leaves a temp behind, and we hold the exclusive vault lock,
            // so clearing it is safe.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                fs::remove_file(&tmp_path).map_err(io_error(&tmp_path))?;
                opts.open(&tmp_path).map_err(io_error(&tmp_path))?
            }
            Err(e) => return Err(io_error(&tmp_path)(e)),
        };
        tmp.write_all(&serialized)
            .and_then(|()| tmp.sync_all())
            .map_err(io_error(&tmp_path))?;
        fs::rename(&tmp_path, &self.path).map_err(io_error(&self.path))
    }
}

/// `true` when `handle` and `path` still name the same inode (dev + inode identity).
fn names_same_file(handle: &File, path: &Path) -> Result<bool, ProviderError> {
    let locked = handle.metadata().map_err(io_error(path))?;
    let current = fs::metadata(path).map_err(io_error(path))?;
    Ok(locked.dev() == current.dev() && locked.ino() == current.ino())
}

impl SecretProvider for FileVaultProvider {
    fn get(&self, namespace: Namespace, key: &str) -> Result<Option<String>, ProviderError> {
        Ok(self.read_vault()?.map(namespace).get(key).cloned())
    }

    fn ensure_secret(&self, key: &str, proposed: &str) -> Result<String, ProviderError> {
        self.update(|record| match record.secrets.get(key) {
            Some(existing) => (existing.clone(), false),
            None => {
                record.secrets.insert(key.to_owned(), proposed.to_owned());
                (proposed.to_owned(), true)
            }
        })
    }
}

/// Layered [`SecretProvider`]: reads `primary` then `fallback`, and persists `!!` generated
/// secrets exclusively into `primary` (`ADR-0001` §3, `ADR-0008` §2).
///
/// `slipforkd` wires `LayeredSecretProvider<FileVaultProvider, OsppSecretProvider>` for `local`
/// mode, so a paired cloud vault is read second and never mutated while the `0600` local vault
/// stays the sole write sink.
#[derive(Debug)]
pub struct LayeredSecretProvider<Primary, Fallback> {
    primary: Primary,
    fallback: Fallback,
}

impl<Primary: SecretProvider, Fallback: SecretProvider> LayeredSecretProvider<Primary, Fallback> {
    /// Combine the readable/writable `primary` provider with a read-only `fallback` provider.
    pub fn new(primary: Primary, fallback: Fallback) -> Self {
        Self { primary, fallback }
    }
}

impl<Primary: SecretProvider, Fallback: SecretProvider> SecretProvider
    for LayeredSecretProvider<Primary, Fallback>
{
    fn get(&self, namespace: Namespace, key: &str) -> Result<Option<String>, ProviderError> {
        if let Some(val) = self.primary.get(namespace, key)? {
            return Ok(Some(val));
        }
        self.fallback.get(namespace, key)
    }

    fn ensure_secret(&self, key: &str, proposed: &str) -> Result<String, ProviderError> {
        if let Some(existing) = self.get(Namespace::Secrets, key)? {
            return Ok(existing);
        }
        self.primary.ensure_secret(key, proposed)
    }
}
