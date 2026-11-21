//! Cached `Dockerfile` steps: sealed Btrfs subvolumes plus their metadata records.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
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
}

impl StepCache {
    /// Wraps the world engine and the step layout.
    #[must_use]
    pub fn new(world: Arc<dyn WorldEngine>, layout: StepLayout) -> Self {
        Self { world, layout }
    }

    /// The sealed rootfs for `key`, when it exists and still carries `BTRFS_SUBVOL_RDONLY`.
    pub fn lookup(&self, key: &str) -> Result<Option<PathBuf>> {
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

fn sanitize(id: &str) -> String {
    let sanitized: String = id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '-'
            }
        })
        .collect();
    if sanitized.is_empty() || sanitized.starts_with('.') {
        format!("mount-{sanitized}")
    } else {
        sanitized
    }
}
