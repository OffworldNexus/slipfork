//! P-256 machine authentication keys: the concrete `SoftwareP256Signer`, the
//! `0600`-file identity store, and raw `r || s` signatures.
//!
//! The machine key authenticates the machine (`auth_response`) and proves
//! possession of the key being enrolled (`pair_begin`); it is never the
//! per-subscription secret-encryption key (`crate::keys`). Key generation uses
//! the operating-system random source directly — no globals, no injected
//! entropy trait, no process-wide cache.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToSec1Point;

use crate::error::OsppError;
use crate::vault::PRIVATE_FILE_MODE;

/// Raw fixed-width ECDSA P-256 signature length (`r || s`, 32 + 32 bytes).
pub const SIGNATURE_BYTES: usize = 64;
/// Hex length of [`SIGNATURE_BYTES`].
pub const SIGNATURE_HEX_LEN: usize = 2 * SIGNATURE_BYTES;
/// Compressed SEC1 P-256 public-key hex length (33 bytes).
pub const PUBLIC_KEY_HEX_LEN: usize = 66;

/// A raw fixed-width ECDSA P-256 signature (`r || s`), never ASN.1 DER.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceSignature([u8; SIGNATURE_BYTES]);

impl DeviceSignature {
    /// Wrap raw `r || s` bytes.
    pub const fn from_raw(bytes: [u8; SIGNATURE_BYTES]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw 64 bytes.
    pub const fn as_bytes(&self) -> &[u8; SIGNATURE_BYTES] {
        &self.0
    }

    /// Lowercase hex.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Debug for DeviceSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceSignature({}..)", &self.to_hex()[..16])
    }
}

/// A private 32-byte P-256 machine-authentication scalar.
///
/// `Debug` is redacted and the scalar is never rendered; the underlying
/// `p256` signing key zeroizes its secret on drop.
pub struct SoftwareP256Key {
    signing: SigningKey,
}

impl SoftwareP256Key {
    /// Parse a 32-byte scalar in the P-256 group order range.
    pub fn from_scalar_bytes(bytes: &[u8]) -> Result<Self, OsppError> {
        let signing = SigningKey::from_slice(bytes)
            .map_err(|_| OsppError::InvalidKey("scalar".to_owned()))?;
        Ok(Self { signing })
    }

    /// Draw a fresh scalar from the operating-system random source.
    pub fn generate() -> Result<Self, OsppError> {
        loop {
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes).map_err(|error| OsppError::Entropy(error.to_string()))?;
            if let Ok(key) = Self::from_scalar_bytes(&bytes) {
                return Ok(key);
            }
        }
    }

    /// The compressed SEC1 33-byte public key as 66 lowercase hex digits.
    pub fn public_key_hex(&self) -> String {
        let point = self.signing.verifying_key().as_affine().to_sec1_point(true);
        hex::encode(point.as_bytes())
    }

    fn sign_message(&self, message: &[u8]) -> DeviceSignature {
        let signature: Signature = self.signing.sign(message);
        let mut raw = [0u8; SIGNATURE_BYTES];
        raw.copy_from_slice(signature.to_bytes().as_slice());
        DeviceSignature(raw)
    }
}

impl fmt::Debug for SoftwareP256Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SoftwareP256Key(<redacted>)")
    }
}

/// `0600` file-backed machine identity: one 64-hex-digit scalar at the
/// caller-injected path, created exclusively on first use.
pub struct FileDeviceKeyStore {
    path: PathBuf,
}

impl FileDeviceKeyStore {
    /// Bind the store to the injected path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The injected path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the stored key or create and persist a fresh one.
    pub fn load_or_create(&self) -> Result<SoftwareP256Key, OsppError> {
        if let Some(key) = self.read()? {
            return Ok(key);
        }
        let key = SoftwareP256Key::generate()?;
        if let Err(error) = self.persist(&key) {
            if let Some(existing) = self.read()? {
                return Ok(existing);
            }
            return Err(error);
        }
        Ok(key)
    }

    fn read(&self) -> Result<Option<SoftwareP256Key>, OsppError> {
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(self.store_error(&self.path, error.to_string())),
        };
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(
                self.store_error(&self.path, "device key file mode must be 0600".to_owned())
            );
        }
        let raw = fs::read_to_string(&self.path)
            .map_err(|error| self.store_error(&self.path, error.to_string()))?;
        let bytes = hex::decode(raw.trim())
            .map_err(|_| self.store_error(&self.path, "stored key is not hex".to_owned()))?;
        SoftwareP256Key::from_scalar_bytes(&bytes).map(Some)
    }

    fn persist(&self, key: &SoftwareP256Key) -> Result<(), OsppError> {
        let parent = self.path.parent().ok_or_else(|| {
            self.store_error(&self.path, "key path has no parent directory".to_owned())
        })?;
        fs::create_dir_all(parent).map_err(|error| self.store_error(parent, error.to_string()))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(PRIVATE_FILE_MODE)
            .open(&self.path)
            .map_err(|error| self.store_error(&self.path, error.to_string()))?;
        let scalar = hex::encode(key.signing.to_bytes());
        file.write_all(scalar.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .map_err(|error| self.store_error(&self.path, error.to_string()))
    }

    fn store_error(&self, path: &Path, message: String) -> OsppError {
        OsppError::KeyStore {
            path: path.display().to_string(),
            message,
        }
    }
}

/// In-memory machine key signer over a [`SoftwareP256Key`].
pub struct SoftwareP256Signer {
    key: SoftwareP256Key,
    public_key_hex: String,
}

impl SoftwareP256Signer {
    /// Wrap an already-loaded private key.
    pub fn new(key: SoftwareP256Key) -> Self {
        let public_key_hex = key.public_key_hex();
        Self {
            key,
            public_key_hex,
        }
    }

    /// Load (or create) the machine key through the injected `0600` store.
    pub fn from_store(store: &FileDeviceKeyStore) -> Result<Self, OsppError> {
        Ok(Self::new(store.load_or_create()?))
    }
}

impl fmt::Debug for SoftwareP256Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SoftwareP256Signer")
            .field("public_key_hex", &self.public_key_hex)
            .finish_non_exhaustive()
    }
}

impl SoftwareP256Signer {
    /// Compressed SEC1 P-256 public key, 66 lowercase hex digits.
    pub fn public_key_hex(&self) -> &str {
        &self.public_key_hex
    }

    /// ECDSA P-256 + SHA-256 over `message`, raw `r || s`.
    pub fn sign(&self, message: &[u8]) -> Result<DeviceSignature, OsppError> {
        Ok(self.key.sign_message(message))
    }
}
