//! A real reference `<pods>/control.sock` server speaking the canonical control frames.
//!
//! It decodes requests with `slipfork_world::control::decode_request` and answers with typed
//! `InitFrame` frames, exactly like `slipfork-world-init` does. No test inspects or builds a
//! frame by hand.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};
use slipfork_world::PodStatus;
use slipfork_world::control::{self, InitFrame, OutputKind, PROTOCOL_VERSION, Request, Stream};

/// A loopback `control.sock` reference server for one retained world.
pub struct ReferenceWorldControl {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ReferenceWorldControl {
    /// Binds `<pods_dir>/control.sock` and answers `List`, `Restart`, `Logs`, `Exec`, `Shutdown`.
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
                        // Keep one reader for the request *and* any `ExecInput` frames: a
                        // dropped `BufReader` would discard bytes already read into its buffer.
                        let mut reader =
                            BufReader::new(stream.try_clone().expect("clone the request stream"));
                        let mut line = String::new();
                        if reader.read_line(&mut line).is_err() {
                            continue;
                        }
                        let Ok(request) = control::decode_request(line.trim()) else {
                            continue;
                        };
                        let _ = handle(&mut stream, &mut reader, request, &pods);
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

fn handle(
    stream: &mut UnixStream,
    reader: &mut BufReader<UnixStream>,
    request: Request,
    pods: &[PodStatus],
) -> std::io::Result<()> {
    match request {
        Request::Logs {
            id,
            service,
            pods: filter,
            follow,
            ..
        } => {
            let selected: Vec<String> = if let Some(target) = service {
                if !pods.iter().any(|pod| pod.id == target && pod.running) {
                    let reply = InitFrame::Reply {
                        v: PROTOCOL_VERSION,
                        id,
                        ok: None,
                        error: Some(slipfork_world::ControlError {
                            code: "no-pod".to_owned(),
                            message: format!("pod `{target}` is not running"),
                        }),
                    };
                    return send(stream, &reply);
                }
                vec![target]
            } else {
                pods.iter()
                    .filter(|pod| pod.running)
                    .map(|pod| pod.id.clone())
                    .filter(|id| {
                        filter
                            .as_ref()
                            .is_none_or(|wanted| wanted.iter().any(|w| w == id))
                    })
                    .collect()
            };
            for pod in &selected {
                send(
                    stream,
                    &output(
                        &id,
                        pod,
                        Stream::Combined,
                        OutputKind::Divider,
                        &format!("── {pod} started ──"),
                    ),
                )?;
                send(
                    stream,
                    &output(
                        &id,
                        pod,
                        Stream::Combined,
                        OutputKind::Line,
                        "hello from the pod",
                    ),
                )?;
            }
            if follow {
                // Hold the world stream until the follower closes, like the real init.
                let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
                let mut buffer = [0_u8; 64];
                let deadline = Instant::now() + Duration::from_secs(10);
                while Instant::now() < deadline {
                    match stream.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock
                                    | std::io::ErrorKind::TimedOut
                                    | std::io::ErrorKind::Interrupted
                            ) => {}
                        Err(_) => break,
                    }
                }
                return Ok(());
            }
            for pod in &selected {
                send(
                    stream,
                    &output(&id, pod, Stream::Combined, OutputKind::Eof, ""),
                )?;
            }
            let reply = InitFrame::Reply {
                v: PROTOCOL_VERSION,
                id,
                ok: Some(json!({ "pods": selected, "follow": false })),
                error: None,
            };
            send(stream, &reply)
        }
        Request::Exec { id, pod, stdin, .. } => {
            if stdin {
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line)? == 0 {
                        break;
                    }
                    let Ok(Request::ExecInput { data, .. }) = control::decode_request(line.trim())
                    else {
                        continue;
                    };
                    if data.is_empty() || data == "EOF" {
                        break;
                    }
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .unwrap_or_default();
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    send(
                        stream,
                        &output(&id, &pod, Stream::Stdout, OutputKind::Line, &text),
                    )?;
                }
            }
            send(
                stream,
                &output(&id, &pod, Stream::Stdout, OutputKind::Line, "exec-ok"),
            )?;
            let reply = InitFrame::Reply {
                v: PROTOCOL_VERSION,
                id,
                ok: Some(json!({ "exit_code": 7, "pod": pod })),
                error: None,
            };
            send(stream, &reply)
        }
        other => {
            let id = other.id().to_owned();
            let reply = InitFrame::Reply {
                v: PROTOCOL_VERSION,
                id,
                ok: Some(payload(&other, pods)),
                error: None,
            };
            send(stream, &reply)
        }
    }
}

fn output(id: &str, pod: &str, stream: Stream, kind: OutputKind, line: &str) -> InitFrame {
    InitFrame::Output {
        v: PROTOCOL_VERSION,
        id: id.to_owned(),
        pod: Some(pod.to_owned()),
        stream,
        kind,
        ts_ms: None,
        line: line.to_owned(),
    }
}

fn send(stream: &mut UnixStream, frame: &InitFrame) -> std::io::Result<()> {
    let bytes = control::encode(frame).map_err(std::io::Error::other)?;
    stream.write_all(&bytes)?;
    stream.flush()
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
