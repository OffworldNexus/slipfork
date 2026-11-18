//! Thin, audited Linux syscall module (`R10`, `H-011` #3).
//!
//! `unsafe` is confined to this module and [`crate::btrfs`]'s kernel `ioctl` driver; every block
//! carries a `// SAFETY:` proof. This module owns:
//! - `geteuid`/`getegid` ([`current_uid_gid`]);
//! - `kill(2)` for bounded service shutdown ([`signal_process`]);
//! - the peer pid of a connected `AF_UNIX` socket ([`peer_pid`]) and the `pidfd` pair
//!   ([`open_pidfd`]/[`signal_pidfd`]) that lets `slipfork daemon install` prove who holds
//!   `daemon.sock` and signal **that** process, never a recycled pid;
//! - the single `sendmsg`/`recvmsg` `SCM_RIGHTS` descriptor-transfer pair
//!   ([`send_fd`]/[`recv_fd`]) used by `netns.sock` to hand an in-world `TcpStream` to
//!   `slipforkd` (`H-011` #3).

use std::io;
use std::mem;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

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

/// Peer pid of a connected `AF_UNIX` socket (`getsockopt(SO_PEERCRED)`).
///
/// The kernel reports the credentials of the process at the other end of `stream`: for a client
/// connected to a daemon's listening socket that is the daemon itself. This is the only ownership
/// proof `slipfork daemon install` accepts before it signals a socket holder.
///
/// The pid is a **snapshot**: the peer may exit (and the number be recycled) after this call, so a
/// caller that will signal the process must pin it with [`open_pidfd`] first and signal through
/// [`signal_pidfd`].
///
/// # Errors
///
/// Returns the raw `getsockopt(2)` error (`ENOTCONN` for an unconnected socket, `ENOTSOCK`) and
/// [`io::ErrorKind::InvalidData`] when the kernel reports an unusable pid (`0` addresses a whole
/// process group under `kill(2)`, and a negative value is not a pid at all).
pub fn peer_pid(stream: &UnixStream) -> io::Result<u32> {
    let mut credentials: libc::ucred = unsafe { mem::zeroed() };
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `credentials` and `length` are live, correctly sized stack values; `getsockopt`
    // writes at most `length` bytes into `credentials` and updates `length` with what it wrote,
    // and `stream` stays open for the whole call.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &mut length,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    checked_peer_pid(credentials.pid)
}

/// Refuse a `SO_PEERCRED` pid this process could not address safely.
///
/// `kill(0, …)` signals the caller's own process group, which is never what a socket holder means,
/// and a peer outside this PID namespace reports `0`.
fn checked_peer_pid(raw: libc::pid_t) -> io::Result<u32> {
    u32::try_from(raw)
        .ok()
        .filter(|pid| *pid != 0)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("SO_PEERCRED reported the unusable pid {raw}"),
            )
        })
}

/// Pin `pid` in a `pidfd` (`pidfd_open(2)`).
///
/// The descriptor names **that** process for as long as it is open: a signal sent through it can
/// never reach a later process that reuses the number. `slipfork daemon install` opens it at the
/// moment ownership is proven, and signals only through it.
///
/// # Errors
///
/// Returns the raw `pidfd_open(2)` error (`ESRCH` when the pid is already gone).
pub fn open_pidfd(pid: u32) -> io::Result<OwnedFd> {
    // SAFETY: `pidfd_open` is a thread-safe syscall wrapper taking a pid and a flags word; it
    // returns a fresh descriptor that this function owns exclusively, and mutates no userspace
    // memory.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, i64::from(pid), 0_i64) } as libc::c_int;
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh, valid descriptor returned by `pidfd_open` and owned by nobody else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Send `signal` through `pidfd` (`pidfd_send_signal(2)`), immune to pid reuse.
///
/// # Errors
///
/// Returns the raw `pidfd_send_signal(2)` error (`ESRCH` when the pinned process is already gone,
/// `EPERM` when the caller may not signal it).
pub fn signal_pidfd(pidfd: BorrowedFd<'_>, signal: ProcessSignal) -> io::Result<()> {
    let signo = match signal {
        ProcessSignal::Terminate => libc::SIGTERM,
        ProcessSignal::Kill => libc::SIGKILL,
    };
    // SAFETY: `pidfd_send_signal` takes a borrowed pidfd and a signal number; a null `siginfo` and
    // zero flags are the documented "plain signal" form, and it mutates no userspace memory.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            i64::from(pidfd.as_raw_fd()),
            i64::from(signo),
            0_i64,
            0_i64,
        )
    } as libc::c_int;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::fd::AsFd;

    /// Send `fds` in one `sendmsg` payload — deliberately more than the protocol allows.
    fn send_descriptors(stream: &UnixStream, fds: &[RawFd]) {
        let mut payload = [STATUS_OK];
        let mut iov = libc::iovec {
            iov_base: payload.as_mut_ptr().cast(),
            iov_len: payload.len(),
        };
        let mut control = vec![0_u8; 64];
        // SAFETY: `msghdr` is a plain C struct whose all-zero bit pattern is valid (a null
        // `msg_name` with zero `msg_namelen`); every field `sendmsg` reads is assigned below.
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

    /// Number of open descriptors that alias the same kernel socket as `fd`.
    ///
    /// Counting by socket identity (the `/proc/self/fd` link target) rather than by the
    /// process-wide descriptor count keeps this assertion deterministic while sibling lib tests
    /// open and close their own descriptors in parallel.
    fn descriptors_aliasing(fd: RawFd) -> usize {
        let target = fs::read_link(format!("/proc/self/fd/{fd}")).expect("read descriptor link");
        fs::read_dir("/proc/self/fd")
            .expect("read /proc/self/fd")
            .filter_map(Result::ok)
            .filter(|entry| fs::read_link(entry.path()).is_ok_and(|link| link == target))
            .count()
    }

    #[test]
    fn recv_fd_rejects_extra_descriptors_without_leaking_them() {
        let (writer, reader) = UnixStream::pair().expect("socketpair");
        let (fd_a, _keep_a) = UnixStream::pair().expect("fd a");
        let (fd_b, _keep_b) = UnixStream::pair().expect("fd b");
        send_descriptors(&writer, &[fd_a.as_raw_fd(), fd_b.as_raw_fd()]);

        let before_a = descriptors_aliasing(fd_a.as_raw_fd());
        let before_b = descriptors_aliasing(fd_b.as_raw_fd());
        let err = recv_fd(&reader).expect_err("two descriptors violate the one-fd contract");
        let after_a = descriptors_aliasing(fd_a.as_raw_fd());
        let after_b = descriptors_aliasing(fd_b.as_raw_fd());
        assert_eq!(
            err.kind(),
            io::ErrorKind::InvalidData,
            "extra descriptors must be rejected as a protocol violation: {err}"
        );
        assert_eq!(
            (after_a, after_b),
            (before_a, before_b),
            "rejected descriptors must be closed, not leaked (fd a {before_a} -> {after_a}, \
             fd b {before_b} -> {after_b})"
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

    #[test]
    fn peer_pid_reports_the_pid_at_the_other_end() {
        let (client, server) = UnixStream::pair().expect("socketpair");
        let expected = std::process::id();
        assert_eq!(peer_pid(&client).expect("client peer pid"), expected);
        assert_eq!(peer_pid(&server).expect("server peer pid"), expected);
    }

    #[test]
    fn a_peer_pid_of_zero_or_less_is_refused() {
        for raw in [0, -1, libc::pid_t::MIN] {
            let err = checked_peer_pid(raw).expect_err("an unusable pid must be refused");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{raw}: {err}");
        }
    }

    #[test]
    fn a_pidfd_signals_exactly_the_pinned_process() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn a real child to pin");
        let pidfd = open_pidfd(child.id()).expect("open the child's pidfd");

        signal_pidfd(pidfd.as_fd(), ProcessSignal::Terminate).expect("SIGTERM the pinned child");
        let status = child.wait().expect("reap the child");
        assert!(
            !status.success(),
            "the pinned child was terminated, never this test: {status}"
        );

        // The pidfd keeps naming the process it was opened for, so it reports `ESRCH` instead of
        // ever retargeting a reused pid.
        let err = signal_pidfd(pidfd.as_fd(), ProcessSignal::Terminate)
            .expect_err("the pinned process is gone");
        assert_eq!(err.raw_os_error(), Some(libc::ESRCH), "{err}");
    }
}
