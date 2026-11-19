//! Btrfs subvolume CoW and host-probe seam ([`KernelBtrfsDriver`], `ADR-0010` §§3–5).
//!
//! This module is the **sole** owner of Btrfs filesystem and ioctl probing in the workspace: the
//! magic-only `slipforkd` boot gate ([`KernelBtrfsDriver::ensure_supported`]), the pre-daemon
//! `slipfork doctor` probe ([`KernelBtrfsDriver::probe`]), the
//! `BTRFS_IOC_*` subvolume lifecycle, and the single mount remediation renderer
//! [`mount_hint`]. [`KernelBtrfsDriver`] is the only implementation: it holds no state, so
//! the composition root constructs it directly and the engine takes it by value.
//!
//! `unsafe` is confined to the `statfs`/`statvfs`/`ioctl` calls below, each with a per-call
//! `// SAFETY:` proof. Every failure is typed: a non-Btrfs `f_type` fails closed with
//! [`EngineError::BtrfsRequired`], a destroy-path `EPERM` with
//! [`EngineError::SubvolumeDestroyDenied`], and every other driver syscall error with
//! [`EngineError::Ioctl`]. There is no fallback filesystem and no degraded mode.

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::mem;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use slipfork_world::EngineError;

/// `BTRFS_SUPER_MAGIC` (`<linux/magic.h>`): the only `statfs(2).f_type` Slipfork accepts.
pub const BTRFS_SUPER_MAGIC: i64 = 0x9123_683e;

/// Single mount remediation shared by [`EngineError::BtrfsRequired`] and
/// [`EngineError::SubvolumeDestroyDenied`] (`ADR-0010` §4).
///
/// The probed `path` is rendered into the command, so the remedy names exactly the directory the
/// kernel refused (`<device>` stays a placeholder: only the operator knows the backing device).
pub fn mount_hint(path: &Path) -> String {
    format!(
        "sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> {}",
        path.display()
    )
}

/// Normative `slipfork doctor` Btrfs evidence (`ADR-0010` §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtrfsProbeReport {
    /// Requested state directory.
    pub state_dir: PathBuf,
    /// Existing directory inspected by `statfs`/`statvfs` (`state_dir` or its nearest existing
    /// ancestor).
    pub probed_path: PathBuf,
    /// Raw `statfs(probed_path).f_type`.
    pub fstype: i64,
    /// `fstype == BTRFS_SUPER_MAGIC`.
    pub btrfs_super_magic: bool,
    /// `None` off Btrfs (nothing was created), `Some(Ok(()))` when the scratch create -> read-only
    /// snapshot -> clear read-only -> destroy round-trip succeeded, or `Some(Err(errno))` with the
    /// raw kernel `errno` of the first failing step (for example `EPERM` when the mount lacks
    /// `user_subvol_rm_allowed`).
    pub user_subvol_rm_allowed: Option<Result<(), i32>>,
    /// `statvfs(probed_path)` `ST_NOATIME` evidence (advisory only).
    pub noatime: bool,
}

/// Statfs/statvfs evidence the kernel driver observed for a path: the only kernel-specific probe
/// input.
///
/// `ADR-0010` §4's report is assembled once, by [`KernelBtrfsDriver::probe`], so the driver
/// reports facts about its own substrate and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeFacts {
    /// Existing directory the driver inspected.
    pub probed_path: PathBuf,
    /// `statfs(probed_path).f_type`.
    pub fstype: i64,
    /// `statvfs(probed_path).f_flag & ST_NOATIME`.
    pub noatime: bool,
}

/// The single `BTRFS_SUPER_MAGIC` gate (`ADR-0010` §4), shared by
/// [`KernelBtrfsDriver::ensure_supported`] and [`KernelBtrfsDriver::list_children`].
pub fn require_btrfs(fstype: i64, path: &Path) -> Result<(), EngineError> {
    if is_btrfs(fstype) {
        Ok(())
    } else {
        Err(EngineError::BtrfsRequired {
            path: path.to_path_buf(),
            actual_fstype: fstype,
            mount_hint: mount_hint(path),
        })
    }
}

/// The one `f_type` classification: `BtrfsProbeReport::btrfs_super_magic` and [`require_btrfs`]
/// both read it, so the magic comparison exists once.
fn is_btrfs(fstype: i64) -> bool {
    fstype == BTRFS_SUPER_MAGIC
}

/// `path` must be absolute: the probe never resolves anything against the ambient
/// `current_dir()`.
fn require_absolute(path: &Path) -> Result<(), EngineError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(EngineError::Io {
            path: path.display().to_string(),
            message: "probe path must be absolute (no ambient current_dir() resolution)".to_owned(),
        })
    }
}

/// Remove the empty state directory this probe created.
///
/// A non-empty directory means the scratch round-trip could not destroy what it created: that
/// residue is the `user_subvol_rm_allowed` evidence itself (reported through the returned report)
/// or foreign content that is not the probe's to delete. Every other failure is surfaced.
fn remove_probe_state_dir(state_dir: &Path) -> Result<(), EngineError> {
    match fs::remove_dir(state_dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(()),
        Err(err) => Err(EngineError::Io {
            path: state_dir.display().to_string(),
            message: format!("cannot remove the state directory this probe created: {err}"),
        }),
    }
}

/// Scratch subvolume names for one probe run, under `<state_dir>` and outside `worlds/` and
/// `snapshots/`.
struct ProbeNames {
    probe: String,
    snapshot: String,
}

/// Which scratch names a failed round-trip left behind, and whether the failing step was a destroy
/// (in which case the primary error already names the residue).
#[derive(Default)]
struct ScratchCreated {
    probe: bool,
    snapshot: bool,
    destroy_failed: bool,
}

/// Run the `ADR-0010` §4 scratch round-trip and leave zero residue.
///
/// A cleanup failure is never silently discarded: whenever the failure is not already the returned
/// destroy error, the error names the residue paths. A successful round-trip leaves nothing behind
/// (every created name was destroyed again by `scratch_steps`).
fn scratch_round_trip(driver: &KernelBtrfsDriver, state_dir: &Path) -> Result<(), EngineError> {
    let pid = std::process::id();
    let names = ProbeNames {
        probe: format!(".doctor-probe-{pid}"),
        snapshot: format!(".doctor-probe-{pid}-snap"),
    };
    let mut created = ScratchCreated::default();
    let outcome = scratch_steps(driver, state_dir, &names, &mut created);
    let mut residue = Vec::new();
    if created.snapshot
        && driver
            .destroy_subvolume(state_dir, &names.snapshot)
            .is_err()
    {
        residue.push(state_dir.join(&names.snapshot));
    }
    if created.probe && driver.destroy_subvolume(state_dir, &names.probe).is_err() {
        residue.push(state_dir.join(&names.probe));
    }
    match (outcome, residue.is_empty()) {
        (Ok(()), true) => Ok(()),
        (Ok(()), false) => Err(residue_error(&residue, None)),
        // A destroy-path denial is the residue evidence `probe` reports as `Some(Err(errno))`, and
        // its own `path` already names the subvolume that survived.
        (Err(err), _) if created.destroy_failed => Err(err),
        (Err(err), true) => Err(err),
        (Err(err), false) => Err(residue_error(&residue, Some(&err))),
    }
}

/// Error naming the scratch residue a failed cleanup left under `<state_dir>`.
fn residue_error(residue: &[PathBuf], cause: Option<&EngineError>) -> EngineError {
    let first = residue
        .first()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let listed = residue
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let message = match cause {
        Some(cause) => format!(
            "scratch probe left residue after '{cause}'; destroy these paths before reusing the state directory: {listed}"
        ),
        None => format!(
            "scratch probe left residue; destroy these paths before reusing the state directory: {listed}"
        ),
    };
    EngineError::Io {
        path: first,
        message,
    }
}

fn scratch_steps(
    driver: &KernelBtrfsDriver,
    state_dir: &Path,
    names: &ProbeNames,
    created: &mut ScratchCreated,
) -> Result<(), EngineError> {
    driver.create_subvolume(state_dir, &names.probe)?;
    created.probe = true;
    driver.snapshot_ro(&state_dir.join(&names.probe), state_dir, &names.snapshot)?;
    created.snapshot = true;
    if let Err(err) = driver.destroy_subvolume(state_dir, &names.snapshot) {
        created.destroy_failed = true;
        return Err(err);
    }
    created.snapshot = false;
    if let Err(err) = driver.destroy_subvolume(state_dir, &names.probe) {
        created.destroy_failed = true;
        return Err(err);
    }
    created.probe = false;
    Ok(())
}

/// Raw errno carried by a driver error, when it has one.
fn errno_of(err: &EngineError) -> Option<i32> {
    match err {
        EngineError::Ioctl { errno, .. } | EngineError::SubvolumeDestroyDenied { errno, .. } => {
            Some(*errno)
        }
        _ => None,
    }
}

/// The only Btrfs driver: `statfs` magic checks plus the `BTRFS_IOC_*` subvolume lifecycle
/// (`ADR-0010` §§3–5). It holds no state, so callers construct it directly and share it freely.
#[derive(Debug, Clone, Copy, Default)]
pub struct KernelBtrfsDriver;

impl KernelBtrfsDriver {
    /// The one production driver; it holds no state.
    pub const fn new() -> Self {
        Self
    }

    /// The single `BTRFS_IOC_SNAP_CREATE_V2` call site: `flags` is the kernel flag word
    /// (`BTRFS_SUBVOL_RDONLY` for a seal, `0` for a writable clone). The public trait keeps the two
    /// operations as distinct methods, so no caller ever sees this flag.
    fn snapshot(
        &self,
        source_subvol: &Path,
        dest_parent: &Path,
        name: &str,
        flags: u64,
    ) -> Result<PathBuf, EngineError> {
        let dest = dest_parent.join(name);
        // Validate the name before touching the filesystem: an empty, absolute, `.`, or `..` name
        // must never reach `open_dir`, let alone the kernel.
        let mut args = vol_args(name, 0, flags, dest_parent)?;
        let source_dir = open_dir(source_subvol)
            .map_err(|err| ioctl_error("open", source_subvol, errno(&err)))?;
        let dest_dir =
            open_dir(dest_parent).map_err(|err| ioctl_error("open", dest_parent, errno(&err)))?;
        let source_fd = source_dir.as_raw_fd();
        // `vol_args_v2.fd = 0` is the kernel's "no source subvolume" encoding for this ioctl
        // family, so a source descriptor that landed on 0 (stdin closed) is rejected instead of
        // being re-interpreted as an absent source.
        if source_fd <= 0 {
            return Err(ioctl_error("snap_create", &dest, libc::EBADF));
        }
        args.fd = i64::from(source_fd);
        snap_create_v2(&dest_dir, &mut args)
            .map_err(|errno| ioctl_error("snap_create", &dest, errno))?;
        Ok(dest)
    }
}

impl KernelBtrfsDriver {
    /// `statfs`/`statvfs` evidence for `path` (or its nearest existing ancestor).
    pub fn probe_facts(&self, path: &Path) -> Result<ProbeFacts, EngineError> {
        let probed_path = nearest_existing_ancestor(path)?;
        Ok(ProbeFacts {
            fstype: statfs_type(&probed_path)?,
            noatime: statvfs_noatime(&probed_path)?,
            probed_path,
        })
    }

    /// Pre-daemon `doctor` probe (`ADR-0010` §4) and the **single** producer of
    /// [`BtrfsProbeReport`]: facts plus the scratch-subvolume `user_subvol_rm_allowed` round-trip.
    pub fn probe(&self, path: &Path) -> Result<BtrfsProbeReport, EngineError> {
        require_absolute(path)?;
        let facts = self.probe_facts(path)?;
        let btrfs_super_magic = is_btrfs(facts.fstype);
        let mut created_state_dir = false;
        let user_subvol_rm_allowed = if btrfs_super_magic {
            if !path.exists() {
                fs::create_dir_all(path).map_err(|err| EngineError::Io {
                    path: path.display().to_string(),
                    message: err.to_string(),
                })?;
                created_state_dir = true;
            }
            let outcome = scratch_round_trip(self, path);
            if created_state_dir {
                remove_probe_state_dir(path)?;
            }
            match outcome {
                Ok(()) => Some(Ok(())),
                Err(err) => match errno_of(&err) {
                    Some(errno) => Some(Err(errno)),
                    // A failure that carries no raw errno is not `user_subvol_rm_allowed` evidence.
                    None => return Err(err),
                },
            }
        } else {
            None
        };
        Ok(BtrfsProbeReport {
            state_dir: path.to_path_buf(),
            probed_path: facts.probed_path,
            fstype: facts.fstype,
            btrfs_super_magic,
            user_subvol_rm_allowed,
            noatime: facts.noatime,
        })
    }

    /// Magic-only `slipforkd` boot gate (`ADR-0010` §4): a non-Btrfs `f_type` fails closed
    /// **before** any snapshot, world, or control directory exists.
    pub fn ensure_supported(&self, path: &Path) -> Result<(), EngineError> {
        require_btrfs(statfs_type(path)?, path)
    }

    pub fn create_subvolume(&self, parent: &Path, name: &str) -> Result<PathBuf, EngineError> {
        let dest = parent.join(name);
        // Validate before opening anything: a bad name never reaches `open_dir`.
        let mut args = vol_args(name, 0, 0, parent)?;
        let dir = open_dir(parent).map_err(|err| ioctl_error("open", parent, errno(&err)))?;
        subvol_create_v2(&dir, &mut args)
            .map_err(|errno| ioctl_error("subvol_create", &dest, errno))?;
        Ok(dest)
    }

    pub(crate) fn snapshot_ro(
        &self,
        source_subvol: &Path,
        dest_parent: &Path,
        name: &str,
    ) -> Result<PathBuf, EngineError> {
        self.snapshot(source_subvol, dest_parent, name, BTRFS_SUBVOL_RDONLY)
    }

    pub(crate) fn snapshot_writable(
        &self,
        source_subvol: &Path,
        dest_parent: &Path,
        name: &str,
    ) -> Result<PathBuf, EngineError> {
        self.snapshot(source_subvol, dest_parent, name, 0)
    }

    pub fn destroy_subvolume(&self, parent: &Path, name: &str) -> Result<(), EngineError> {
        let path = parent.join(name);
        // Validate before opening the parent or the subvolume: `..`/empty names never reach
        // `open_dir`, so destroy can never be pointed at a sibling or an ancestor.
        let mut args = vol_args(name, 0, 0, parent)?;
        let parent_dir =
            open_dir(parent).map_err(|err| destroy_error("open", parent, errno(&err)))?;
        let subvol = open_dir(&path).map_err(|err| destroy_error("open", &path, errno(&err)))?;
        let flags = subvol_getflags(&subvol)
            .map_err(|errno| destroy_error("subvol_getflags", &path, errno))?;
        if flags & BTRFS_SUBVOL_RDONLY != 0 {
            subvol_setflags(&subvol, flags & !BTRFS_SUBVOL_RDONLY)
                .map_err(|errno| destroy_error("subvol_setflags", &path, errno))?;
        }
        snap_destroy_v2(&parent_dir, &mut args)
            .map_err(|errno| destroy_error("snap_destroy", &path, errno))
    }

    /// Returns the direct child subvolume roots of `parent`, sorted by path.
    ///
    /// A subvolume root is identified by its inode number: every Btrfs subvolume root has
    /// `BTRFS_FIRST_FREE_OBJECTID` (`256`), the first objectid the filesystem hands to
    /// user-visible inodes, so no plain directory inside `parent` can share it. The parent must be
    /// on Btrfs (fail-closed [`require_btrfs`] gate), plain directories and files are skipped, and
    /// nested subvolumes are not descended into. This is a `readdir` plus `lstat` classification —
    /// no shell-out, no privileged tree search.
    ///
    /// Consumers: the `M3.14` engine suite will assert sealed-base subvolume names through it, and
    /// the Phase 2 `M3.2.13` GC will enumerate snapshot lineage through it.
    pub fn list_children(&self, parent: &Path) -> Result<Vec<PathBuf>, EngineError> {
        require_btrfs(statfs_type(parent)?, parent)?;
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
            let metadata = entry.metadata().map_err(|err| EngineError::Io {
                path: entry.path().display().to_string(),
                message: err.to_string(),
            })?;
            if metadata.is_dir() && metadata.ino() == BTRFS_FIRST_FREE_OBJECTID {
                children.push(entry.path());
            }
        }
        children.sort();
        Ok(children)
    }
}

/// `BTRFS_IOCTL_MAGIC` (`<linux/btrfs.h>`).
const BTRFS_IOCTL_MAGIC: libc::Ioctl = 0x94;
/// `BTRFS_SUBVOL_NAME_MAX` (`<linux/btrfs.h>`).
const BTRFS_SUBVOL_NAME_MAX: usize = 4039;
/// `BTRFS_SUBVOL_RDONLY` (`1ULL << 1`).
const BTRFS_SUBVOL_RDONLY: u64 = 1 << 1;
/// `BTRFS_FIRST_FREE_OBJECTID` (`<linux/btrfs_tree.h>`): the root inode of every subvolume.
const BTRFS_FIRST_FREE_OBJECTID: u64 = 256;

const IOC_WRITE: libc::Ioctl = 1;
const IOC_READ: libc::Ioctl = 2;

// `_IOC` numbers are asm-generic only: mips/powerpc/sparc shift the direction and size fields
// differently, so a wrong encoding would silently issue a different ioctl.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("M3.12 ioctl encoding is asm-generic only");

/// `_IOC(dir, type, nr, size)` as encoded by `<asm-generic/ioctl.h>` on Linux.
const fn ioc(dir: libc::Ioctl, nr: libc::Ioctl, size: usize) -> libc::Ioctl {
    (dir << 30) | ((size as libc::Ioctl) << 16) | (BTRFS_IOCTL_MAGIC << 8) | nr
}

const BTRFS_IOC_SNAP_CREATE_V2: libc::Ioctl = ioc(IOC_WRITE, 23, mem::size_of::<VolArgsV2>());
const BTRFS_IOC_SUBVOL_CREATE_V2: libc::Ioctl = ioc(IOC_WRITE, 24, mem::size_of::<VolArgsV2>());
const BTRFS_IOC_SUBVOL_GETFLAGS: libc::Ioctl = ioc(IOC_READ, 25, mem::size_of::<u64>());
const BTRFS_IOC_SUBVOL_SETFLAGS: libc::Ioctl = ioc(IOC_WRITE, 26, mem::size_of::<u64>());
const BTRFS_IOC_SNAP_DESTROY_V2: libc::Ioctl = ioc(IOC_WRITE, 63, mem::size_of::<VolArgsV2>());

/// `struct btrfs_ioctl_vol_args_v2` (`<linux/btrfs.h>`): 4096 bytes, name at offset 56.
#[repr(C)]
struct VolArgsV2 {
    fd: i64,
    transid: u64,
    flags: u64,
    unused: [u64; 4],
    name: [u8; BTRFS_SUBVOL_NAME_MAX + 1],
}

const _: () = assert!(mem::size_of::<VolArgsV2>() == 4096);

/// Build the kernel argument block for one `BTRFS_IOC_*_V2` call.
///
/// This is also the single pre-syscall validation point: every caller builds its arguments (and so
/// validates `name`) before opening any directory. `name` is a relative path the kernel resolves
/// against the directory descriptor it is given, so empty, `.`, `..`, and absolute names are
/// rejected before any filesystem access.
fn vol_args(name: &str, fd: i64, flags: u64, path: &Path) -> Result<VolArgsV2, EngineError> {
    let bytes = name.as_bytes();
    let invalid = bytes.is_empty()
        || bytes.len() > BTRFS_SUBVOL_NAME_MAX
        || bytes.contains(&0)
        || name.starts_with('/')
        || name
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..");
    if invalid {
        return Err(EngineError::Io {
            path: path.display().to_string(),
            message: format!(
                "subvolume name '{name}' must be a relative path of at most {BTRFS_SUBVOL_NAME_MAX} bytes without empty, '.' or '..' components"
            ),
        });
    }
    let mut args = VolArgsV2 {
        fd,
        transid: 0,
        flags,
        unused: [0; 4],
        name: [0; BTRFS_SUBVOL_NAME_MAX + 1],
    };
    args.name[..bytes.len()].copy_from_slice(bytes);
    Ok(args)
}

fn snap_create_v2(dir: &File, args: &mut VolArgsV2) -> Result<(), i32> {
    // SAFETY: `dir` owns a live directory descriptor for the whole call and `args` is a
    // `#[repr(C)]` 4096-byte `struct btrfs_ioctl_vol_args_v2` with a NUL-terminated name;
    // `BTRFS_IOC_SNAP_CREATE_V2` is `_IOW`, so the kernel only reads this userspace memory.
    let rc = unsafe { libc::ioctl(dir.as_raw_fd(), BTRFS_IOC_SNAP_CREATE_V2, args) };
    if rc == 0 { Ok(()) } else { Err(last_errno()) }
}

fn subvol_create_v2(dir: &File, args: &mut VolArgsV2) -> Result<(), i32> {
    // SAFETY: `dir` owns a live directory descriptor for the whole call and `args` is a
    // `#[repr(C)]` 4096-byte `struct btrfs_ioctl_vol_args_v2` with a NUL-terminated name;
    // `BTRFS_IOC_SUBVOL_CREATE_V2` is `_IOW`, so the kernel only reads this userspace memory.
    let rc = unsafe { libc::ioctl(dir.as_raw_fd(), BTRFS_IOC_SUBVOL_CREATE_V2, args) };
    if rc == 0 { Ok(()) } else { Err(last_errno()) }
}

fn snap_destroy_v2(dir: &File, args: &mut VolArgsV2) -> Result<(), i32> {
    // SAFETY: `dir` owns a live directory descriptor for the whole call and `args` is a
    // `#[repr(C)]` 4096-byte `struct btrfs_ioctl_vol_args_v2` with a NUL-terminated name;
    // `BTRFS_IOC_SNAP_DESTROY_V2` is `_IOW`, so the kernel only reads this userspace memory.
    let rc = unsafe { libc::ioctl(dir.as_raw_fd(), BTRFS_IOC_SNAP_DESTROY_V2, args) };
    if rc == 0 { Ok(()) } else { Err(last_errno()) }
}

fn subvol_getflags(dir: &File) -> Result<u64, i32> {
    let mut flags: u64 = 0;
    // SAFETY: `dir` owns a live subvolume-root descriptor and `flags` is a properly aligned `u64`
    // that `BTRFS_IOC_SUBVOL_GETFLAGS` (`_IOR`) writes exactly once; both outlive the call.
    let rc = unsafe { libc::ioctl(dir.as_raw_fd(), BTRFS_IOC_SUBVOL_GETFLAGS, &raw mut flags) };
    if rc == 0 {
        Ok(flags)
    } else {
        Err(last_errno())
    }
}

fn subvol_setflags(dir: &File, flags: u64) -> Result<(), i32> {
    // SAFETY: `dir` owns a live subvolume-root descriptor and `flags` is a properly aligned `u64`
    // that `BTRFS_IOC_SUBVOL_SETFLAGS` (`_IOW`) only reads; it outlives the call.
    let rc = unsafe { libc::ioctl(dir.as_raw_fd(), BTRFS_IOC_SUBVOL_SETFLAGS, &flags) };
    if rc == 0 { Ok(()) } else { Err(last_errno()) }
}

fn statfs_type(path: &Path) -> Result<i64, EngineError> {
    let c_path = c_path(path)?;
    // SAFETY: `libc::statfs` is a plain C struct of integers and integer arrays, for which an
    // all-zero bit pattern is valid.
    let mut buf: libc::statfs = unsafe { mem::zeroed() };
    // SAFETY: `c_path` is a valid NUL-terminated C string and `buf` is a correctly sized,
    // properly aligned `libc::statfs` that `statfs(2)` only writes; both outlive the call.
    let rc = unsafe { libc::statfs(c_path.as_ptr(), &raw mut buf) };
    if rc != 0 {
        return Err(ioctl_error("statfs", path, last_errno()));
    }
    // `f_type` is a signed machine word; zero-extending through `u32` keeps a filesystem magic
    // such as `0x9123683e` identical on 32- and 64-bit targets instead of sign-extending it.
    Ok(buf.f_type as u32 as i64)
}

fn statvfs_noatime(path: &Path) -> Result<bool, EngineError> {
    let c_path = c_path(path)?;
    // SAFETY: `libc::statvfs` is a plain C struct of integers and integer arrays, for which an
    // all-zero bit pattern is valid.
    let mut buf: libc::statvfs = unsafe { mem::zeroed() };
    // SAFETY: `c_path` is a valid NUL-terminated C string and `buf` is a correctly sized,
    // properly aligned `libc::statvfs` that `statvfs(3)` only writes; both outlive the call.
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &raw mut buf) };
    if rc != 0 {
        return Err(ioctl_error("statvfs", path, last_errno()));
    }
    Ok(buf.f_flag & libc::ST_NOATIME != 0)
}

/// Open `path` as a directory descriptor for the `BTRFS_IOC_*` calls.
///
/// `O_NOFOLLOW` keeps a symlink from being silently followed into a different subvolume, and
/// `O_CLOEXEC` keeps the descriptor out of every child process. This is the one directory-open
/// helper for the whole driver.
fn open_dir(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

fn c_path(path: &Path) -> Result<CString, EngineError> {
    CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| EngineError::Io {
        path: path.display().to_string(),
        message: "path contains an interior NUL byte".to_owned(),
    })
}

fn last_errno() -> i32 {
    io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}

fn errno(err: &io::Error) -> i32 {
    err.raw_os_error().unwrap_or(libc::EIO)
}

/// Map a failing `statfs`/`statvfs`/`BTRFS_IOC_*` call outside the destroy path (`ADR-0010` §4).
fn ioctl_error(op: &'static str, path: &Path, errno: i32) -> EngineError {
    EngineError::Ioctl {
        op,
        path: path.to_path_buf(),
        errno,
    }
}

/// Map a failing destroy-path call: `EPERM` means the mount lacks `user_subvol_rm_allowed`
/// (`ADR-0010` §4), every other errno is an ordinary ioctl failure.
fn destroy_error(op: &'static str, path: &Path, errno: i32) -> EngineError {
    if errno == libc::EPERM {
        EngineError::SubvolumeDestroyDenied {
            path: path.to_path_buf(),
            errno,
            mount_hint: mount_hint(path),
        }
    } else {
        ioctl_error(op, path, errno)
    }
}

/// `path` when it is an existing directory, else its nearest existing ancestor directory.
///
/// Only absolute paths are accepted: the driver never resolves anything against the ambient
/// `current_dir()`.
fn nearest_existing_ancestor(path: &Path) -> Result<PathBuf, EngineError> {
    require_absolute(path)?;
    for candidate in path.ancestors() {
        if fs::metadata(candidate).is_ok_and(|metadata| metadata.is_dir()) {
            return Ok(candidate.to_path_buf());
        }
    }
    Err(EngineError::Io {
        path: path.display().to_string(),
        message: "no existing ancestor directory to probe".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const XFS_SUPER_MAGIC: i64 = 0x5846_5342;
    const EXT4_SUPER_MAGIC: i64 = 0xef53;

    #[test]
    fn require_btrfs_accepts_only_the_btrfs_super_magic() {
        let path = Path::new("/var/lib/slipfork");
        assert!(require_btrfs(BTRFS_SUPER_MAGIC, path).is_ok());
        for fstype in [XFS_SUPER_MAGIC, EXT4_SUPER_MAGIC, 0x0102_1994, 0x794c_7630] {
            match require_btrfs(fstype, path) {
                Err(EngineError::BtrfsRequired {
                    path: got,
                    actual_fstype,
                    mount_hint: hint,
                }) => {
                    assert_eq!(got, path);
                    assert_eq!(actual_fstype, fstype);
                    assert_eq!(hint, mount_hint(path));
                }
                other => {
                    panic!("fstype {fstype:#x} must fail closed with BtrfsRequired: {other:?}")
                }
            }
        }
    }

    #[test]
    fn non_absolute_probe_paths_are_rejected_without_touching_the_cwd() {
        for path in [Path::new("state"), Path::new("./state"), Path::new("")] {
            match require_absolute(path) {
                Err(EngineError::Io { path: got, message }) => {
                    assert_eq!(got, path.display().to_string());
                    assert!(message.contains("absolute"), "{message}");
                }
                other => panic!("{path:?} must be rejected: {other:?}"),
            }
        }
        assert!(require_absolute(Path::new("/state")).is_ok());
    }

    #[test]
    fn ioctl_requests_match_the_linux_header_encoding() {
        assert_eq!(BTRFS_IOC_SNAP_CREATE_V2, 0x5000_9417);
        assert_eq!(BTRFS_IOC_SUBVOL_CREATE_V2, 0x5000_9418);
        assert_eq!(BTRFS_IOC_SUBVOL_GETFLAGS, 0x8008_9419);
        assert_eq!(BTRFS_IOC_SUBVOL_SETFLAGS, 0x4008_941a);
        assert_eq!(BTRFS_IOC_SNAP_DESTROY_V2, 0x5000_943f);
        assert_eq!(mem::size_of::<VolArgsV2>(), 4096);
    }

    #[test]
    fn btrfs_magic_and_mount_hint_are_the_documented_contract() {
        assert_eq!(BTRFS_SUPER_MAGIC, 0x9123_683e);
        assert_eq!(
            mount_hint(Path::new("/var/lib/slipfork")),
            "sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> /var/lib/slipfork"
        );
    }

    #[test]
    fn kernel_probe_reports_the_host_contract_without_leaving_residue() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let report = KernelBtrfsDriver::new()
            .probe(&state_dir)
            .expect("probe never fails on a readable ancestor");

        assert_eq!(report.state_dir, state_dir);
        assert_eq!(
            report.probed_path,
            tmp.path(),
            "the nearest existing ancestor is probed"
        );
        assert_eq!(report.btrfs_super_magic, report.fstype == BTRFS_SUPER_MAGIC);
        if report.btrfs_super_magic {
            assert!(report.user_subvol_rm_allowed.is_some());
            if report.user_subvol_rm_allowed == Some(Ok(())) {
                assert!(
                    !state_dir.exists(),
                    "a successful probe leaves zero residue"
                );
            }
        } else {
            assert_eq!(report.user_subvol_rm_allowed, None);
            assert!(!state_dir.exists(), "off Btrfs probe creates nothing");
        }
    }

    #[test]
    fn ensure_supported_agrees_with_probe_on_this_host() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = KernelBtrfsDriver::new();
        let report = driver.probe(tmp.path()).expect("probe");
        match driver.ensure_supported(tmp.path()) {
            Ok(()) => assert!(
                report.btrfs_super_magic,
                "ensure_supported accepts only Btrfs"
            ),
            Err(EngineError::BtrfsRequired {
                actual_fstype,
                mount_hint: hint,
                ..
            }) => {
                assert!(!report.btrfs_super_magic);
                assert_eq!(actual_fstype, report.fstype);
                assert_eq!(hint, mount_hint(&report.probed_path));
            }
            Err(other) => panic!("ensure_supported must fail closed with BtrfsRequired: {other}"),
        }
    }

    #[test]
    fn ioctl_errnos_map_to_typed_ioctl_errors() {
        let path = Path::new("/state/worlds/acme");
        for errno in [libc::ENAMETOOLONG, libc::EROFS, libc::ENOTEMPTY] {
            match ioctl_error("subvol_create", path, errno) {
                EngineError::Ioctl {
                    op,
                    path: got,
                    errno: got_errno,
                } => {
                    assert_eq!(op, "subvol_create");
                    assert_eq!(got, path);
                    assert_eq!(got_errno, errno);
                }
                other => panic!("errno {errno} must map to Ioctl, got {other}"),
            }
        }
    }

    #[test]
    fn destroy_eperm_maps_to_subvolume_destroy_denied() {
        let path = Path::new("/state/worlds/acme");
        match destroy_error("snap_destroy", path, libc::EPERM) {
            EngineError::SubvolumeDestroyDenied {
                path: got,
                errno,
                mount_hint: hint,
            } => {
                assert_eq!(got, path);
                assert_eq!(errno, libc::EPERM);
                assert_eq!(hint, mount_hint(path));
            }
            other => panic!("EPERM must map to SubvolumeDestroyDenied, got {other}"),
        }
        match destroy_error("snap_destroy", path, libc::EBUSY) {
            EngineError::Ioctl { op, errno, .. } => {
                assert_eq!(op, "snap_destroy");
                assert_eq!(errno, libc::EBUSY);
            }
            other => panic!("EBUSY must map to Ioctl, got {other}"),
        }
    }

    #[test]
    fn error_display_carries_the_mount_remediation() {
        let required = EngineError::BtrfsRequired {
            path: PathBuf::from("/var/lib/slipfork"),
            actual_fstype: XFS_SUPER_MAGIC,
            mount_hint: mount_hint(Path::new("/var/lib/slipfork")),
        };
        let text = required.to_string();
        assert!(text.contains("/var/lib/slipfork"), "{text}");
        assert!(text.contains("0x58465342"), "{text}");
        assert!(text.contains("user_subvol_rm_allowed,noatime"), "{text}");

        let denied = EngineError::SubvolumeDestroyDenied {
            path: PathBuf::from("/var/lib/slipfork/snapshots/base"),
            errno: libc::EPERM,
            mount_hint: mount_hint(Path::new("/var/lib/slipfork")),
        };
        let text = denied.to_string();
        assert!(text.contains("errno 1"), "{text}");
        assert!(text.contains("user_subvol_rm_allowed,noatime"), "{text}");
    }

    #[test]
    fn invalid_subvolume_names_are_rejected_before_any_filesystem_access() {
        let driver = KernelBtrfsDriver::new();
        // No such parent: an `open_dir` would fail with ENOENT, so an `Io` name error proves the
        // name was rejected before the driver touched the filesystem.
        let missing = Path::new("/nonexistent-slipfork-parent");
        for name in ["", ".", "..", "/abs", "with\0nul"] {
            for result in [
                driver.create_subvolume(missing, name).map(|_| ()),
                driver.snapshot_ro(missing, missing, name).map(|_| ()),
                driver.snapshot_writable(missing, missing, name).map(|_| ()),
                driver.destroy_subvolume(missing, name),
            ] {
                match result {
                    Err(EngineError::Io { message, .. }) => {
                        assert!(message.contains("subvolume name"), "{message}");
                    }
                    other => panic!("name {name:?} must fail closed before opening: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn subvolume_names_must_be_relative_components() {
        let parent = Path::new("/state/worlds");
        for name in ["", ".", "..", "a//b", "a/../b", "/abs", "with\0nul"] {
            assert!(
                vol_args(name, 0, 0, parent).is_err(),
                "name {name:?} must fail closed before any syscall"
            );
        }
        let too_long = "x".repeat(BTRFS_SUBVOL_NAME_MAX + 1);
        assert!(vol_args(&too_long, 0, 0, parent).is_err());
        let ok = vol_args("seed-0123456789ab", 7, BTRFS_SUBVOL_RDONLY, parent).expect("valid name");
        assert_eq!(ok.fd, 7);
        assert_eq!(ok.flags, BTRFS_SUBVOL_RDONLY);
        assert_eq!(&ok.name[..17], b"seed-0123456789ab");
        assert_eq!(ok.name[17], 0, "the name is NUL-terminated");
    }

    #[test]
    fn nearest_existing_ancestor_walks_up_to_the_first_existing_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("a").join("b").join("c");
        assert_eq!(
            nearest_existing_ancestor(&missing).expect("ancestor"),
            tmp.path()
        );
        assert_eq!(
            nearest_existing_ancestor(tmp.path()).expect("existing directory"),
            tmp.path()
        );
        assert!(
            nearest_existing_ancestor(Path::new("relative/state")).is_err(),
            "a relative path is never resolved against the cwd"
        );
    }
}
