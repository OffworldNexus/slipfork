//! A real reference `<pods>/control.sock` server speaking the canonical control frames.
//!
//! It decodes requests with `slipfork_world::control::decode_request` and answers with typed
//! `InitFrame::Reply` frames, exactly like `slipfork-world-init` does. No test inspects or builds a
//! frame by hand.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{Value, json};
use slipfork_world::PodStatus;
use slipfork_world::control::{self, InitFrame, PROTOCOL_VERSION, Request};

/// A loopback `control.sock` reference server for one retained world.
pub struct ReferenceWorldControl {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ReferenceWorldControl {
    /// Binds `<pods_dir>/control.sock` and answers `List`, `Restart`, and `Shutdown`.
    pub fn bind(pods_dir: &Path, pods: Vec<PodStatus>) -> Self {
        fs::create_dir_all(pods_dir).expect("create the pod root");
        let path = pods_dir.join("control.sock");
        let listener = UnixListener::bind(&path).expect("bind control.sock");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod control.sock");
        listener
            .set_nonblocking(true)
            .expect("nonblocking control listener");
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("blocking reply stream");
                        let mut line = String::new();
                        if BufReader::new(stream.try_clone().expect("clone"))
                            .read_line(&mut line)
                            .is_err()
                        {
                            continue;
                        }
                        let Ok(request) = control::decode_request(line.trim()) else {
                            continue;
                        };
                        let reply = InitFrame::Reply {
                            v: PROTOCOL_VERSION,
                            id: request.id().to_owned(),
                            ok: Some(payload(&request, &pods)),
                            error: None,
                        };
                        if let Ok(bytes) = control::encode(&reply) {
                            let _ = stream.write_all(&bytes);
                            let _ = stream.flush();
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

fn payload(request: &Request, pods: &[PodStatus]) -> Value {
    match request {
        Request::List { .. } => json!({
            "pods": pods
                .iter()
                .map(|pod| json!({ "id": pod.id, "pid": pod.pid, "running": pod.running }))
                .collect::<Vec<_>>()
        }),
        Request::Restart { .. } => json!({ "restarted": pods.len() }),
        Request::Shutdown { .. } => json!({ "stopped": true }),
        _ => json!({}),
    }
}

impl Drop for ReferenceWorldControl {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
