//! Step run captures and ANSI scrubbing: opened before a step runs, finished when it settles.

use std::path::Path;

use slipfork_world::run_log::RunCapture;
use slipfork_world::{BootFailure, LogStore};

/// One step's run capture: opened before the step runs, finished with its final state.
pub(crate) struct RunTarget {
    capture: Option<RunCapture>,
}

impl RunTarget {
    pub(crate) fn open(
        store: &LogStore,
        project: &Path,
        kind: &str,
        name: &str,
        command: &str,
    ) -> Result<Self, BootFailure> {
        RunCapture::begin(store, project, kind, name, command)
            .map(|capture| Self {
                capture: Some(capture),
            })
            .map_err(|error| {
                BootFailure::new(format!("cannot open a run capture for `{name}`: {error}"))
            })
    }

    /// The minted id, so the record owner can annotate the capture while it is still running.
    pub(crate) fn run_id(&self) -> Option<&str> {
        self.capture.as_ref().map(RunCapture::run_id)
    }

    pub(crate) fn append_stream(&mut self, stream: slipfork_world::Stream, text: &str) {
        if let Some(capture) = &mut self.capture {
            let _ = capture.append_stream(stream, text);
        }
    }

    pub(crate) fn finish(&mut self, state: &str, duration_ms: u64) -> Option<String> {
        let mut capture = self.capture.take()?;
        let run_id = capture.run_id().to_owned();
        let _ = capture.finish(state, duration_ms);
        Some(run_id)
    }

    pub(crate) fn discard(&mut self) {
        if let Some(capture) = self.capture.take() {
            capture.discard();
        }
    }
}

/// Strips CSI escape sequences from a child's raw line, so a `log` frame never carries ANSI.
pub(crate) fn strip_ansi(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\u{1b}') {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for ch in chars.by_ref() {
                    if ('@'..='~').contains(&ch) {
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    std::borrow::Cow::Owned(out)
}
