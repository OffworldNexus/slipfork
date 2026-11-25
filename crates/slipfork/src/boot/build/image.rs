//! Image end routing: parse one `finished` frame into a typed state, then render its end row.
//!
//! This is the only place an image success (`✓`) is constructed; failures and unknown states
//! cannot accidentally fall through to a success row.

use serde_json::Value;

use crate::ui::Palette;

use super::super::layout::{ImageEnd, ImageState, image_end_row};
use super::{Action, LiveKey, Narrative};

/// Parses a finished frame once; a missing or unexpected state is explicitly [`ImageState::Unknown`].
fn state_from_frame(event: &Value) -> ImageState {
    if event["event"].as_str() == Some("up-to-date") {
        return ImageState::Cached;
    }
    match event["state"].as_str() {
        Some("failed") => ImageState::Failed,
        Some("cached" | "up to date" | "up-to-date") => ImageState::Cached,
        Some("built") => ImageState::Built,
        Some("pulled") => ImageState::Pulled,
        _ => ImageState::Unknown,
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
        let state = state_from_frame(event);
        let position = self.images.iter().position(|tally| tally.image == image);
        let mut actions = Vec::new();
        let (outer, at, tally_millis, tally_total) = match position {
            Some(position) => {
                let mut tally = self.images.remove(position);
                let outer = tally.outer;
                let at = tally.failure_at();
                // The image's instruction total comes from its own `step` frames; the frame's
                // `total` is the outer PLAN step count and must never be read as instructions.
                let total = tally.total;
                if tally.is_unused() {
                    (outer, at, 0, total)
                } else {
                    let (rows, _, millis) = tally.settle();
                    actions.extend(rows.into_iter().map(Action::Settle));
                    (outer, at, millis, total)
                }
            }
            None => ((0, 0), None, 0, 0),
        };
        self.observe_outer("image", image);
        let millis = event["duration_ms"]
            .as_u64()
            .or_else(|| event["elapsed_ms"].as_u64())
            .unwrap_or(tally_millis);
        let executed = event["executed"]
            .as_u64()
            .and_then(|count| usize::try_from(count).ok());
        let total = (tally_total > 0).then_some(tally_total);
        actions.push(Action::Print(image_end_row(
            outer.0,
            outer.1,
            image,
            ImageEnd::new(state, executed, total, millis, at),
            &self.outer,
            palette,
        )));
        actions
    }

    /// One `finished`/`up-to-date` frame: an image always settles through [`ImageState`], so no
    /// generic `completed_row` fallthrough can mint a `✓` from a missing or unknown state.
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
