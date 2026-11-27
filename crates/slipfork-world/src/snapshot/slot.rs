//! A writable `.tmp-<key>-<pid>-<n>/` slot: materialize, seal, atomically publish (§6.2).

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::{NodeKey, SnapshotError, SnapshotResult};
use crate::port::{Dirtiness, WorldEngine};

/// What `publish` did to `<snapshots>/<key>/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishOutcome {
    /// Renamed into place.
    Published,
    /// A racing reconcile won; the winner was adopted and this slot destroyed.
    Adopted,
    /// `RENAME_EXCHANGE` replaced a same-key sealed twin (cold rebuild).
    Replaced,
}

/// A writable staging subvolume nobody else ever reads.
pub struct SnapshotSlot<'a> {
    engine: &'a dyn WorldEngine,
    root: PathBuf,
    path: PathBuf,
    key: NodeKey,
    sealed: bool,
    done: bool,
}

impl<'a> SnapshotSlot<'a> {
    /// Creates the slot by cloning `parent`, or as an empty subvolume when there is none.
    pub fn create(
        engine: &'a dyn WorldEngine,
        root: &Path,
        key: NodeKey,
        ordinal: u64,
        parent: Option<&Path>,
    ) -> SnapshotResult<Self> {
        let path = root.join(format!(
            ".tmp-{}-{}-{ordinal}",
            key.as_hex(),
            std::process::id()
        ));
        match parent {
            Some(parent) => engine.clone_subvolume(parent, &path)?,
            None => engine.create_subvolume(&path)?,
        }
        Ok(Self {
            engine,
            root: root.to_path_buf(),
            path,
            key,
            sealed: false,
            done: false,
        })
    }

    /// The writable staging path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The node this slot will publish.
    #[must_use]
    pub fn key(&self) -> NodeKey {
        self.key
    }

    /// Runs the materializer against the writable slot.
    pub fn materialize<T>(
        &mut self,
        run: impl FnOnce(&Path) -> SnapshotResult<T>,
    ) -> SnapshotResult<T> {
        run(&self.path)
    }

    /// Seals the slot `BTRFS_SUBVOL_RDONLY`; publishing an unsealed slot is refused.
    pub fn seal(&mut self) -> SnapshotResult<()> {
        self.engine.seal(&self.path)?;
        self.sealed = true;
        Ok(())
    }

    /// Renames the sealed slot to `<key>/`; losing the race adopts the winner.
    pub fn publish(&mut self) -> SnapshotResult<PublishOutcome> {
        self.require_sealed()?;
        let dest = self.dest();
        match fs::rename(&self.path, &dest) {
            Ok(()) => {
                self.done = true;
                Ok(PublishOutcome::Published)
            }
            Err(source)
                if matches!(
                    source.raw_os_error(),
                    Some(libc::EEXIST | libc::ENOTEMPTY | libc::EISDIR)
                ) =>
            {
                self.abandon();
                Ok(PublishOutcome::Adopted)
            }
            Err(source) => Err(SnapshotError::io("publish", &dest, source)),
        }
    }

    /// Replaces a same-key sealed twin with `RENAME_EXCHANGE`, then destroys the old content.
    ///
    /// Pods cloned from the twin are independent CoW subvolumes, so they keep the old content
    /// while `<key>/` shows the rebuild. The caller guarantees a sealed twin exists.
    #[allow(unsafe_code)]
    pub fn publish_exchange(&mut self) -> SnapshotResult<PublishOutcome> {
        self.require_sealed()?;
        let dest = self.dest();
        rename_exchange(&self.path, &dest)?;
        // SAFETY-free after the syscall: the exchanged-out twin now sits at the slot path.
        self.engine.destroy(&self.path, Dirtiness::Clean, true)?;
        self.done = true;
        Ok(PublishOutcome::Replaced)
    }

    /// Destroys this slot; a no-op once published or adopted.
    pub fn abandon(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        destroy_slot(self.engine, &self.path);
    }

    fn require_sealed(&self) -> SnapshotResult<()> {
        if self.sealed {
            Ok(())
        } else {
            Err(SnapshotError::message(
                "a snapshot slot must be sealed before it can publish\nnext: this is an internal ordering bug; report it with `slipfork doctor`",
            ))
        }
    }

    fn dest(&self) -> PathBuf {
        self.root.join(self.key.as_hex())
    }
}

fn destroy_slot(engine: &dyn WorldEngine, path: &Path) {
    if !path.exists() {
        return;
    }
    if crate::btrfs::is_subvolume_path(path).unwrap_or(false) {
        let _ = engine.destroy(path, Dirtiness::Clean, true);
    } else {
        let _ = fs::remove_dir_all(path);
    }
}

#[allow(unsafe_code)]
fn rename_exchange(from: &Path, to: &Path) -> SnapshotResult<()> {
    let from_name = CString::new(from.as_os_str().as_bytes()).map_err(|_| {
        SnapshotError::message(
            "a snapshot path contains a NUL byte\nnext: this is an internal path bug; report it with `slipfork doctor`",
        )
    })?;
    let to_name = CString::new(to.as_os_str().as_bytes()).map_err(|_| {
        SnapshotError::message(
            "a snapshot path contains a NUL byte\nnext: this is an internal path bug; report it with `slipfork doctor`",
        )
    })?;
    // SAFETY: both C strings outlive the syscall and name directories inside one snapshots root.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from_name.as_ptr(),
            libc::AT_FDCWD,
            to_name.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(SnapshotError::io(
            "exchange",
            to,
            std::io::Error::last_os_error(),
        ))
    }
}
