//! Build context walking, `.dockerignore` filtering, and deterministic content hashing.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::digest::hex;
use crate::error::{Error, Result};
use crate::ignore::Dockerignore;

/// One included file or directory, relative to the context root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextEntry {
    /// POSIX-style relative path used by `COPY`/`ADD` and cache keys.
    pub relative: PathBuf,
    /// Absolute path on disk.
    pub absolute: PathBuf,
    /// Whether the entry is a directory.
    pub is_dir: bool,
}

/// A build context: the `.dockerignore` rules plus the files they let through.
#[derive(Debug, Clone)]
pub struct BuildContext {
    root: PathBuf,
    ignore: Dockerignore,
    entries: Vec<ContextEntry>,
}

impl BuildContext {
    /// An empty context for `Dockerfile`s with zero host `COPY`/`ADD` instructions, avoiding
    /// unnecessary directory walks across mounted dev checkouts.
    #[must_use]
    pub fn empty(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            ignore: Dockerignore::default(),
            entries: Vec::new(),
        }
    }

    /// Loads `<root>/.dockerignore` and indexes the included entries.
    pub fn load(root: &Path) -> Result<Self> {
        let ignore_path = root.join(".dockerignore");
        let ignore = match fs::read_to_string(&ignore_path) {
            Ok(text) => Dockerignore::parse(&text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Dockerignore::default(),
            Err(source) => return Err(Error::io("read", &ignore_path, source)),
        };
        let mut context = Self {
            root: root.to_path_buf(),
            ignore,
            entries: Vec::new(),
        };
        let mut entries = Vec::new();
        context.walk(&mut entries)?;
        entries.sort_by(|left, right| left.relative.cmp(&right.relative));
        context.entries = entries;
        Ok(context)
    }

    /// The context root the caller injected.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every entry that survived `.dockerignore`, sorted for determinism.
    #[must_use]
    pub fn entries(&self) -> &[ContextEntry] {
        &self.entries
    }

    /// Deterministic content hash of the whole included context.
    pub fn hash(&self) -> Result<String> {
        let mut hasher = Sha256::new();
        for entry in &self.entries {
            if entry.is_dir {
                continue;
            }
            hasher.update(entry.relative.to_string_lossy().as_bytes());
            hasher.update(b"\0");
            // A symlink is hashed by its target: `fs::read` would follow it (and fail on a
            // link to a directory, which a generated `.venv/bin/python` really is).
            let metadata = fs::symlink_metadata(&entry.absolute)
                .map_err(|source| Error::io("inspect", &entry.absolute, source))?;
            let bytes = if metadata.file_type().is_symlink() {
                fs::read_link(&entry.absolute)
                    .map_err(|source| Error::io("readlink", &entry.absolute, source))?
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec()
            } else {
                fs::read(&entry.absolute)
                    .map_err(|source| Error::io("read", &entry.absolute, source))?
            };
            hasher.update((bytes.len() as u64).to_be_bytes());
            hasher.update(&bytes);
        }
        Ok(hex(&hasher.finalize()))
    }

    fn walk(&self, entries: &mut Vec<ContextEntry>) -> Result<()> {
        let mut stack = vec![PathBuf::new()];
        while let Some(relative) = stack.pop() {
            let directory = self.root.join(&relative);
            let children =
                fs::read_dir(&directory).map_err(|source| Error::io("read", &directory, source))?;
            for child in children {
                let child = child.map_err(|source| Error::io("read", &directory, source))?;
                let name = child.file_name();
                let relative = relative.join(&name);
                if Self::reserved(&relative) || self.ignore.is_ignored(&relative) {
                    continue;
                }
                let absolute = child.path();
                let metadata = fs::symlink_metadata(&absolute)
                    .map_err(|source| Error::io("inspect", &absolute, source))?;
                let is_dir = metadata.is_dir();
                entries.push(ContextEntry {
                    relative: relative.clone(),
                    absolute,
                    is_dir,
                });
                if is_dir {
                    stack.push(relative);
                }
            }
        }
        Ok(())
    }

    /// The Dockerfile and `.dockerignore` are metadata, never copied into the image.
    fn reserved(relative: &Path) -> bool {
        // Slipfork's own state (`world.json`, logs, CoW stores, forks) is never build input:
        // walking `.slipfork-store` would try to hash sealed image subvolumes as files.
        let managed = relative
            .components()
            .next()
            .and_then(|first| first.as_os_str().to_str())
            .is_some_and(|first| matches!(first, ".slipfork" | ".slipfork-store" | ".forks"));
        managed
            || matches!(
                relative.to_str(),
                Some(".dockerignore") | Some("Dockerfile") | Some("dockerfile")
            )
            || relative
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".dockerignore"))
    }
}
