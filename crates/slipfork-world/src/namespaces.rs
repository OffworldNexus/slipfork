//! Namespace identity for one world.
//!
//! `slipfork-world-init` is PID 1 of a child PID namespace, so inside it only `/proc/self/ns/*`
//! exists; `/proc/<host-pid>` does not. It therefore records the kernel inode identity
//! (`net:[4026532…]`) and the supervisor canonicalises those into host-resolvable
//! `/proc/<init-pid>/ns/*` paths after verifying every inode matches.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::secure_file::write_private;

/// Every namespace a world holds for its whole lifetime.
pub const NAMESPACE_KINDS: [&str; 6] = ["user", "net", "ipc", "pid", "mnt", "uts"];

/// The namespace inode identities observed from inside the world's PID namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NamespaceIdentity {
    identities: BTreeMap<String, String>,
}

impl NamespaceIdentity {
    /// Reads `net:[…]`-style identities from `/proc/self/ns/<kind>` (never `/proc/1/ns/*`).
    pub fn observe() -> Result<Self> {
        let mut identities = BTreeMap::new();
        for kind in NAMESPACE_KINDS {
            let path = PathBuf::from(format!("/proc/self/ns/{kind}"));
            let identity = fs::read_link(&path)
                .map_err(|source| Error::io("read", &path, source))?
                .to_string_lossy()
                .into_owned();
            let expected = format!("{kind}:");
            if !identity.starts_with(&expected) || !identity.ends_with(']') {
                return Err(Error::Protocol {
                    message: format!("`/proc/self/ns/{kind}` reported `{identity}`"),
                });
            }
            identities.insert(kind.to_owned(), identity);
        }
        Ok(Self { identities })
    }

    /// The recorded identity for `kind` (`"net:[4026532…]"`).
    #[must_use]
    pub fn identity(&self, kind: &str) -> Option<&str> {
        self.identities.get(kind).map(String::as_str)
    }

    /// Writes the identity table mode `0600` to `path`.
    pub fn write(&self, path: &Path) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|source| Error::Json {
            path: path.to_path_buf(),
            source,
        })?;
        write_private(path, &bytes)
    }

    /// Reads an identity table written by [`Self::write`].
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).map_err(|source| Error::io("read", path, source))?;
        serde_json::from_slice(&bytes).map_err(|source| Error::Json {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// Host-resolvable `/proc/<init-pid>/ns/<kind>` paths for one live world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceTable {
    paths: BTreeMap<String, PathBuf>,
}

impl NamespaceTable {
    /// Builds the table for the init process `pid`, requiring all six namespace links.
    pub fn from_proc(pid: u32) -> Result<Self> {
        let mut paths = BTreeMap::new();
        for kind in NAMESPACE_KINDS {
            let path = PathBuf::from(format!("/proc/{pid}/ns/{kind}"));
            let metadata =
                fs::symlink_metadata(&path).map_err(|source| Error::io("stat", &path, source))?;
            if !metadata.file_type().is_symlink() {
                return Err(Error::Protocol {
                    message: format!("`{}` is not a namespace link", path.display()),
                });
            }
            paths.insert(kind.to_owned(), path);
        }
        Ok(Self { paths })
    }

    /// The canonical path for `kind`, if present.
    #[must_use]
    pub fn path(&self, kind: &str) -> Option<&Path> {
        self.paths.get(kind).map(PathBuf::as_path)
    }

    /// Iterates every `(kind, path)` pair in stable order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Path)> {
        self.paths
            .iter()
            .map(|(kind, path)| (kind.as_str(), path.as_path()))
    }

    /// Fails closed unless every link's inode identity equals `identity`; this is the check the
    /// supervisor runs before trusting a PID as the world's init.
    pub fn verify(&self, identity: &NamespaceIdentity) -> Result<()> {
        for (kind, path) in &self.paths {
            let actual = fs::read_link(path)
                .map_err(|source| Error::io("read", path, source))?
                .to_string_lossy()
                .into_owned();
            let expected = identity.identity(kind).ok_or_else(|| Error::Protocol {
                message: format!("init did not report a `{kind}` namespace"),
            })?;
            if actual != expected {
                return Err(Error::NamespaceMismatch {
                    kind: kind.clone(),
                    expected: expected.to_owned(),
                    actual,
                });
            }
        }
        Ok(())
    }

    /// Writes the canonical table mode `0600`; consumers can `nsenter --user=<path>` from it.
    pub fn write_json(&self, path: &Path) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.paths).map_err(|source| Error::Json {
            path: path.to_path_buf(),
            source,
        })?;
        write_private(path, &bytes)
    }

    /// Reads a canonical table written by [`Self::write_json`].
    pub fn read_json(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).map_err(|source| Error::io("read", path, source))?;
        let paths: BTreeMap<String, PathBuf> =
            serde_json::from_slice(&bytes).map_err(|source| Error::Json {
                path: path.to_path_buf(),
                source,
            })?;
        if NAMESPACE_KINDS
            .iter()
            .any(|kind| !paths.contains_key(*kind))
        {
            return Err(Error::Protocol {
                message: format!("`{}` is missing a namespace entry", path.display()),
            });
        }
        Ok(Self { paths })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observes_and_verifies_the_current_process_namespaces() {
        let identity = NamespaceIdentity::observe().expect("read /proc/self/ns");
        let table = NamespaceTable::from_proc(std::process::id()).expect("table for self");
        table.verify(&identity).expect("identity must match");
        assert!(table.path("net").is_some());
        assert!(table.path("ipc").is_some());
        assert!(table.iter().all(|(kind, path)| path.ends_with(kind)));
    }

    #[test]
    fn reports_a_mismatch_instead_of_trusting_a_foreign_pid() {
        let identity = NamespaceIdentity::observe().expect("read /proc/self/ns");
        let mut swapped = identity.clone();
        swapped
            .identities
            .insert("net".to_owned(), "net:[1]".to_owned());
        let table = NamespaceTable::from_proc(std::process::id()).expect("table for self");
        assert!(matches!(
            table.verify(&swapped),
            Err(Error::NamespaceMismatch { .. })
        ));
    }

    #[test]
    fn round_trips_the_identity_and_canonical_tables() {
        let base = PathBuf::from(format!("/var/tmp/slipfork-ns-{}", std::process::id()));
        let identity_path = base.join("inside.json");
        let canonical_path = base.join("namespaces.json");
        let identity = NamespaceIdentity::observe().expect("read /proc/self/ns");
        identity.write(&identity_path).expect("write identity");
        assert_eq!(
            NamespaceIdentity::read(&identity_path).expect("read identity"),
            identity
        );
        let table = NamespaceTable::from_proc(std::process::id()).expect("table for self");
        table.write_json(&canonical_path).expect("write table");
        assert_eq!(
            NamespaceTable::read_json(&canonical_path).expect("read table"),
            table
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
