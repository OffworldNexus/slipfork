//! Interactive `exec`: one `crun exec` child inside a live pod, streaming stdio both ways.

use std::io::{BufRead, BufReader};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;

use slipfork_world::control::{FrameLines, InitFrame, PROTOCOL_VERSION, Stream};

use crate::pod::{WorldShared, reap_zombies};
use crate::shared::wait_child;
use crate::{Fault, REMEDY_CRUN, REMEDY_PROTOCOL};

use super::lines::{IDLE, TempProcessSpec, peer_closed, pump_input, refuse, reply, write_output};

/// Runs one command inside a live pod through `crun exec` and streams its stdio.
pub(crate) fn serve_exec(
    shared: Arc<WorldShared>,
    request_id: String,
    pod: String,
    argv: Vec<String>,
    stdin: bool,
    mut stream: UnixStream,
    buffered: FrameLines,
) -> Result<(), Fault> {
    let Some(info) = shared.info(&pod) else {
        return refuse(
            &mut stream,
            &request_id,
            "no-pod",
            format!("pod `{pod}` is not running"),
        );
    };
    if argv.is_empty() {
        return refuse(
            &mut stream,
            &request_id,
            "empty-argv",
            "no command given".into(),
        );
    }
    // `crun exec` without `--process` defaults to uid 0 / gid 0 (unmapped in our identity-mapped
    // rootless pod userns -> `setresgid(0): EINVAL`, exit 255) and drops `config.json` `env`.
    // Deriving a mode-`0600` tmpfs `process.json` preserves identity uid/gid + pod env without
    // exposing secrets in `/proc/<pid>/cmdline`.
    let spec_file = match TempProcessSpec::write(&info, shared.next_exec_seq(), &argv) {
        Ok(file) => file,
        Err(fault) => {
            eprintln!("slipfork-world-init: {fault}");
            return refuse(&mut stream, &request_id, "exec-failed", fault.summary());
        }
    };
    let mut command = shared.crun_command();
    command
        .args(["exec", "--process"])
        .arg(spec_file.path())
        .arg(&pod)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin { Stdio::piped() } else { Stdio::null() })
        .process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(source) => {
            let fault = Fault::cause(
                format!("exec in pod `{pod}` with crun"),
                REMEDY_CRUN,
                source,
            );
            eprintln!("slipfork-world-init: {fault}");
            return refuse(&mut stream, &request_id, "exec-failed", fault.summary());
        }
    };
    let child_pid = i32::try_from(child.id()).unwrap_or_default();
    shared.register_exec(child_pid);
    let client_gone = Arc::new(AtomicBool::new(false));
    let slot = child
        .stdin
        .take()
        .map(|stdin| Arc::new(Mutex::new(Some(stdin))));
    let input_thread = match slot.clone() {
        Some(slot) => {
            let input = stream
                .try_clone()
                .map_err(|source| Fault::cause("exec input clone", REMEDY_PROTOCOL, source))?;
            let gone = Arc::clone(&client_gone);
            Some(thread::spawn(move || {
                pump_input(input, buffered, slot, gone)
            }))
        }
        None => None,
    };

    let (sender, receiver) = mpsc::channel::<(Stream, String)>();
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let sender = sender.clone();
        readers.push(thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = sender.send((Stream::Stdout, line));
            }
        }));
    }
    if let Some(stderr) = child.stderr.take() {
        let sender = sender.clone();
        readers.push(thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = sender.send((Stream::Stderr, line));
            }
        }));
    }
    drop(sender);

    let mut deliver = true;
    loop {
        match receiver.recv_timeout(IDLE) {
            Ok((kind, line)) => {
                if write_output(
                    &mut stream,
                    &request_id,
                    &pod,
                    kind,
                    slipfork_world::OutputKind::Line,
                    &line,
                )
                .is_err()
                {
                    deliver = false;
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if shared.stopped() || client_gone.load(Ordering::Relaxed) || peer_closed(&stream) {
                    deliver = false;
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    if (!deliver || client_gone.load(Ordering::Relaxed) || shared.stopped()) && child_pid != 0 {
        slipfork_world::kill_group(child_pid);
    }
    let code = wait_child(&mut child, &shared, child_pid);
    drop(spec_file);
    shared.unregister_exec(child_pid);
    // Unblock the stdin pump (it blocks on this socket, not on the child) and its readers.
    let _ = stream.shutdown(Shutdown::Read);
    if let Some(handle) = input_thread {
        let _ = handle.join();
    }
    for reader in readers {
        let _ = reader.join();
    }
    reap_zombies(&shared);
    if !deliver {
        return Ok(());
    }
    reply(
        &mut stream,
        &InitFrame::Reply {
            v: PROTOCOL_VERSION,
            id: request_id,
            ok: Some(serde_json::json!({ "exit_code": code, "pod": pod })),
            error: None,
        },
    )
}
