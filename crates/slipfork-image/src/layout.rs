//! On-disk layout of the OCI image cache.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

const OCI_DIR: &str = "oci";
const STAGING_PREFIX: &str = ".partial-";

/// `<images>/oci` plus the per-digest staging sibling.
///
/// The composition root injects the images root, so this type never guesses a mount point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageLayout {
    root: PathBuf,
}

impl ImageLayout {
    /// Builds the layout under `<images_root>/oci`.
    #[must_use]
    pub fn new(images_root: impl Into<PathBuf>) -> Self {
        Self {
            root: images_root.into().join(OCI_DIR),
        }
    }

    /// The directory holding one sealed subvolume per manifest digest.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/<manifest-digest>` for a sealed image.
    #[must_use]
    pub fn image(&self, digest: &str) -> PathBuf {
        self.root.join(digest_component(digest))
    }

    /// `<root>/.partial-<manifest-digest>` while the layers are still unpacking.
    #[must_use]
    pub fn staging(&self, digest: &str) -> PathBuf {
        self.root
            .join(format!("{STAGING_PREFIX}{}", digest_component(digest)))
    }

    /// `<root>/<manifest-digest>.json`: the public OCI image config beside the sealed rootfs.
    #[must_use]
    pub fn config(&self, digest: &str) -> PathBuf {
        self.root.join(format!("{}.json", digest_component(digest)))
    }

    /// `<root>/refs.json`: `reference -> manifest digest`, so a reboot never needs the registry.
    #[must_use]
    pub fn refs(&self) -> PathBuf {
        self.root.join("refs.json")
    }

    /// The digest a previous pull recorded for this reference, if any.
    #[must_use]
    pub fn cached_ref(&self, reference: &str) -> Option<String> {
        let bytes = fs::read(self.refs()).ok()?;
        let map: std::collections::BTreeMap<String, String> =
            serde_json::from_slice(&bytes).ok()?;
        map.get(reference).cloned()
    }

    /// Records `reference -> digest`; a lost write only costs one future registry round trip.
    pub fn remember_ref(&self, reference: &str, digest: &str) {
        let path = self.refs();
        let mut map: std::collections::BTreeMap<String, String> = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        if map.get(reference).map(String::as_str) == Some(digest) {
            return;
        }
        map.insert(reference.to_owned(), digest.to_owned());
        if let Ok(bytes) = serde_json::to_vec_pretty(&map) {
            let _ = fs::write(&path, bytes);
        }
    }

    /// Creates the cache root when missing.
    pub fn ensure(&self) -> Result<()> {
        fs::create_dir_all(&self.root).map_err(|source| Error::io("create", &self.root, source))
    }
}

/// Turns a registry digest into one filesystem component.
///
/// `sha256:…` becomes `sha256-…`; anything a registry could not legally emit is folded to `-` so a
/// hostile digest can never add a path separator.
fn digest_component(digest: &str) -> String {
    digest
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+') {
                c
            } else {
                '-'
            }
        })
        .collect()
}
