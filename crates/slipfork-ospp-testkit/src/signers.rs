//! Deterministic test doubles used by the protected `critical_ospp` suite.

use slipfork_ospp::{
    DeviceKeySigner, DeviceSignature, OsppError, SoftwareP256Key, SoftwareP256Signer,
};
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

/// Deterministic splitmix64 entropy stream for the reference server's nonces,
/// handles, and dump tokens.
#[derive(Debug)]
pub struct SeededEntropy {
    state: std::sync::atomic::AtomicU64,
}

impl SeededEntropy {
    /// Seed the stream.
    pub fn new(seed: u64) -> Self {
        Self {
            state: std::sync::atomic::AtomicU64::new(seed),
        }
    }
}

impl EntropySource for SeededEntropy {
    fn fill_bytes(&self, buf: &mut [u8]) -> Result<(), ProviderError> {
        for chunk in buf.chunks_mut(8) {
            let mut z = self
                .state
                .fetch_add(0x9e37_79b9_7f4a_7c15, std::sync::atomic::Ordering::SeqCst)
                .wrapping_add(0x9e37_79b9_7f4a_7c15);
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            chunk.copy_from_slice(&z.to_le_bytes()[..chunk.len()]);
        }
        Ok(())
    }
}

/// A signer that claims a real public key but returns an all-zero signature.
///
/// The reference server must reject it with `bad_proof`; this proves the client
/// surfaces the typed refusal instead of retrying or downgrading.
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
    fn public_key_hex(&self) -> &str {
        self.inner.public_key_hex()
    }

    fn sign(&self, _message: &[u8]) -> Result<DeviceSignature, OsppError> {
        Ok(DeviceSignature::from_raw([0u8; 64]))
    }
}

/// Deterministic in-memory signer over the fixed scalar `0x11 * 32`.
pub fn software_signer() -> SoftwareP256Signer {
    SoftwareP256Signer::new(known_software_key())
}

/// The fixed scalar `0x11 * 32`, for redaction assertions.
pub fn known_software_key() -> SoftwareP256Key {
    SoftwareP256Key::from_scalar_bytes(&[0x11u8; 32]).expect("valid P-256 scalar")
}
