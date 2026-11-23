//! Secret-tainted values that never leak through `Debug` or `Display`.

use std::fmt;

/// A secret string: redacted in formatting and zeroed when dropped.
#[derive(Clone, Default)]
pub struct SecretValue(String);

impl SecretValue {
    /// Wraps a resolved secret.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Exposes the raw secret to the one call that injects it into a process or wire frame.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        // SAFETY: overwriting initialized UTF-8 bytes with ASCII zero bytes keeps the
        // buffer valid before the allocation is freed.
        unsafe { self.0.as_bytes_mut().fill(0) }
    }
}
