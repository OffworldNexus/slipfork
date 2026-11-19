//! LSP hover envelope around the canonical [`slipfork_forkfile::hover`] text.

use serde_json::{Value, json};

use crate::utf16;

/// The `result` value for a `textDocument/hover` request, or `Value::Null`.
pub fn response(text: &str, line: u32, character: u32) -> Value {
    let Some(offset) = utf16::position_to_offset(text, line, character) else {
        return Value::Null;
    };
    match slipfork_forkfile::hover(text, offset) {
        Some(markdown) => json!({
            "contents": { "kind": "markdown", "value": markdown }
        }),
        None => Value::Null,
    }
}
