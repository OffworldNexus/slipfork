//! The tmpfs-only pod runtime directory (`$XDG_RUNTIME_DIR/slipfork/pods/<world>`).

use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::layout::WorldName;
use crate::secure_file::ensure_private_dir;

/// `TMPFS_MAGIC` from `<linux/magic.h>`.
const TMPFS_SUPER_MAGIC: u64 = 0x0102_1994;

/// One world's mode-`0700` tmpfs directory tree for OCI bundles and runtime secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePods {
    root: PathBuf,
}

impl RuntimePods {
    /// Creates `<runtime_dir>/slipfork/pods/<world>` mode `0700` and fails closed unless the
    /// directory really sits on tmpfs — secrets must never reach persistent disk.
    pub fn prepare(runtime_dir: &Path, world: &WorldName) -> Result<Self> {
        let root = Self::world_root(runtime_dir, world);
        ensure_private_dir(&root)?;
        verify_tmpfs(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .map_err(|source| Error::io("chmod", &root, source))?;
        Ok(Self { root })
    }

    /// `<runtime_dir>/slipfork/pods/<world>`: the one place that composes the world's tmpfs root.
    #[must_use]
    pub fn world_root(runtime_dir: &Path, world: &WorldName) -> PathBuf {
        runtime_dir
            .join("slipfork")
            .join("pods")
            .join(world.as_str())
    }

    /// Removes one world's ephemeral pod root (bundles, `crun` container state, logs) after an
    /// aborted boot, so the same fork name retries against a clean tmpfs instead of inherited
    /// `crun` state (`container 'x' already exists`) or half-written bundles.
    pub fn purge(runtime_dir: &Path, world: &WorldName) -> io::Result<()> {
        match fs::remove_dir_all(Self::world_root(runtime_dir, world)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// The world's pod root.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// `<root>/<pod-id>`; the caller creates it mode `0700`.
    #[must_use]
    pub fn pod_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }
}

fn verify_tmpfs(path: &Path) -> Result<()> {
    let dir = File::open(path).map_err(|source| Error::io("open", path, source))?;
    // SAFETY: `dir` keeps a live descriptor and `statfs` is plain-old-data.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: the descriptor is live and the kernel writes exactly one `statfs`.
    let ret = unsafe { libc::fstatfs(dir.as_raw_fd(), &mut stat) };
    if ret == -1 {
        return Err(Error::io(
            "probe the filesystem of",
            path,
            io::Error::last_os_error(),
        ));
    }
    if stat.f_type as u64 == TMPFS_SUPER_MAGIC {
        Ok(())
    } else {
        Err(Error::RuntimeNotTmpfs {
            path: path.to_path_buf(),
        })
    }
}
