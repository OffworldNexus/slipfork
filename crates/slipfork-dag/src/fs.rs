//! Filesystem-backed adapters for the DAG planner (`M1.6` #11).
//!
//! These are concrete adapters, not domain logic: they live behind the `fs` feature so the
//! default `slipfork-dag` build stays filesystem-free and the Composition Root wires them
//! explicitly (`crates/slipfork` enables `features = ["fs"]`).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::{DagError, DigestCache, InputHasher};

/// Filesystem [`InputHasher`] walking a repository root deterministically.
#[derive(Debug, Clone)]
pub struct FsMerkleHasher {
    root: PathBuf,
}

impl FsMerkleHasher {
    /// Hash inputs relative to `root`.
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }
}

impl InputHasher for FsMerkleHasher {
    fn hash_globs(&self, globs: &[String]) -> Result<String, DagError> {
        if globs.is_empty() {
            return Ok(hex::encode(Sha256::digest(b"")));
        }
        let mut matched: BTreeMap<String, PathBuf> = BTreeMap::new();
        collect_files(&self.root, &self.root, globs, &mut matched)?;
        let mut h = Sha256::new();
        for (rel, full) in matched {
            let bytes = fs::read(&full).map_err(|e| DagError::Io {
                path: full.display().to_string(),
                message: e.to_string(),
            })?;
            h.update(rel.as_bytes());
            h.update(b"\0");
            h.update(Sha256::digest(&bytes));
        }
        Ok(hex::encode(h.finalize()))
    }
}

fn collect_files(
    root: &Path,
    dir: &Path,
    globs: &[String],
    out: &mut BTreeMap<String, PathBuf>,
) -> Result<(), DagError> {
    let entries = fs::read_dir(dir).map_err(|e| DagError::Io {
        path: dir.display().to_string(),
        message: e.to_string(),
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| DagError::Io {
            path: dir.display().to_string(),
            message: e.to_string(),
        })?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if matches!(
            name_str.as_ref(),
            ".git" | ".worktrees" | ".slipfork" | "target" | "node_modules" | ".venv"
        ) {
            continue;
        }
        let path = entry.path();
        let ft = entry.file_type().map_err(|e| DagError::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        if ft.is_dir() {
            collect_files(root, &path, globs, out)?;
        } else if ft.is_file()
            && let Ok(rel) = path.strip_prefix(root)
        {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if globs.iter().any(|glob| crate::matches_glob(glob, &rel_str)) {
                out.insert(rel_str, path);
            }
        }
    }
    Ok(())
}

/// Directory-backed [`DigestCache`] checking `<cache_root>/artifacts/<digest>` and
/// `<cache_root>/fixtures/<digest>`.
#[derive(Debug, Clone)]
pub struct FsDigestCache {
    root: PathBuf,
}

impl FsDigestCache {
    /// Query a cache rooted at `root`.
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }
}

impl DigestCache for FsDigestCache {
    fn has_artifact(&self, digest: &str) -> bool {
        self.root.join("artifacts").join(digest).exists()
    }

    fn has_fixture(&self, digest: &str) -> bool {
        self.root.join("fixtures").join(digest).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_hasher_is_deterministic_and_skips_ignored_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("src")).expect("src dir");
        fs::create_dir_all(dir.path().join("node_modules")).expect("ignored dir");
        fs::write(dir.path().join("src/lib.rs"), b"pub fn a() {}").expect("source");
        fs::write(dir.path().join("node_modules/ignored.rs"), b"ignored").expect("ignored");

        let hasher = FsMerkleHasher::new(dir.path());
        let first = hasher.hash_globs(&["src/**".to_owned()]).expect("hash");
        let second = hasher.hash_globs(&["src/**".to_owned()]).expect("hash");
        assert_eq!(first, second);
        assert_ne!(
            first,
            hasher
                .hash_globs(&["node_modules/**".to_owned()])
                .expect("ignored hash"),
            "ignored directories never contribute input bytes"
        );
    }

    #[test]
    fn fs_digest_cache_reports_existing_artifacts_and_fixtures() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = FsDigestCache::new(dir.path());
        assert!(!cache.has_artifact("abc"));
        assert!(!cache.has_fixture("abc"));

        fs::create_dir_all(dir.path().join("artifacts/abc")).expect("artifact dir");
        fs::create_dir_all(dir.path().join("fixtures/def")).expect("fixture dir");
        assert!(cache.has_artifact("abc"));
        assert!(cache.has_fixture("def"));
        assert!(!cache.has_fixture("abc"));
    }
}
