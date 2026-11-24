//! The one `DECSTBM` scroll region and the signals that keep it honest.
//!
//! `install` applies the region and captures the previous `SIGINT`/`SIGWINCH` handlers; `Drop`
//! always restores `ESC[r` and both handlers, on success, error, panic unwind, and Ctrl-C. A
//! terminal shorter than three rows never gets a region at all, so `ESC[1;1r` cannot be written.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static RESIZED: AtomicBool = AtomicBool::new(false);
static REGION_ROWS: AtomicUsize = AtomicUsize::new(0);
static PREVIOUS_SIGINT: AtomicUsize = AtomicUsize::new(0);
static PREVIOUS_SIGWINCH: AtomicUsize = AtomicUsize::new(0);

/// Whether a `SIGWINCH` arrived since the last ticker check.
pub(super) fn take_resized() -> bool {
    RESIZED.swap(false, Ordering::Relaxed)
}

extern "C" fn on_sigwinch(_: libc::c_int) {
    RESIZED.store(true, Ordering::Relaxed);
}

extern "C" fn on_sigint(signal: libc::c_int) {
    let rows = REGION_ROWS.load(Ordering::Relaxed);
    if rows >= 3 {
        let mut buffer = [0_u8; 32];
        let mut length = 0;
        buffer[length] = 0x1b;
        length += 1;
        buffer[length] = b'[';
        length += 1;
        length = push_decimal(&mut buffer, length, rows);
        for byte in b";1H\x1b[2K\x1b[r" {
            buffer[length] = *byte;
            length += 1;
        }
        // SAFETY: `write(2)` only reads `buffer`; escape restoration is async-signal-safe.
        unsafe {
            libc::write(libc::STDOUT_FILENO, buffer.as_ptr().cast(), length);
        }
    }
    // SAFETY: `_exit(2)` ends the process without touching shared state or unwinding.
    unsafe { libc::_exit(128 + signal) };
}

fn push_decimal(buffer: &mut [u8], mut at: usize, mut value: usize) -> usize {
    if value == 0 {
        buffer[at] = b'0';
        return at + 1;
    }
    let mut digits = [0_u8; 8];
    let mut count = 0;
    while value > 0 {
        digits[count] = b'0' + (value % 10) as u8;
        value /= 10;
        count += 1;
    }
    while count > 0 {
        count -= 1;
        buffer[at] = digits[count];
        at += 1;
    }
    at
}

/// The RAII scroll-region guard: one `ESC[1;{rows-1}r` in, one `ESC[r` out.
pub(super) struct ScrollRegionGuard {
    rows: usize,
    active: bool,
}

impl ScrollRegionGuard {
    pub(super) fn install(rows: usize) -> Option<Self> {
        if rows < 3 {
            return None;
        }
        let mut stdout = std::io::stdout().lock();
        let _ = write!(stdout, "\x1b[1;{}r\x1b[{};1H\x1b[2K", rows - 1, rows);
        let _ = stdout.flush();
        REGION_ROWS.store(rows, Ordering::Relaxed);
        RESIZED.store(false, Ordering::Relaxed);
        // SAFETY: signal handlers only set atomics or issue one async-signal-safe `write`.
        unsafe {
            PREVIOUS_SIGINT.store(
                libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t),
                Ordering::Relaxed,
            );
            PREVIOUS_SIGWINCH.store(
                libc::signal(
                    libc::SIGWINCH,
                    on_sigwinch as *const () as libc::sighandler_t,
                ),
                Ordering::Relaxed,
            );
        }
        Some(Self { rows, active: true })
    }
}

impl Drop for ScrollRegionGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let mut stdout = std::io::stdout().lock();
        let _ = write!(stdout, "\x1b[{};1H\x1b[2K\x1b[r", self.rows);
        let _ = stdout.flush();
        REGION_ROWS.store(0, Ordering::Relaxed);
        // SAFETY: restoring the handlers captured in `install` only rewrites process handlers.
        unsafe {
            let previous = PREVIOUS_SIGINT.load(Ordering::Relaxed);
            if previous != 0 && previous != libc::SIG_ERR {
                libc::signal(libc::SIGINT, previous);
            }
            let previous = PREVIOUS_SIGWINCH.load(Ordering::Relaxed);
            if previous != 0 && previous != libc::SIG_ERR {
                libc::signal(libc::SIGWINCH, previous);
            }
        }
    }
}
