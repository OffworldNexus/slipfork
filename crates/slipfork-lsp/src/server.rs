//! The synchronous JSON-RPC loop: initialize, full-sync documents, diagnostics, formatting, hover,
//! and a clean `shutdown` + `exit`. One misbehaving request can never take the process down.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

use serde_json::{Value, json};
use slipfork_forkfile::{fmt as format_forkfile, parse_forkfile};

use crate::{framing, hover, utf16};

/// Run the language server over the supplied reader and writer, returning the process exit code.
///
/// `0` after a clean `shutdown` + `exit` (or end of input); `1` on a broken frame or an `exit`
/// without `shutdown`.
pub fn run_on(reader: impl BufRead, writer: impl Write) -> i32 {
    Server::new(reader, writer).serve()
}

struct Server<R, W> {
    reader: R,
    writer: W,
    documents: BTreeMap<String, String>,
    shutdown_requested: bool,
    exit_requested: bool,
    transport_failed: bool,
}

impl<R: BufRead, W: Write> Server<R, W> {
    fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            documents: BTreeMap::new(),
            shutdown_requested: false,
            exit_requested: false,
            transport_failed: false,
        }
    }

    fn serve(&mut self) -> i32 {
        loop {
            let body = match framing::read_message(&mut self.reader) {
                Ok(Some(body)) => body,
                Ok(None) => break,
                Err(_) => {
                    self.transport_failed = true;
                    break;
                }
            };
            if catch_unwind(AssertUnwindSafe(|| self.handle_body(&body))).is_err()
                && !self.transport_failed
            {
                self.respond_error(
                    Value::Null,
                    -32603,
                    "internal error (next: retry the request; the server is still running)",
                );
            }
            if self.exit_requested || self.transport_failed {
                break;
            }
        }
        if (self.exit_requested && !self.shutdown_requested) || self.transport_failed {
            1
        } else {
            0
        }
    }

    fn handle_body(&mut self, body: &[u8]) {
        let message: Value = match serde_json::from_slice(body) {
            Ok(message) => message,
            Err(_) => {
                self.respond_error(Value::Null, -32700, "parse error");
                return;
            }
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let id = message.get("id").cloned().filter(|value| !value.is_null());
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match method.as_str() {
            "initialize" => {
                if let Some(id) = id {
                    self.respond(&id, initialize_result());
                }
            }
            "initialized" | "$/setTrace" | "workspace/didChangeConfiguration" => {}
            "shutdown" => {
                self.shutdown_requested = true;
                if let Some(id) = id {
                    self.respond(&id, Value::Null);
                }
            }
            "exit" => self.exit_requested = true,
            "textDocument/didOpen" => {
                if let (Some(uri), Some(text)) = (
                    pointer_str(&params, "/textDocument/uri"),
                    pointer_str(&params, "/textDocument/text"),
                ) {
                    self.documents.insert(uri.clone(), text.clone());
                    self.publish(&uri, &text);
                }
            }
            "textDocument/didChange" => {
                if let Some(uri) = pointer_str(&params, "/textDocument/uri")
                    && let Some(text) = last_change_text(&params)
                {
                    self.documents.insert(uri.clone(), text.to_owned());
                    self.publish(&uri, text);
                }
            }
            "textDocument/didClose" => {
                if let Some(uri) = pointer_str(&params, "/textDocument/uri") {
                    self.documents.remove(&uri);
                    self.send(&json!({
                        "jsonrpc": "2.0",
                        "method": "textDocument/publishDiagnostics",
                        "params": { "uri": uri, "diagnostics": [] }
                    }));
                }
            }
            "textDocument/formatting" => {
                if let Some(id) = id
                    && let Some(uri) = pointer_str(&params, "/textDocument/uri")
                {
                    match self.documents.get(&uri).cloned() {
                        Some(text) => self.format(id, &text),
                        None => self.respond(&id, Value::Null),
                    }
                }
            }
            "textDocument/hover" => {
                if let Some(id) = id
                    && let Some(uri) = pointer_str(&params, "/textDocument/uri")
                    && let (Some(line), Some(character)) = (
                        pointer_u32(&params, "/position/line"),
                        pointer_u32(&params, "/position/character"),
                    )
                {
                    match self.documents.get(&uri) {
                        Some(text) => self.respond(&id, hover::response(text, line, character)),
                        None => self.respond(&id, Value::Null),
                    }
                }
            }
            #[cfg(test)]
            "test/panic" => panic!("intentional panic for recovery coverage"),
            _ => {
                if let Some(id) = id {
                    self.respond_error(id, -32601, "method not found");
                }
            }
        }
    }

    fn format(&mut self, id: Value, text: &str) {
        let Ok(formatted) = format_forkfile(text) else {
            self.respond(&id, Value::Null);
            return;
        };
        if formatted == text {
            self.respond(&id, Value::Array(Vec::new()));
            return;
        }
        let ((start_line, start_character), (end_line, end_character)) =
            utf16::document_range(text);
        self.respond(
            &id,
            json!([{
                "range": {
                    "start": { "line": start_line, "character": start_character },
                    "end": { "line": end_line, "character": end_character }
                },
                "newText": formatted
            }]),
        );
    }

    fn publish(&mut self, uri: &str, text: &str) {
        let diagnostics: Vec<Value> = match parse_forkfile(text) {
            Ok(_) => Vec::new(),
            Err(diagnostics) => diagnostics
                .iter()
                .map(|diagnostic| {
                    let ((start_line, start_character), (end_line, end_character)) =
                        utf16::span_range(text, &diagnostic.span);
                    json!({
                        "range": {
                            "start": { "line": start_line, "character": start_character },
                            "end": { "line": end_line, "character": end_character }
                        },
                        "severity": 1,
                        "source": "forkfile",
                        "message": match &diagnostic.hint {
                            Some(hint) => format!("{} (next: {hint})", diagnostic.message),
                            None => diagnostic.message.clone(),
                        }
                    })
                })
                .collect(),
        };
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": diagnostics }
        }));
    }

    fn respond(&mut self, id: &Value, result: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn respond_error(&mut self, id: Value, code: i64, message: &str) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        }));
    }

    fn send(&mut self, message: &Value) {
        if self.transport_failed {
            return;
        }
        let Ok(body) = serde_json::to_vec(message) else {
            return;
        };
        if framing::write_message(&mut self.writer, &body).is_err() {
            self.transport_failed = true;
        }
    }
}

fn initialize_result() -> Value {
    json!({
        "capabilities": {
            "textDocumentSync": { "openClose": true, "change": 1 },
            "documentFormattingProvider": true,
            "hoverProvider": true
        },
        "serverInfo": { "name": "slipfork-lsp", "version": env!("CARGO_PKG_VERSION") }
    })
}

fn pointer_str(params: &Value, pointer: &str) -> Option<String> {
    params
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn pointer_u32(params: &Value, pointer: &str) -> Option<u32> {
    params
        .pointer(pointer)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn last_change_text(params: &Value) -> Option<&str> {
    params
        .pointer("/contentChanges")
        .and_then(Value::as_array)
        .and_then(|changes| changes.last())
        .and_then(|change| change.get("text"))
        .and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn frame(message: &Value) -> Vec<u8> {
        let mut out = Vec::new();
        framing::write_message(&mut out, &serde_json::to_vec(message).expect("json"))
            .expect("frame");
        out
    }

    fn next(output: &mut Cursor<Vec<u8>>) -> Value {
        let body = framing::read_message(output)
            .expect("read")
            .expect("complete frame");
        serde_json::from_slice(&body).expect("JSON body")
    }

    #[test]
    fn a_panicking_request_recovers_with_one_internal_error_and_keeps_serving() {
        let mut input = Vec::new();
        input.extend(frame(
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "test/panic" }),
        ));
        input.extend(frame(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "initialize", "params": {}
        })));
        input.extend(frame(
            &json!({ "jsonrpc": "2.0", "id": 3, "method": "shutdown" }),
        ));
        input.extend(frame(&json!({ "jsonrpc": "2.0", "method": "exit" })));

        let mut output = Vec::new();
        let code = Server::new(Cursor::new(input), &mut output).serve();
        assert_eq!(code, 0);

        let mut output = Cursor::new(output);
        let recovered = next(&mut output);
        assert_eq!(recovered["id"], Value::Null);
        assert_eq!(recovered["error"]["code"], json!(-32603));
        assert!(
            recovered["error"]["message"]
                .as_str()
                .expect("message")
                .contains("(next: ")
        );

        let initialized = next(&mut output);
        assert_eq!(initialized["id"], json!(2));
        assert!(initialized["result"]["capabilities"].is_object());
    }
}
