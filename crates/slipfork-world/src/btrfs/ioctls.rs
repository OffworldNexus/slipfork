use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::btrfs::flags::SubvolumeFlags;
use crate::error::{Error, Result};

const BTRFS_IOCTL_MAGIC: u32 = 0x94;
/// `BTRFS_SUBVOL_NAME_MAX` from `linux/btrfs.h`: one path component, no `/`.
const BTRFS_SUBVOL_NAME_MAX: usize = 4039;
const BTRFS_FSID_SIZE: usize = 16;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

/// Encodes a `_IOC(dir, type, nr, size)` request exactly like `linux/ioctl.h`.
///
/// Kept as `u32`: `ioctl(2)` takes `c_ulong` on glibc and `c_int` on musl, and each call site adapts
/// with `as _` because the kernel only reads the low 32 bits of the request word.
const fn request(dir: u32, nr: u32, size: u32) -> u32 {
    (dir << 30) | (size << 16) | (BTRFS_IOCTL_MAGIC << 8) | nr
}

const SUBVOL_GETFLAGS: u32 = request(IOC_READ, 25, 8);
const SUBVOL_SETFLAGS: u32 = request(IOC_WRITE, 26, 8);
const SUBVOL_CREATE: u32 = request(IOC_WRITE, 14, 4096);
const SNAP_CREATE_V2: u32 = request(IOC_WRITE, 23, 4096);
const FS_INFO: u32 = request(IOC_READ, 31, 1024);
/// `BTRFS_IOC_SNAP_DESTROY_V2` is command **63** in `linux/btrfs.h`, not 47.
const SNAP_DESTROY_V2: u32 = request(IOC_WRITE, 63, 1024 * 4);

/// `struct btrfs_ioctl_vol_args` from `fs/btrfs/ioctl.h` (4096 bytes).
#[allow(dead_code)] // Fields mirror the kernel ABI and are read by the kernel, not by us.
#[repr(C)]
struct VolArgs {
    fd: i64,
    name: [u8; 4088],
}

/// `struct btrfs_ioctl_vol_args_v2` from `fs/btrfs/ioctl.h` (4096 bytes).
#[allow(dead_code)] // Fields mirror the kernel ABI and are read by the kernel, not by us.
#[repr(C)]
struct VolArgsV2 {
    fd: i64,
    transid: u64,
    flags: u64,
    unused: [u64; 4],
    name: [u8; 4040],
}

/// `struct btrfs_ioctl_fs_info_args` from `<linux/btrfs.h>` (1024 bytes).
#[allow(dead_code)] // Fields mirror the kernel ABI; only `fsid` is inspected by userspace.
#[repr(C)]
struct FsInfoArgs {
    max_id: u64,
    num_devices: u64,
    fsid: [u8; BTRFS_FSID_SIZE],
    nodesize: u32,
    sectorsize: u32,
    clone_alignment: u32,
    csum_type: u16,
    csum_size: u16,
    flags: u64,
    generation: u64,
    metadata_uuid: [u8; BTRFS_FSID_SIZE],
    reserved: [u8; 944],
}

pub(crate) fn fsid(dir: &File, path: &Path) -> Result<[u8; BTRFS_FSID_SIZE]> {
    let mut args = FsInfoArgs {
        max_id: 0,
        num_devices: 0,
        fsid: [0; BTRFS_FSID_SIZE],
        nodesize: 0,
        sectorsize: 0,
        clone_alignment: 0,
        csum_type: 0,
        csum_size: 0,
        flags: 0,
        generation: 0,
        metadata_uuid: [0; BTRFS_FSID_SIZE],
        reserved: [0; 944],
    };
    // SAFETY: `dir` is a live Btrfs directory fd and `args` matches `btrfs_ioctl_fs_info_args`.
    check("read btrfs filesystem uuid", path, unsafe {
        libc::ioctl(dir.as_raw_fd(), FS_INFO as _, &mut args)
    })?;
    Ok(args.fsid)
}

pub(crate) fn get_flags(dir: &File, path: &Path) -> Result<SubvolumeFlags> {
    let mut flags: u64 = 0;
    // SAFETY: `dir` holds a live descriptor and the kernel writes exactly one `u64` through it.
    check("read subvolume flags", path, unsafe {
        libc::ioctl(dir.as_raw_fd(), SUBVOL_GETFLAGS as _, &mut flags)
    })?;
    Ok(SubvolumeFlags::from_raw(flags))
}

pub(crate) fn set_flags(dir: &File, path: &Path, flags: SubvolumeFlags) -> Result<()> {
    let mut raw = flags.raw();
    // SAFETY: `dir` holds a live descriptor and the kernel only reads the `u64` behind it.
    check("set subvolume flags", path, unsafe {
        libc::ioctl(dir.as_raw_fd(), SUBVOL_SETFLAGS as _, &mut raw)
    })
}

pub(crate) fn create_subvolume(parent: &File, path: &Path, name: &OsStr) -> Result<()> {
    let mut args = VolArgs {
        fd: 0,
        name: [0; 4088],
    };
    set_name(&mut args.name, name, path, "create subvolume")?;
    // SAFETY: `parent` is a live directory descriptor and `args` mirrors `btrfs_ioctl_vol_args`.
    check("create subvolume", path, unsafe {
        libc::ioctl(parent.as_raw_fd(), SUBVOL_CREATE as _, &mut args)
    })
}

pub(crate) fn snapshot(parent: &File, source: &File, path: &Path, name: &OsStr) -> Result<()> {
    let mut args = VolArgsV2 {
        fd: i64::from(source.as_raw_fd()),
        transid: 0,
        flags: 0,
        unused: [0; 4],
        name: [0; 4040],
    };
    set_name(&mut args.name, name, path, "snapshot")?;
    // SAFETY: `parent`/`source` are live descriptors and `args` mirrors `btrfs_ioctl_vol_args_v2`.
    check("snapshot", path, unsafe {
        libc::ioctl(parent.as_raw_fd(), SNAP_CREATE_V2 as _, &mut args)
    })
}

pub(crate) fn destroy(parent: &File, path: &Path, name: &OsStr) -> Result<()> {
    let mut args = VolArgsV2 {
        fd: 0,
        transid: 0,
        flags: 0,
        unused: [0; 4],
        name: [0; 4040],
    };
    set_name(&mut args.name, name, path, "delete subvolume")?;
    // SAFETY: `parent` is a live directory descriptor and `args` mirrors its kernel ABI struct.
    check("delete subvolume", path, unsafe {
        libc::ioctl(parent.as_raw_fd(), SNAP_DESTROY_V2 as _, &mut args)
    })
}

fn set_name(buf: &mut [u8], name: &OsStr, path: &Path, action: &'static str) -> Result<()> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > BTRFS_SUBVOL_NAME_MAX || bytes.contains(&b'/') {
        return Err(Error::io(
            action,
            path,
            io::Error::from_raw_os_error(libc::ENAMETOOLONG),
        ));
    }
    buf[..bytes.len()].copy_from_slice(bytes);
    Ok(())
}

/// Single syscall boundary: every `-1` return becomes one typed [`Error`].
fn check(action: &'static str, path: &Path, ret: libc::c_int) -> Result<()> {
    if ret == -1 {
        Err(Error::io(action, path, io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_requests_match_linux_btrfs_h() {
        // Command numbers straight from `<linux/btrfs.h>`; every one is a compile-time encoding.
        assert_eq!(request(IOC_WRITE, 14, 4096), 0x5000_940e, "SUBVOL_CREATE");
        assert_eq!(request(IOC_WRITE, 23, 4096), 0x5000_9417, "SNAP_CREATE_V2");
        assert_eq!(
            request(IOC_WRITE, 24, 4096),
            0x5000_9418,
            "SUBVOL_CREATE_V2"
        );
        assert_eq!(request(IOC_READ, 25, 8), 0x8008_9419, "SUBVOL_GETFLAGS");
        assert_eq!(request(IOC_WRITE, 26, 8), 0x4008_941a, "SUBVOL_SETFLAGS");
        assert_eq!(request(IOC_READ, 31, 1024), 0x8400_941f, "FS_INFO");
        assert_eq!(request(IOC_WRITE, 63, 4096), 0x5000_943f, "SNAP_DESTROY_V2");
        assert_eq!(SUBVOL_CREATE, request(IOC_WRITE, 14, 4096));
        assert_eq!(SNAP_CREATE_V2, request(IOC_WRITE, 23, 4096));
        assert_eq!(SUBVOL_GETFLAGS, request(IOC_READ, 25, 8));
        assert_eq!(SUBVOL_SETFLAGS, request(IOC_WRITE, 26, 8));
        assert_eq!(FS_INFO, 0x8400_941f);
        // `47` was the command that answered `ENOTTY`; keep a hard pin on the real number.
        assert_eq!(SNAP_DESTROY_V2, 0x5000_943f);
    }

    #[test]
    fn ioctl_argument_structs_match_the_kernel_layout() {
        assert_eq!(std::mem::size_of::<VolArgs>(), 4096);
        assert_eq!(std::mem::size_of::<VolArgsV2>(), 4096);
        assert_eq!(std::mem::size_of::<FsInfoArgs>(), 1024);
        assert_eq!(std::mem::offset_of!(VolArgsV2, fd), 0);
        assert_eq!(std::mem::offset_of!(VolArgsV2, name), 56);
        assert_eq!(std::mem::offset_of!(VolArgs, name), 8);
        assert_eq!(std::mem::offset_of!(FsInfoArgs, fsid), 16);
        assert_eq!(std::mem::offset_of!(FsInfoArgs, reserved), 80);
    }

    #[test]
    fn names_must_be_one_bounded_component() {
        let mut buf = [0u8; 4040];
        assert!(
            set_name(
                &mut buf,
                OsStr::new("feat-auth"),
                Path::new("x"),
                "snapshot"
            )
            .is_ok()
        );
        assert!(set_name(&mut buf, OsStr::new("a/b"), Path::new("x"), "snapshot").is_err());
        assert!(set_name(&mut buf, OsStr::new(""), Path::new("x"), "snapshot").is_err());
        let too_long = "x".repeat(4040);
        assert!(set_name(&mut buf, OsStr::new(&too_long), Path::new("x"), "snapshot").is_err());
    }
}
