use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

use crate::btrfs::{SubvolumeFlags, ioctls};
use crate::error::{Error, Result};

/// `BTRFS_SUPER_MAGIC` from `<linux/magic.h>`.
pub(crate) const BTRFS_SUPER_MAGIC: u64 = 0x9123_683E;

/// Reports whether `path` sits on a Btrfs filesystem.
pub fn is_btrfs_path(path: &Path) -> Result<bool> {
    let dir = File::open(path).map_err(|source| Error::io("open", path, source))?;
    is_btrfs(&dir, path)
}

/// Reports whether `path` is the root directory of a Btrfs subvolume.
pub fn is_subvolume_path(path: &Path) -> Result<bool> {
    let dir = File::open(path).map_err(|source| Error::io("open", path, source))?;
    if !is_btrfs(&dir, path)? {
        return Ok(false);
    }
    is_subvolume(&dir, path)
}

pub(crate) fn is_btrfs(dir: &File, path: &Path) -> Result<bool> {
    let mut stat = zeroed_statfs();
    // SAFETY: `dir` keeps a live descriptor for the call and `stat` is a writable `statfs`.
    let ret = unsafe { libc::fstatfs(dir.as_raw_fd(), &mut stat) };
    if ret == -1 {
        return Err(Error::io(
            "probe the filesystem of",
            path,
            io::Error::last_os_error(),
        ));
    }
    Ok(stat.f_type as u64 == BTRFS_SUPER_MAGIC)
}

pub(crate) fn is_subvolume(dir: &File, path: &Path) -> Result<bool> {
    match ioctls::get_flags(dir, path) {
        Ok(_) => Ok(true),
        Err(Error::Io { source, .. }) if not_a_subvolume_root(&source) => Ok(false),
        Err(err) => Err(err),
    }
}

pub(crate) fn subvolume_flags(dir: &File, path: &Path) -> Result<SubvolumeFlags> {
    ioctls::get_flags(dir, path)
}

/// `GETFLAGS` returns `EINVAL` for a plain directory and `ENOTTY` off Btrfs entirely.
fn not_a_subvolume_root(err: &io::Error) -> bool {
    matches!(err.raw_os_error(), Some(code) if code == libc::EINVAL || code == libc::ENOTTY)
}

fn zeroed_statfs() -> libc::statfs {
    // SAFETY: `statfs` is plain-old-data; every field is either rewritten by `fstatfs` or unused.
    unsafe { std::mem::zeroed() }
}
