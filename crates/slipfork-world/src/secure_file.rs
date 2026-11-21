//! Private-file helpers shared by the tmpfs pod bundles and namespace tables.

use std::fs::{self, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use crate::error::{Error, Result};

/// Creates `path` (and its parents) mode `0600` and writes `bytes` through one descriptor.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| Error::io("write", path, source))?;
    file.write_all(bytes)
        .map_err(|source| Error::io("write", path, source))
}

/// Creates `path` mode `0700`; pod bundles and their parent never need group/other access.
pub(crate) fn ensure_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|source| Error::io("create", path, source))?;
    fs::set_permissions(path, Permissions::from_mode(0o700))
        .map_err(|source| Error::io("chmod", path, source))
}

/// Creates an empty placeholder file (mode `0600`) for a file-to-file bind target.
pub(crate) fn touch_private(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    write_private(path, b"")
}
