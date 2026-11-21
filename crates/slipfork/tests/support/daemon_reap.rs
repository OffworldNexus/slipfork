//! Reaps the per-scratch daemon a lifecycle verb may have started.
//!
//! `DaemonClient::connect` spawns a real `__daemon` for every lifecycle verb, so a lab that only
//! removes its scratch root leaves a live process holding deleted sockets. These helpers send the
//! canonical typed `shutdown`, wait for the socket to disappear, then SIGKILL any survivor whose
//! environment still carries the scratch root.

use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

/// Sends the typed shutdown, waits for the socket to vanish, then kills any survivor.
pub fn reap(root: &Path) {
    shutdown(root);
    kill_daemons(root);
}

/// Sends the daemon owned by this scratch runtime a typed shutdown and waits for its socket.
pub fn shutdown(root: &Path) {
    let socket = root.join("run/daemon.sock");
    if let Ok(mut stream) = UnixStream::connect(&socket) {
        let _ = stream.write_all(b"{\"v\":1,\"id\":\"reap-bye\",\"method\":\"shutdown\"}\n");
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut reply = [0_u8; 128];
        let _ = stream.read(&mut reply);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Kills any `__daemon` still carrying this lab's runtime directory, so no survivor leaks.
pub fn kill_daemons(root: &Path) {
    let scope = root.as_os_str().as_bytes();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        if !cmdline.windows(b"__daemon".len()).any(|w| w == b"__daemon") {
            continue;
        }
        let Ok(environ) = std::fs::read(format!("/proc/{pid}/environ")) else {
            continue;
        };
        if environ.windows(scope.len()).any(|window| window == scope)
            && let Ok(pid) = i32::try_from(pid)
        {
            // SAFETY: `kill(2)` never dereferences memory.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}
