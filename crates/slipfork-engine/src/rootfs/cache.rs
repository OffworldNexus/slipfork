//! Non-`tmpfs` content-addressed cache layout (`ADR-0009` §3, design §6).
//!
//! ```text
//! <state_dir>/cache/
//! ├── rootfs/<l0_merkle_sha256>/{app,manifest.json}
//! ├── oci/<image_sha256>/{rootfs,config.json}   # OciImageStore adapter
//! ├── runtime/<pinned crun>                     # pin-verified static crun
//! └── tmp/Dockerfile.<uuid> + runtime.<uuid>/   # scratch, never in the worktree
//! ```

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use crate::rootfs::{BaseImageError, ImageContractMetadata, RootfsDigest};

/// Scratch subdirectory for the appended `Dockerfile` and its named build context.
pub(crate) const TMP_SUBDIR: &str = "tmp";

/// Cache directories under one Slipfork state directory.
#[derive(Debug, Clone)]
pub(crate) struct CacheLayout {
    root: PathBuf,
}

impl CacheLayout {
    /// Create `<state_dir>/cache/{rootfs,oci,runtime,tmp}` and refuse a `tmpfs` root.
    pub fn new(state_dir: &Path) -> Result<Self, BaseImageError> {
        let root = state_dir.join("cache");
        for subdir in ["rootfs", "oci", "runtime", TMP_SUBDIR] {
            let path = root.join(subdir);
            std::fs::create_dir_all(&path).map_err(|error| BaseImageError::Io {
                path: path.display().to_string(),
                message: error.to_string(),
            })?;
        }
        let layout = Self { root };
        layout.ensure_non_tmpfs()?;
        Ok(layout)
    }

    /// `<cache>/rootfs/<hex>` (contains `app/` and `manifest.json`).
    pub fn rootfs_dir(&self, digest: &RootfsDigest) -> PathBuf {
        self.root.join("rootfs").join(digest.hex())
    }

    /// `<cache>/rootfs/<hex>/app`.
    pub fn app_rootfs(&self, digest: &RootfsDigest) -> PathBuf {
        self.rootfs_dir(digest).join("app")
    }

    /// `<cache>/tmp` (scratch Dockerfile + runtime context, cleaned after every build).
    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join(TMP_SUBDIR)
    }

    /// Refuse `tmpfs`: extracted rootfs trees are large, shared `EROFS` lowerdirs.
    fn ensure_non_tmpfs(&self) -> Result<(), BaseImageError> {
        let canonical = std::fs::canonicalize(&self.root).map_err(|error| BaseImageError::Io {
            path: self.root.display().to_string(),
            message: error.to_string(),
        })?;
        let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").map_err(|error| {
            BaseImageError::Io {
                path: "/proc/self/mountinfo".to_owned(),
                message: error.to_string(),
            }
        })?;
        if let Some(mount_point) = tmpfs_mount_point(&mountinfo, &canonical) {
            return Err(BaseImageError::CacheOnTmpfs {
                path: canonical.display().to_string(),
                mount_point,
            });
        }
        Ok(())
    }
}

/// Longest-prefix `tmpfs` mount point covering `path`, if any.
fn tmpfs_mount_point(mountinfo: &str, path: &Path) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for line in mountinfo.lines() {
        let Some((pre, post)) = line.split_once(" - ") else {
            continue;
        };
        let fields: Vec<&str> = pre.split_whitespace().collect();
        if fields.len() < 5 {
            continue;
        }
        let mount_point = unescape_mountinfo(fields[4]);
        let filesystem = post.split_whitespace().next().unwrap_or_default();
        if filesystem != "tmpfs" {
            continue;
        }
        let mount_path = Path::new(&mount_point);
        if !path.starts_with(mount_path) {
            continue;
        }
        let length = mount_path.as_os_str().len();
        if best.as_ref().is_none_or(|(current, _)| length >= *current) {
            best = Some((length, mount_point));
        }
    }
    best.map(|(_, mount_point)| mount_point)
}

/// Decode the octal escapes (`\040` for space) used by `/proc/self/mountinfo`.
fn unescape_mountinfo(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = String::with_capacity(field.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && bytes[index + 1..index + 4].iter().all(u8::is_ascii_digit)
        {
            let value = (bytes[index + 1] - b'0') * 64
                + (bytes[index + 2] - b'0') * 8
                + (bytes[index + 3] - b'0');
            out.push(char::from(value));
            index += 4;
        } else {
            out.push(char::from(bytes[index]));
            index += 1;
        }
    }
    out
}

/// Read `manifest.json`; a corrupt manifest is treated as a cache miss by the caller.
pub(crate) fn read_manifest(dir: &Path) -> Option<ImageContractMetadata> {
    let bytes = std::fs::read(dir.join("manifest.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Commit `manifest.json` atomically (temporary file + rename) inside `dir`.
pub(crate) fn write_manifest(
    dir: &Path,
    metadata: &ImageContractMetadata,
    suffix: &str,
) -> Result<(), BaseImageError> {
    let bytes = serde_json::to_vec_pretty(metadata).map_err(|error| BaseImageError::Io {
        path: dir.join("manifest.json").display().to_string(),
        message: error.to_string(),
    })?;
    let temporary = dir.join(format!("manifest.json.{suffix}"));
    std::fs::write(&temporary, bytes).map_err(|error| BaseImageError::Io {
        path: temporary.display().to_string(),
        message: error.to_string(),
    })?;
    let final_path = dir.join("manifest.json");
    std::fs::rename(&temporary, &final_path).map_err(|error| BaseImageError::Io {
        path: final_path.display().to_string(),
        message: error.to_string(),
    })
}

/// Remove a scratch directory, ignoring absence.
pub(crate) fn remove_dir_all(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{tmpfs_mount_point, unescape_mountinfo};

    #[test]
    fn mountinfo_escapes_decode() {
        assert_eq!(unescape_mountinfo("/tmp/with\\040space"), "/tmp/with space");
    }

    #[test]
    fn tmpfs_detection_uses_the_longest_prefix() {
        let mountinfo = "1 0 0:1 / / rw - ext4 /dev/root rw\n\
                         2 1 0:2 / /home/remy/.cache/slipfork rw - tmpfs tmpfs rw\n\
                         3 1 0:3 / /home rw - ext4 /dev/x rw\n";
        assert_eq!(
            tmpfs_mount_point(mountinfo, Path::new("/home/remy/.cache/slipfork/p/cache"))
                .as_deref(),
            Some("/home/remy/.cache/slipfork")
        );
        assert!(tmpfs_mount_point(mountinfo, Path::new("/home/remy/work")).is_none());
    }
}
