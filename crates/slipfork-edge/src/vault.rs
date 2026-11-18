//! The local-only `SecretVault` port (`M3.29`, `ADR-0001` §3, `docs/design/ospp.md`).
//!
//! `slipforkd` injects exactly one implementation of this port (wired at the Composition Root over
//! `FileVaultProvider`). It is the home of credentials that must survive a daemon or host restart:
//! locally provisioned PostgreSQL/Redis passwords, local `!!` secrets, and `slipfork secret`
//! overrides. Remote OSPP values never enter it — they are decrypted in memory and injected
//! straight into child environments.
//!
//! [`SecretVault::keys`] and [`SecretVault::vars`] return identifier names only (never values).
//! They feed the ID-only `OsppClaims` set on `subscribe`, telling the server which entries the
//! local world owns and must leave as `${{ ... }}` templates.

use slipfork_world::ProjectSlug;

use slipfork_wire::control::{DaemonError, DaemonErrorKind};

use crate::EdgeError;

/// Capability-honest local vault port.
pub trait SecretVault: Send + Sync {
    /// Read one locally managed secret.
    fn get(&self, project: &ProjectSlug, key: &str) -> Result<Option<String>, EdgeError>;

    /// Store one locally managed secret, refusing an overwrite unless `force`.
    fn put(
        &self,
        project: &ProjectSlug,
        key: &str,
        value: &str,
        force: bool,
    ) -> Result<(), EdgeError>;

    /// Remove one locally managed secret, reporting whether a value was deleted.
    fn remove(&self, project: &ProjectSlug, key: &str) -> Result<bool, EdgeError>;

    /// Every locally managed secret name (never a value), sorted.
    fn keys(&self, project: &ProjectSlug) -> Result<Vec<String>, EdgeError>;

    /// Every locally managed `vars` name (never a value), sorted.
    ///
    /// Local `vars` are explicit variable overrides a developer owns; they are claimed so the
    /// server leaves their `${{ vars::<KEY> }}` expressions untouched.
    fn vars(&self, project: &ProjectSlug) -> Result<Vec<String>, EdgeError>;
}

/// A refused overwrite, expressed through the one control error type.
pub fn already_exists(project: &ProjectSlug, key: &str) -> EdgeError {
    EdgeError::Control(DaemonError::new(
        DaemonErrorKind::Rejected,
        format!(
            "secret '{key}' already exists in the local vault for project '{project}'; pass --force to rotate it"
        ),
    ))
}
