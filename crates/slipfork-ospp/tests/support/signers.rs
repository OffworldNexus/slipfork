//! Deterministic test doubles used by the protected `critical_ospp` suite.

use slipfork_ospp::software_key::{FileDeviceKeyStore, SoftwareP256Key};
use slipfork_ospp::{DeviceKeySigner, DeviceSignature, KeyId, SoftwareP256Signer};
use slipfork_snow::{EntropySource, ProviderError};

/// Entropy source that repeats one byte, so generated keys and nonces are reproducible.
#[derive(Debug, Clone, Copy)]
pub struct FixedEntropy {
    byte: u8,
}

impl FixedEntropy {
    /// Fill every requested byte with `byte`.
    pub const fn new(byte: u8) -> Self {
        Self { byte }
    }
}

impl EntropySource for FixedEntropy {
    fn fill_bytes(&self, buf: &mut [u8]) -> Result<(), ProviderError> {
        buf.fill(self.byte);
        Ok(())
    }
}

/// A signer that claims a real `key_id` but returns an all-zero signature.
///
/// The server must reject it as `bad_signature`; this proves the client surfaces the typed
/// refusal instead of retrying or downgrading.
pub struct MisSigningSigner<S: DeviceKeySigner> {
    inner: S,
}

impl<S: DeviceKeySigner> MisSigningSigner<S> {
    /// Wrap `inner` and keep its public identity.
    pub const fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S: DeviceKeySigner> DeviceKeySigner for MisSigningSigner<S> {
    fn key_id(&self) -> &KeyId {
        self.inner.key_id()
    }

    fn backend_label(&self) -> &'static str {
        self.inner.backend_label()
    }

    fn sign(&self, _message: &[u8]) -> Result<DeviceSignature, slipfork_ospp::OsppError> {
        Ok(DeviceSignature::from_raw([0u8; 64]))
    }
}

/// Build a software signer over a scratch `0600` key file with deterministic entropy.
pub fn software_signer() -> SoftwareP256Signer {
    let dir = tempfile::tempdir().expect("scratch dir");
    let store = FileDeviceKeyStore::new(dir.path().join("device.key"));
    SoftwareP256Signer::open(&store, &FixedEntropy::new(0x11)).expect("software signer")
}

/// Build a software key with a known scalar for redaction tests.
pub fn known_software_key() -> SoftwareP256Key {
    SoftwareP256Key::from_scalar_bytes([0x11; 32]).expect("valid scalar")
}
