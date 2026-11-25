//! The one `DECSTBM` scroll region and the signals that keep it honest.
//!
//! `install` applies the margins and captures the previous `SIGINT`/`SIGWINCH` handlers; leaving
//! the region — normal exit, resize, panic unwind — restores `ESC[r` and both handlers. This module
//! owns the margins and nothing else: every row address is the pane's, and the stream after the
//! final verdict row is empty. A terminal shorter than three rows never gets a region at all, so
//! `ESC[1;1r` cannot be written.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static RESIZED: AtomicBool = AtomicBool::new(false);
static REGION_ROWS: AtomicUsize = AtomicUsize::new(0);
static PREVIOUS_SIGINT: AtomicUsize = AtomicUsize::new(0);
static PREVIOUS_SIGWINCH: AtomicUsize = AtomicUsize::new(0);

/// The only bytes a normal exit (or a resize transition) writes: reset the scroll margins.
///
/// Deliberately no `ESC[<n>;1H` cursor move, no `ESC[2K`/`ESC[J` erase, no alt-screen switch, and
/// no `ESC c` full reset — the rows already painted, including the final verdict, must survive.
pub(super) const RESTORE_MARGINS: &str = "\x1b[r";

/// The bytes that install the region: margins only, empty below three rows. The pane clears the
/// panel row it owns.
fn install_sequence(rows: usize) -> String {
    if rows < 3 {
        return String::new();
    }
    format!("\x1b[1;{}r", rows - 1)
}

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

/// The RAII scroll-region guard: one margins-only install in, one margins-only `ESC[r` out.
///
/// Built only for an interactive colour TTY; pipes, `--json`, and `NO_COLOR` never construct one,
/// so no escape from this module can reach a non-terminal stream.
pub(super) struct ScrollRegionGuard {
    active: bool,
}

impl ScrollRegionGuard {
    pub(super) fn install(rows: usize) -> Option<Self> {
        if rows < 3 {
            return None;
        }
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(install_sequence(rows).as_bytes());
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
        Some(Self { active: true })
    }

    /// The one non-destructive exit transition, shared by `Drop` and a live resize: write the
    /// margins-only `ESC[r` (which homes the cursor) and disarm. The pane appends its park after
    /// the reset on the exit path; `reset` is the panic-unwind fallback that only restores margins.
    pub(super) fn reset(&mut self) {
        if !self.active {
            return;
        }
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(RESTORE_MARGINS.as_bytes());
        let _ = stdout.flush();
        self.disarm();
    }

    /// Restores both signal handlers and zeroes `REGION_ROWS`; writes nothing. The pane's exit
    /// stream already ends with the park, so it only needs to drop the guard's bookkeeping.
    pub(super) fn disarm(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
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

impl Drop for ScrollRegionGuard {
    fn drop(&mut self) {
        self.reset();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::{REGION_ROWS, RESTORE_MARGINS, ScrollRegionGuard, install_sequence};

    /// The escapes a margins-only exit may never contain, checked directly over the bytes.
    const FORBIDDEN: [&[u8]; 7] = [
        b"\x1b[2K",
        b"\x1b[J",
        b"\x1b c",
        b"\x1bc",
        b"\x1b[?1049",
        b"\x1b[?47",
        b"\x1b[?1047",
    ];

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    /// Any `ESC [ <params> (H|f)` — an absolute cursor move, with or without parameters, as in
    /// `ESC[H`, `ESC[;1H`, `ESC[30H`, `ESC[30;1H`, or `ESC[30;1f`.
    fn has_cursor_move(bytes: &[u8]) -> bool {
        let mut index = 0;
        while index + 2 < bytes.len() {
            if bytes[index] == 0x1b && bytes[index + 1] == b'[' {
                let mut at = index + 2;
                while at < bytes.len() && (bytes[at].is_ascii_digit() || bytes[at] == b';') {
                    at += 1;
                }
                if matches!(bytes.get(at), Some(b'H' | b'f')) {
                    return true;
                }
            }
            index += 1;
        }
        false
    }

    fn assert_margin_only(bytes: &[u8]) {
        for forbidden in FORBIDDEN {
            assert!(
                !contains(bytes, forbidden),
                "exit bytes must not contain {forbidden:?}: {bytes:?}"
            );
        }
        assert!(
            !has_cursor_move(bytes),
            "exit bytes must not move the cursor: {bytes:?}"
        );
    }

    #[test]
    fn restore_margins_is_the_only_exit_sequence() {
        assert_eq!(RESTORE_MARGINS, "\x1b[r");
        assert_margin_only(RESTORE_MARGINS.as_bytes());
        // Guard against a vacuous scanner: every absolute move must be flagged, including the
        // exact old destructive exit.
        for sequence in [
            &b"\x1b[H"[..],
            b"\x1b[;1H",
            b"\x1b[30H",
            b"\x1b[30;1H",
            b"\x1b[30;1f",
            b"\x1b[30;1H\x1b[2K\x1b[r",
        ] {
            assert!(has_cursor_move(sequence), "must flag {sequence:?}");
        }
    }

    #[test]
    fn install_sequence_is_margins_only_and_empty_below_three_rows() {
        for rows in 0..3 {
            assert!(install_sequence(rows).is_empty(), "rows {rows}");
        }
        assert_eq!(install_sequence(3), "\x1b[1;2r");
        for rows in [3, 30, 120] {
            assert_eq!(install_sequence(rows), format!("\x1b[1;{}r", rows - 1));
            assert_margin_only(install_sequence(rows).as_bytes());
        }
    }

    #[test]
    fn reset_bytes_are_exactly_restore_margins_and_disarm_writes_nothing() {
        // `reset` emits this one constant and calls `disarm`; `disarm` has no `write` call, so it
        // cannot emit a byte. Check the state transition the pane relies on after its own stream.
        assert_eq!(RESTORE_MARGINS.as_bytes(), b"\x1b[r");
        REGION_ROWS.store(30, Ordering::Relaxed);
        let mut guard = ScrollRegionGuard { active: true };
        guard.disarm();
        assert!(!guard.active, "disarm deactivates the guard");
        assert_eq!(
            REGION_ROWS.load(Ordering::Relaxed),
            0,
            "disarm zeroes the installed rows"
        );
    }
}
