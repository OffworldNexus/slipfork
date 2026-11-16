//! Thin, audited Linux syscall module (`R10`, `H-011` #3).
//!
//! This is the **only** module in the workspace permitted to contain `unsafe` code. It owns:
//! - `ioctl(dst_fd, FICLONE, src_fd)` extent cloning ([`reflink_file`]);
//! - `geteuid`/`getegid` ([`current_uid_gid`]);
//! - the single `sendmsg`/`recvmsg` `SCM_RIGHTS` descriptor-transfer pair
//!   ([`send_fd`]/[`recv_fd`]) used by `netns.sock` to hand an in-world `TcpStream` to
//!   `slipforkd` (`H-011` #3);
//! - `kill(2)` for bounded service shutdown ([`signal_process`]).
//!
//! Every `unsafe` block carries a `// SAFETY:` proof. Zero silent fallback: if the backing
//! filesystem rejects `FICLONE` (such as `tmpfs` `/tmp` returning `EOPNOTSUPP`), [`reflink_tree`]
//! fails closed with [`EngineError::ReflinkUnsupported`].

use std::fs::{self, File, OpenOptions};
use std::io;
use std::mem;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::os::unix::net::UnixStream;
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

/// Signal a world-owned process by host pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSignal {
    /// `SIGTERM`: graceful shutdown request.
    Terminate,
    /// `SIGKILL`: forced teardown (also destroys a world's whole PID namespace when sent to its
    /// PID 1).
    Kill,
}

/// Send `signal` to `pid` (`kill(2)`).
///
/// # Errors
///
/// Returns the raw `kill(2)` error (`ESRCH` for an already-reaped pid, `EPERM` for a foreign
/// process).
pub fn signal_process(pid: u32, signal: ProcessSignal) -> io::Result<()> {
    let signo = match signal {
        ProcessSignal::Terminate => libc::SIGTERM,
        ProcessSignal::Kill => libc::SIGKILL,
    };
    // SAFETY: `kill` is a thread-safe syscall wrapper taking a pid and a signal number; a positive
    // pid never addresses a process group and the call mutates no userspace memory.
    let rc = unsafe { libc::kill(pid as libc::pid_t, signo) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// One-byte status companion of every descriptor transfer (`0` = success, otherwise errno).
pub const STATUS_OK: u8 = 0;

/// Control-message buffer large enough for exactly one `SCM_RIGHTS` descriptor.
fn cmsg_space() -> usize {
    // SAFETY: `CMSG_SPACE` is pure compile-time-style arithmetic over a byte length; it reads no
    // memory and the length is a constant `size_of::<RawFd>()`.
    unsafe { libc::CMSG_SPACE(size_of::<RawFd>() as libc::c_uint) as usize }
}

/// Send one status byte plus, when `fd` is `Some`, one `SCM_RIGHTS` descriptor.
///
/// `world-init` uses this to hand the connected in-world `TcpStream` to `slipforkd` (`H-011` #3).
/// The receiving process obtains an independent descriptor; the sender may close its own.
///
/// # Errors
///
/// Propagates `sendmsg(2)` failures (`EPIPE` once the peer hung up).
pub fn send_fd(stream: &UnixStream, status: u8, fd: Option<BorrowedFd<'_>>) -> io::Result<()> {
    let mut payload = [status];
    let mut iov = libc::iovec {
        iov_base: payload.as_mut_ptr().cast(),
        iov_len: payload.len(),
    };
    let mut control = vec![0_u8; cmsg_space()];
    // SAFETY: an all-zero `msghdr` is a valid empty value; every field this function relies on is
    // overwritten below before `sendmsg` reads it.
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    if let Some(fd) = fd {
        let capacity = control.len() as libc::size_t;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = capacity;
        // SAFETY: `message` references `control`, which is `CMSG_SPACE(size_of::<RawFd>())` bytes,
        // so the single control header and its data slot fit; `CMSG_DATA` then points at the
        // `RawFd` slot this block writes exactly once before `sendmsg` reads it.
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&message);
            if header.is_null() {
                return Err(io::Error::other("cannot build SCM_RIGHTS control message"));
            }
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(size_of::<RawFd>() as libc::c_uint) as usize;
            std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), fd.as_raw_fd());
            message.msg_controllen = (*header).cmsg_len;
        }
    }
    // SAFETY: `message` points at the initialized `iov`/`control` buffers above, `msg_iovlen` is 1,
    // and `sendmsg` only reads them for the duration of the call.
    let sent = unsafe { libc::sendmsg(stream.as_raw_fd(), &message, 0) };
    if sent < 0 {
        return Err(io::Error::last_os_error());
    }
    if sent != payload.len() as isize {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "short SCM_RIGHTS payload write",
        ));
    }
    Ok(())
}

/// Receive one status byte plus, when the peer attached one, its `SCM_RIGHTS` descriptor.
///
/// Received descriptors are `CLOEXEC` (`MSG_CMSG_CLOEXEC`) and owned by the returned [`OwnedFd`].
///
/// # Errors
///
/// Propagates `recvmsg(2)` failures and reports a cleanly closed peer as
/// [`io::ErrorKind::UnexpectedEof`]. A peer that attaches **more than one** descriptor is rejected
/// with [`io::ErrorKind::InvalidData`] and every delivered descriptor is closed before returning, so
/// a malicious or buggy peer can never leak descriptors into `slipforkd`.
pub fn recv_fd(stream: &UnixStream) -> io::Result<(u8, Option<OwnedFd>)> {
    let mut payload = [0_u8];
    let mut iov = libc::iovec {
        iov_base: payload.as_mut_ptr().cast(),
        iov_len: payload.len(),
    };
    let mut control = vec![0_u8; cmsg_space()];
    // SAFETY: an all-zero `msghdr` is a valid empty value; every field `recvmsg` reads is set below
    // before the call.
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len() as libc::size_t;
    // SAFETY: `message` references the live `iov`/`control` buffers above; `recvmsg` writes at most
    // `msg_controllen` bytes into `control` and at most one byte into `payload`.
    let received =
        unsafe { libc::recvmsg(stream.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) };
    if received < 0 {
        return Err(io::Error::last_os_error());
    }
    if received == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "peer closed before sending a descriptor",
        ));
    }
    let mut received_fds: Vec<OwnedFd> = Vec::new();
    // SAFETY: `message` was populated by `recvmsg`; `CMSG_FIRSTHDR`/`CMSG_NXTHDR` walk only within
    // the `control` buffer bounded by `msg_controllen`, the per-header descriptor count is derived
    // from `cmsg_len`, and each `SCM_RIGHTS` data slot is read as one `RawFd`. `OwnedFd::from_raw_fd`
    // takes ownership of every freshly received descriptor exactly once.
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                let data_len = (*header)
                    .cmsg_len
                    .saturating_sub(libc::CMSG_LEN(0) as usize);
                let count = data_len / size_of::<RawFd>();
                let data = libc::CMSG_DATA(header).cast::<RawFd>();
                for index in 0..count {
                    let raw = std::ptr::read_unaligned(data.add(index));
                    received_fds.push(OwnedFd::from_raw_fd(raw));
                }
            }
            header = libc::CMSG_NXTHDR(&message, header);
        }
    }
    match received_fds.len() {
        0 => Ok((payload[0], None)),
        1 => Ok((payload[0], received_fds.pop())),
        // Dropping `received_fds` closes every delivered descriptor before we report the protocol
        // violation, so a peer cannot grow `slipforkd`'s descriptor table.
        extra => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("SCM_RIGHTS peer delivered {extra} descriptors; exactly one was expected"),
        )),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    /// Send `fds` in one `sendmsg` payload — deliberately more than the protocol allows.
    fn send_descriptors(stream: &UnixStream, fds: &[RawFd]) {
        let mut payload = [STATUS_OK];
        let mut iov = libc::iovec {
            iov_base: payload.as_mut_ptr().cast(),
            iov_len: payload.len(),
        };
        let mut control = vec![0_u8; 64];
        let mut message: libc::msghdr = unsafe { mem::zeroed() };
        message.msg_iov = &raw mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = control.len() as libc::size_t;
        // SAFETY: `control` is 64 bytes, enough for one control header carrying two `RawFd`s; the
        // header is initialized before `sendmsg` reads it and nothing outlives the call.
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(size_of_val(fds) as libc::c_uint) as usize;
            std::ptr::copy_nonoverlapping(
                fds.as_ptr(),
                libc::CMSG_DATA(header).cast::<RawFd>(),
                fds.len(),
            );
            message.msg_controllen = (*header).cmsg_len;
        }
        // SAFETY: `message` references the live `iov`/`control` buffers above for the call only.
        let sent = unsafe { libc::sendmsg(stream.as_raw_fd(), &message, 0) };
        assert_eq!(sent, 1, "one status byte must be sent");
    }

    fn open_descriptors() -> usize {
        fs::read_dir("/proc/self/fd")
            .expect("read /proc/self/fd")
            .count()
    }

    #[test]
    fn recv_fd_rejects_extra_descriptors_without_leaking_them() {
        let (writer, reader) = UnixStream::pair().expect("socketpair");
        let (fd_a, _keep_a) = UnixStream::pair().expect("fd a");
        let (fd_b, _keep_b) = UnixStream::pair().expect("fd b");
        send_descriptors(&writer, &[fd_a.as_raw_fd(), fd_b.as_raw_fd()]);

        let before = open_descriptors();
        let err = recv_fd(&reader).expect_err("two descriptors violate the one-fd contract");
        let after = open_descriptors();
        assert_eq!(
            err.kind(),
            io::ErrorKind::InvalidData,
            "extra descriptors must be rejected as a protocol violation: {err}"
        );
        assert!(
            after <= before,
            "rejected descriptors must be closed, not leaked ({before} -> {after})"
        );

        drop(writer);
        drop(fd_a);
        drop(fd_b);
    }

    #[test]
    fn recv_fd_accepts_exactly_one_descriptor() {
        let (writer, reader) = UnixStream::pair().expect("socketpair");
        let (payload_stream, _keep) = UnixStream::pair().expect("payload stream");
        send_fd(&writer, STATUS_OK, Some(payload_stream.as_fd())).expect("send one fd");
        let (status, descriptor) = recv_fd(&reader).expect("receive one fd");
        assert_eq!(status, STATUS_OK);
        assert!(
            descriptor.is_some(),
            "the contract's single descriptor must arrive"
        );
    }
}
