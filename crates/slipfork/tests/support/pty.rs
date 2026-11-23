//! A real `openpty` lab harness: the child owns the slave as its controlling terminal, the parent
//! reads the master into bytes **and** a [`Screen`], answering `DSR` (`ESC[6n`) so the shipped
//! boot pane can query the real cursor of a pre-seeded shell prompt.

#![allow(dead_code)]

#[path = "vt.rs"]
pub mod vt;

pub use self::vt::Screen;

use std::fs;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// The lab terminal is 120 columns so a full panel line never wraps.
pub const COLS: u16 = 120;
/// The lab terminal is 40 rows so the `DECSTBM` region is `1;39` and the panel is row 40.
pub const ROWS: u16 = 40;

const RUN_TIMEOUT: Duration = Duration::from_secs(240);

/// Everything one PTY run observed: raw bytes, exit status, and the final parsed screen.
pub struct PtyRun {
    /// Exact bytes the child wrote (including every escape).
    pub output: String,
    /// The child's exit status.
    pub status: ExitStatus,
    /// The terminal grid after feeding every byte.
    pub screen: Screen,
}

/// Runs `command` under a fresh 120×40 `openpty`, optionally seeding a shell prompt line.
///
/// `seed` is `(0-based row, text)`; the prompt is placed before the child starts, and a `DSR`
/// response reports the emulator's real cursor so the pane sees the prompt's row.
pub fn run(
    mut command: Command,
    seed: Option<(usize, &'static str)>,
    resize: Option<(Duration, u16, u16)>,
) -> PtyRun {
    let mut master = 0;
    let mut slave = 0;
    let winsize = libc::winsize {
        ws_row: ROWS,
        ws_col: COLS,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `openpty` fills both descriptors and reads one valid `winsize`.
    let allocated = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &winsize,
        )
    };
    assert_eq!(allocated, 0, "openpty must allocate a real PTY");
    // SAFETY: the descriptors are freshly owned by this process.
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    // SAFETY: `master` is owned here and closed when `File` drops.
    let master = unsafe { fs::File::from_raw_fd(master) };
    let master_fd = master.as_raw_fd();
    command
        .stdin(Stdio::from(slave.try_clone().expect("clone PTY slave")))
        .stdout(Stdio::from(slave.try_clone().expect("clone PTY slave")))
        .stderr(Stdio::from(slave));
    // SAFETY: the closure only performs `setsid(2)`/`ioctl(TIOCSCTTY)` between `fork` and `exec`.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("spawn the PTY child");
    // `Command` still owns the parent's three slave descriptors; until it is dropped the master
    // never sees EOF, so closing them here is what lets the reader finish.
    drop(command);
    let finished = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&finished);
    let reader = thread::spawn(move || {
        let mut master = master;
        let mut out = Vec::new();
        let mut screen = Screen::new(usize::from(ROWS), usize::from(COLS));
        if let Some((row, prompt)) = seed {
            screen.place(row, 0, prompt);
        }
        let mut answered = 0;
        let mut chunk = [0_u8; 8192];
        loop {
            let mut poll = libc::pollfd {
                fd: master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid `pollfd` lives across the call; a 100ms tick bounds the wait so a
            // finished child can never leave the reader blocked on a held slave descriptor.
            let ready = unsafe { libc::poll(&mut poll, 1, 100) };
            if ready < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            if ready == 0 {
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                continue;
            }
            if poll.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
                continue;
            }
            match master.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    out.extend_from_slice(&chunk[..read]);
                    screen.feed(&chunk[..read]);
                    let pending = screen.dsr_requests() - answered;
                    if pending > 0 {
                        let (row, col) = screen.cursor();
                        for _ in 0..pending {
                            let _ = write!(master, "\x1b[{};{}R", row + 1, col + 1);
                        }
                        let _ = master.flush();
                        answered += pending;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    continue;
                }
                Err(_) => break,
            }
        }
        (out, screen)
    });
    if let Some((delay, rows, cols)) = resize {
        thread::sleep(delay);
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: the master descriptor stays open until the reader thread finishes.
        unsafe { libc::ioctl(master_fd, libc::TIOCSWINSZ, &size) };
    }
    let status = wait_bounded(&mut child);
    finished.store(true, Ordering::Relaxed);
    let (bytes, screen) = reader.join().expect("join the PTY reader");
    PtyRun {
        output: String::from_utf8_lossy(&bytes).into_owned(),
        status,
        screen,
    }
}

fn wait_bounded(child: &mut std::process::Child) -> ExitStatus {
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {}
            Err(error) => panic!("cannot wait for the PTY child: {error}"),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "the PTY child did not finish within {}s",
                RUN_TIMEOUT.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(25));
    }
}
