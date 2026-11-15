//! Shared helpers for the `slipfork-engine` rootfs critical tests.
//!
//! The pinned static `crun` asset is a real input: it is downloaded once from the URL recorded in
//! `rootfs::pin` and verified against the compile-time SHA-256. Nothing here mocks Docker,
//! filesystems, or the pin.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Compile-time pin mirrored for the tests (the crate's `pin` module is the source of truth).
pub const CRUN_FILE_NAME: &str = "crun-1.20-x86_64-ff8407185ec4.bin";
/// Compile-time SHA-256 of the pinned static `crun`.
pub const CRUN_SHA256: &str = "ff8407185ec474d7c60f697032b7b1fc1fb028fbd0bf13c7d5da9a7d9a479e38";
/// Release asset the pin was measured from (`docs/design/world-base-image.md` §7.1).
pub const CRUN_URL: &str = "https://github.com/containers/crun/releases/download/1.20/crun-1.20-linux-amd64-disable-systemd";

/// Local cache holding the pinned `crun` between test runs.
pub fn runtime_cache_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME is set"))
        .join(".cache")
        .join("slipfork")
        .join("test-runtime")
}

/// Disk-backed scratch root (`/tmp` is RAM-backed `tmpfs` on Linux runners).
pub fn disk_scratch_root() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME is set"))
        .join(".cache")
        .join("slipfork")
        .join("test-tmp")
}

/// A temporary directory on disk, never on `tmpfs`.
pub fn disk_tempdir(label: &str) -> tempfile::TempDir {
    let root = disk_scratch_root();
    std::fs::create_dir_all(&root).expect("create disk scratch root");
    tempfile::Builder::new()
        .prefix(label)
        .tempdir_in(root)
        .expect("disk tempdir")
}

/// Path of the pinned static `crun`, downloading and verifying it on a cold cache.
pub fn pinned_crun() -> PathBuf {
    let path = runtime_cache_dir().join(CRUN_FILE_NAME);
    if verify_sha256(&path) {
        return path;
    }
    let cache = runtime_cache_dir();
    std::fs::create_dir_all(&cache).expect("create runtime cache");
    let temporary = cache.join(format!("{CRUN_FILE_NAME}.download"));
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "2", "-o"])
        .arg(&temporary)
        .arg(CRUN_URL)
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "failed to execute curl to fetch the pinned static crun: {error}\n  fix: install curl, or place {CRUN_FILE_NAME} (sha256 {CRUN_SHA256}) in {}",
                cache.display()
            )
        });
    assert!(
        status.success(),
        "failed to download the pinned static crun from {CRUN_URL}\n  fix: place {CRUN_FILE_NAME} (sha256 {CRUN_SHA256}) in {}",
        cache.display()
    );
    assert!(
        verify_sha256(&temporary),
        "downloaded crun does not match the compile-time pin {CRUN_SHA256}"
    );
    std::fs::rename(&temporary, &path).expect("install pinned crun");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make crun executable");
    path
}

fn verify_sha256(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes)) == CRUN_SHA256
}

/// Recursively copy `from` into `to`, preserving Unix modes.
pub fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&source)?;
        if metadata.is_dir() {
            copy_tree(&source, &target)?;
        } else if metadata.file_type().is_symlink() {
            let link = std::fs::read_link(&source)?;
            std::os::unix::fs::symlink(link, &target)?;
        } else {
            std::fs::copy(&source, &target)?;
        }
    }
    Ok(())
}
