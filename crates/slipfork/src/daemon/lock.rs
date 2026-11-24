//! The single per-user runtime lock, so a second `slipfork __daemon` exits without binding.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

/// Takes the exclusive runtime lock; `None` means another daemon already owns it.
pub(super) fn acquire(runtime_dir: &Path) -> io::Result<Option<File>> {
    fs::create_dir_all(runtime_dir)?;
    let path = runtime_dir.join("slipfork-daemon.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    // SAFETY: `flock` only locks the live descriptor; no memory is shared.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if locked == 0 {
        return Ok(Some(file));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(None)
    } else {
        Err(error)
    }
}
