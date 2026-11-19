//! Child stdio plumbing: run a command to completion while streaming interleaved
//! stdout/stderr, or capture one service stream into its retained [`ServiceLogs`] window.

#![forbid(unsafe_code)]

use std::io::{self, Read};
use std::process::{Child, Command};
use std::sync::mpsc;
use std::thread;

use slipfork_world::{EngineError, ExecOutput, ServiceLogs, StreamChunk, StreamSource};

/// Run `command` to completion, streaming interleaved stdout/stderr chunks to `on_output`.
pub(crate) fn run_streamed(
    command: Command,
    on_output: &mut dyn FnMut(StreamChunk),
) -> Result<ExecOutput, EngineError> {
    run_streamed_inner(command, None, on_output)
}

/// Run `command` with `input` copied to its stdin while streaming interleaved stdout/stderr.
pub(crate) fn run_streamed_with_input(
    command: Command,
    input: Box<dyn Read + Send>,
    on_output: &mut dyn FnMut(StreamChunk),
) -> Result<ExecOutput, EngineError> {
    run_streamed_inner(command, Some(input), on_output)
}

fn run_streamed_inner(
    mut command: Command,
    input: Option<Box<dyn Read + Send>>,
    on_output: &mut dyn FnMut(StreamChunk),
) -> Result<ExecOutput, EngineError> {
    let mut child = command
        .spawn()
        .map_err(|err| EngineError::Execution(format!("cannot execute: {err}")))?;
    let writer = match (input, child.stdin.take()) {
        (Some(mut input), Some(mut stdin)) => Some(thread::spawn(move || {
            let copied = io::copy(&mut input, &mut stdin);
            drop(stdin);
            copied
        })),
        (None, _) => None,
        (Some(_), None) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(EngineError::Execution(
                "cannot stream input: the child has no piped stdin".to_owned(),
            ));
        }
    };
    let (sender, receiver) = mpsc::channel::<StreamChunk>();
    let mut readers = Vec::with_capacity(2);
    if let Some(stdout) = child.stdout.take() {
        readers.push(spawn_reader(stdout, StreamSource::Stdout, sender.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(spawn_reader(stderr, StreamSource::Stderr, sender.clone()));
    }
    drop(sender);

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    for chunk in receiver {
        match chunk.source {
            StreamSource::Stdout => stdout.extend_from_slice(&chunk.bytes),
            StreamSource::Stderr => stderr.extend_from_slice(&chunk.bytes),
        }
        on_output(chunk);
    }
    for reader in readers {
        let _ = reader.join();
    }
    let status = child
        .wait()
        .map_err(|err| EngineError::Execution(err.to_string()))?;
    if let Some(writer) = writer {
        match writer.join() {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                return Err(EngineError::Execution(format!(
                    "streamed input failed: {error}"
                )));
            }
            Err(_) => {
                return Err(EngineError::Execution(
                    "streamed input writer panicked".to_owned(),
                ));
            }
        }
    }
    Ok(ExecOutput {
        exit_code: status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// Read `source` to EOF in 4 KiB chunks, handing each chunk to `sink`; stop when `sink` returns
/// `false` (a closed channel or a closed log window).
fn pump<R: Read>(mut source: R, mut sink: impl FnMut(Vec<u8>) -> bool) {
    let mut buffer = [0_u8; 4096];
    while let Ok(read) = source.read(&mut buffer) {
        if read == 0 {
            break;
        }
        if !sink(buffer[..read].to_vec()) {
            break;
        }
    }
}

fn spawn_reader<R: Read + Send + 'static>(
    source: R,
    kind: StreamSource,
    sender: mpsc::Sender<StreamChunk>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        pump(source, |bytes| {
            sender
                .send(StreamChunk {
                    source: kind,
                    bytes,
                })
                .is_ok()
        })
    })
}

/// Capture one service stream into its retained [`ServiceLogs`] window.
pub(crate) fn spawn_log_reader<R: Read + Send + 'static>(
    source: R,
    kind: StreamSource,
    log: ServiceLogs,
) {
    thread::spawn(move || {
        pump(source, |bytes| {
            log.push(kind, bytes);
            true
        })
    });
}

/// Best-effort helper stderr tail for actionable boot diagnostics.
pub(crate) fn drain_stderr(child: &mut Child) -> String {
    let mut raw = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut raw);
    }
    let trimmed = raw.trim();
    if trimmed.chars().count() > 2000 {
        trimmed.chars().take(2000).collect()
    } else {
        trimmed.to_owned()
    }
}
