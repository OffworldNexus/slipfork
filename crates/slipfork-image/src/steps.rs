//! Cached `Dockerfile` steps: sealed Btrfs subvolumes plus their metadata records.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use slipfork_world::{Dirtiness, WorldEngine};

use crate::error::{Error, Result};

const STEPS_DIR: &str = "steps";
const CACHE_DIR: &str = "cache-mounts";

/// On-disk layout of the per-instruction step cache under the image store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepLayout {
    images: PathBuf,
}

impl StepLayout {
    /// Builds the layout under `<images_root>/steps`.
    #[must_use]
    pub fn new(images_root: impl Into<PathBuf>) -> Self {
        Self {
            images: images_root.into(),
        }
    }

    /// `<images>/steps`.
    #[must_use]
    pub fn steps(&self) -> PathBuf {
        self.images.join(STEPS_DIR)
    }

    /// `<images>/cache-mounts`, host directories for `RUN --mount=type=cache`.
    #[must_use]
    pub fn cache_mounts(&self) -> PathBuf {
        self.images.join(CACHE_DIR)
    }

    /// The sealed subvolume for `key`.
    #[must_use]
    pub fn sealed(&self, key: &str) -> PathBuf {
        self.steps().join(key)
    }

    /// The writable clone being populated for `key`.
    #[must_use]
    pub fn writer(&self, key: &str) -> PathBuf {
        self.steps().join(format!(".writer-{key}"))
    }

    /// The JSON record beside the sealed subvolume.
    #[must_use]
    pub fn record(&self, key: &str) -> PathBuf {
        self.steps().join(format!("{key}.json"))
    }

    /// Host directory backing a cache mount (`id` or target), never inside a sealed rootfs.
    #[must_use]
    pub fn cache_mount(&self, id: &str) -> PathBuf {
        self.cache_mounts().join(sanitize(id))
    }

    /// Creates `steps/` and `cache-mounts/` when missing.
    pub fn ensure(&self) -> Result<()> {
        for path in [self.steps(), self.cache_mounts()] {
            fs::create_dir_all(&path).map_err(|source| Error::io("create", &path, source))?;
        }
        Ok(())
    }
}

/// Whether a build may reuse sealed steps or must re-execute every instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    /// Reuse every sealed step the layout already holds.
    Warm,
    /// Ignore sealed steps: re-execute and replace each same-key subvolume.
    Cold,
}

/// Metadata that identifies a cached step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRecord {
    /// Cache key (also the subvolume name).
    pub key: String,
    /// Parent step key.
    pub parent: String,
    /// Canonical instruction text.
    pub instruction: String,
    /// 1-based `Dockerfile` line.
    pub line: usize,
}

/// Sealed-step lookup and installation.
#[derive(Clone)]
pub struct StepCache {
    world: Arc<dyn WorldEngine>,
    layout: StepLayout,
    mode: CacheMode,
}

impl StepCache {
    /// Wraps the world engine and the step layout, reusing sealed steps.
    #[must_use]
    pub fn new(world: Arc<dyn WorldEngine>, layout: StepLayout) -> Self {
        Self::with_mode(world, layout, CacheMode::Warm)
    }

    /// Wraps the engine and layout, choosing whether sealed steps may be reused.
    #[must_use]
    pub fn with_mode(world: Arc<dyn WorldEngine>, layout: StepLayout, mode: CacheMode) -> Self {
        Self {
            world,
            layout,
            mode,
        }
    }

    /// The sealed rootfs for `key`, when it exists and still carries `BTRFS_SUBVOL_RDONLY`.
    pub fn lookup(&self, key: &str) -> Result<Option<PathBuf>> {
        if self.mode == CacheMode::Cold {
            return Ok(None);
        }
        let sealed = self.layout.sealed(key);
        if !sealed.exists() {
            return Ok(None);
        }
        if self.world.is_sealed(&sealed)? {
            Ok(Some(sealed))
        } else {
            // A half-finished previous run: never trust an unsealed step.
            self.discard(&sealed)?;
            Ok(None)
        }
    }

    /// Seals `writer`, installs it as `<key>/`, and writes `<key>.json`.
    pub fn store(&self, key: &str, writer: &Path, record: &StepRecord) -> Result<PathBuf> {
        let sealed = self.layout.sealed(key);
        // A cold build replaces the very same key: drop the stale sealed node and its record
        // first, so the rename below installs the rebuilt subvolume instead of losing the race.
        if self.mode == CacheMode::Cold && sealed.exists() {
            self.discard(&sealed)?;
        }
        self.world.seal(writer)?;
        match fs::rename(writer, &sealed) {
            Ok(()) => {}
            Err(source) => {
                // Another build won the race; keep the existing sealed step.
                let _ = self.world.destroy(writer, Dirtiness::Dirty, true);
                if !sealed.exists() {
                    return Err(Error::io("install", &sealed, source));
                }
            }
        }
        let json = serde_json::to_vec_pretty(record).map_err(|source| Error::Json {
            url: format!("step {key}"),
            source,
        })?;
        fs::write(self.layout.record(key), json)
            .map_err(|source| Error::io("write", &self.layout.record(key), source))?;
        Ok(sealed)
    }

    /// Removes a writer that failed before it could be sealed.
    pub fn abandon(&self, writer: &Path) {
        if writer.exists() {
            let _ = self.world.destroy(writer, Dirtiness::Dirty, true);
        }
    }

    /// Destroys a cached step (used when it is present but no longer sealed).
    fn discard(&self, path: &Path) -> Result<()> {
        self.world.destroy(path, Dirtiness::Dirty, true)?;
        if let Some(stem) = path.file_name().and_then(|name| name.to_str()) {
            let record = self.layout.record(stem);
            let _ = fs::remove_file(record);
        }
        Ok(())
    }
}

/// Longest slug kept before the disambiguation hash is appended.
const SLUG_MAX: usize = 80;

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
    use super::sanitize;

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
}
