//! The 120ms repaint thread that keeps the one status line's spinner and elapsed clock live.
//!
//! The thread never probes the terminal itself: the composition root injects the column probe, so
//! this module stays free of ambient OS state and the boot driver owns the single measurement.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::StatusLine;

/// Refreshes the status line every 120ms, re-measuring through the injected probe.
pub(crate) struct Ticker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Ticker {
    pub(crate) fn start(
        status: Arc<Mutex<StatusLine>>,
        probe: impl Fn() -> Option<usize> + Send + 'static,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                if let Some(cols) = probe()
                    && cols > 0
                    && let Ok(mut status) = status.lock()
                {
                    status.resize(cols);
                }
                if let Ok(mut status) = status.lock() {
                    status.paint();
                }
                thread::sleep(Duration::from_millis(120));
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
