//! The live boot narrative: concurrent phase rows, streamed output, collapse, and the end row.
//!
//! The daemon's `step`, `log`, and `note` frames are the whole story — every instruction is
//! announced before it runs, several steps may run at once inside the single [`LiveSteps`]
//! registry, and every step settles independently as one row. [`super::BootTui`] routes each
//! [`Action`] to the status line (a colour TTY) or straight to stdout/stderr (a pipe /
//! `NO_COLOR`), so both shapes tell the same story and `--json` never reaches here at all.

mod image;
mod tally;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use slipfork_world::snapshot::TailPhase;

use crate::ui::Palette;
use crate::ui::elapsed_text;

use self::tally::ImageTally;
use super::failure;
use super::phase::{tail_label, tail_phase};
use super::render::{OuterWidths, outer_row, outer_verb};

use super::layout::clamp_visible;
pub(super) use super::layout::{Columns, gutter_line, wrap_gutter};
#[cfg(test)]
pub(super) use super::layout::{
    ImageEnd, ImageState, image_done_row, image_end_row, image_failed_row,
};
pub(super) use super::phase::{LiveKey, LiveSteps, Phase};

/// One routing decision from the narrative to the live driver.
#[derive(Debug)]
pub(super) enum Action {
    /// One child line was recorded in [`LiveSteps`]: print it wrapped at
    /// the owning row's gutter indent (`2` under an outer step, `4` under an inner build step).
    Output {
        stream: String,
        text: String,
        indent: usize,
    },
    /// One step settled: print its buffered settled rows in index order.
    Settle(String),
    /// Print this row immediately (the once-only stream header, build-end row, `-v` note).
    Print(String),
    /// The verdict envelope (`✗ …`, optional `runtime:`, one `next:`), printed on stderr.
    Diagnose(String),
}

/// The one boot narrative: shares [`LiveSteps`] with [`super::status::StatusLine`] and tracks
/// image totals.
#[derive(Debug)]
pub(super) struct Narrative {
    steps: Arc<Mutex<LiveSteps>>,
    images: Vec<ImageTally>,
    streamed: Vec<String>,
    outer: OuterWidths,
    verbose: bool,
    /// Append-only: the step the previous printed line belonged to.
    last_printed: Option<LiveKey>,
    /// Every key whose one `→` stream header was already emitted.
    headed: BTreeSet<LiveKey>,
    /// The most recently started or outputting step.
    active: Option<LiveKey>,
}

impl Narrative {
    pub(super) fn new(verbose: bool) -> Self {
        Self {
            steps: Arc::new(Mutex::new(LiveSteps::new())),
            images: Vec::new(),
            streamed: Vec::new(),
            outer: OuterWidths::default(),
            verbose,
            last_printed: None,
            headed: BTreeSet::new(),
            active: None,
        }
    }

    /// Handle to the single live-step registry, injected into [`super::status::StatusLine`].
    pub(super) fn steps(&self) -> Arc<Mutex<LiveSteps>> {
        Arc::clone(&self.steps)
    }

    /// Pins the plan-derived outer `kind` and `target` column widths.
    pub(super) fn set_outer(&mut self, widths: OuterWidths) {
        self.outer.kind = self.outer.kind.max(widths.kind);
        self.outer.target = self.outer.target.max(widths.target);
    }

    fn observe_outer(&mut self, verb: &str, target: &str) {
        self.outer.kind = self.outer.kind.max(verb.chars().count());
        self.outer.target = self.outer.target.max(target.chars().count());
    }

    /// A `started` boot frame for an outer step (image slot registration, service, hook, setup).
    ///
    /// Image builds announce their instructions through `step` frames, so a cached image prints
    /// zero inner rows while a fresh build opens `→ [i/n] image <id>` on its first `step` frame.
    pub(super) fn started(
        &mut self,
        event: &Value,
        position: Option<(usize, usize)>,
    ) -> Vec<Action> {
        let kind = event["kind"].as_str().unwrap_or("step");
        let name = event["name"]
            .as_str()
            .or_else(|| event["label"].as_str())
            .unwrap_or("step");
        let count = position.or_else(|| counter(event));
        if kind == "image" {
            self.observe_outer("image", name);
            if let Some(count) = count {
                self.tally(name).outer = count;
            }
            return Vec::new();
        }
        let command = match event["command"].as_str().filter(|text| !text.is_empty()) {
            Some(command) => command.to_owned(),
            None if kind == "service" => String::new(),
            None => return Vec::new(),
        };
        let verb = outer_verb(kind).to_owned();
        self.observe_outer(&verb, name);
        let key = LiveKey::Boot {
            kind: kind.to_owned(),
            name: name.to_owned(),
        };
        let phase = Phase::new(
            key.clone(),
            count,
            verb,
            name.to_owned(),
            command,
            Columns::from_outer(self.outer),
        );
        if let Ok(mut steps) = self.steps.lock() {
            steps.begin(phase);
        }
        self.active = Some(key);
        // No row prints here: a step that streams nothing earns only its later verdict, and a
        // streamed step earns one header at its first output line.
        Vec::new()
    }

    /// One `step` frame: the instruction announced before it runs and how it settled.
    pub(super) fn step(&mut self, event: &Value, palette: Palette) -> Vec<Action> {
        let image = event["image"].as_str().unwrap_or("image");
        let index = usize_of(event["index"].as_u64());
        let total = usize_of(event["total"].as_u64());
        let instruction = event["instruction"].as_str().unwrap_or("RUN");
        let args = event["args"].as_str().unwrap_or_default();
        let elapsed = event["elapsed_ms"].as_u64().unwrap_or(0);
        // The plan's widest instruction, floored by the lowercase keyword this row prints.
        let width = usize_of(event["instruction_width"].as_u64())
            .max(instruction.to_lowercase().chars().count());
        let key = LiveKey::Step {
            image: image.to_owned(),
            index,
        };
        match event["state"].as_str().unwrap_or_default() {
            "started" => {
                self.observe_outer("image", image);
                let verb_width = self.tally(image).begin_step(index, total, width);
                let mut actions = Vec::new();
                // A cold image opens `→ [i/n] image … running` at its first real instruction,
                // never at `started` frame time (a cached image must print only its verdict).
                if let Some(row) = self.image_header(image, palette) {
                    actions.push(Action::Print(row));
                }
                actions.extend(self.tally(image).release().into_iter().map(Action::Settle));
                let phase = Phase::new(
                    key.clone(),
                    (index > 0 && total > 0).then_some((index, total)),
                    instruction.to_owned(),
                    image.to_owned(),
                    args.to_owned(),
                    Columns {
                        verb: verb_width,
                        target: 0,
                    },
                );
                if let Ok(mut steps) = self.steps.lock() {
                    steps.begin(phase);
                }
                self.active = Some(key);
                actions
            }
            "done" => {
                let verb_width = {
                    let tally = self.tally(image);
                    tally.verb_width = tally.verb_width.max(width);
                    tally.verb_width
                };
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
                            Columns {
                                verb: verb_width,
                                target: 0,
                            },
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
                // Release in plan index order right away: a completed step never waits for the
                // next `started` frame, so a parallel late index cannot leak ahead of its peers.
                let rows = self.tally(image).release();
                rows.into_iter().map(Action::Settle).collect()
            }
            // A tail frame updates the existing live row in place: it never restarts the clock and
            // never prints a row of its own. The status line repaints on its next tick.
            "tail" => {
                let Some(phase) = event["phase"].as_str().and_then(tail_phase) else {
                    return Vec::new();
                };
                let (open, changed) = {
                    let Ok(mut steps) = self.steps.lock() else {
                        return Vec::new();
                    };
                    let changed = steps.phase(&key).and_then(|row| row.tail) != Some(phase);
                    if changed {
                        steps.set_tail(&key, phase);
                    }
                    (steps.phase(&key).cloned(), changed)
                };
                // The status line repaints the row in place; only a pipe / `NO_COLOR` / CI log
                // gets a line, and only for a real, non-`Executing` phase transition.
                if !changed || palette.color() || phase == TailPhase::Executing {
                    return Vec::new();
                }
                let Some(row) = open else {
                    return Vec::new();
                };
                let millis = event["elapsed_ms"]
                    .as_u64()
                    .filter(|millis| *millis > 0)
                    .unwrap_or_else(|| row.elapsed_millis());
                vec![Action::Print(tail_row(&row, millis, palette))]
            }
            // The boot `failed` frame that follows carries the remedy; the output must survive
            // until then, so a failed step is never collapsed.
            _ => Vec::new(),
        }
    }

    /// One `log` frame: records every line in [`LiveSteps`]; the first line of a streamed step
    /// prints its one `→` header, later lines keep its gutter.
    pub(super) fn log(&mut self, event: &Value, width: usize, palette: Palette) -> Vec<Action> {
        let stream = event["stream"].as_str().unwrap_or("stdout");
        let stored = event["line"].as_str().unwrap_or_default();
        // The daemon keeps the child's raw bytes; a pipe or `NO_COLOR` viewer drops the escapes,
        // a colour terminal keeps SGR colour but never a cursor/alt-screen control.
        let text = if palette.color() {
            crate::ui::colour_only(stored)
        } else {
            crate::ui::strip(stored)
        };
        let candidates = keys_for(event);
        let target = event["name"].as_str();
        let matched = self.steps.lock().ok().and_then(|mut steps| {
            let key = steps.resolve_key(&candidates, target, self.active.as_ref())?;
            let phase = steps.touch(&key)?;
            Some((key, phase))
        });
        let Some((key, phase)) = matched else {
            if candidates.is_empty() && target.is_none() {
                return vec![Action::Print(gutter_line(stream, &text, width, palette, 2))];
            }
            return Vec::new();
        };
        let mut actions = Vec::new();
        if phase.is_inner()
            && let Some(row) = self.image_header(&phase.target, palette)
        {
            actions.push(Action::Print(row));
        }
        // One header per key, ever: interleaved output from two live steps never repeats a header.
        if self.headed.insert(key.clone()) {
            actions.push(Action::Print(clamp_visible(
                &phase.anchor_row(palette),
                width,
            )));
        }
        self.active = Some(key.clone());
        self.last_printed = Some(key);
        actions.push(Action::Output {
            stream: stream.to_owned(),
            text: text.into_owned(),
            indent: phase.gutter_indent(),
        });
        actions
    }

    /// One `note` frame: engine narration, shown as a dim row only under `-v`.
    pub(super) fn note(&mut self, event: &Value, palette: Palette) -> Vec<Action> {
        let text = event["text"].as_str().unwrap_or_default();
        if !self.verbose || text.is_empty() {
            return Vec::new();
        }
        vec![Action::Print(format!("  {}", palette.meta(text)))]
    }

    /// A boot `finished`/`up-to-date` frame: settle the command step that opened a phase row.
    pub(super) fn finished(&mut self, event: &Value, palette: Palette) -> Option<Action> {
        let name = event["name"].as_str()?;
        let kind = event["kind"].as_str().unwrap_or("step");
        let millis = event["duration_ms"]
            .as_u64()
            .or_else(|| event["elapsed_ms"].as_u64())?;
        let mut live = self.steps.lock().ok()?.take_boot(kind, name)?;
        live.phase.columns = Columns::from_outer(self.outer);
        let up_to_date = event["event"].as_str() == Some("up-to-date")
            || matches!(event["state"].as_str(), Some("up to date" | "up-to-date"));
        let state = if up_to_date {
            "up to date"
        } else {
            event["state"].as_str().unwrap_or("done")
        }
        .to_lowercase();
        // The command column always holds the step's own command, registered from its
        // plan/`started` frame. A `finished` frame's `detail` is engine narration and never a
        // command; prep rows get their detail from `Node::from_event` instead.
        let row = live
            .phase
            .settled_outer(millis, &state, None, up_to_date, palette);
        self.retire_key(&live.phase.key);
        Some(Action::Settle(row))
    }

    /// Whether `image` already streamed its instructions, so its `finished` row must not repeat.
    pub(super) fn streamed_image(&self, image: &str) -> bool {
        self.streamed.iter().any(|done| done == image)
    }

    /// The failure block: the failing step was already streamed line by line, so only its `✗` row
    /// and the remedy envelope are printed here.
    pub(super) fn failed(&mut self, event: &Value, palette: Palette) -> Vec<Action> {
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
        if let Some(live) = &live {
            actions.push(Action::Print(live.phase.failed_row(palette)));
        }
        for row in failure::envelope(event, &verb, self.verbose, palette, step_less) {
            actions.push(Action::Diagnose(row));
        }
        self.active = None;
        self.last_printed = None;
        // No tally may survive a verdict, so no provisional row can print afterwards, and a
        // replacement run re-earns its own single headers.
        self.images.clear();
        self.headed.clear();
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

    /// The image's one `→ [i/n] image <id> running` opener, emitted at its first inner step.
    ///
    /// A cached image has no inner steps, so it never earns this row; the verdict alone stays.
    fn image_header(&mut self, image: &str, palette: Palette) -> Option<String> {
        let sentinel = LiveKey::Boot {
            kind: "image".to_owned(),
            name: image.to_owned(),
        };
        if self.headed.contains(&sentinel) {
            return None;
        }
        let tally = self.images.iter().find(|tally| tally.image == image)?;
        if tally.last_index == 0 || tally.outer.0 == 0 || tally.outer.1 == 0 {
            return None;
        }
        let (index, total) = tally.outer;
        let row = outer_row(
            "→",
            Some((index, total)),
            "image",
            image,
            "running",
            None,
            None,
            &self.outer,
            palette,
        );
        self.headed.insert(sentinel);
        Some(row)
    }
}

/// One append-only tail line: `    · 4/6 run  3.1s · sealing`.
fn tail_row(phase: &Phase, millis: u64, palette: Palette) -> String {
    let mut row = String::from("    ");
    row.push_str(&palette.meta("·"));
    row.push(' ');
    if let Some((index, total)) = phase.count {
        row.push_str(&palette.meta(&format!("{index}/{total}")));
        row.push(' ');
    }
    row.push_str(&palette.substep(&phase.verb.to_lowercase()));
    row.push_str("  ");
    row.push_str(&palette.meta(&elapsed_text(millis)));
    if let Some(tail) = phase.tail {
        row.push_str(&palette.meta(&format!(" · {}", tail_label(tail))));
    }
    row
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
pub(super) fn counter(event: &Value) -> Option<(usize, usize)> {
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
