//! Image end routing: parse one `finished` frame into a typed [`Outcome`], then render its row.
//!
//! This is the only place an image success (`✓`) is constructed; failures and unknown states
//! cannot accidentally fall through to a success row.

use std::num::NonZeroUsize;

use serde_json::Value;

use crate::ui::Palette;
use crate::ui::report::{Counter, Done, Outcome, Row, StepSettlement};

use super::super::render::completed_row;
use super::{Action, LiveKey, Narrative};

/// The typed outcome of one image `finished` frame.
///
/// A `built` frame without an executed count renders the neutral `finished` slot rather than
/// inventing a step count from buffered rows.
fn image_outcome(
    event: &Value,
    executed: Option<usize>,
    instruction_total: usize,
    at: Option<(usize, usize)>,
) -> Outcome {
    let at =
        at.and_then(|(index, total)| (index > 0 && total > 0).then_some(Counter { index, total }));
    // The daemon's serialized settlement is the authority for cached/skipped/failed.
    if let Some(settlement) = event
        .get("settlement")
        .and_then(|value| serde_json::from_value::<StepSettlement>(value.clone()).ok())
    {
        match settlement {
            StepSettlement::Cached { .. } | StepSettlement::Skipped { .. } => {
                return Outcome::Cached;
            }
            StepSettlement::Failed(facts) => return Outcome::Failed { at: facts.at },
            StepSettlement::Completed { done, .. } if event["state"].is_null() => {
                return Outcome::Ok(done);
            }
            StepSettlement::Completed { .. } => {}
        }
    }
    if event["event"].as_str() == Some("up-to-date") {
        return Outcome::Cached;
    }
    match event["state"].as_str().unwrap_or_default() {
        "built" => match executed {
            Some(0) => Outcome::Cached,
            Some(executed) => {
                let built = NonZeroUsize::new(executed).expect("a non-zero executed count");
                match NonZeroUsize::new(instruction_total.saturating_sub(executed)) {
                    Some(cached) if instruction_total > executed => {
                        Outcome::Ok(Done::Partial { cached, built })
                    }
                    _ => Outcome::Ok(Done::Built { steps: built }),
                }
            }
            None => Outcome::Ok(Done::Finished),
        },
        "failed" => Outcome::Failed { at },
        state => match Outcome::from_state(state) {
            Some(Outcome::Ok(Done::Declined)) => Outcome::Cached,
            Some(Outcome::Ok(Done::Settled)) | None => Outcome::Ok(Done::Finished),
            Some(outcome) => outcome,
        },
    }
}

impl Narrative {
    /// Settles an image's outer slot and always returns its typed end row.
    fn settle_image(&mut self, image: &str, event: &Value, palette: Palette) -> Vec<Action> {
        // An image settles exactly once: mark it before any tally work so a duplicate
        // `finished`/`up-to-date` frame (in either order) returns zero rows.
        if self.streamed_image(image) {
            return Vec::new();
        }
        self.streamed.push(image.to_owned());
        // The image's step keys are spent: a later re-announcement of the same image re-earns its
        // one header instead of silently inheriting a stale one.
        self.headed.retain(|key| match key {
            LiveKey::Step { image: keyed, .. } => keyed != image,
            LiveKey::Boot { kind, name } => !(kind == "image" && name == image),
        });
        let position = self.images.iter().position(|tally| tally.image == image);
        let (rows, outer, at, tally_millis, instruction_total) = match position {
            Some(position) => {
                let mut tally = self.images.remove(position);
                let outer = tally.outer;
                let at = tally.failure_at();
                // The image's instruction total comes from its own `step` frames; the frame's
                // `total` is the outer PLAN step count and must never be read as instructions.
                let total = tally.total;
                if tally.is_unused() {
                    (Vec::new(), outer, at, 0, total)
                } else {
                    let (rows, millis) = tally.drain();
                    (rows, outer, at, millis, total)
                }
            }
            None => (Vec::new(), (0, 0), None, 0, 0),
        };
        self.observe_outer("image", image);
        let millis = event["duration_ms"]
            .as_u64()
            .or_else(|| event["elapsed_ms"].as_u64())
            .unwrap_or(tally_millis);
        let executed = event["executed"]
            .as_u64()
            .and_then(|count| usize::try_from(count).ok());
        let mut actions: Vec<Action> = rows.into_iter().map(Action::Settle).collect();
        let outcome = image_outcome(event, executed, instruction_total, at);
        let row = Row::settled(
            (outer.0 > 0 && outer.1 > 0).then_some(Counter {
                index: outer.0,
                total: outer.1,
            }),
            "image",
            image,
            outcome,
            Some(millis),
        );
        actions.push(Action::Print(completed_row(&row, &self.outer, palette)));
        actions
    }

    /// One `finished`/`up-to-date` frame: an image always settles through [`image_outcome`], so no
    /// generic fallthrough can mint a `✓` from a missing or unknown state.
    pub(in crate::boot) fn finish_node(
        &mut self,
        kind: &str,
        name: &str,
        event: &Value,
        palette: Palette,
    ) -> Option<Vec<Action>> {
        if kind == "image" {
            return Some(self.settle_image(name, event, palette));
        }
        self.finished(event, palette).map(|row| vec![row])
    }
}
