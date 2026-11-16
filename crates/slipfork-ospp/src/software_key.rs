//! `SoftwareP256Signer`: the `DeviceKeySigner` fallback backed by an injected [`DeviceKeyStore`]
//! and `EntropySource`, plus the production `0600` file store it normally loads from.
//!
//! The private scalar lives only in [`SoftwareP256Key`], prints as `[REDACTED]`, and is zeroized on
//! `Drop`. Signing hashes the message with SHA-256 once and returns the raw fixed-width `r || s`
//! form (`ADR-0008` §1).

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use p256::ecdsa::SigningKey;
use p256::ecdsa::signature::hazmat::PrehashSigner;
use sha2::{Digest, Sha256};

use slipfork_snow::EntropySource;

use crate::device_key::{DeviceKeySigner, DeviceKeyStore, DeviceSignature, KeyId, SIGNATURE_BYTES};
use crate::error::OsppError;

/// Number of entropy draws attempted when validating a fresh scalar.
///
/// A P-256 scalar must be non-zero and below the group order; the probability of rejection is
/// negligible, so a handful of retries is a deterministic failure bound rather than a loop.
const SCALAR_ATTEMPTS: usize = 8;

/// A P-256 private scalar that zeroizes itself on `Drop`.
///
/// There is no `Debug` implementation that prints the scalar, no `Clone`, and no public accessor
/// for the raw bytes.
#[derive(PartialEq, Eq)]
pub struct SoftwareP256Key {
    scalar: [u8; 32],
}

impl SoftwareP256Key {
    /// Wrap exactly 32 scalar bytes, rejecting zero and out-of-range values.
    pub fn from_scalar_bytes(bytes: [u8; 32]) -> Result<Self, OsppError> {
        SigningKey::from_slice(&bytes)
            .map_err(|_| invalid_store("generated scalar is not valid on P-256"))?;
        Ok(Self { scalar: bytes })
    }

    /// Draw a fresh scalar from the injected entropy source.
    pub fn generate(entropy: &dyn EntropySource) -> Result<Self, OsppError> {
        for _ in 0..SCALAR_ATTEMPTS {
            let mut bytes = [0u8; 32];
            entropy.fill_bytes(&mut bytes)?;
            if SigningKey::from_slice(&bytes).is_ok() {
                return Ok(Self { scalar: bytes });
            }
            bytes.fill(0);
        }
        Err(invalid_store(
            "entropy source produced no valid P-256 scalar within the retry bound",
        ))
    }

    /// The public fingerprint of this key (`p256:<66 lowercase hex>`).
    pub fn key_id(&self) -> Result<KeyId, OsppError> {
        let encoded = self.signing_key()?.verifying_key().to_sec1_point(true);
        KeyId::from_public_key_bytes(encoded.as_bytes())
    }

    /// Sign `message` with SHA-256 + ECDSA P-256 and return the raw 64-byte form.
    pub fn sign(&self, message: &[u8]) -> Result<DeviceSignature, OsppError> {
        let digest = Sha256::digest(message);
        let signature: p256::ecdsa::Signature = self
            .signing_key()?
            .sign_prehash(&digest)
            .map_err(|_| invalid_store("P-256 signing failed"))?;
        let bytes: [u8; SIGNATURE_BYTES] = signature
            .to_bytes()
            .as_slice()
            .try_into()
            .map_err(|_| invalid_store("P-256 produced a non-64-byte signature"))?;
        Ok(DeviceSignature::from_raw(bytes))
    }

    fn signing_key(&self) -> Result<SigningKey, OsppError> {
        SigningKey::from_slice(&self.scalar).map_err(|_| {
            invalid_store("the stored 32-byte scalar is not a valid P-256 private key")
        })
    }
}

impl Drop for SoftwareP256Key {
    fn drop(&mut self) {
        self.scalar.fill(0);
    }
}

impl fmt::Debug for SoftwareP256Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SoftwareP256Key([REDACTED])")
    }
}

/// `DeviceKeySigner` over a [`SoftwareP256Key`] loaded from an injected [`DeviceKeyStore`].
///
/// `slipforkd` selects this adapter only through `FallbackDeviceKeySigner` when `tpmd` is absent,
/// and reports the `software:file-0600` label so the downgrade is never silent.
pub struct SoftwareP256Signer {
    key: SoftwareP256Key,
    key_id: KeyId,
}

impl SoftwareP256Signer {
    /// Load (or atomically create) the software key from `store`.
    pub fn open(
        store: &dyn DeviceKeyStore,
        entropy: &dyn EntropySource,
    ) -> Result<Self, OsppError> {
        let key = store.load_or_create_p256(entropy)?;
        let key_id = key.key_id()?;
        Ok(Self { key, key_id })
    }
}

impl fmt::Debug for SoftwareP256Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SoftwareP256Signer")
            .field("key", &"[REDACTED]")
            .field("key_id", &self.key_id)
            .finish()
    }
}

impl DeviceKeySigner for SoftwareP256Signer {
    fn key_id(&self) -> &KeyId {
        &self.key_id
    }

    fn backend_label(&self) -> &'static str {
        "software:file-0600"
    }

    fn sign(&self, message: &[u8]) -> Result<DeviceSignature, OsppError> {
        let signature = self.key.sign(message)?;
        crate::device_key::verify_device_signature(&self.key_id, message, &signature)?;
        Ok(signature)
    }
}

/// A `0600` file holding the software device key, created with `O_EXCL` and atomically renamed.
///
/// The path is injected; this adapter never consults `$XDG_RUNTIME_DIR`, `$HOME`, or any other
/// ambient value. A file with group/other permission bits fails closed rather than being trusted.
#[derive(Debug, Clone)]
pub struct FileDeviceKeyStore {
    path: PathBuf,
}

impl FileDeviceKeyStore {
    /// Use `path` for the software key.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The injected path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_existing(&self) -> Result<Option<SoftwareP256Key>, OsppError> {
        let raw = match fs::read(&self.path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(store_io(&self.path, error)),
        };
        let metadata = fs::metadata(&self.path).map_err(|error| store_io(&self.path, error))?;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(invalid_store(format!(
                "device key {} is not owner-only (mode {:o})",
                self.path.display(),
                metadata.permissions().mode() & 0o777
            )));
        }
        let bytes: [u8; 32] = raw.as_slice().try_into().map_err(|_| {
            invalid_store(format!(
                "device key {} must contain exactly 32 scalar bytes",
                self.path.display()
            ))
        })?;
        SoftwareP256Key::from_scalar_bytes(bytes).map(Some)
    }

    fn write_atomic(&self, key: &SoftwareP256Key) -> Result<(), OsppError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| store_io(parent, error))?;
        }
        let temp = temp_path(&self.path);
        if temp.exists() {
            fs::remove_file(&temp).map_err(|error| store_io(&temp, error))?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|error| store_io(&temp, error))?;
        file.write_all(&key.scalar)
            .and_then(|()| file.sync_all())
            .map_err(|error| store_io(&temp, error))?;
        drop(file);
        fs::rename(&temp, &self.path).map_err(|error| store_io(&self.path, error))
    }
}

impl DeviceKeyStore for FileDeviceKeyStore {
    fn load_or_create_p256(
        &self,
        entropy: &dyn EntropySource,
    ) -> Result<SoftwareP256Key, OsppError> {
        if let Some(key) = self.read_existing()? {
            return Ok(key);
        }
        let key = SoftwareP256Key::generate(entropy)?;
        self.write_atomic(&key)?;
        Ok(key)
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

fn store_io(path: &Path, error: std::io::Error) -> OsppError {
    OsppError::DeviceKeyStore {
        path: path.display().to_string(),
        message: error.to_string(),
    }
}

fn invalid_store(message: impl Into<String>) -> OsppError {
    OsppError::DeviceKeyStore {
        path: "<software key>".to_owned(),
        message: message.into(),
    }
}
