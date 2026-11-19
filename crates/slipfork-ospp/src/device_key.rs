//! P-256 machine authentication keys: the injected `DeviceKeySigner` port, the
//! `0600`-file `DeviceKeyStore` adapter, and raw `r || s` signatures.
//!
//! The machine key authenticates the machine (`auth_response`) and proves
//! possession of the key being enrolled (`pair_begin`); it is never the
//! per-subscription secret-encryption key (`crate::keys`).

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::elliptic_curve::sec1::ToSec1Point;
use slipfork_snow::EntropySource;

use crate::error::OsppError;

/// Raw fixed-width ECDSA P-256 signature length (`r || s`, 32 + 32 bytes).
pub const SIGNATURE_BYTES: usize = 64;
/// Hex length of [`SIGNATURE_BYTES`].
pub const SIGNATURE_HEX_LEN: usize = 2 * SIGNATURE_BYTES;
/// Compressed SEC1 P-256 public-key hex length (33 bytes).
pub const PUBLIC_KEY_HEX_LEN: usize = 66;

/// Injected `0600` machine-authentication key path owned by `slipforkd`'s
/// Composition Root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceKeyPath(pub PathBuf);

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

    /// Parse exactly 128 hex digits.
    pub fn from_hex(value: &str) -> Result<Self, OsppError> {
        if value.len() != SIGNATURE_HEX_LEN {
            return Err(OsppError::InvalidSignature);
        }
        let bytes = hex::decode(value).map_err(|_| OsppError::InvalidSignature)?;
        let raw: [u8; SIGNATURE_BYTES] =
            bytes.try_into().map_err(|_| OsppError::InvalidSignature)?;
        Ok(Self(raw))
    }
}

impl fmt::Debug for DeviceSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Public wire material, but keep the rendering compact.
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

    /// Draw a fresh scalar from the injected entropy source.
    pub fn generate(entropy: &dyn EntropySource) -> Result<Self, OsppError> {
        loop {
            let mut bytes = [0u8; 32];
            entropy.fill_bytes(&mut bytes)?;
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

/// Machine-authentication key port: `public_key_hex()` is the compressed SEC1
/// 33-byte P-256 public key; `sign()` computes ECDSA P-256 + SHA-256 over the
/// caller-supplied transcript. Used for `p256` auth and pairing only, never for
/// secret encryption.
pub trait DeviceKeySigner: Send + Sync {
    /// Compressed SEC1 P-256 public key, 66 lowercase hex digits.
    fn public_key_hex(&self) -> &str;

    /// ECDSA P-256 + SHA-256 over `message`, raw `r || s`.
    fn sign(&self, message: &[u8]) -> Result<DeviceSignature, OsppError>;
}

/// Injected machine-key store port (`0600` file in production).
pub trait DeviceKeyStore: Send + Sync {
    /// Load the stored key or create and persist a fresh one.
    fn load_or_create_p256(
        &self,
        entropy: &dyn EntropySource,
    ) -> Result<SoftwareP256Key, OsppError>;
}

/// `0600` file-backed [`DeviceKeyStore`]: one 64-hex-digit scalar at the
/// injected path, created exclusively on first use.
pub struct FileDeviceKeyStore {
    path: DeviceKeyPath,
}

impl FileDeviceKeyStore {
    /// Bind the store to the injected path.
    pub const fn new(path: DeviceKeyPath) -> Self {
        Self { path }
    }

    /// The injected path.
    pub fn path(&self) -> &DeviceKeyPath {
        &self.path
    }

    fn read(&self) -> Result<Option<SoftwareP256Key>, OsppError> {
        match fs::read_to_string(&self.path.0) {
            Ok(raw) => {
                let trimmed = raw.trim();
                let bytes = hex::decode(trimmed).map_err(|_| OsppError::KeyStore {
                    path: self.path.0.display().to_string(),
                    message: "stored key is not hex".to_owned(),
                })?;
                SoftwareP256Key::from_scalar_bytes(&bytes).map(Some)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(OsppError::KeyStore {
                path: self.path.0.display().to_string(),
                message: error.to_string(),
            }),
        }
    }

    fn persist(&self, key: &SoftwareP256Key) -> Result<(), OsppError> {
        let parent = self.path.0.parent().ok_or_else(|| OsppError::KeyStore {
            path: self.path.0.display().to_string(),
            message: "key path has no parent directory".to_owned(),
        })?;
        fs::create_dir_all(parent).map_err(|error| OsppError::KeyStore {
            path: parent.display().to_string(),
            message: error.to_string(),
        })?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&self.path.0)
            .map_err(|error| OsppError::KeyStore {
                path: self.path.0.display().to_string(),
                message: error.to_string(),
            })?;
        let scalar = hex::encode(key.signing.to_bytes());
        file.write_all(scalar.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .map_err(|error| OsppError::KeyStore {
                path: self.path.0.display().to_string(),
                message: error.to_string(),
            })
    }
}

impl DeviceKeyStore for FileDeviceKeyStore {
    fn load_or_create_p256(
        &self,
        entropy: &dyn EntropySource,
    ) -> Result<SoftwareP256Key, OsppError> {
        if let Some(key) = self.read()? {
            return Ok(key);
        }
        let key = SoftwareP256Key::generate(entropy)?;
        if let Err(error) = self.persist(&key) {
            // Another composition-root writer may have won the exclusive create.
            if let Some(existing) = self.read()? {
                return Ok(existing);
            }
            return Err(error);
        }
        Ok(key)
    }
}

use std::os::unix::fs::OpenOptionsExt;

/// In-memory [`DeviceKeySigner`] over a [`SoftwareP256Key`].
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

    /// Load (or create) the machine key through the injected store.
    pub fn from_store(
        store: &dyn DeviceKeyStore,
        entropy: &dyn EntropySource,
    ) -> Result<Self, OsppError> {
        Ok(Self::new(store.load_or_create_p256(entropy)?))
    }
}

impl fmt::Debug for SoftwareP256Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SoftwareP256Signer")
            .field("public_key_hex", &self.public_key_hex)
            .finish_non_exhaustive()
    }
}

impl DeviceKeySigner for SoftwareP256Signer {
    fn public_key_hex(&self) -> &str {
        &self.public_key_hex
    }

    fn sign(&self, message: &[u8]) -> Result<DeviceSignature, OsppError> {
        Ok(self.key.sign_message(message))
    }
}

/// Verify a raw `r || s` ECDSA P-256 + SHA-256 signature against a compressed
/// SEC1 public key.
pub fn verify_device_signature(
    public_key_hex: &str,
    message: &[u8],
    signature: &DeviceSignature,
) -> Result<(), OsppError> {
    let point = hex::decode(public_key_hex)
        .map_err(|_| OsppError::InvalidKey("public key is not hex".to_owned()))?;
    let verifying =
        VerifyingKey::from_sec1_bytes(&point).map_err(|_| OsppError::InvalidSignature)?;
    let signature =
        Signature::from_slice(signature.as_bytes()).map_err(|_| OsppError::InvalidSignature)?;
    verifying
        .verify(message, &signature)
        .map_err(|_| OsppError::InvalidSignature)
}
