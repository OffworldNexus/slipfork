//! One step's event log: the frames it publishes plus the run capture it appends to.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use slipfork_world::Stream;

use crate::world::runtime::capture::RunTarget;
use crate::world::runtime::frames::{StepPin, log_frame, note_frame};

use super::sink::Emitter;

#[derive(Clone)]
pub(super) struct EventLog {
    emitter: Arc<Mutex<Emitter>>,
    step: usize,
    total: usize,
    label: String,
    name: String,
    pub(super) run: Option<Arc<Mutex<RunTarget>>>,
}

impl EventLog {
    pub(super) fn for_step(
        emitter: Arc<Mutex<Emitter>>,
        step: usize,
        total: usize,
        label: &str,
        name: &str,
    ) -> Self {
        Self {
            emitter,
            step,
            total,
            label: label.to_owned(),
            name: name.to_owned(),
            run: None,
        }
    }

    pub(super) fn append_stream(&mut self, stream: Stream, text: &str) {
        self.emit(stream, text.trim_end_matches('\r'));
    }

    fn emit(&self, stream: Stream, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        if let Some(run) = &self.run
            && let Ok(mut target) = run.lock()
        {
            target.append_stream(stream, text);
        }
        let pin = StepPin {
            step: self.step,
            total: self.total,
            label: &self.label,
            name: &self.name,
            image: None,
            index: None,
        };
        self.send(log_frame(&pin, stream.as_str(), text));
    }

    /// Flushes one engine narration frame pinned to this step, without touching any capture.
    pub(super) fn note(&self, text: &str) {
        let text = crate::ui::strip(text);
        let text = text.trim_end_matches('\r');
        if text.is_empty() {
            return;
        }
        let pin = StepPin {
            step: self.step,
            total: self.total,
            label: &self.label,
            name: &self.name,
            image: None,
            index: None,
        };
        self.send(note_frame(&pin, text));
    }

    /// Flushes one complete frame through the shared emitter, immediately.
    fn send(&self, frame: Value) {
        if let Ok(mut emitter) = self.emitter.lock() {
            emitter.send(frame);
        }
    }
}
