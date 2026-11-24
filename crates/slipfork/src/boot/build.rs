//! The live boot narrative: concurrent phase rows, streamed output, collapse, and the end row.
//!
//! The daemon's `step`, `log`, and `note` frames are the whole story — every instruction is
//! announced before it runs, several steps may run at once inside the single [`LiveSteps`]
//! registry, and every step settles independently as one row. [`super::BootTui`] routes each
//! [`Action`] to the anchored region (a colour TTY) or straight to stdout/stderr (a pipe /
//! `NO_COLOR`), so both shapes tell the same story and `--json` never reaches here at all.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::ui::Palette;

use super::failure;

pub(super) use super::layout::{Columns, gutter_line, image_done_row, wrap_gutter};
pub(super) use super::phase::{LiveKey, LiveSteps, Phase};

/// One routing decision from the narrative to the live driver.
#[derive(Debug)]
pub(super) enum Action {
    /// A step started running in [`LiveSteps`]: repaint the region (TTY) or print `→` (append).
    Begin(Phase),
    /// One child line was recorded in [`LiveSteps`]: repaint the region (TTY) or print wrapped.
    Output { stream: String, text: String },
    /// One step settled: repaint the region to release its block rows, then print the settled row.
    Settle(String),
    /// Print this row immediately (the build-end row, `-v` engine note, or unpinned output).
    Print(String),
    /// Narrative scrollback printed on stdout once the scroll region is restored.
    Hold(String),
    /// The verdict envelope (`✗ …`, optional `runtime:`, one `next:`), printed on stderr.
    Diagnose(String),
}

/// One image's running totals, so the build-end row can count steps and sum wall time.
///
/// Finished rows are buffered by instruction index and released strictly from `next` upward, so
/// rows print once, in instruction order; a provisional external-pull row is superseded when the
/// real instruction at that index starts.
#[derive(Debug, Clone)]
struct ImageTally {
    image: String,
    total: usize,
    millis: u64,
    buffer: BTreeMap<usize, String>,
    next: usize,
}

impl ImageTally {
    fn new(image: &str) -> Self {
        Self {
            image: image.to_owned(),
            total: 0,
            millis: 0,
            buffer: BTreeMap::new(),
            next: 1,
        }
    }

    /// Releases every buffered row contiguous from `next` upward; a hole stops the release.
    fn release(&mut self) -> Vec<String> {
        let mut rows = Vec::new();
        while let Some(row) = self.buffer.remove(&self.next) {
            rows.push(row);
            self.next += 1;
        }
        rows
    }

    /// Drains the contiguous rows, any residual rows, and the instruction count and wall time.
    fn settle(&mut self) -> (Vec<String>, usize, u64) {
        let mut rows = self.release();
        rows.extend(std::mem::take(&mut self.buffer).into_values());
        let steps = if self.total > 0 {
            self.total
        } else {
            rows.len()
        };
        (rows, steps, self.millis)
    }
}

/// The one boot narrative: shares [`LiveSteps`] with [`super::pane::Pane`] and tracks image totals.
#[derive(Debug)]
pub(super) struct Narrative {
    steps: Arc<Mutex<LiveSteps>>,
    images: Vec<ImageTally>,
    streamed: Vec<String>,
    columns: Columns,
    verbose: bool,
    /// Append-only: the step the previous printed line belonged to.
    last_printed: Option<LiveKey>,
    /// The most recently started or outputting step.
    active: Option<LiveKey>,
}

impl Narrative {
    pub(super) fn new(verbose: bool) -> Self {
        Self {
            steps: Arc::new(Mutex::new(LiveSteps::new())),
            images: Vec::new(),
            streamed: Vec::new(),
            columns: Columns::default(),
            verbose,
            last_printed: None,
            active: None,
        }
    }

    /// Handle to the single live-step registry, injected into [`super::pane::Pane`].
    pub(super) fn steps(&self) -> Arc<Mutex<LiveSteps>> {
        Arc::clone(&self.steps)
    }

    /// A `started` boot frame for a command-carrying step (service, hook, setup).
    ///
    /// Image builds announce their instructions through `step` frames instead of their container
    /// command, so they never open a phase row here.
    pub(super) fn started(&mut self, event: &Value) -> Vec<Action> {
        let kind = event["kind"].as_str().unwrap_or("step");
        if kind == "image" {
            return Vec::new();
        }
        let Some(command) = event["command"].as_str().filter(|text| !text.is_empty()) else {
            return Vec::new();
        };
        let name = event["name"]
            .as_str()
            .or_else(|| event["label"].as_str())
            .unwrap_or("step");
        let verb = match kind {
            "service" => "start".to_owned(),
            other => other.to_owned(),
        };
        self.columns.observe(&verb, name);
        let key = LiveKey::Boot {
            kind: kind.to_owned(),
            name: name.to_owned(),
        };
        let phase = Phase::new(
            key.clone(),
            counter(event),
            verb,
            name.to_owned(),
            command.to_owned(),
            self.columns,
        );
        if let Ok(mut steps) = self.steps.lock() {
            steps.begin(phase.clone());
        }
        self.active = Some(key.clone());
        self.last_printed = Some(key);
        vec![Action::Begin(phase)]
    }

    /// One `step` frame: the instruction announced before it runs and how it settled.
    pub(super) fn step(&mut self, event: &Value, palette: Palette) -> Vec<Action> {
        let image = event["image"].as_str().unwrap_or("image");
        let index = usize_of(event["index"].as_u64());
        let total = usize_of(event["total"].as_u64());
        let instruction = event["instruction"].as_str().unwrap_or("RUN");
        let args = event["args"].as_str().unwrap_or_default();
        let elapsed = event["elapsed_ms"].as_u64().unwrap_or(0);
        let key = LiveKey::Step {
            image: image.to_owned(),
            index,
        };
        match event["state"].as_str().unwrap_or_default() {
            "started" => {
                let mut actions = {
                    let tally = self.tally(image);
                    tally.total = total;
                    // A real instruction re-announcing a provisional external-pull index wins.
                    tally.buffer.remove(&index);
                    tally
                        .release()
                        .into_iter()
                        .map(Action::Settle)
                        .collect::<Vec<_>>()
                };
                self.columns.observe(instruction, image);
                let phase = Phase::new(
                    key.clone(),
                    (index > 0 && total > 0).then_some((index, total)),
                    instruction.to_owned(),
                    image.to_owned(),
                    args.to_owned(),
                    self.columns,
                );
                if let Ok(mut steps) = self.steps.lock() {
                    steps.begin(phase.clone());
                }
                self.active = Some(key.clone());
                self.last_printed = Some(key);
                actions.push(Action::Begin(phase));
                actions
            }
            "done" => {
                let phase = self
                    .steps
                    .lock()
                    .ok()
                    .and_then(|mut steps| steps.take(&key).map(|step| step.phase))
                    .unwrap_or_else(|| {
                        Phase::new(
                            key.clone(),
                            (index > 0 && total > 0).then_some((index, total)),
                            instruction.to_owned(),
                            image.to_owned(),
                            args.to_owned(),
                            self.columns,
                        )
                    });
                let settled = phase.settled_row(elapsed, palette);
                {
                    let tally = self.tally(image);
                    tally.total = tally.total.max(total);
                    tally.buffer.insert(index, settled);
                    tally.millis += elapsed;
                }
                self.retire_key(&key);
                // Rows are released later, in index order, at the next start or `finish_node`.
                Vec::new()
            }
            // The boot `failed` frame that follows carries the remedy; the output must survive
            // until then, so a failed step is never collapsed.
            _ => Vec::new(),
        }
    }

    /// One `log` frame: records every line in [`LiveSteps`]; append-only re-anchors on step swaps.
    pub(super) fn log(
        &mut self,
        event: &Value,
        width: usize,
        palette: Palette,
        anchored_pane: bool,
    ) -> Vec<Action> {
        let stream = event["stream"].as_str().unwrap_or("stdout");
        let text = event["line"].as_str().unwrap_or_default();
        let candidates = keys_for(event);
        let target = event["name"].as_str();
        let matched = self.steps.lock().ok().and_then(|mut steps| {
            let key = steps.resolve_key(&candidates, target, self.active.as_ref())?;
            let phase = steps.append_output(&key, stream.to_owned(), text.to_owned())?;
            Some((key, phase))
        });
        let Some((key, phase)) = matched else {
            if candidates.is_empty() && target.is_none() {
                return vec![Action::Print(gutter_line(stream, text, width, palette))];
            }
            return Vec::new();
        };
        let mut actions = Vec::new();
        if !anchored_pane && self.last_printed.as_ref() != Some(&key) {
            actions.push(Action::Print(phase.anchor_row(palette)));
        }
        self.active = Some(key.clone());
        self.last_printed = Some(key);
        actions.push(Action::Output {
            stream: stream.to_owned(),
            text: text.to_owned(),
        });
        actions
    }

    /// One `note` frame: engine narration, shown as a dim row only under `-v`.
    pub(super) fn note(&mut self, event: &Value, palette: Palette) -> Vec<Action> {
        let text = event["text"].as_str().unwrap_or_default();
        if !self.verbose || text.is_empty() {
            return Vec::new();
        }
        vec![Action::Print(format!("  {}", palette.dim(text)))]
    }

    /// A boot `finished`/`up-to-date` frame: settle the command step that opened a phase row.
    pub(super) fn finished(&mut self, event: &Value, palette: Palette) -> Option<Action> {
        let name = event["name"].as_str()?;
        let kind = event["kind"].as_str().unwrap_or("step");
        let millis = event["duration_ms"]
            .as_u64()
            .or_else(|| event["elapsed_ms"].as_u64())?;
        let live = self.steps.lock().ok()?.take_boot(kind, name)?;
        let row = live.phase.settled_row(millis, palette);
        self.retire_key(&live.phase.key);
        Some(Action::Settle(row))
    }

    /// Whether `image` already streamed its instructions, so its `finished` row must not repeat.
    pub(super) fn streamed_image(&self, image: &str) -> bool {
        self.streamed.iter().any(|done| done == image)
    }

    /// Releases an image's remaining step rows in index order, then its build-end row.
    fn settle_image(&mut self, image: &str, palette: Palette) -> Vec<Action> {
        let Some(position) = self.images.iter().position(|tally| tally.image == image) else {
            return Vec::new();
        };
        let mut tally = self.images.remove(position);
        let (rows, steps, millis) = tally.settle();
        let mut actions: Vec<Action> = rows.into_iter().map(Action::Settle).collect();
        actions.push(Action::Print(image_done_row(image, steps, millis, palette)));
        self.streamed.push(image.to_owned());
        actions
    }

    /// One `finished`/`up-to-date` frame: images settle their buffered rows before `take_boot`'s
    /// `target == name` fallback can steal an in-flight image step and drop those rows.
    pub(super) fn finish_node(
        &mut self,
        kind: &str,
        name: &str,
        event: &Value,
        palette: Palette,
    ) -> Option<Vec<Action>> {
        if kind == "image" {
            let actions = self.settle_image(name, palette);
            if !actions.is_empty() {
                return Some(actions);
            }
            if self.streamed_image(name) {
                return Some(Vec::new());
            }
        }
        self.finished(event, palette).map(|row| vec![row])
    }

    /// The failure block: on an anchored TTY, expands the failing phase row and its FULL output
    /// into scrollback; on an append-only stream, lines already printed live are never repeated.
    pub(super) fn failed(
        &mut self,
        event: &Value,
        width: usize,
        palette: Palette,
        anchored_pane: bool,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        let target = event["target"].as_str().or_else(|| event["name"].as_str());
        let live = self
            .steps
            .lock()
            .ok()
            .and_then(|mut steps| steps.take_failure(target, self.active.as_ref()));
        let step_less = live.is_none();
        let verb = live.as_ref().map_or_else(
            || match event["kind"].as_str() {
                // No instruction row existed, so the envelope speaks for the image itself.
                Some("image") => "image".to_owned(),
                _ => phase_verb(event["phase"].as_str().unwrap_or("step")),
            },
            |live| live.phase.verb.clone(),
        );
        if anchored_pane && let Some(live) = &live {
            for row in live.expansion_rows(width, palette) {
                actions.push(Action::Hold(row));
            }
        }
        for row in failure::envelope(event, &verb, self.verbose, palette, step_less) {
            actions.push(Action::Diagnose(row));
        }
        self.active = None;
        self.last_printed = None;
        // No tally may survive a verdict, so no provisional row can print afterwards.
        self.images.clear();
        actions
    }

    fn retire_key(&mut self, key: &LiveKey) {
        if self.active.as_ref() == Some(key) {
            self.active = self.steps.lock().ok().and_then(|steps| steps.last_key());
        }
        if self.last_printed.as_ref() == Some(key) {
            self.last_printed = None;
        }
    }

    fn tally(&mut self, image: &str) -> &mut ImageTally {
        if let Some(position) = self.images.iter().position(|tally| tally.image == image) {
            return &mut self.images[position];
        }
        self.images.push(ImageTally::new(image));
        self.images.last_mut().expect("just pushed")
    }
}

fn keys_for(event: &Value) -> Vec<LiveKey> {
    let mut keys = Vec::new();
    if let (Some(image), Some(index)) = (event["image"].as_str(), event["index"].as_u64()) {
        keys.push(LiveKey::Step {
            image: image.to_owned(),
            index: usize_of(Some(index)),
        });
    }
    if let Some(name) = event["name"].as_str() {
        if let Some(kind) = event["kind"].as_str() {
            keys.push(LiveKey::Boot {
                kind: kind.to_owned(),
                name: name.to_owned(),
            });
        } else if let Some((kind, _)) = event["label"].as_str().and_then(|l| l.split_once(' ')) {
            keys.push(LiveKey::Boot {
                kind: kind.to_owned(),
                name: name.to_owned(),
            });
        }
    }
    keys
}

/// `[step/total]` for a boot step frame, when both are known.
fn counter(event: &Value) -> Option<(usize, usize)> {
    let step = usize_of(event["step"].as_u64());
    let total = usize_of(event["total"].as_u64());
    (step > 0 && total > 0).then_some((step, total))
}

/// The verb a failure envelope prints when no phase row is live: `run` → `RUN`, `copy` → `COPY`.
fn phase_verb(phase: &str) -> String {
    match phase {
        "" => "step".to_owned(),
        other => other.to_ascii_uppercase(),
    }
}

fn usize_of(value: Option<u64>) -> usize {
    value
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
