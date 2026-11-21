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
}
