//! `syncfs(2)`: flush one whole filesystem before a CoW snapshot.
//!
//! PostgreSQL runs with `fsync=off`, so a fresh `CREATE DATABASE` still lives in page cache when
//! `SNAP_CREATE_V2` clones `PGDATA`. One `syncfs` on a single directory descriptor makes the
//! clone crash-consistent without walking every file or running a SQL `CHECKPOINT`.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

use crate::error::{Error, Result};

/// Flushes every dirty page and inode on the filesystem backing `dir`.
pub(crate) fn syncfs(dir: &File, path: &Path) -> Result<()> {
    // SAFETY: `syncfs` only reads the valid descriptor and writes no caller memory.
    if unsafe { libc::syncfs(dir.as_raw_fd()) } == 0 {
        Ok(())
    } else {
        Err(Error::FlushStore {
            path: path.to_path_buf(),
            source: io::Error::last_os_error(),
        })
    }
}
