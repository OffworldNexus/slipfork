//! Real-terminal attachment: raw mode, resize forwarding, and byte-for-byte relay.
//!
//! The daemon hands the CLI a `/dev/pts/N` master; this module never prefixes, colours, or buffers
//! those bytes. [`RawModeGuard`] restores cooked mode from `Drop`, so a panic or an early return
//! cannot leave the invoking terminal unusable.

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use crossterm::cursor::Show;
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};

use crate::daemon::DaemonClient;

/// The owned `SIGWINCH` queue: the kernel latches one record per resize into this descriptor, and
/// the attachment drains it whenever `poll` reports the descriptor readable.
///
/// Owning the signal as a filename instead of a process-wide handler keeps the resize state local
/// to the attachment, so nothing survives the call in a global and no handler races the relay.
///
/// Invariant: `SIGWINCH` is blocked in the attaching thread only. That is safe because the CLI
/// attach path is single-threaded — the sole CLI thread spawn is the `up`-only boot ticker, which
/// never attaches. Any future thread that outlives this call inherits the blocked mask and would
/// silently swallow resizes; spawn before `attach` or re-examine this invariant first.
struct ResizeSignal {
    fd: OwnedFd,
    previous: libc::sigset_t,
}

impl ResizeSignal {
    /// Blocks `SIGWINCH` in this thread and opens the `signalfd` that carries it.
    fn install() -> io::Result<Self> {
        // SAFETY: every pointer refers to an initialized local of the type libc expects, the
        // signal set is filled by `sigemptyset`/`sigaddset`, and the returned descriptor is wrapped
        // in `OwnedFd` immediately so it cannot leak.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            let mut previous: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGWINCH);
            let blocked = libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut previous);
            if blocked != 0 {
                return Err(io::Error::from_raw_os_error(blocked));
            }
            let raw = libc::signalfd(-1, &set, libc::SFD_CLOEXEC | libc::SFD_NONBLOCK);
            if raw < 0 {
                libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                fd: OwnedFd::from_raw_fd(raw),
                previous,
            })
        }
    }

    /// Consumes every queued resize; `true` when at least one arrived.
    fn drain(&self) -> bool {
        let mut arrived = false;
        loop {
            let mut info: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is one writable `signalfd_siginfo` and the descriptor is live; a
            // non-blocking read returns `EAGAIN` once the queue is empty.
            let read = unsafe {
                libc::read(
                    self.fd.as_raw_fd(),
                    std::ptr::from_mut(&mut info).cast(),
                    std::mem::size_of::<libc::signalfd_siginfo>(),
                )
            };
            if read <= 0 {
                return arrived;
            }
            arrived = true;
        }
    }
}

impl Drop for ResizeSignal {
    fn drop(&mut self) {
        // SAFETY: restores the mask captured at install; the owned descriptor closes itself.
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.previous, std::ptr::null_mut());
        }
    }
}

/// Raw mode for the duration of one attachment; `Drop` always restores the terminal.
struct RawModeGuard {
    active: bool,
}

impl RawModeGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self { active: true })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, Show);
        let _ = stdout.flush();
    }
}

/// The terminal geometry this invocation should advertise to the world.
#[must_use]
pub fn geometry() -> (u16, u16) {
    size().unwrap_or((80, 24))
}

/// Relays the real terminal against `master` until the child closes it.
///
/// `stdin` bytes go to the PTY, PTY bytes go to `stdout`, and one or more coalesced `SIGWINCH`
/// records become one `ExecResize` frame on the daemon connection (which forwards it to the world).
///
/// # Errors
///
/// Propagates terminal setup and transport failures; an `EIO`/`EPIPE` on the master is a clean
/// end-of-child, not an error.
pub fn attach(client: &mut DaemonClient, id: &str, master: &OwnedFd) -> io::Result<()> {
    let mut attachment = Attachment {
        client,
        id,
        master: master.as_raw_fd(),
        resize: ResizeSignal::install()?,
        _raw: RawModeGuard::enter()?,
    };
    attachment.run()
}

/// One live attachment: raw mode, the owned resize queue, and the relay loop over the PTY master.
struct Attachment<'a> {
    client: &'a mut DaemonClient,
    id: &'a str,
    master: i32,
    resize: ResizeSignal,
    _raw: RawModeGuard,
}

impl Attachment<'_> {
    fn run(&mut self) -> io::Result<()> {
        let signal_fd = self.resize.fd.as_raw_fd();
        let mut stdin = io::stdin();
        let mut stdout = io::stdout();
        let mut buffer = [0_u8; 8192];
        loop {
            let mut fds = [
                libc::pollfd {
                    fd: libc::STDIN_FILENO,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.master,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: signal_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: three valid `pollfd`s live across the call; the descriptors wake it.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 3, -1) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if fds[2].revents & libc::POLLIN != 0
                && self.resize.drain()
                && let Ok((cols, rows)) = size()
                && self.client.send_resize(self.id, cols, rows).is_err()
            {
                return Ok(());
            }
            let readable = libc::POLLIN | libc::POLLHUP | libc::POLLERR;
            if fds[1].revents & readable != 0 {
                match master_read(self.master, &mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        stdout.write_all(&buffer[..read])?;
                        stdout.flush()?;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                    Err(error) => return Err(error),
                }
            }
            if fds[0].revents & readable != 0 {
                match stdin.read(&mut buffer) {
                    Ok(0) | Err(_) if fds[0].revents & libc::POLLHUP != 0 => break,
                    Ok(0) => break,
                    Ok(read) => write_fd(self.master, &buffer[..read])?,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }
}

fn master_read(fd: i32, buffer: &mut [u8]) -> io::Result<usize> {
    // SAFETY: `buffer` is a valid writable slice and `fd` is the live PTY master.
    let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
    if read < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(read as usize)
}

fn write_fd(fd: i32, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        // SAFETY: `bytes` points at initialized memory for its whole length; `fd` is the live
        // PTY master.
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        bytes = &bytes[written as usize..];
    }
    Ok(())
}
