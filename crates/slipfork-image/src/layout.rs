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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::ImageLayout;

    #[test]
    fn digests_become_single_filesystem_components() {
        let layout = ImageLayout::new("/store/images");
        assert_eq!(layout.root(), Path::new("/store/images/oci"));
        let digest = "sha256:0123456789abcdef";
        assert_eq!(
            layout.image(digest),
            Path::new("/store/images/oci/sha256-0123456789abcdef")
        );
        assert_eq!(
            layout.staging(digest),
            Path::new("/store/images/oci/.partial-sha256-0123456789abcdef")
        );
        let hostile = layout.image("sha256:../evil");
        assert_eq!(hostile.parent(), Some(Path::new("/store/images/oci")));
        assert_eq!(hostile.components().count(), 5);
    }
}
