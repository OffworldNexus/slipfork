//! Instruction-order release: one settled row per index, in index order, image end row last.

use serde_json::{Value, json};

use super::super::super::Action;

#[allow(clippy::too_many_arguments)]
pub(super) fn frame(
    image: &str,
    index: u64,
    total: u64,
    instruction: &str,
    args: &str,
    state: &str,
    elapsed_ms: u64,
    instruction_width: u64,
) -> Value {
    frame_with(
        image,
        index,
        total,
        instruction,
        args,
        state,
        elapsed_ms,
        instruction_width,
        false,
    )
}

/// The frame a pre-build external pull really sends: identical to a planned instruction's frame
/// plus `provisional: true`.
#[allow(clippy::too_many_arguments)]
pub(super) fn provisional_frame(
    image: &str,
    index: u64,
    total: u64,
    instruction: &str,
    args: &str,
    state: &str,
    elapsed_ms: u64,
    instruction_width: u64,
) -> Value {
    frame_with(
        image,
        index,
        total,
        instruction,
        args,
        state,
        elapsed_ms,
        instruction_width,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn frame_with(
    image: &str,
    index: u64,
    total: u64,
    instruction: &str,
    args: &str,
    state: &str,
    elapsed_ms: u64,
    instruction_width: u64,
    provisional: bool,
) -> Value {
    let mut frame = json!({
        "event": "step",
        "image": image,
        "index": index,
        "total": total,
        "instruction": instruction,
        "args": args,
        "state": state,
        "elapsed_ms": elapsed_ms,
        "instruction_width": instruction_width,
    });
    if provisional {
        frame["provisional"] = json!(true);
    }
    frame
}

pub(super) fn finished_frame() -> Value {
    finished_frame_steps(2)
}

pub(super) fn finished_frame_steps(executed: usize) -> Value {
    json!({
        "event": "finished",
        "kind": "image",
        "name": "api",
        "state": "built",
        "executed": executed,
        "duration_ms": 2500,
    })
}

pub(super) fn settled_rows(actions: &[Action]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Settle(row) => Some(row.clone()),
            _ => None,
        })
        .collect()
}

pub(super) fn printed_rows(actions: &[Action]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Print(row) => Some(row.clone()),
            _ => None,
        })
        .collect()
}

pub(super) fn end_row(actions: &[Action]) -> Option<&str> {
    actions.iter().find_map(|action| match action {
        Action::Print(row) => Some(row.as_str()),
        _ => None,
    })
}
