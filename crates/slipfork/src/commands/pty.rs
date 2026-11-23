//! Real-terminal attachment: raw mode, `SIGWINCH` forwarding, and byte-for-byte relay.
//!
//! The daemon hands the CLI a `/dev/pts/N` master; this module never prefixes, colours, or buffers
//! those bytes. [`RawModeGuard`] restores cooked mode from `Drop`, so a panic or an early return
//! cannot leave the invoking terminal unusable.

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::cursor::Show;
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};

use crate::daemon::DaemonClient;

/// Set by the `SIGWINCH` handler; the pump forwards one resize frame per observed flag.
static RESIZED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_winch(_signal: libc::c_int) {
    RESIZED.store(true, Ordering::SeqCst);
}

extern "C" fn on_continue(_signal: libc::c_int) {}

fn install_winch() {
    // SAFETY: both handlers only store into atomics / do nothing, and `sigaction` writes one
    // value into the local `action`. Clearing `SA_RESTART` makes a blocking `poll` return `EINTR`
    // so the resize flag is seen promptly.
    unsafe {
        libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t);
        libc::signal(
            libc::SIGCONT,
            on_continue as *const () as libc::sighandler_t,
        );
        let mut action: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGWINCH, std::ptr::null(), &mut action) == 0 {
            action.sa_flags &= !libc::SA_RESTART;
            let _ = libc::sigaction(libc::SIGWINCH, &action, std::ptr::null_mut());
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
/// `stdin` bytes go to the PTY, PTY bytes go to `stdout`, and a `SIGWINCH` becomes one
/// `ExecResize` frame on the daemon connection (which forwards it to the world).
///
/// # Errors
///
/// Propagates terminal setup and transport failures; an `EIO`/`EPIPE` on the master is a clean
/// end-of-child, not an error.
pub fn attach(client: &mut DaemonClient, id: &str, master: &OwnedFd) -> io::Result<()> {
    let _guard = RawModeGuard::enter()?;
    install_winch();
    let master_fd = master.as_raw_fd();
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut buffer = [0_u8; 8192];
    loop {
        if RESIZED.swap(false, Ordering::SeqCst)
            && let Ok((cols, rows)) = size()
            && client.send_resize(id, cols, rows).is_err()
        {
            return Ok(());
        }
        let mut fds = [
            libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: master_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: two valid `pollfd`s live across the call; 100ms keeps resize checks responsive.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, 100) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let readable = libc::POLLIN | libc::POLLHUP | libc::POLLERR;
        if fds[1].revents & readable != 0 {
            match master_read(master_fd, &mut buffer) {
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
                Ok(read) => write_fd(master_fd, &buffer[..read])?,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
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
