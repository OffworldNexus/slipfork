//! Fragment-only browser pairing and the ALTCHA proof-of-work solver.
//!
//! The client never computes the pairing URL: it appends `#handle=<handle>` to the
//! server-supplied `pair_handle.url` and hands the result to the injected
//! [`BrowserOpener`]. The single-use handle never appears in a path or query.

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};
use slipfork_world::WorldOrigin;

use crate::error::OsppError;

/// Handle hex length (32 bytes).
pub const HANDLE_HEX_LEN: usize = 64;
/// Upper bound on the ALTCHA search space we are willing to brute-force.
pub const MAX_ALTCHA_NUMBER: u64 = 5_000_000;

/// A server-supplied pairing URL with the single-use handle in its fragment.
///
/// Built only by [`LivePairingUrl::with_handle`]; the handle bytes are zeroized on
/// `Drop` and `Debug` never prints them.
pub struct LivePairingUrl {
    url: String,
    handle_offset: usize,
}

impl LivePairingUrl {
    /// Append `#handle=<handle>` to the server-supplied `url`.
    pub fn with_handle(url: &str, handle: &str) -> Result<Self, OsppError> {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(OsppError::InvalidPairingUrl(
                "pairing URL must be http(s)".to_owned(),
            ));
        }
        if url.contains('#') {
            return Err(OsppError::InvalidPairingUrl(
                "server-supplied pairing URL already has a fragment".to_owned(),
            ));
        }
        if handle.len() != HANDLE_HEX_LEN || !handle.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(OsppError::InvalidPairingUrl(
                "pairing handle must be 32 bytes of hex".to_owned(),
            ));
        }
        let mut full = String::with_capacity(url.len() + 8 + handle.len());
        full.push_str(url);
        full.push_str("#handle=");
        let handle_offset = full.len();
        full.push_str(handle);
        Ok(Self {
            url: full,
            handle_offset,
        })
    }

    /// The full URL to open, including the fragment.
    pub fn as_str(&self) -> &str {
        &self.url
    }

    /// Consume the URL, returning the owned string.
    pub fn into_string(mut self) -> String {
        std::mem::take(&mut self.url)
    }
}

impl fmt::Debug for LivePairingUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "LivePairingUrl({}#handle=<redacted>)",
            &self.url[..self.handle_offset - 8]
        )
    }
}

impl Drop for LivePairingUrl {
    fn drop(&mut self) {
        // Overwrite the handle hex in place; the URL prefix carries no secret.
        let zeros = "0".repeat(self.url.len() - self.handle_offset);
        self.url.replace_range(self.handle_offset.., &zeros);
    }
}

/// Neutral browser-launch port.
///
/// The production `SystemBrowserOpener` lives in `crates/slipfork/src/browser.rs`
/// and is constructed from resolved environment inputs at the Composition Root,
/// keeping `slipfork-ospp` free of OS and environment access.
pub trait BrowserOpener: Send + Sync {
    /// Open the server-supplied `<url>#handle=<handle>` pairing page.
    fn open_pairing_url(&self, url: LivePairingUrl) -> Result<(), OsppError>;

    /// Open a world's validated `.localhost` origin.
    fn open_origin(&self, origin: &WorldOrigin) -> Result<(), OsppError>;
}

/// Solve the server's ALTCHA proof-of-work challenge and return the opaque
/// solution payload expected by `pair_begin.altcha`.
pub fn solve_altcha(challenge: &serde_json::Value) -> Result<String, OsppError> {
    let object = challenge
        .as_object()
        .ok_or_else(|| OsppError::Protocol("altcha challenge must be an object".to_owned()))?;
    let algorithm = object
        .get("algorithm")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| OsppError::Protocol("altcha algorithm is missing".to_owned()))?;
    if algorithm != "SHA-256" {
        return Err(OsppError::Protocol(format!(
            "unsupported altcha algorithm {algorithm}"
        )));
    }
    let target = object
        .get("challenge")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| hex::decode(value).ok())
        .ok_or_else(|| OsppError::Protocol("altcha challenge hash is invalid".to_owned()))?;
    let salt = object
        .get("salt")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| OsppError::Protocol("altcha salt is missing".to_owned()))?;
    let maxnumber = object
        .get("maxnumber")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1_000_000)
        .min(MAX_ALTCHA_NUMBER);

    let number = (0..=maxnumber)
        .find(|number| {
            let digest = Sha256::digest(format!("{salt}{number}").as_bytes());
            digest.as_slice() == target.as_slice()
        })
        .ok_or_else(|| OsppError::Protocol("altcha proof-of-work was not solved".to_owned()))?;

    let mut solution = object.clone();
    solution.insert("number".to_owned(), serde_json::Value::from(number));
    let json = serde_json::to_vec(&serde_json::Value::Object(solution))
        .map_err(|error| OsppError::Protocol(error.to_string()))?;
    Ok(STANDARD.encode(json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pairing_url_appends_the_handle_to_the_fragment() {
        let handle = "ab".repeat(32);
        let url = LivePairingUrl::with_handle("https://ci.example.com/pair", &handle)
            .expect("valid pairing url");
        assert_eq!(
            url.as_str(),
            format!("https://ci.example.com/pair#handle={handle}")
        );
        assert!(!format!("{url:?}").contains(&handle));
    }

    #[test]
    fn pairing_url_rejects_a_query_handle_or_bad_fragment() {
        assert!(
            LivePairingUrl::with_handle("https://ci.example.com/pair?h=1", &"ab".repeat(32))
                .is_ok()
        );
        assert!(
            LivePairingUrl::with_handle("https://ci.example.com/pair#x", &"ab".repeat(32)).is_err()
        );
        assert!(LivePairingUrl::with_handle("https://ci.example.com/pair", "not-hex").is_err());
    }

    #[test]
    fn altcha_solver_finds_the_number_and_preserves_the_payload() {
        let salt = "0f1e2d3c";
        let number = 7u64;
        let hash = hex::encode(Sha256::digest(format!("{salt}{number}").as_bytes()));
        let challenge = json!({
            "algorithm": "SHA-256",
            "challenge": hash,
            "salt": salt,
            "maxnumber": 64,
        });
        let solution = solve_altcha(&challenge).expect("solvable");
        let decoded = STANDARD.decode(solution).expect("base64");
        let payload: serde_json::Value = serde_json::from_slice(&decoded).expect("json");
        assert_eq!(payload["number"], 7);
        assert_eq!(payload["salt"], salt);
        assert_eq!(payload["challenge"], challenge["challenge"]);
    }

    #[test]
    fn altcha_solver_fails_closed_on_an_unsolvable_challenge() {
        let challenge = json!({
            "algorithm": "SHA-256",
            "challenge": "00".repeat(32),
            "salt": "xyz",
            "maxnumber": 8,
        });
        assert!(solve_altcha(&challenge).is_err());
    }
}
