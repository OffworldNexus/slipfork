//! The `DeviceKeySigner` port, its validated [`KeyId`] (`p256:<66 lowercase hex>`) and
//! [`DeviceSignature`] (raw 64-byte `r || s`) newtypes, the injected [`DeviceKeyStore`] that
//! yields private key material, and the single transcript verification helper every adapter uses.
//!
//! `ADR-0008` §1 is normative: a `KeyId` *is* the 33-byte SEC1-compressed P-256 public key, and a
//! signature is always the fixed-width `r || s` form — never a variable-length ASN.1 encoding.

use std::fmt;

use p256::ecdsa::VerifyingKey;
use p256::ecdsa::signature::hazmat::PrehashVerifier;
use sha2::{Digest, Sha256};

use crate::error::OsppError;
use crate::software_key::SoftwareP256Key;

/// Byte length of a P-256 SEC1-compressed public key.
pub const KEY_ID_BYTES: usize = 33;

/// Byte length of a raw P-256 `r || s` signature.
pub const SIGNATURE_BYTES: usize = 64;

/// Prefix that distinguishes a P-256 fingerprint from other algorithms.
pub const KEY_ID_PREFIX: &str = "p256:";

/// Hex character count of a P-256 fingerprint (33 bytes).
pub const KEY_ID_HEX_LEN: usize = KEY_ID_BYTES * 2;

/// Hex character count of a raw signature (64 bytes).
pub const SIGNATURE_HEX_LEN: usize = SIGNATURE_BYTES * 2;

/// A device-key identity: the SEC1-compressed P-256 public key as `p256:<66 lowercase hex>`.
///
/// Validation proves the bytes decode to a real point on P-256, so a malformed identity can never
/// reach the wire. The value is public by construction and safe to print; private key material
/// lives only in [`SoftwareP256Key`].
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyId {
    text: String,
    bytes: [u8; KEY_ID_BYTES],
}

impl KeyId {
    /// Parse and validate `p256:<66 lowercase hex>`.
    pub fn parse(raw: &str) -> Result<Self, OsppError> {
        let Some(hex_part) = raw.strip_prefix(KEY_ID_PREFIX) else {
            return Err(invalid_key_id("expected the `p256:` prefix"));
        };
        Self::from_hex(hex_part)
    }

    /// Validate 66 lowercase hex characters as a SEC1-compressed P-256 public key.
    pub fn from_hex(hex_part: &str) -> Result<Self, OsppError> {
        if hex_part.len() != KEY_ID_HEX_LEN {
            return Err(invalid_key_id(format!(
                "expected {KEY_ID_HEX_LEN} lowercase hex characters, got {}",
                hex_part.len()
            )));
        }
        if !hex_part.bytes().all(is_lower_hex_digit) {
            return Err(invalid_key_id("expected lowercase hex digits only"));
        }
        let mut bytes = [0u8; KEY_ID_BYTES];
        // The length and alphabet are checked above, so decode cannot fail.
        let _ = hex::decode_to_slice(hex_part, &mut bytes);
        Self::from_public_key_bytes(&bytes)
    }

    /// Validate 33 SEC1-compressed bytes and build the canonical text form.
    pub fn from_public_key_bytes(bytes: &[u8]) -> Result<Self, OsppError> {
        let bytes: [u8; KEY_ID_BYTES] = bytes.try_into().map_err(|_| {
            invalid_key_id(format!(
                "expected {KEY_ID_BYTES} SEC1-compressed public-key bytes"
            ))
        })?;
        if !matches!(bytes[0], 0x02 | 0x03) {
            return Err(invalid_key_id(
                "expected a SEC1-compressed point (prefix 0x02 or 0x03)",
            ));
        }
        VerifyingKey::from_sec1_bytes(&bytes)
            .map_err(|_| invalid_key_id("bytes are not a point on P-256"))?;
        Ok(Self {
            text: format!("{KEY_ID_PREFIX}{}", hex::encode(bytes)),
            bytes,
        })
    }

    /// The canonical `p256:<66 lowercase hex>` wire spelling.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The raw 33 SEC1-compressed public-key bytes.
    pub const fn public_key_bytes(&self) -> &[u8; KEY_ID_BYTES] {
        &self.bytes
    }

    /// The decoded P-256 verification key.
    pub fn verifying_key(&self) -> Result<VerifyingKey, OsppError> {
        VerifyingKey::from_sec1_bytes(&self.bytes)
            .map_err(|_| invalid_key_id("bytes are not a point on P-256"))
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl fmt::Debug for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyId({})", self.text)
    }
}

/// A raw fixed-width ECDSA P-256 signature (`r || s`, 64 bytes).
///
/// Construction rejects every other encoding, including ASN.1 DER, by length and alphabet; there
/// is deliberately no conversion from an encoding that `ospp-v1` does not define.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DeviceSignature([u8; SIGNATURE_BYTES]);

impl DeviceSignature {
    /// Wrap exactly 64 raw big-endian `r || s` bytes.
    pub const fn from_raw(bytes: [u8; SIGNATURE_BYTES]) -> Self {
        Self(bytes)
    }

    /// Parse 128 lowercase hex characters as a raw signature.
    pub fn from_hex(hex_part: &str) -> Result<Self, OsppError> {
        if hex_part.len() != SIGNATURE_HEX_LEN {
            return Err(invalid_signature(format!(
                "expected a raw {SIGNATURE_BYTES}-byte signature ({SIGNATURE_HEX_LEN} hex \
                 characters), got {} characters",
                hex_part.len()
            )));
        }
        if !hex_part.bytes().all(is_lower_hex_digit) {
            return Err(invalid_signature("expected lowercase hex digits only"));
        }
        let mut bytes = [0u8; SIGNATURE_BYTES];
        let _ = hex::decode_to_slice(hex_part, &mut bytes);
        Ok(Self(bytes))
    }

    /// The raw 64 bytes.
    pub const fn as_bytes(&self) -> &[u8; SIGNATURE_BYTES] {
        &self.0
    }

    /// Lowercase hex wire form.
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Debug for DeviceSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceSignature({})", self.to_hex())
    }
}

/// A synchronous, thread-safe device-key signing port (`ADR-0008` §1).
///
/// Implementations hash `message` with SHA-256 once and sign the digest; they never expose the
/// private key and never reuse `ospp-v1`'s nonces.
pub trait DeviceKeySigner: Send + Sync {
    /// The public fingerprint this signer signs with.
    fn key_id(&self) -> &KeyId;

    /// A stable, human-readable adapter label (for example `tpmd:sealed` or `software:file-0600`).
    fn backend_label(&self) -> &'static str;

    /// Sign `message` and return the raw 64-byte `r || s` form.
    fn sign(&self, message: &[u8]) -> Result<DeviceSignature, OsppError>;
}

/// A store that owns the software device key at an injected path.
///
/// The trait exists so `SoftwareP256Signer` never reads ambient paths or environment variables:
/// the Composition Root decides where the `0600` key lives.
pub trait DeviceKeyStore: Send + Sync {
    /// Load the existing P-256 key or atomically create one using `entropy`.
    fn load_or_create_p256(
        &self,
        entropy: &dyn slipfork_snow::EntropySource,
    ) -> Result<SoftwareP256Key, OsppError>;
}

/// Verify `signature` over `message` against `key_id`.
///
/// The message is SHA-256-hashed exactly once, matching `tpmd`'s contract, so a signature minted by
/// hardware and one minted in software verify identically.
pub fn verify_device_signature(
    key_id: &KeyId,
    message: &[u8],
    signature: &DeviceSignature,
) -> Result<(), OsppError> {
    let verifying_key = key_id.verifying_key()?;
    let digest = Sha256::digest(message);
    let parsed = p256::ecdsa::Signature::from_slice(signature.as_bytes())
        .map_err(|_| invalid_signature("bytes are not a valid raw P-256 signature"))?;
    verifying_key
        .verify_prehash(&digest, &parsed)
        .map_err(|_| OsppError::SignatureVerification {
            backend: "device-key",
            key_id: key_id.as_str().to_owned(),
        })
}

/// `true` for `0-9` and `a-f`, the only characters `ospp-v1` accepts in key ids and signatures.
const fn is_lower_hex_digit(byte: u8) -> bool {
    matches!(byte, b'0'..=b'9' | b'a'..=b'f')
}

fn invalid_key_id(reason: impl Into<String>) -> OsppError {
    OsppError::InvalidKeyId {
        reason: reason.into(),
    }
}

fn invalid_signature(reason: impl Into<String>) -> OsppError {
    OsppError::InvalidSignature {
        reason: reason.into(),
    }
}
