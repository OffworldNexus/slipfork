//! Typed row projections for a finished boot step and the append-only tail line.
//!
//! A live phase keeps its own counter, verb, target, and command; a prep/join frame with no live
//! phase renders its `detail` as the command cell. Both project into [`Row`], so the renderer
//! owns every byte and this module owns none of the palette.

use serde_json::Value;

use crate::ui::elapsed_text;
use crate::ui::report::Palette;
use crate::ui::report::{Counter, Done, Outcome, Row};

use super::super::phase::{Phase, tail_label};
use super::Narrative;

impl Narrative {
    /// The typed row for a finished step that opened a live phase: its own counter, verb, target,
    /// and registered command, never the frame's human `detail`.
    pub(in crate::boot) fn live_row(
        &self,
        phase: &Phase,
        up_to_date: bool,
        state: &str,
        millis: u64,
    ) -> Row {
        let outcome = if up_to_date {
            Outcome::from_state("up to date").unwrap_or(Outcome::Ok(Done::Settled))
        } else {
            Outcome::from_state(state).unwrap_or(Outcome::Ok(Done::Settled))
        };
        // A cache hit ran no command; a live service keeps its run command in the command column.
        let command = if up_to_date {
            None
        } else {
            Some(phase.shown_args().to_owned())
        };
        Row::settled(
            phase.count.map(|(index, total)| Counter { index, total }),
            &phase.verb,
            &phase.target,
            outcome,
            Some(millis),
        )
        .with_command(command)
    }

    /// The fallback row for a `finished` frame that never opened a live phase (a prep/join step):
    /// its `detail` is the command cell only when the frame carried no lifecycle state.
    pub(in crate::boot) fn settled_row(
        &self,
        event: &Value,
        kind: &str,
        name: &str,
        millis: u64,
        (index, total): (usize, usize),
    ) -> Row {
        let raw = event["state"].as_str().unwrap_or_default();
        let state = raw.trim();
        let command = event["command"].as_str().or_else(|| {
            if state.is_empty() {
                event["detail"].as_str()
            } else {
                None
            }
        });
        let up_to_date = event["event"].as_str() == Some("up-to-date")
            || Outcome::from_state(state) == Some(Outcome::Ok(Done::Declined));
        let outcome = if up_to_date {
            Outcome::from_state("up to date").unwrap_or(Outcome::Ok(Done::Settled))
        } else {
            Outcome::from_state(state).unwrap_or(Outcome::Ok(Done::Settled))
        };
        Row::settled(
            (index > 0 && total > 0).then_some(Counter { index, total }),
            kind,
            name,
            outcome,
            Some(millis),
        )
        .with_command(command.map(str::to_owned))
    }
}

/// One append-only tail line: `    · 4/6 run  3.1s · sealing`.
pub(super) fn tail_row(phase: &Phase, millis: u64, palette: Palette) -> String {
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
