//! Thin, audited wrapper around Linux `ioctl(dst_fd, FICLONE, src_fd)` (`R10`).
//!
//! Zero silent fallback: if the backing filesystem rejects `FICLONE` (such as `tmpfs` `/tmp`
//! returning `EOPNOTSUPP`), [`reflink_tree`] fails closed with [`EngineError::ReflinkUnsupported`].

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::time::Instant;

use crate::EngineError;

/// `_IOW(0x94, 9, int)` = `0x40049409` on Linux `x86_64` and `aarch64`.
#[cfg(target_os = "linux")]
const FICLONE_IOCTL: libc::Ioctl = 0x4004_9409;

/// Metrics returned by a [`reflink_tree`] clone operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReflinkStats {
    pub files_cloned: usize,
    pub dirs_created: usize,
    pub logical_bytes: u64,
    pub elapsed_micros: u128,
}

/// Clone a single regular file's extents via kernel `ioctl(FICLONE)` without copying data blocks.
pub fn reflink_file(src: &Path, dst: &Path, mode: u32) -> Result<u64, EngineError> {
    let src_file = File::open(src).map_err(|e| EngineError::Io {
        path: src.display().to_string(),
        message: e.to_string(),
    })?;
    let meta = src_file.metadata().map_err(|e| EngineError::Io {
        path: src.display().to_string(),
        message: e.to_string(),
    })?;
    let dst_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(dst)
        .map_err(|e| EngineError::Io {
            path: dst.display().to_string(),
            message: e.to_string(),
        })?;

    if meta.len() == 0 {
        return Ok(0);
    }

    // SAFETY: `dst_file` is an open writable file descriptor and `src_file` is an open readable
    // file descriptor; `FICLONE` (`_IOW(0x94, 9, int)`) takes the source fd by integer value and
    // mutates no userspace memory.
    let rc = unsafe { libc::ioctl(dst_file.as_raw_fd(), FICLONE_IOCTL, src_file.as_raw_fd()) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        let errno = err.raw_os_error().unwrap_or(libc::EIO);
        let _ = fs::remove_file(dst);
        if matches!(
            errno,
            libc::EOPNOTSUPP | libc::EXDEV | libc::EINVAL | libc::ENOSYS
        ) {
            return Err(EngineError::ReflinkUnsupported {
                path: dst.display().to_string(),
                errno,
            });
        }
        return Err(EngineError::Io {
            path: dst.display().to_string(),
            message: err.to_string(),
        });
    }
    Ok(meta.len())
}

/// Recursively clone a directory tree using kernel `FICLONE` for every regular file.
pub fn reflink_tree(src: &Path, dst: &Path) -> Result<ReflinkStats, EngineError> {
    let started = Instant::now();
    let mut stats = ReflinkStats::default();
    reflink_dir_inner(src, dst, &mut stats)?;
    stats.elapsed_micros = started.elapsed().as_micros();
    Ok(stats)
}

fn reflink_dir_inner(src: &Path, dst: &Path, stats: &mut ReflinkStats) -> Result<(), EngineError> {
    let src_meta = fs::symlink_metadata(src).map_err(|e| EngineError::Io {
        path: src.display().to_string(),
        message: e.to_string(),
    })?;
    fs::create_dir_all(dst).map_err(|e| EngineError::Io {
        path: dst.display().to_string(),
        message: e.to_string(),
    })?;
    fs::set_permissions(
        dst,
        fs::Permissions::from_mode(src_meta.permissions().mode()),
    )
    .map_err(|e| EngineError::Io {
        path: dst.display().to_string(),
        message: e.to_string(),
    })?;
    stats.dirs_created += 1;

    let entries = fs::read_dir(src).map_err(|e| EngineError::Io {
        path: src.display().to_string(),
        message: e.to_string(),
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| EngineError::Io {
            path: src.display().to_string(),
            message: e.to_string(),
        })?;
        let child_src = entry.path();
        let child_dst = dst.join(entry.file_name());
        let ft = entry.file_type().map_err(|e| EngineError::Io {
            path: child_src.display().to_string(),
            message: e.to_string(),
        })?;
        if ft.is_dir() {
            reflink_dir_inner(&child_src, &child_dst, stats)?;
        } else if ft.is_symlink() {
            let target = fs::read_link(&child_src).map_err(|e| EngineError::Io {
                path: child_src.display().to_string(),
                message: e.to_string(),
            })?;
            symlink(&target, &child_dst).map_err(|e| EngineError::Io {
                path: child_dst.display().to_string(),
                message: e.to_string(),
            })?;
        } else if ft.is_file() {
            let meta = entry.metadata().map_err(|e| EngineError::Io {
                path: child_src.display().to_string(),
                message: e.to_string(),
            })?;
            let bytes = reflink_file(&child_src, &child_dst, meta.permissions().mode())?;
            stats.files_cloned += 1;
            stats.logical_bytes += bytes;
        }
    }
    Ok(())
}

/// Return `(geteuid, getegid)` of the calling process.
pub fn current_uid_gid() -> (u32, u32) {
    // SAFETY: `geteuid` and `getegid` are thread-safe, infallible POSIX getters taking no arguments.
    unsafe { (libc::geteuid(), libc::getegid()) }
}

/// Live probe verifying whether `dir` supports `ioctl(FICLONE)` CoW reflinks, returning elapsed microseconds.
pub fn probe_reflink_dir(dir: &Path) -> Result<u128, EngineError> {
    fs::create_dir_all(dir).map_err(|e| EngineError::Io {
        path: dir.display().to_string(),
        message: e.to_string(),
    })?;
    let pid = std::process::id();
    let src = dir.join(format!(".slipfork-probe-{pid}-a"));
    let dst = dir.join(format!(".slipfork-probe-{pid}-b"));
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&dst);
    fs::write(&src, vec![0xA5_u8; 64 * 1024]).map_err(|e| EngineError::Io {
        path: src.display().to_string(),
        message: e.to_string(),
    })?;
    let t0 = Instant::now();
    let res = reflink_file(&src, &dst, 0o600);
    let elapsed = t0.elapsed().as_micros();
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&dst);
    res.map(|_| elapsed)
}
