//! The 120ms repaint thread that keeps every spinner and elapsed time live on a quiet daemon.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::super::region::take_resized;
use super::{Pane, terminal_size};

/// Refreshes the visible pane every 120ms so every spinner and elapsed time stays live even when
/// the daemon is quiet.
pub(crate) struct Ticker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Ticker {
    pub(crate) fn start(pane: Arc<Mutex<Pane>>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                if take_resized() {
                    let (cols, rows) = terminal_size();
                    if let Ok(mut pane) = pane.lock() {
                        pane.set_size(cols, rows);
                    }
                }
                if let Ok(mut pane) = pane.lock() {
                    pane.draw();
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
