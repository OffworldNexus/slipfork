//! The public OSPP pairing record at `<state_dir>/ospp.json` (`0600`, atomic replace).
//!
//! Only the three public coordinates a reconnect needs are persisted: the endpoint, the account,
//! and the machine name. The device private key, session tokens, HPKE claim material, and every
//! secret value stay out of this file by construction — [`PairingRecord`] has no other fields.

use crate::layout::ensure_private_dir;
use serde::{Deserialize, Serialize};
use slipfork_ospp::OsppEndpoint;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The public coordinates of one successful `slipfork auth` pairing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingRecord {
    /// The daemon's single OSPP server identity (`wss://…`).
    pub endpoint: String,
    /// Authenticated account name.
    pub account: String,
    /// Approved machine name.
    pub machine: String,
}

/// The on-disk `<state_dir>/ospp.json` pairing record, written `0600` and replaced atomically.
#[derive(Debug, Clone)]
pub struct PairingStore {
    path: PathBuf,
}

impl PairingStore {
    /// Bind the store to one explicit `<state_dir>/ospp.json` path.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The record path, named verbatim in diagnostics.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the saved public pairing, or `None` when this machine never paired.
    pub fn load(&self) -> Result<Option<PairingRecord>, String> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|err| {
                format!(
                    "{} is not a valid pairing record: {err}",
                    self.path.display()
                )
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(format!("cannot read {}: {err}", self.path.display())),
        }
    }

    /// Persist the public pairing record `0600`, replacing any previous file atomically.
    pub(super) fn record(&self, record: &PairingRecord) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        ensure_private_dir(parent)?;
        let mut body = serde_json::to_vec_pretty(record)
            .map_err(|err| format!("cannot encode the OSPP pairing: {err}"))?;
        body.push(b'\n');
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|err| {
            format!(
                "cannot create a temporary pairing file in {}: {err}",
                parent.display()
            )
        })?;
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|err| format!("cannot restrict the pairing record to 0600: {err}"))?;
        temp.write_all(&body)
            .map_err(|err| format!("cannot write the pairing record: {err}"))?;
        temp.as_file()
            .sync_all()
            .map_err(|err| format!("cannot flush the pairing record: {err}"))?;
        temp.persist(&self.path)
            .map_err(|err| format!("cannot replace {}: {}", self.path.display(), err.error))?;
        Ok(())
    }
}

/// The boot-time OSPP endpoint plus the one warning a malformed saved record produces.
pub struct BootEndpoint {
    /// The injected endpoint; `None` starts a local-only daemon.
    pub endpoint: Option<OsppEndpoint>,
    /// One actionable warning line for stderr (`None` on a clean boot).
    pub warning: Option<String>,
}

/// Resolve the daemon's OSPP endpoint: an explicit flag wins, otherwise the saved public pairing.
///
/// A corrupt or malformed record is **not** fatal — OSPP is never a prerequisite for local worlds
/// (`ADR-0005` §1) — so it yields one warning line and a local-only daemon.
pub fn resolve_boot_endpoint(explicit: Option<&str>, path: &Path) -> Result<BootEndpoint, String> {
    if let Some(raw) = explicit {
        let endpoint =
            OsppEndpoint::parse(raw).map_err(|err| format!("invalid --ospp-endpoint: {err}"))?;
        return Ok(BootEndpoint {
            endpoint: Some(endpoint),
            warning: None,
        });
    }
    let record = match PairingStore::new(path.to_path_buf()).load() {
        Ok(Some(record)) => record,
        Ok(None) => {
            return Ok(BootEndpoint {
                endpoint: None,
                warning: None,
            });
        }
        Err(err) => {
            return Ok(BootEndpoint {
                endpoint: None,
                warning: Some(format!(
                    "cannot read the saved OSPP pairing at {}: {err}; booting local-only; next: fix the state directory permissions or re-run `slipfork auth`",
                    path.display()
                )),
            });
        }
    };
    match OsppEndpoint::parse(&record.endpoint) {
        Ok(endpoint) => Ok(BootEndpoint {
            endpoint: Some(endpoint),
            warning: None,
        }),
        Err(err) => Ok(BootEndpoint {
            endpoint: None,
            warning: Some(format!(
                "the saved OSPP pairing at {} names an invalid endpoint '{}': {err}; booting local-only; next: re-run `slipfork auth`",
                path.display(),
                record.endpoint
            )),
        }),
    }
}
