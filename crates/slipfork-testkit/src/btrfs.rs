//! Test-only [`SubvolumeDriver`] over real directories (`ARCHITECTURE.md` §1).
//!
//! [`TestTreeDriver`] emulates the observable [`SubvolumeDriver`] contract without a single kernel
//! ioctl, so unit and integration suites run on any developer machine:
//!
//! - **Subvolume identity lives on disk.** Creating or cloning a subvolume writes the sibling
//!   marker `.<name>.slipfork-subvol` in its parent; [`SubvolumeDriver::list_children`] reports
//!   exactly the directories carrying that marker (plain directories, files, and symlinks are
//!   ignored) and [`SubvolumeDriver::destroy_subvolume`] resolves identity from it. A second
//!   driver instance over the same state directory therefore behaves like the kernel and sees what
//!   the first one created.
//! - **Read-only seal.** [`SubvolumeDriver::snapshot_ro`] chmods the snapshot **root** to `0o555`;
//!   a writable clone carries `0o755`. The kernel seals the whole subvolume and this double seals
//!   the root, so nested files are not individually sealed: the contract suites assert is that a
//!   write **at the snapshot root** fails (`EACCES`) while the sealed payload stays readable.
//! - [`SubvolumeDriver::destroy_subvolume`] clears the read-only bit first and then removes the
//!   tree, appending [`SubvolumeOp::ClearReadonly`] before [`SubvolumeOp::DestroySubvolume`] so
//!   suites assert the `ADR-0010` §3 ordering through the injected driver.
//! - [`SubvolumeDriver::probe_facts`] reports the emulated mount facts; the provided
//!   [`SubvolumeDriver::probe`] then runs the shared scratch round-trip, so `doctor`'s field
//!   mapping is exercised end to end. The emulated mount covers every path, so `probed_path` is the
//!   requested path itself rather than an existing ancestor.
//!
//! Names are single path components (the engine only ever passes one), and every call is
//! journalled. The driver is reachable only through the `publish = false` testkit crate's
//! `[dev-dependencies]` consumers; production builds contain no reference to it and no Cargo
//! feature selects it.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use slipfork_engine::EngineError;
use slipfork_engine::btrfs::{
    BTRFS_MOUNT_HINT, BTRFS_SUPER_MAGIC, ProbeFacts, SubvolumeDriver, require_btrfs,
};

/// Suffix of the sibling marker that records a subvolume: `.<name>.slipfork-subvol`.
const MARKER_SUFFIX: &str = ".slipfork-subvol";

/// One observable driver call, in call order.
///
/// Suites assert lifecycle ordering (for example that a read-only flag is cleared before a
/// subvolume is destroyed) against the journal instead of against real filesystem state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubvolumeOp {
    /// [`SubvolumeDriver::create_subvolume`].
    CreateSubvolume {
        /// Created subvolume path.
        path: PathBuf,
    },
    /// [`SubvolumeDriver::snapshot_ro`].
    SnapshotRo {
        /// Source subvolume.
        source: PathBuf,
        /// Read-only snapshot path.
        dest: PathBuf,
    },
    /// [`SubvolumeDriver::snapshot_writable`].
    SnapshotWritable {
        /// Source subvolume.
        source: PathBuf,
        /// Writable clone path.
        dest: PathBuf,
    },
    /// The read-only flag was cleared on `path` (inside `destroy_subvolume`).
    ClearReadonly {
        /// Subvolume whose flag was cleared.
        path: PathBuf,
    },
    /// [`SubvolumeDriver::destroy_subvolume`].
    DestroySubvolume {
        /// Destroyed subvolume path.
        path: PathBuf,
    },
    /// [`SubvolumeDriver::list_children`].
    ListChildren {
        /// Enumerated parent directory.
        parent: PathBuf,
    },
}

/// Directory-backed [`SubvolumeDriver`] for tests (`ARCHITECTURE.md` §1).
///
/// Every emulated subvolume lives in the real filesystem, so suites can write payloads into it and
/// assert read-only/writable behaviour with ordinary file operations.
pub struct TestTreeDriver {
    fstype: i64,
    noatime: bool,
    destroy_errno: Option<i32>,
    journal: Mutex<Vec<SubvolumeOp>>,
}

impl TestTreeDriver {
    /// A driver emulating a Btrfs mount with `user_subvol_rm_allowed` and `noatime`.
    pub fn btrfs() -> Self {
        Self {
            fstype: BTRFS_SUPER_MAGIC,
            noatime: true,
            destroy_errno: None,
            journal: Mutex::new(Vec::new()),
        }
    }

    /// A driver emulating a foreign filesystem (`xfs`, `ext4`, `tmpfs`, ...) reporting `fstype`.
    pub fn non_btrfs(fstype: i64) -> Self {
        Self {
            fstype,
            noatime: false,
            destroy_errno: None,
            journal: Mutex::new(Vec::new()),
        }
    }

    /// Emulate a Btrfs mount that denies subvolume deletion with `errno` (for example `EPERM`
    /// when `user_subvol_rm_allowed` is absent).
    pub fn with_destroy_errno(mut self, errno: i32) -> Self {
        self.destroy_errno = Some(errno);
        self
    }

    /// Every driver call so far, in call order.
    pub fn journal(&self) -> Vec<SubvolumeOp> {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<SubvolumeOp>> {
        self.journal
            .lock()
            .expect("test driver journal is never poisoned")
    }

    fn record(&self, op: SubvolumeOp) {
        self.lock().push(op);
    }

    fn subvolume_error(op: &'static str, path: &Path, err: &io::Error) -> EngineError {
        match err.raw_os_error() {
            Some(errno) => EngineError::Ioctl {
                op,
                path: path.to_path_buf(),
                errno,
            },
            None => EngineError::Io {
                path: path.display().to_string(),
                message: err.to_string(),
            },
        }
    }

    fn destroy_error(path: &Path, errno: i32) -> EngineError {
        if errno == libc::EPERM {
            EngineError::SubvolumeDestroyDenied {
                path: path.to_path_buf(),
                errno,
                mount_hint: BTRFS_MOUNT_HINT,
            }
        } else {
            EngineError::Ioctl {
                op: "snap_destroy",
                path: path.to_path_buf(),
                errno,
            }
        }
    }

    fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            let target = to.join(entry.file_name());
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                Self::copy_tree(&entry.path(), &target)?;
            } else if file_type.is_file() {
                fs::copy(entry.path(), &target)?;
            } else if file_type.is_symlink() {
                std::os::unix::fs::symlink(fs::read_link(entry.path())?, &target)?;
            } else {
                return Err(io::Error::other(format!(
                    "test driver cannot clone '{}': unsupported entry type",
                    entry.path().display()
                )));
            }
        }
        Ok(())
    }

    fn set_mode(path: &Path, mode: u32) -> Result<(), EngineError> {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|err| Self::subvolume_error("chmod", path, &err))
    }

    fn clone_tree(
        &self,
        source: &Path,
        dest_parent: &Path,
        name: &str,
        readonly: bool,
    ) -> Result<PathBuf, EngineError> {
        let dest = dest_parent.join(name);
        if dest.exists() {
            return Err(EngineError::Ioctl {
                op: "snap_create",
                path: dest,
                errno: libc::EEXIST,
            });
        }
        Self::copy_tree(source, &dest)
            .map_err(|err| Self::subvolume_error("snap_create", &dest, &err))?;
        Self::set_mode(&dest, if readonly { 0o555 } else { 0o755 })?;
        Self::write_marker(dest_parent, name)?;
        Ok(dest)
    }

    fn write_marker(parent: &Path, name: &str) -> Result<(), EngineError> {
        let marker = marker_path(parent, name);
        fs::write(&marker, b"").map_err(|err| Self::subvolume_error("subvol_create", &marker, &err))
    }
}

/// Sibling marker recording that `<parent>/<name>` is a subvolume.
///
/// Identity is written to disk (like the kernel's), so it survives a driver instance and is shared
/// by every driver over the same state directory.
fn marker_path(parent: &Path, name: &str) -> PathBuf {
    parent.join(format!(".{name}{MARKER_SUFFIX}"))
}

/// The emulation only creates top-level subvolumes, so a name must be one path component.
fn validate_name(parent: &Path, name: &str) -> Result<(), EngineError> {
    let invalid =
        name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0');
    if invalid {
        return Err(EngineError::Io {
            path: parent.join(name).display().to_string(),
            message: format!("test driver names are single path components, got '{name}'"),
        });
    }
    Ok(())
}

impl SubvolumeDriver for TestTreeDriver {
    fn probe_facts(&self, path: &Path) -> Result<ProbeFacts, EngineError> {
        Ok(ProbeFacts {
            probed_path: path.to_path_buf(),
            fstype: self.fstype,
            noatime: self.noatime,
        })
    }

    fn ensure_supported(&self, path: &Path) -> Result<(), EngineError> {
        require_btrfs(self.fstype, path)
    }

    fn create_subvolume(&self, parent: &Path, name: &str) -> Result<PathBuf, EngineError> {
        validate_name(parent, name)?;
        let path = parent.join(name);
        fs::create_dir(&path).map_err(|err| Self::subvolume_error("subvol_create", &path, &err))?;
        if let Err(err) = Self::write_marker(parent, name) {
            // Roll back the directory: a subvolume whose identity marker could not be written must
            // not exist, or the kernel-equivalent contract would be broken for every peer driver.
            fs::remove_dir(&path).map_err(|rollback| EngineError::Io {
                path: path.display().to_string(),
                message: format!(
                    "cannot roll back subvolume directory after marker failure '{err}': {rollback}"
                ),
            })?;
            return Err(err);
        }
        self.record(SubvolumeOp::CreateSubvolume { path: path.clone() });
        Ok(path)
    }

    fn snapshot_ro(
        &self,
        source_subvol: &Path,
        dest_parent: &Path,
        name: &str,
    ) -> Result<PathBuf, EngineError> {
        validate_name(dest_parent, name)?;
        let dest = self.clone_tree(source_subvol, dest_parent, name, true)?;
        self.record(SubvolumeOp::SnapshotRo {
            source: source_subvol.to_path_buf(),
            dest: dest.clone(),
        });
        Ok(dest)
    }

    fn snapshot_writable(
        &self,
        source_subvol: &Path,
        dest_parent: &Path,
        name: &str,
    ) -> Result<PathBuf, EngineError> {
        validate_name(dest_parent, name)?;
        let dest = self.clone_tree(source_subvol, dest_parent, name, false)?;
        self.record(SubvolumeOp::SnapshotWritable {
            source: source_subvol.to_path_buf(),
            dest: dest.clone(),
        });
        Ok(dest)
    }

    fn destroy_subvolume(&self, parent: &Path, name: &str) -> Result<(), EngineError> {
        validate_name(parent, name)?;
        let path = parent.join(name);
        let marker = marker_path(parent, name);
        if !marker.is_file() {
            return Err(EngineError::Ioctl {
                op: "snap_destroy",
                path,
                errno: libc::ENOENT,
            });
        }
        if let Some(errno) = self.destroy_errno {
            self.record(SubvolumeOp::DestroySubvolume { path: path.clone() });
            return Err(Self::destroy_error(&path, errno));
        }
        if !path.is_dir() {
            fs::remove_file(&marker)
                .map_err(|err| Self::subvolume_error("snap_destroy", &marker, &err))?;
            return Err(EngineError::Ioctl {
                op: "snap_destroy",
                path,
                errno: libc::ENOENT,
            });
        }
        let mode = fs::metadata(&path)
            .map_err(|err| Self::subvolume_error("subvol_getflags", &path, &err))?
            .permissions()
            .mode();
        if mode & 0o200 == 0 {
            Self::set_mode(&path, 0o755)?;
            self.record(SubvolumeOp::ClearReadonly { path: path.clone() });
        }
        self.record(SubvolumeOp::DestroySubvolume { path: path.clone() });
        fs::remove_dir_all(&path)
            .map_err(|err| Self::subvolume_error("snap_destroy", &path, &err))?;
        fs::remove_file(&marker).map_err(|err| Self::subvolume_error("snap_destroy", &marker, &err))
    }

    fn list_children(&self, parent: &Path) -> Result<Vec<PathBuf>, EngineError> {
        self.record(SubvolumeOp::ListChildren {
            parent: parent.to_path_buf(),
        });
        // Same fail-closed gate as the kernel driver: enumerating subvolumes off Btrfs is a
        // contract error, not an empty list.
        require_btrfs(self.fstype, parent)?;
        let entries = fs::read_dir(parent).map_err(|err| EngineError::Io {
            path: parent.display().to_string(),
            message: err.to_string(),
        })?;
        let mut children = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| EngineError::Io {
                path: parent.display().to_string(),
                message: err.to_string(),
            })?;
            let is_dir = entry.file_type().is_ok_and(|file_type| file_type.is_dir());
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if is_dir && marker_path(parent, &name).is_file() {
                children.push(entry.path());
            }
        }
        children.sort();
        Ok(children)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_btrfs_driver_reports_its_emulated_fstype_and_touches_no_subvolume() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let driver = TestTreeDriver::non_btrfs(0xef53);
        let report = driver.probe(&state_dir).expect("probe");

        assert_eq!(report.fstype, 0xef53);
        assert!(!report.btrfs_super_magic);
        assert_eq!(report.user_subvol_rm_allowed, None);
        assert!(!state_dir.exists(), "off Btrfs nothing is created");
        assert!(
            driver.journal().is_empty(),
            "off Btrfs the probe touches no subvolume at all"
        );
    }

    #[test]
    fn btrfs_driver_probe_runs_the_scratch_round_trip_and_leaves_no_residue() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let driver = TestTreeDriver::btrfs();
        let report = driver.probe(&state_dir).expect("probe");

        assert!(report.btrfs_super_magic);
        assert_eq!(report.user_subvol_rm_allowed, Some(Ok(())));
        assert!(report.noatime);
        assert!(
            !state_dir.exists(),
            "the state dir probe created is removed again"
        );
        assert!(
            driver
                .journal()
                .iter()
                .any(|op| matches!(op, SubvolumeOp::CreateSubvolume { .. })),
            "the round-trip created a scratch subvolume"
        );
    }

    #[test]
    fn probe_records_the_raw_errno_of_a_failing_scratch_round_trip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let driver = TestTreeDriver::btrfs().with_destroy_errno(libc::EPERM);
        let report = driver
            .probe(&state_dir)
            .expect("probe records the errno, it does not fail");

        assert_eq!(report.user_subvol_rm_allowed, Some(Err(libc::EPERM)));
        assert!(
            state_dir.exists(),
            "an undeletable scratch subvolume is reported, not hidden"
        );
    }

    #[test]
    fn probe_rejects_a_relative_path() {
        let driver = TestTreeDriver::btrfs();
        match driver.probe(Path::new("relative/state")) {
            Err(EngineError::Io { path, message }) => {
                assert_eq!(path, "relative/state");
                assert!(message.contains("absolute"), "{message}");
            }
            other => panic!("a relative probe path must be rejected: {other:?}"),
        }
        assert!(driver.journal().is_empty());
    }

    #[test]
    fn ensure_supported_is_magic_only_and_ignores_the_scratch_round_trip() {
        TestTreeDriver::btrfs()
            .with_destroy_errno(libc::EPERM)
            .ensure_supported(Path::new("/emulated-btrfs"))
            .expect("a mount without user_subvol_rm_allowed still boots");
        match TestTreeDriver::non_btrfs(0x5846_5342).ensure_supported(Path::new("/emulated-xfs")) {
            Err(EngineError::BtrfsRequired {
                actual_fstype,
                mount_hint,
                ..
            }) => {
                assert_eq!(actual_fstype, 0x5846_5342);
                assert_eq!(mount_hint, BTRFS_MOUNT_HINT);
            }
            other => panic!("expected BtrfsRequired, got {other:?}"),
        }
    }

    #[test]
    fn read_only_snapshot_is_cleared_before_destroy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = TestTreeDriver::btrfs();
        let source = driver
            .create_subvolume(tmp.path(), "world")
            .expect("create subvolume");
        fs::write(source.join("data"), b"payload").expect("write payload");
        let snapshot = driver
            .snapshot_ro(&source, tmp.path(), "world-ro")
            .expect("read-only snapshot");
        assert_eq!(
            fs::metadata(&snapshot)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o200,
            0,
            "a read-only snapshot carries no owner-write bit"
        );
        assert!(
            fs::write(snapshot.join("new-file"), b"nope").is_err(),
            "a write at the snapshot root fails"
        );

        driver
            .destroy_subvolume(tmp.path(), "world-ro")
            .expect("destroy read-only snapshot");

        let journal = driver.journal();
        let cleared = journal
            .iter()
            .position(|op| matches!(op, SubvolumeOp::ClearReadonly { .. }))
            .expect("read-only flag was cleared");
        let destroyed = journal
            .iter()
            .position(
                |op| matches!(op, SubvolumeOp::DestroySubvolume { path } if path == &snapshot),
            )
            .expect("snapshot was destroyed");
        assert!(
            cleared < destroyed,
            "clear read-only must precede destroy: {journal:?}"
        );
        assert!(!snapshot.exists());
    }

    #[test]
    fn driver_destroy_denied_by_the_mount_maps_to_subvolume_destroy_denied() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = TestTreeDriver::btrfs().with_destroy_errno(libc::EPERM);
        let path = driver
            .create_subvolume(tmp.path(), "world")
            .expect("create subvolume");
        match driver.destroy_subvolume(tmp.path(), "world") {
            Err(EngineError::SubvolumeDestroyDenied {
                path: got,
                errno,
                mount_hint,
            }) => {
                assert_eq!(got, path);
                assert_eq!(errno, libc::EPERM);
                assert_eq!(mount_hint, BTRFS_MOUNT_HINT);
            }
            other => panic!("expected SubvolumeDestroyDenied, got {other:?}"),
        }
    }

    #[test]
    fn destroying_an_unknown_subvolume_fails_closed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        match TestTreeDriver::btrfs().destroy_subvolume(tmp.path(), "missing") {
            Err(EngineError::Ioctl { errno, .. }) => assert_eq!(errno, libc::ENOENT),
            other => panic!("expected Ioctl(ENOENT), got {other:?}"),
        }
    }

    #[test]
    fn writable_clone_of_a_read_only_snapshot_is_writable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = TestTreeDriver::btrfs();
        let source = driver
            .create_subvolume(tmp.path(), "world")
            .expect("create");
        fs::write(source.join("data"), b"payload").expect("write payload");
        let sealed = driver
            .snapshot_ro(&source, tmp.path(), "sealed")
            .expect("ro");
        let clone = driver
            .snapshot_writable(&sealed, tmp.path(), "clone")
            .expect("writable clone");

        assert_ne!(
            fs::metadata(&clone).expect("metadata").permissions().mode() & 0o200,
            0,
            "a writable clone carries the owner-write bit"
        );
        fs::write(clone.join("data"), b"changed").expect("clone is writable");
        assert_eq!(fs::read(sealed.join("data")).expect("sealed"), b"payload");
    }

    #[test]
    fn list_children_returns_only_subvolume_roots_sorted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = TestTreeDriver::btrfs();
        for name in ["zeta", "alpha", "mid"] {
            driver.create_subvolume(tmp.path(), name).expect("create");
        }
        fs::create_dir(tmp.path().join("plain-directory")).expect("plain dir");
        fs::write(tmp.path().join(".seed.lock"), b"").expect("lock file");

        let children = driver.list_children(tmp.path()).expect("list children");
        assert_eq!(
            children,
            vec![
                tmp.path().join("alpha"),
                tmp.path().join("mid"),
                tmp.path().join("zeta"),
            ]
        );
    }

    #[test]
    fn non_btrfs_driver_fails_closed_from_list_children() {
        let tmp = tempfile::tempdir().expect("tempdir");
        match TestTreeDriver::non_btrfs(0x5846_5342).list_children(tmp.path()) {
            Err(EngineError::BtrfsRequired {
                actual_fstype,
                mount_hint,
                ..
            }) => {
                assert_eq!(actual_fstype, 0x5846_5342);
                assert_eq!(mount_hint, BTRFS_MOUNT_HINT);
            }
            other => panic!("expected BtrfsRequired, got {other:?}"),
        }
    }

    #[test]
    fn a_second_driver_instance_lists_and_destroys_what_the_first_created() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let creator = TestTreeDriver::btrfs();
        let created = creator
            .create_subvolume(tmp.path(), "world")
            .expect("create subvolume");
        creator
            .snapshot_ro(&created, tmp.path(), "world-ro")
            .expect("read-only snapshot");

        let peer = TestTreeDriver::btrfs();
        assert_eq!(
            peer.list_children(tmp.path()).expect("list children"),
            vec![created.clone(), tmp.path().join("world-ro")],
            "subvolume identity is on disk, not in the creating instance"
        );

        peer.destroy_subvolume(tmp.path(), "world-ro")
            .expect("destroy from the peer instance");
        peer.destroy_subvolume(tmp.path(), "world")
            .expect("destroy from the peer instance");
        assert_eq!(
            peer.list_children(tmp.path()).expect("list children"),
            Vec::<PathBuf>::new()
        );
        assert!(
            creator.journal().iter().all(|op| !matches!(
                op,
                SubvolumeOp::DestroySubvolume { .. } | SubvolumeOp::ClearReadonly { .. }
            )),
            "the peer's destroys are journalled on the peer instance"
        );
    }
}
