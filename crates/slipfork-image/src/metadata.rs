//! Single owner for restoring entry metadata (mode and mtime) onto a materialized rootfs path.
//!
//! `OCI` layers and `COPY` carry the metadata of the file they describe. A naive
//! `File::create` + `io::copy` stamps `now`, which makes `apt` treat a baked
//! `/var/lib/apt/lists/*InRelease` as freshly fetched, send a future `If-Modified-Since`, get a
//! `304`, and then reject the image-build-era `Valid-Until`. Every materializer (`unpack`,
//! `copy`) routes mode and mtime through this module, so exactly one place owns `utimensat`.

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};

/// A modification time stated by a layer header or a host source file, in the two fields
/// `utimensat` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mtime {
    secs: i64,
    nanos: u32,
}

impl Mtime {
    /// A tar header's `mtime`: whole seconds since the Unix epoch.
    ///
    /// A second that does not fit `i64` cannot be carried by `utimensat`; the value comes from an
    /// untrusted layer, so it fails closed instead of pinning a bogus far-future timestamp.
    pub(crate) fn from_unix_secs(secs: u64, path: &Path) -> Result<Self> {
        let secs = i64::try_from(secs).map_err(|_| overflow(path))?;
        Ok(Self { secs, nanos: 0 })
    }

    /// A host source entry's mtime, which Docker `COPY` preserves onto the destination.
    ///
    /// A pre-epoch value is a real clock or checkout fault, not metadata to invent: it fails
    /// closed instead of stamping a 1969 timestamp older than every mirror's copy.
    pub(crate) fn from_system_time(time: SystemTime, path: &Path) -> Result<Self> {
        let since = time.duration_since(UNIX_EPOCH).map_err(|_| {
            Error::io(
                "read the mtime of",
                path,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the source predates the Unix epoch",
                ),
            )
        })?;
        let secs = i64::try_from(since.as_secs()).map_err(|_| overflow(path))?;
        Ok(Self {
            secs,
            nanos: since.subsec_nanos(),
        })
    }
}

/// The typed failure for a timestamp `utimensat` cannot carry.
fn overflow(path: &Path) -> Error {
    Error::io(
        "represent the mtime of",
        path,
        io::Error::from_raw_os_error(libc::EOVERFLOW),
    )
}

/// Restores `mode` onto the entry at `path`.
///
/// This follows symlinks (`fs::set_permissions` has no nofollow form), so a symlink must never be
/// passed here: its target can be container-absolute (`/usr/bin/python3`) and would resolve inside
/// the host root. Callers skip symlink entries — `COPY` checks the entry type itself, and the
/// `ADD` tree walk filters `file_type().is_symlink()`. `set_mtime` is the model: it takes an
/// explicit `follow_symlinks` flag instead of guessing.
pub(crate) fn set_mode(path: &Path, mode: u32) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777))
        .map_err(|source| Error::io("chmod", path, source))
}

/// Restores `mtime` onto the entry at `path`, leaving its atime untouched.
///
/// `follow_symlinks` is false for a symlink entry: the link itself keeps the timestamp and its
/// target is never touched. atime is `UTIME_OMIT`, so restoring an image never rewrites a read
/// timestamp and the call is a pure mtime write.
pub(crate) fn set_mtime(path: &Path, mtime: Mtime, follow_symlinks: bool) -> Result<()> {
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        Error::io(
            "restore the mtime of",
            path,
            io::Error::from_raw_os_error(libc::EINVAL),
        )
    })?;
    let times = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT as libc::c_long,
        },
        libc::timespec {
            tv_sec: mtime.secs as libc::time_t,
            tv_nsec: libc::c_long::from(mtime.nanos),
        },
    ];
    let flags = if follow_symlinks {
        0
    } else {
        libc::AT_SYMLINK_NOFOLLOW
    };
    // SAFETY: `c_path` is NUL-terminated and outlives the call; `times` is exactly the
    // two-element `[atime, mtime]` array `utimensat` reads; `AT_FDCWD` resolves `c_path` as given.
    let status = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), flags) };
    if status != 0 {
        return Err(Error::io(
            "restore the mtime of",
            path,
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}
