//! Fragment-only browser pairing, the client-side ALTCHA solver, and the public
//! `<state_dir>/ospp.json` record.
//!
//! The client appends `#handle=<handle>` to the server-supplied `pair_handle.url`
//! and hands the validated [`LivePairingUrl`] to the caller's `on_handle`
//! callback while the handshake waits for approval. The persisted record holds only
//! `{ endpoint, account, machine }` — never a secret or key byte.

use std::fmt;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::endpoint::OsppEndpoint;
use crate::error::OsppError;
use crate::vault::{read_private, write_private};

/// Handle hex length (32 bytes).
pub const HANDLE_HEX_LEN: usize = 64;
/// Upper bound on the ALTCHA search space we are willing to brute-force.
pub const MAX_ALTCHA_NUMBER: u64 = 5_000_000;

/// Observable pairing progress, so the CLI can render a live status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairPhase {
    /// Solving the server's ALTCHA proof of work.
    SolvingAltcha,
    /// The browser URL was opened; waiting for the human verdict.
    AwaitingApproval,
    /// The server confirmed the enrolled machine.
    Approved,
}

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
        let zeros = "0".repeat(self.url.len() - self.handle_offset);
        self.url.replace_range(self.handle_offset.., &zeros);
    }
}

/// The public pairing record persisted at `<state_dir>/ospp.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingState {
    /// Settled `ws(s)://` OSPP endpoint.
    pub endpoint: String,
    /// Authenticated account name.
    pub account: String,
    /// Approved machine name.
    pub machine: String,
}

impl PairingState {
    fn validate(&self) -> Result<(), OsppError> {
        OsppEndpoint::parse(&self.endpoint)
            .map_err(|error| OsppError::InvalidPairingUrl(error.to_string()))?;
        if self.account.is_empty() || self.machine.is_empty() {
            return Err(OsppError::InvalidPairingUrl(
                "pairing account and machine must be non-empty".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Caller-injected `ospp.json` store.
pub struct PairingStore {
    path: PathBuf,
}

impl PairingStore {
    /// Bind the store to a caller-injected state directory (`<state_dir>/ospp.json`).
    pub fn new(state_dir: impl AsRef<Path>) -> Self {
        Self::at(state_dir.as_ref().join("ospp.json"))
    }

    /// Bind the store to one caller-injected record path.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The injected record path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the settled pairing, if one exists.
    pub fn load(&self) -> Result<Option<PairingState>, OsppError> {
        let Some(bytes) = read_private(&self.path)? else {
            return Ok(None);
        };
        let state: PairingState =
            serde_json::from_slice(&bytes).map_err(|error| OsppError::State {
                path: self.path.display().to_string(),
                message: error.to_string(),
            })?;
        state.validate()?;
        Ok(Some(state))
    }

    /// Atomically persist the settled pairing as one `0600` record.
    pub fn save(&self, state: &PairingState) -> Result<(), OsppError> {
        state.validate()?;
        let bytes = serde_json::to_vec_pretty(state).map_err(|error| OsppError::State {
            path: self.path.display().to_string(),
            message: error.to_string(),
        })?;
        write_private(&self.path, &bytes)
    }
}

impl fmt::Debug for PairingStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
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
    let challenge = object
        .get("challenge")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| OsppError::Protocol("altcha challenge hash is invalid".to_owned()))?;
    if hex::decode(challenge).is_err() {
        return Err(OsppError::Protocol(
            "altcha challenge hash is invalid".to_owned(),
        ));
    }
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
        .find(|number| altcha_challenge_hash(salt, *number) == challenge)
        .ok_or_else(|| OsppError::Protocol("altcha proof-of-work was not solved".to_owned()))?;

    let mut solution = object.clone();
    solution.insert("number".to_owned(), serde_json::Value::from(number));
    let json = serde_json::to_vec(&serde_json::Value::Object(solution))
        .map_err(|error| OsppError::Protocol(error.to_string()))?;
    Ok(STANDARD.encode(json))
}

/// The exact client-side `altcha` challenge hash (`sha256(salt + number)`).
pub fn altcha_challenge_hash(salt: &str, number: u64) -> String {
    hex::encode(Sha256::digest(format!("{salt}{number}").as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

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
        assert!(LivePairingUrl::with_handle("https://ci.example.com/pair#x", &handle).is_err());
    }

    #[test]
    fn altcha_solver_finds_the_number_inside_the_search_bound() {
        let salt = "0f1e2d3c";
        let challenge = serde_json::json!({
            "algorithm": "SHA-256",
            "challenge": altcha_challenge_hash(salt, 7),
            "salt": salt,
            "maxnumber": 64,
        });
        let solution = solve_altcha(&challenge).expect("solvable");
        let decoded = STANDARD.decode(solution).expect("standard base64 solution");
        let payload: serde_json::Value = serde_json::from_slice(&decoded).expect("json");
        assert_eq!(payload["number"], 7);
        assert_eq!(payload["salt"], salt);
    }

    #[test]
    fn pairing_state_round_trips_through_a_0600_record() {
        let directory = tempfile::TempDir::new_in("/var/tmp").expect("disk temp dir");
        let store = PairingStore::new(directory.path());
        assert!(store.load().expect("empty").is_none());
        let state = PairingState {
            endpoint: "wss://ci.example.com/ospp".to_owned(),
            account: "alice".to_owned(),
            machine: "dev-laptop".to_owned(),
        };
        store.save(&state).expect("save");
        assert_eq!(store.load().expect("load"), Some(state));
        assert_eq!(
            std::fs::metadata(store.path())
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            crate::vault::PRIVATE_FILE_MODE
        );
    }
}
