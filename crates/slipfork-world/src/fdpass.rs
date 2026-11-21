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

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::AsFd;

    use super::*;

    fn socket_pair() -> (UnixStream, UnixStream) {
        let mut fds = [0_i32; 2];
        // SAFETY: `fds` is two live `RawFd` slots; `socketpair` writes exactly two descriptors.
        let ret =
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(ret, 0, "socketpair failed: {}", io::Error::last_os_error());
        // SAFETY: both descriptors were just created and are owned exactly once here.
        unsafe {
            (
                UnixStream::from_raw_fd(fds[0]),
                UnixStream::from_raw_fd(fds[1]),
            )
        }
    }

    fn pipe() -> (OwnedFd, OwnedFd) {
        let mut fds = [0_i32; 2];
        // SAFETY: `fds` is two live `RawFd` slots; `pipe2` writes exactly two descriptors.
        let ret = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
        assert_eq!(ret, 0, "pipe2 failed: {}", io::Error::last_os_error());
        // SAFETY: both descriptors were just created and are owned exactly once here.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    /// Sends a control message with an arbitrary number of descriptors, bypassing [`send_fd`].
    fn send_many(socket: &UnixStream, payload: &[u8], fds: &[RawFd]) {
        let mut iov = libc::iovec {
            iov_base: payload.as_ptr().cast_mut().cast(),
            iov_len: payload.len(),
        };
        let space = std::mem::size_of_val(fds);
        let declared = narrow::<libc::c_uint>(space).expect("test descriptor block fits");
        // SAFETY: `CMSG_SPACE` is pure arithmetic over the declared byte length.
        let mut control = vec![0_u8; widen(unsafe { libc::CMSG_SPACE(declared) }).expect("fits")];
        // SAFETY: an all-zero `msghdr` is valid; every read field is set below.
        let mut message: libc::msghdr = unsafe { mem::zeroed() };
        message.msg_iov = &raw mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = narrow(control.len()).expect("test control buffer fits");
        // SAFETY: `control` is exactly `CMSG_SPACE(n * sizeof(RawFd))` bytes, so the single header
        // and its `n` data slots fit; each slot is written exactly once.
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&message);
            assert!(!header.is_null());
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            let control_len = widen(libc::CMSG_LEN(declared)).expect("test control length fits");
            (*header).cmsg_len = narrow(control_len).expect("test control length fits");
            let data = libc::CMSG_DATA(header).cast::<RawFd>();
            for (index, fd) in fds.iter().enumerate() {
                std::ptr::write_unaligned(data.add(index), *fd);
            }
            message.msg_controllen = narrow(control_len).expect("test control length fits");
        }
        // SAFETY: the live `message` references initialized buffers for the call's duration.
        let sent = unsafe { libc::sendmsg(socket.as_raw_fd(), &message, 0) };
        assert_eq!(sent, payload.len() as isize);
    }

    /// One anonymous `memfd` whose `/proc/self/fd` link is unique to this test.
    fn memfd(name: &str) -> OwnedFd {
        let name = std::ffi::CString::new(name).expect("no interior NUL");
        // SAFETY: the name is a live NUL-terminated C string; the returned fd is owned once here.
        let raw = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
        assert!(
            raw >= 0,
            "memfd_create failed: {}",
            io::Error::last_os_error()
        );
        // SAFETY: `raw` was just created and is owned exactly once.
        unsafe { OwnedFd::from_raw_fd(raw) }
    }

    /// How many live descriptors point at `memfd:<name>`; a leaked duplicate would make it two.
    fn links_to(name: &str) -> usize {
        let needle = format!("memfd:{name}");
        std::fs::read_dir("/proc/self/fd")
            .expect("read /proc/self/fd")
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_link(entry.path()).ok())
            .filter(|target| target.to_string_lossy().contains(&needle))
            .count()
    }

    #[test]
    fn one_descriptor_round_trips_and_stays_usable() {
        let (sender, receiver) = socket_pair();
        let (read_end, write_end) = pipe();
        let mut writer = File::from(write_end);
        writer.write_all(b"scm-rights").expect("seed the pipe");
        drop(writer);

        send_fd(&sender, b"one", Some(read_end.as_fd())).expect("send one descriptor");
        let (payload, descriptor) = recv_fd(&receiver).expect("receive one descriptor");
        assert_eq!(payload, b"one");
        let descriptor = descriptor.expect("one descriptor arrived");

        let mut received = File::from(descriptor);
        let mut text = String::new();
        received
            .read_to_string(&mut text)
            .expect("read through the fd");
        assert_eq!(text, "scm-rights");
    }

    #[test]
    fn zero_descriptors_is_a_plain_payload() {
        let (sender, receiver) = socket_pair();
        send_fd(&sender, b"plain", None).expect("send a payload");
        let (payload, descriptor) = recv_fd(&receiver).expect("receive a payload");
        assert_eq!(payload, b"plain");
        assert!(descriptor.is_none(), "no descriptor may be invented");
    }

    #[test]
    fn two_descriptors_are_refused_and_closed() {
        let (sender, receiver) = socket_pair();
        let first = memfd("slipfork-fdpass-two-a");
        let second = memfd("slipfork-fdpass-two-b");
        send_many(&sender, b"two", &[first.as_raw_fd(), second.as_raw_fd()]);
        let error = recv_fd(&receiver).expect_err("two descriptors must be refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        // Both delivered descriptors were dropped with the error; only the sender's survive.
        assert_eq!(
            links_to("slipfork-fdpass-two-a"),
            1,
            "received fd must close"
        );
        assert_eq!(
            links_to("slipfork-fdpass-two-b"),
            1,
            "received fd must close"
        );
    }

    #[test]
    fn a_truncated_control_buffer_is_refused() {
        let (sender, receiver) = socket_pair();
        // Four descriptors exceed the one-descriptor control buffer, so the kernel truncates it.
        let names = [
            "slipfork-fdpass-wide-a",
            "slipfork-fdpass-wide-b",
            "slipfork-fdpass-wide-c",
            "slipfork-fdpass-wide-d",
        ];
        let held: Vec<OwnedFd> = names.iter().map(|name| memfd(name)).collect();
        let fds: Vec<RawFd> = held.iter().map(AsRawFd::as_raw_fd).collect();
        send_many(&sender, b"wide", &fds);
        let error = recv_fd(&receiver).expect_err("an oversized control message must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        // Every descriptor that fit was closed before the error returned.
        for name in names {
            assert_eq!(links_to(name), 1, "buffered descriptor `{name}` must close");
        }
    }

    #[test]
    fn a_closed_peer_is_an_unexpected_eof() {
        let (sender, receiver) = socket_pair();
        drop(sender);
        let error = recv_fd(&receiver).expect_err("a closed peer must not yield a frame");
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }
}
