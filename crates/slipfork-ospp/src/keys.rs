//! Per-subscription `hpke-p256` recipient keys and RFC 9180 secret opening.
//!
//! Ciphersuite: Base mode, `DHKEM(P-256, HKDF-SHA256)`, `HKDF-SHA256`,
//! `AES-256-GCM`; secret `info = "ospp-v1-secret"`; `aad = "<component>:<env_key>"`.
//! The private scalar lives only in memory for one subscription and is zeroized
//! by `hpke` on drop; nothing here reads the filesystem or the environment.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hpke::aead::AesGcm256;
use hpke::kdf::HkdfSha256;
use hpke::kem::{DhP256HkdfSha256, Kem};
use hpke::{Deserializable, OpModeR, Serializable, setup_receiver};

use crate::error::OsppError;

/// `info` for per-variable secret sealing.
pub const SECRET_INFO: &[u8] = b"ospp-v1-secret";
/// `info` for the stateful dump stream.
pub const DUMP_INFO: &[u8] = b"ospp-v1-dump";
/// Uncompressed P-256 encapsulated-key header length.
pub const KEM_ENCODED_KEY_BYTES: usize = 65;
/// AES-256-GCM authentication tag length.
pub const AEAD_TAG_BYTES: usize = 16;

type PrivateKey = <DhP256HkdfSha256 as Kem>::PrivateKey;
type EncappedKey = <DhP256HkdfSha256 as Kem>::EncappedKey;
/// Stateful HPKE recipient context used to open a dump stream frame by frame.
pub type ReceiverContext = hpke::aead::AeadCtxR<AesGcm256, HkdfSha256, DhP256HkdfSha256>;

/// One ephemeral `hpke-p256` subscription keypair, held strictly in memory.
pub struct SubscriptionKey {
    private: PrivateKey,
    public_key_hex: String,
}

impl SubscriptionKey {
    /// Draw a fresh P-256 recipient keypair from the operating-system random source.
    pub fn generate() -> Result<Self, OsppError> {
        loop {
            let mut scalar = [0u8; 32];
            getrandom::fill(&mut scalar).map_err(|error| OsppError::Entropy(error.to_string()))?;
            if let Ok(private) = PrivateKey::from_bytes(&scalar) {
                let public = DhP256HkdfSha256::sk_to_pk(&private);
                return Ok(Self {
                    private,
                    public_key_hex: hex::encode(public.to_bytes()),
                });
            }
        }
    }

    /// Build a recipient keypair from a known 32-byte scalar (pinned tests).
    pub fn from_scalar_bytes(bytes: &[u8]) -> Result<Self, OsppError> {
        let private = PrivateKey::from_bytes(bytes)
            .map_err(|_| OsppError::InvalidKey("hpke scalar".to_owned()))?;
        let public = DhP256HkdfSha256::sk_to_pk(&private);
        Ok(Self {
            private,
            public_key_hex: hex::encode(public.to_bytes()),
        })
    }

    /// Uncompressed SEC1 P-256 public key, 130 lowercase hex digits.
    pub fn public_key_hex(&self) -> &str {
        &self.public_key_hex
    }

    /// Open one `enc || ciphertext` standard-base64 secret for `<component>:<env_key>`.
    pub fn open_secret(
        &self,
        component: &str,
        env_key: &str,
        encoded: &str,
    ) -> Result<String, OsppError> {
        let aad = secret_aad(component, env_key);
        let raw = STANDARD
            .decode(encoded)
            .map_err(|_| OsppError::Hpke("secret is not standard base64".to_owned()))?;
        if raw.len() < KEM_ENCODED_KEY_BYTES + AEAD_TAG_BYTES {
            return Err(OsppError::Hpke("secret ciphertext is too short".to_owned()));
        }
        let (enc, ciphertext) = raw.split_at(KEM_ENCODED_KEY_BYTES);
        let encapped = EncappedKey::from_bytes(enc)
            .map_err(|_| OsppError::Hpke("secret enc header is invalid".to_owned()))?;
        let mut context = setup_receiver::<AesGcm256, HkdfSha256, DhP256HkdfSha256>(
            &OpModeR::Base,
            &self.private,
            &encapped,
            SECRET_INFO,
        )
        .map_err(|error| OsppError::Hpke(error.to_string()))?;
        let plaintext = context
            .open(ciphertext, aad.as_bytes())
            .map_err(|_| OsppError::Hpke("secret failed to open (wrong aad or key)".to_owned()))?;
        String::from_utf8(plaintext)
            .map_err(|_| OsppError::Hpke("secret is not valid UTF-8".to_owned()))
    }

    /// Build a stateful `SetupBaseR` receiver context for one dump stream.
    pub fn dump_context(
        &self,
        encoded_header: &[u8],
        subscription_id: &str,
        resource: &str,
    ) -> Result<(ReceiverContext, String), OsppError> {
        let encapped = EncappedKey::from_bytes(encoded_header)
            .map_err(|_| OsppError::Hpke("dump enc header is invalid".to_owned()))?;
        let context = setup_receiver::<AesGcm256, HkdfSha256, DhP256HkdfSha256>(
            &OpModeR::Base,
            &self.private,
            &encapped,
            DUMP_INFO,
        )
        .map_err(|error| OsppError::Hpke(error.to_string()))?;
        Ok((context, dump_aad(subscription_id, resource)))
    }
}

impl std::fmt::Debug for SubscriptionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubscriptionKey")
            .field("public_key_hex", &self.public_key_hex)
            .finish_non_exhaustive()
    }
}

/// `aad` binding a sealed secret to its component and variable name.
pub fn secret_aad(component: &str, env_key: &str) -> String {
    format!("{component}:{env_key}")
}

/// `aad` binding a sealed dump frame to its subscription and resource.
pub fn dump_aad(subscription_id: &str, resource: &str) -> String {
    format!("{subscription_id}:{resource}")
}
