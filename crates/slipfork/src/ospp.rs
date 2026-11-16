//! The injected paired-`OSPP v1` session coordinate store (`ADR-0008` §1–§3).
//!
//! `slipforkd` persists only `{ host, port, tls, account }` after a successful
//! `slipfork auth login`; the device key already lives in the `tpmd`/`0600` software store and no
//! secret ever reaches this file. The daemon reads it back to lazily dial `/_ospp/v1/connect` and
//! to build the `LayeredSecretProvider<FileVaultProvider, OsppSecretProvider>` for `local` worlds.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use slipfork_edge::{DaemonError, DaemonErrorKind, EdgeError};

use crate::layout::ensure_private_dir;

/// The paired OSPP host coordinates (zero secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OsppSessionCoordinates {
    host: String,
    port: u16,
    tls: bool,
    account: String,
}

impl OsppSessionCoordinates {
    /// Build coordinates from an already-validated pairing URL and its approval.
    pub fn new(host: impl Into<String>, port: u16, tls: bool, account: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port,
            tls,
            account: account.into(),
        }
    }

    /// Lowercase ASCII hostname or IP address.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// TCP port of the OSPP host.
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Whether the endpoint uses TLS (`wss://`).
    pub const fn tls(&self) -> bool {
        self.tls
    }

    /// Account the pairing handle was minted for.
    pub fn account(&self) -> &str {
        &self.account
    }

    fn validate(&self) -> Result<(), EdgeError> {
        if self.host.is_empty()
            || !self.host.is_ascii()
            || self.host.bytes().any(|byte| byte.is_ascii_whitespace())
            || self.host.contains('/')
            || self.host.contains('@')
        {
            return Err(session_error(format!(
                "invalid OSPP session host '{}': expected a bare ASCII hostname or IP address",
                self.host
            )));
        }
        if self.port == 0 {
            return Err(session_error(
                "invalid OSPP session port 0: expected a non-zero TCP port",
            ));
        }
        Ok(())
    }
}

/// Injected port persisting the paired OSPP host coordinates.
pub trait OsppSessionStore: Send + Sync {
    /// Return the paired coordinates, or `None` when this developer never paired.
    fn load(&self) -> Result<Option<OsppSessionCoordinates>, EdgeError>;

    /// Persist the coordinates atomically (`0600`); never stores a secret.
    fn save(&self, coordinates: &OsppSessionCoordinates) -> Result<(), EdgeError>;
}

/// [`OsppSessionStore`] over an explicit `0600` `<data_home>/slipfork/ospp-session.json`.
#[derive(Debug, Clone)]
pub struct FileOsppSessionStore {
    path: PathBuf,
}

impl FileOsppSessionStore {
    /// Build the store over the composition root's explicit path.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The explicit session file this store owns.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl OsppSessionStore for FileOsppSessionStore {
    fn load(&self) -> Result<Option<OsppSessionCoordinates>, EdgeError> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(session_error(format!(
                    "cannot read {}: {error}",
                    self.path.display()
                )));
            }
        };
        let coordinates: OsppSessionCoordinates = serde_json::from_str(&raw).map_err(|error| {
            session_error(format!(
                "cannot parse {}: {error}; re-run `slipfork auth login`",
                self.path.display()
            ))
        })?;
        coordinates.validate()?;
        Ok(Some(coordinates))
    }

    fn save(&self, coordinates: &OsppSessionCoordinates) -> Result<(), EdgeError> {
        coordinates.validate()?;
        let directory = self.path.parent().ok_or_else(|| {
            session_error(format!(
                "OSPP session path '{}' has no parent directory",
                self.path.display()
            ))
        })?;
        ensure_private_dir(directory).map_err(session_error)?;
        let temporary = directory.join(format!(".ospp-session.{}.tmp", std::process::id()));
        let encoded = serde_json::to_vec_pretty(coordinates)
            .map_err(|error| session_error(format!("cannot encode the OSPP session: {error}")))?;
        let write = (|| -> io::Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&encoded)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temporary, &self.path)
        })();
        if let Err(error) = write {
            let _ = fs::remove_file(&temporary);
            return Err(session_error(format!(
                "cannot write {}: {error}",
                self.path.display()
            )));
        }
        Ok(())
    }
}

fn session_error(message: impl Into<String>) -> EdgeError {
    EdgeError::Control(DaemonError::new(DaemonErrorKind::Internal, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_session_is_unpaired_and_saved_coordinates_round_trip_at_0600() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileOsppSessionStore::new(dir.path().join("slipfork/ospp-session.json"));
        assert_eq!(store.load().expect("missing is unpaired"), None);

        let coordinates = OsppSessionCoordinates::new("127.0.0.1", 8443, true, "dev@example");
        store.save(&coordinates).expect("save");
        assert_eq!(store.load().expect("load"), Some(coordinates));

        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(store.path())
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "the session file must stay 0600");
    }

    #[test]
    fn malformed_session_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ospp-session.json");
        fs::write(&path, "{\"host\":\"127.0.0.1\"}").expect("write");
        let store = FileOsppSessionStore::new(path);
        assert!(store.load().is_err(), "a partial session must fail closed");
    }
}
