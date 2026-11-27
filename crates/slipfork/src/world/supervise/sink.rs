//! The shared stdout emitter and the step-scoped NDJSON sink that locks it only to flush.

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use slipfork_world::{BootFailure, LogStore, Stream};

use crate::world::runtime::capture::RunTarget;

use super::log::EventLog;

pub(in crate::world) struct Emitter {
    pub(in crate::world) open: bool,
}
impl Emitter {
    pub(super) fn send(&mut self, value: Value) {
        if !self.open {
            return;
        }
        let mut bytes = value.to_string().into_bytes();
        bytes.push(b'\n');
        let mut stdout = std::io::stdout().lock();
        if stdout
            .write_all(&bytes)
            .and_then(|()| stdout.flush())
            .is_err()
        {
            // `up` returned and closed the pipe; the world must keep serving.
            self.open = false;
        }
    }
}

/// Step-scoped NDJSON writer: one line buffer per DAG node, locking [`Emitter`] only to flush.
pub(in crate::world) struct BootSink {
    log: EventLog,
    mirror: std::io::Stderr,
}

impl Clone for BootSink {
    fn clone(&self) -> Self {
        Self {
            log: self.log.clone(),
            mirror: std::io::stderr(),
        }
    }
}

impl BootSink {
    pub(in crate::world) fn for_step(
        emitter: Arc<Mutex<Emitter>>,
        step: usize,
        total: usize,
        label: &str,
        name: &str,
    ) -> Self {
        Self {
            log: EventLog::for_step(emitter, step, total, label, name),
            mirror: std::io::stderr(),
        }
    }

    /// Opens this step's run capture **before** it executes, so an early error still leaves one.
    pub(in crate::world) fn capture_run(
        mut self,
        store: &LogStore,
        project: &Path,
        kind: &str,
        name: &str,
        command: &str,
    ) -> Result<Self, BootFailure> {
        self.log.run = Some(Arc::new(Mutex::new(RunTarget::open(
            store, project, kind, name, command,
        )?)));
        Ok(self)
    }

    /// Appends the catalog line and returns the minted id, or `None` when nothing ran.
    pub(in crate::world) fn finish_run(&self, state: &str, duration_ms: u64) -> Option<String> {
        self.log.run.as_ref().and_then(|run| {
            run.lock()
                .ok()
                .and_then(|mut target| target.finish(state, duration_ms))
        })
    }

    /// Drops a no-op capture: no id, no `.log`/`.idx`, no catalog line.
    pub(in crate::world) fn discard_run(&self) {
        if let Some(run) = &self.log.run
            && let Ok(mut target) = run.lock()
        {
            target.discard();
        }
    }

    /// Emits one engine narration frame pinned to this step; the renderer shows it only under `-v`.
    pub(in crate::world) fn engine_note(&mut self, text: &str) {
        let _ = writeln!(self.mirror, "{text}");
        self.log.note(text);
    }

    /// Emits one readiness note pinned to this step; never a child gutter line or capture line.
    pub(in crate::world) fn progress(&mut self, message: &str) {
        self.engine_note(message);
    }

    /// Flushes the supervisor diagnostic mirror.
    pub(in crate::world) fn flush(&mut self) {
        let _ = self.mirror.flush();
    }

    /// Appends one stream-tagged child line to the stderr mirror and this step's run capture.
    ///
    /// The bytes are stored exactly as the child wrote them (ANSI included): a later colour
    /// terminal can replay them, while a pipe/`NO_COLOR`/`--json` viewer strips at the edge.
    pub(in crate::world) fn append_stream(&mut self, stream: Stream, line: &str) {
        let text = line.trim_end_matches('\r');
        if text.trim().is_empty() {
            return;
        }
        let _ = writeln!(self.mirror, "{text}");
        self.log.append_stream(stream, text);
    }
}
