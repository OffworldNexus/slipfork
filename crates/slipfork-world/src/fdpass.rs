//! The audited single-descriptor `SCM_RIGHTS` transport.
//!
//! Callers inject the `AF_UNIX` socket and the descriptor; this module only frames one payload plus,
//! optionally, exactly one descriptor. No descriptor, path, or environment is discovered here.

use std::io;
use std::mem;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

/// Largest control payload one `sendmsg`/`recvmsg` carries.
pub const PAYLOAD_CAP: usize = 4096;

/// Narrows a `usize` length into a platform control-message field.
///
/// glibc types `cmsg_len`/`msg_controllen` as `size_t` while musl types them as `socklen_t`; the
/// target field type is inferred at each call site and an over-wide length is refused rather than
/// truncated.
fn narrow<T>(len: usize) -> io::Result<T>
where
    T: TryFrom<usize>,
{
    T::try_from(len).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket control length exceeds the platform's field width",
        )
    })
}

/// Widens a platform control-message field into the `usize` lengths this module works with.
fn widen<T>(len: T) -> io::Result<usize>
where
    T: TryInto<usize>,
{
    len.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "socket control length does not fit in usize",
        )
    })
}

/// Control-message buffer large enough for exactly one `SCM_RIGHTS` descriptor.
fn cmsg_space() -> io::Result<usize> {
    let payload = narrow::<libc::c_uint>(size_of::<RawFd>())?;
    // SAFETY: `CMSG_SPACE` is pure arithmetic over a byte length; it reads no memory and the
    // length is the constant `size_of::<RawFd>()`.
    widen(unsafe { libc::CMSG_SPACE(payload) })
}

/// Send one payload plus, when `fd` is `Some`, exactly one `SCM_RIGHTS` descriptor.
///
/// The receiver obtains an independent descriptor; the sender may close its own immediately.
///
/// # Errors
///
/// Propagates `sendmsg(2)` failures (`EPIPE` once the peer hung up) and rejects an empty or
/// oversized payload before touching the socket.
pub fn send_fd(socket: &UnixStream, payload: &[u8], fd: Option<BorrowedFd<'_>>) -> io::Result<()> {
    if payload.is_empty() || payload.len() > PAYLOAD_CAP {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("control payload must be 1..={PAYLOAD_CAP} bytes"),
        ));
    }
    let mut iov = libc::iovec {
        iov_base: payload.as_ptr().cast_mut().cast(),
        iov_len: payload.len(),
    };
    let mut control = vec![0_u8; cmsg_space()?];
    // SAFETY: an all-zero `msghdr` is a valid empty value; every field this function relies on is
    // overwritten below before `sendmsg` reads it.
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    if let Some(fd) = fd {
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = narrow(control.len())?;
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
            let field = narrow::<libc::c_uint>(size_of::<RawFd>())?;
            let control_len = widen(libc::CMSG_LEN(field))?;
            (*header).cmsg_len = narrow(control_len)?;
            std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), fd.as_raw_fd());
            message.msg_controllen = narrow(control_len)?;
        }
    }
    // SAFETY: `message` points at the initialized `iov`/`control` buffers above, `msg_iovlen` is 1,
    // and `sendmsg` only reads them for the duration of the call.
    let sent = unsafe { libc::sendmsg(socket.as_raw_fd(), &message, 0) };
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

/// Receive one payload plus, when the peer attached one, its `SCM_RIGHTS` descriptor.
///
/// Received descriptors are `CLOEXEC` (`MSG_CMSG_CLOEXEC`) and owned by the returned [`OwnedFd`].
///
/// # Errors
///
/// Propagates `recvmsg(2)` failures and reports a cleanly closed peer as
/// [`io::ErrorKind::UnexpectedEof`]. A truncated control buffer (`MSG_TRUNC`/`MSG_CTRUNC`) or a peer
/// that attaches **more than one** descriptor is rejected with [`io::ErrorKind::InvalidData`], and
/// every delivered descriptor is closed before returning, so a malicious or buggy peer can never
/// leak descriptors into the daemon.
pub fn recv_fd(socket: &UnixStream) -> io::Result<(Vec<u8>, Option<OwnedFd>)> {
    let mut payload = vec![0_u8; PAYLOAD_CAP];
    let mut iov = libc::iovec {
        iov_base: payload.as_mut_ptr().cast(),
        iov_len: payload.len(),
    };
    let mut control = vec![0_u8; cmsg_space()?];
    // SAFETY: an all-zero `msghdr` is a valid empty value; every field `recvmsg` reads is set below
    // before the call.
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = narrow(control.len())?;
    // SAFETY: `message` references the live `iov`/`control` buffers above; `recvmsg` writes at most
    // `msg_controllen` bytes into `control` and at most the payload length into `payload`.
    let received =
        unsafe { libc::recvmsg(socket.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) };
    if received < 0 {
        return Err(io::Error::last_os_error());
    }
    if received == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "peer closed before sending a control frame",
        ));
    }
    payload.truncate(received as usize);
    let mut received_fds: Vec<OwnedFd> = Vec::new();
    // SAFETY: `message` was populated by `recvmsg`; `CMSG_FIRSTHDR`/`CMSG_NXTHDR` walk only within
    // the `control` buffer bounded by `msg_controllen`, the per-header descriptor count is derived
    // from `cmsg_len`, and each `SCM_RIGHTS` data slot is read as one `RawFd`.
    // `OwnedFd::from_raw_fd` takes ownership of every freshly received descriptor exactly once.
    unsafe {
        let header_len = widen(libc::CMSG_LEN(0))?;
        let mut header = libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                let data_len = widen((*header).cmsg_len)?.saturating_sub(header_len);
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
    // The buffered descriptors are dropped here, closing them, before the protocol violation is
    // reported. A truncated control buffer may already have had its excess descriptors closed by
    // the kernel; the ones that fit are closed by this drop.
    if message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "control message was truncated; exactly one descriptor is expected",
        ));
    }
    match received_fds.len() {
        0 => Ok((payload, None)),
        1 => Ok((payload, received_fds.pop())),
        extra => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("SCM_RIGHTS peer delivered {extra} descriptors; exactly one was expected"),
        )),
    }
}
