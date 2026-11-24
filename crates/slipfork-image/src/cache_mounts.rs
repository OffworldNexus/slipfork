//! Host directories backing `RUN --mount=type=cache`, under `<images>/cache-mounts`.

use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

const CACHE_DIR: &str = "cache-mounts";
/// Longest slug kept before the disambiguation hash is appended.
const SLUG_MAX: usize = 80;

/// One cohesive owner of the persistent build-cache mount directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheMounts {
    images: PathBuf,
}

impl CacheMounts {
    /// Binds the mounts to `<images_root>/cache-mounts`.
    #[must_use]
    pub fn new(images_root: impl Into<PathBuf>) -> Self {
        Self {
            images: images_root.into(),
        }
    }

    /// `<images>/cache-mounts`.
    #[must_use]
    pub fn root(&self) -> PathBuf {
        self.images.join(CACHE_DIR)
    }

    /// Host directory backing a cache mount (`id` or target), never inside a sealed rootfs.
    #[must_use]
    pub fn dir(&self, id: &str) -> PathBuf {
        self.root().join(sanitize(id))
    }

    /// Creates `<images>/cache-mounts` when missing.
    pub fn ensure(&self) -> Result<()> {
        let root = self.root();
        fs::create_dir_all(&root).map_err(|source| Error::io("create", &root, source))
    }

    /// Creates `dir` when missing, returning the host path a `RUN` bind-mounts.
    pub fn prepare(&self, id: &str) -> Result<PathBuf> {
        let path = self.dir(id);
        fs::create_dir_all(&path).map_err(|source| Error::io("create", &path, source))?;
        Ok(path)
    }
}

/// Turns a cache-mount id into one safe directory component.
///
/// ASCII letters, digits, `.` and `_` survive verbatim. Accents and other
/// non-ASCII characters are transliterated first (`Rémy` → `Remy`), every
/// maximal run of disallowed characters collapses to one `-`, and leading or
/// trailing `-` is trimmed. Any information lost along the way (transliteration,
/// a collapsed run, trimming, truncation) appends `-<8 hex of id>` so `Remy`
/// and `Rémy` never share a directory.
fn sanitize(id: &str) -> String {
    let ascii = deunicode::deunicode(id);
    let mut lossy = ascii != id;

    let mut mapped = String::with_capacity(ascii.len());
    for character in ascii.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_') {
            mapped.push(character);
        } else {
            lossy |= character != '-';
            mapped.push('-');
        }
    }

    let mut collapsed = String::with_capacity(mapped.len());
    for character in mapped.chars() {
        if character == '-' && collapsed.ends_with('-') {
            lossy = true;
        } else {
            collapsed.push(character);
        }
    }

    let trimmed = collapsed.trim_matches('-');
    lossy |= trimmed.len() != collapsed.len();
    let mut slug = trimmed.to_owned();

    if slug.is_empty() || slug.chars().all(|character| character == '.') {
        slug = String::from("mount");
        lossy = true;
    } else if slug.starts_with('.') {
        let rest = slug.trim_start_matches('.');
        slug = if rest.is_empty() {
            String::from("mount")
        } else {
            format!("mount-{rest}")
        };
        lossy = true;
    }

    if slug.len() > SLUG_MAX {
        slug.truncate(SLUG_MAX);
        while slug.ends_with('-') {
            slug.pop();
        }
        lossy = true;
    }

    if lossy {
        let digest = Sha256::digest(id.as_bytes());
        format!(
            "{slug}-{:02x}{:02x}{:02x}{:02x}",
            digest[0], digest[1], digest[2], digest[3]
        )
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::{CacheMounts, sanitize};

    fn has_hex_suffix(slug: &str) -> bool {
        slug.rsplit_once('-')
            .is_some_and(|(_, hash)| hash.len() == 8 && hash.chars().all(|c| c.is_ascii_hexdigit()))
    }

    #[test]
    fn plain_ids_are_lossless() {
        assert_eq!(sanitize("plain_name"), "plain_name");
        assert_eq!(sanitize("a.b_c"), "a.b_c");
        assert_eq!(sanitize("plain-name"), "plain-name");
    }

    #[test]
    fn accents_are_transliterated_not_dashed() {
        let remy = sanitize("Rémy");
        assert!(remy.starts_with("Remy-"), "{remy}");
        assert!(has_hex_suffix(&remy));
        assert_eq!(remy.len(), "Remy-".len() + 8);
        assert!(sanitize("ß").starts_with("ss-"));
        assert!(sanitize("ø").starts_with("o-"));
        assert_ne!(sanitize("Remy"), remy);
    }

    #[test]
    fn separator_runs_collapse_to_one_dash() {
        for id in ["a!!b", "a----b", "a !! b"] {
            let slug = sanitize(id);
            assert!(slug.starts_with("a-b-"), "{id}: {slug}");
            assert!(has_hex_suffix(&slug), "{id}: {slug}");
        }
        for id in ["Rémy", "a!!b", "a----b", "..", "", "???", "a !! b"] {
            let slug = sanitize(id);
            assert!(!slug.contains("--"), "{id}: {slug}");
            assert!(!slug.starts_with('-'), "{id}: {slug}");
        }
    }

    #[test]
    fn dot_only_and_empty_become_mount() {
        for id in ["..", "", ".", "..."] {
            let slug = sanitize(id);
            assert!(slug.starts_with("mount-"), "{id}: {slug}");
            assert!(!slug.starts_with('.'), "{id}: {slug}");
            assert!(has_hex_suffix(&slug), "{id}: {slug}");
        }
        assert!(sanitize(".hidden").starts_with("mount-hidden-"));
    }

    #[test]
    fn long_ids_are_truncated_before_the_hash() {
        let id = "x".repeat(200);
        let slug = sanitize(&id);
        assert_eq!(slug.len(), 80 + 1 + 8);
        assert!(slug.starts_with(&id[..80]));
        assert!(has_hex_suffix(&slug));
    }

    #[test]
    fn dirs_live_under_one_cache_root() {
        let mounts = CacheMounts::new("/store/images");
        assert_eq!(
            mounts.root(),
            std::path::Path::new("/store/images/cache-mounts")
        );
        assert!(
            mounts
                .dir("/root/.cache/x")
                .starts_with("/store/images/cache-mounts")
        );
        let dir = mounts.dir("/root/.cache/x");
        assert!(
            dir.file_name()
                .expect("the mount directory has a name")
                .to_string_lossy()
                .starts_with("root-.cache-x-"),
            "a leading `/` is transliterated and disambiguated, saw {}",
            dir.display()
        );
    }
}
