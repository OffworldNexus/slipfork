//! `flock(2)` on `<snapshots>/.lock`: reconcile shares, gc tries exclusive.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

use super::SnapshotStore;
use crate::snapshot::{SnapshotError, SnapshotResult};

/// Reconcile/gc `flock` file inside `<store>/snapshots/`.
pub(crate) const LOCK_FILE: &str = ".lock";

pub(crate) struct StoreLock {
    _file: File,
}

impl StoreLock {
    #[allow(unsafe_code)]
    fn acquire(
        path: &Path,
        operation: libc::c_int,
        non_blocking: bool,
    ) -> SnapshotResult<Option<Self>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|source| SnapshotError::io("lock", path, source))?;
        let mut operation = operation;
        if non_blocking {
            operation |= libc::LOCK_NB;
        }
        // SAFETY: `flock(2)` only consumes the live descriptor; it dereferences no memory.
        let rc = unsafe { libc::flock(file.as_raw_fd(), operation) };
        if rc == 0 {
            return Ok(Some(Self { _file: file }));
        }
        let source = io::Error::last_os_error();
        if non_blocking && matches!(source.raw_os_error(), Some(libc::EWOULDBLOCK)) {
            Ok(None)
        } else {
            Err(SnapshotError::io("lock", path, source))
        }
    }
}

impl SnapshotStore<'_> {
    pub(crate) fn lock_shared(&self) -> SnapshotResult<StoreLock> {
        let path = self.root.join(LOCK_FILE);
        StoreLock::acquire(&path, libc::LOCK_SH, false)?.ok_or_else(|| {
            SnapshotError::message(
                "the snapshots lock could not be shared\nnext: re-run `slipfork up`; if it persists, check the store permissions",
            )
        })
    }

    pub(crate) fn try_lock_exclusive(&self) -> SnapshotResult<Option<StoreLock>> {
        let path = self.root.join(LOCK_FILE);
        StoreLock::acquire(&path, libc::LOCK_EX, true)
    }
}
