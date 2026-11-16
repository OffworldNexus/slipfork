//! Crate-private lowercase-hex helpers shared by the handshake, key ids, and pairing.
//!
//! `ospp-v1` accepts lowercase hex only, so one alphabet predicate and one set of encoding
//! helpers exist here instead of in every module that renders a nonce, signature, key id, or
//! handle.

use crate::error::OsppError;

/// `true` for `0-9` and `a-f`, the only characters `ospp-v1` accepts in hex fields.
pub(crate) const fn is_lower_hex_digit(byte: u8) -> bool {
    matches!(byte, b'0'..=b'9' | b'a'..=b'f')
}

/// `true` for exactly 64 lowercase hex characters (a SHA-256 digest, nonce, or handle).
pub(crate) fn is_sha256_hex(raw: &str) -> bool {
    raw.len() == 64 && raw.bytes().all(is_lower_hex_digit)
}

/// Hex-encode `bytes` into a caller-owned fixed buffer.
pub(crate) fn encode_hex_into(
    bytes: &[u8],
    out: &mut [u8],
    phase: &'static str,
) -> Result<(), OsppError> {
    hex::encode_to_slice(bytes, out).map_err(|error| OsppError::Protocol {
        phase,
        message: format!("could not hex-encode {} bytes: {error}", bytes.len()),
    })
}

/// Hex-encode `bytes` into a fixed-size array.
pub(crate) fn hex_fixed<const N: usize>(
    bytes: &[u8],
    phase: &'static str,
) -> Result<[u8; N], OsppError> {
    let mut out = [0u8; N];
    encode_hex_into(bytes, &mut out, phase)?;
    Ok(out)
}

/// Borrow a hex encoding as `&str`; hex output is always ASCII.
pub(crate) fn hex_str<'a>(bytes: &'a [u8], phase: &'static str) -> Result<&'a str, OsppError> {
    std::str::from_utf8(bytes).map_err(|error| OsppError::Protocol {
        phase,
        message: format!("hex encoding was not UTF-8: {error}"),
    })
}
