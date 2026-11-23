//! `<state_dir>/worlds.json`: the daemon's durable list of worlds it owns.
//!
//! The file is rewritten atomically (mode `0600`) on every attach, boot, and teardown. A malformed
//! file is a startup error, never an empty registry: silently forgetting worlds would orphan them.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One world the daemon knows about; `<project>/.slipfork/world.json` caches the same facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub project: PathBuf,
    pub world: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<String>,
    pub pods_dir: PathBuf,
    pub supervisor_pid: u32,
    /// Milliseconds since the Unix epoch when the world became ready; `0` for older registries.
    #[serde(default)]
    pub started_at_ms: u64,
}

/// The `Forkfile` a world booted from: content digest plus active feature names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    /// Stable content digest of the compiled `Forkfile` source.
    #[serde(default)]
    pub digest: String,
    /// Active feature names, sorted, so `features` edits force a reconcile.
    #[serde(default)]
    pub features: Vec<String>,
}

/// `<state_dir>/worlds-fingerprints.json`: project path → booted `Forkfile` fingerprint.
#[must_use]
pub fn fingerprints_path(state_dir: &Path) -> PathBuf {
    state_dir.join("worlds-fingerprints.json")
}

/// Reads every recorded fingerprint; a missing file is empty, a malformed one is an error.
///
/// # Errors
///
/// Propagates read and JSON parse failures so the daemon fails closed.
pub fn load_fingerprints(
    state_dir: &Path,
) -> io::Result<std::collections::BTreeMap<String, Fingerprint>> {
    let path = fingerprints_path(state_dir);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("`{}` is not valid JSON: {error}", path.display()),
            )
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(std::collections::BTreeMap::new())
        }
        Err(error) => Err(error),
    }
}

/// Records one world's booted fingerprint atomically with mode `0600`.
///
/// # Errors
///
/// Propagates directory creation, read, write, chmod, and rename failures.
pub fn store_fingerprint(
    state_dir: &Path,
    project: &Path,
    fingerprint: Option<&Fingerprint>,
) -> io::Result<()> {
    let key = project.to_string_lossy().into_owned();
    let mut all = load_fingerprints(state_dir).unwrap_or_default();
    match fingerprint {
        Some(fingerprint) => {
            all.insert(key, fingerprint.clone());
        }
        None => {
            all.remove(&key);
        }
    }
    fs::create_dir_all(state_dir)?;
    let destination = fingerprints_path(state_dir);
    let temporary = destination.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(&all).map_err(io::Error::other)?;
    fs::write(&temporary, bytes)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    fs::rename(&temporary, &destination)
}

/// `<state_dir>/worlds.json`.
#[must_use]
pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join("worlds.json")
}

/// Reads every entry; a missing file is an empty registry, a malformed one is an error.
///
/// # Errors
///
/// Propagates read failures and JSON parse failures so the daemon fails closed.
pub fn load(state_dir: &Path) -> io::Result<Vec<RegistryEntry>> {
    let path = path(state_dir);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("`{}` is not valid JSON: {error}", path.display()),
            )
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// Rewrites the registry atomically with mode `0600`.
///
/// # Errors
///
/// Propagates directory creation, write, chmod, and rename failures; the previous file survives a
/// failed rename.
pub fn store(state_dir: &Path, entries: &[RegistryEntry]) -> io::Result<()> {
    fs::create_dir_all(state_dir)?;
    let destination = path(state_dir);
    let temporary = destination.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(entries).map_err(io::Error::other)?;
    fs::write(&temporary, bytes)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    fs::rename(&temporary, &destination)
}
