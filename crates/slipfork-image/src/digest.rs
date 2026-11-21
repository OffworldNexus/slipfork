//! Streaming `sha256` helpers shared by manifest and layer verification.

use std::io::{self, Read};

use sha2::{Digest, Sha256};

/// Wraps a reader and hashes every byte that passes through it.
pub(crate) struct DigestReader<R> {
    inner: R,
    hasher: Sha256,
}

impl<R: Read> DigestReader<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    /// The `sha256:<hex>` digest of everything read so far.
    pub(crate) fn digest(&self) -> String {
        format!("sha256:{}", hex(&self.hasher.clone().finalize()))
    }
}

impl<R: Read> Read for DigestReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.hasher.update(&buf[..read]);
        Ok(read)
    }
}

/// Lowercase hex encoding.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The `sha256:<hex>` digest of an in-memory document.
pub(crate) fn sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{}", hex(&hasher.finalize()))
}
