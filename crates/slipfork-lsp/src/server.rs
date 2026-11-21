//! The framed stdio JSON-RPC server.

use std::collections::BTreeMap;
use std::io::{self, BufReader, Read, Write};

use serde_json::{Value, json};
use slipfork_forkfile::Diagnostic;

use crate::document::Analysis;
use crate::features;
use crate::protocol::{read_message, write_message};
use crate::text::{Position, Range, TextDocument};

const VERSION: &str = env!("CARGO_PKG_VERSION");

struct Document {
    text: TextDocument,
    analysis: Analysis,
}

impl Document {
    fn new(text: TextDocument) -> Self {
        let analysis = Analysis::new(&text);
        Self { text, analysis }
    }

    fn refresh(&mut self) {
        self.analysis = Analysis::new(&self.text);
    }
}

/// A language server over one request stream and one response sink.
pub struct Server<R, W> {
    reader: BufReader<R>,
    writer: W,
    documents: BTreeMap<String, Document>,
}

impl<R: Read, W: Write> Server<R, W> {
    /// Builds a server around injected reader and writer handles.
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
            documents: BTreeMap::new(),
        }
    }

    /// Serves until `exit` or end of input.
    pub fn serve(mut self) -> io::Result<()> {
        while let Some(message) = read_message(&mut self.reader)? {
            if !self.dispatch(&message)? {
                break;
            }
        }
        Ok(())
    }

    fn dispatch(&mut self, message: &Value) -> io::Result<bool> {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match method.as_str() {
            "initialize" => {
                self.reply(
                    id,
                    json!({
                        "capabilities": capabilities(),
                        "serverInfo": { "name": "slipfork lsp", "version": VERSION },
                    }),
                )?;
            }
            "initialized" => {}
            "shutdown" => self.reply(id, Value::Null)?,
            "exit" => return Ok(false),
            "textDocument/didOpen" => self.did_open(&params)?,
            "textDocument/didChange" => self.did_change(&params)?,
            "textDocument/didClose" => self.did_close(&params)?,
            "textDocument/formatting" => self.formatting(id, &params)?,
            "textDocument/definition" => self.definition(id, &params)?,
            "textDocument/hover" => self.hover(id, &params)?,
            _ => {
                if id.is_some() {
                    self.reply_error(id, -32601, format!("unknown method `{method}`"))?;
                }
            }
        }
        Ok(true)
    }

    fn did_open(&mut self, params: &Value) -> io::Result<()> {
        let Some(document) = params.get("textDocument") else {
            return Ok(());
        };
        let Some(uri) = document.get("uri").and_then(Value::as_str) else {
            return Ok(());
        };
        let Some(text) = document.get("text").and_then(Value::as_str) else {
            return Ok(());
        };
        self.documents
            .insert(uri.to_string(), Document::new(TextDocument::new(text)));
        self.publish(uri)
    }

    fn did_change(&mut self, params: &Value) -> io::Result<()> {
        let Some(uri) = params
            .get("textDocument")
            .and_then(|document| document.get("uri"))
            .and_then(Value::as_str)
        else {
            return Ok(());
        };
        let Some(changes) = params.get("contentChanges").and_then(Value::as_array) else {
            return Ok(());
        };
        let Some(document) = self.documents.get_mut(uri) else {
            return Ok(());
        };
        for change in changes {
            let Some(new_text) = change.get("text").and_then(Value::as_str) else {
                continue;
            };
            let range = change.get("range").and_then(Range::from_value);
            document.text.apply(range, new_text);
        }
        document.refresh();
        self.publish(uri)
    }

    fn did_close(&mut self, params: &Value) -> io::Result<()> {
        if let Some(uri) = params
            .get("textDocument")
            .and_then(|document| document.get("uri"))
            .and_then(Value::as_str)
        {
            self.documents.remove(uri);
        }
        Ok(())
    }

    fn publish(&mut self, uri: &str) -> io::Result<()> {
        let payload = match self.documents.get(uri) {
            Some(document) => json!({
                "uri": uri,
                "diagnostics": document
                    .analysis
                    .diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic_value(&document.text, diagnostic))
                    .collect::<Vec<_>>(),
            }),
            None => return Ok(()),
        };
        self.notify("textDocument/publishDiagnostics", payload)
    }

    fn formatting(&mut self, id: Option<Value>, params: &Value) -> io::Result<()> {
        let result = self
            .with_document(params, |_, document| {
                features::formatting(&document.analysis, &document.text)
            })
            .unwrap_or(Value::Null);
        self.reply(id, result)
    }

    fn definition(&mut self, id: Option<Value>, params: &Value) -> io::Result<()> {
        let position = params.get("position").and_then(Position::from_value);
        let result = position
            .and_then(|position| {
                self.with_document(params, |uri, document| {
                    features::definition(uri, &document.analysis, &document.text, position)
                })
            })
            .unwrap_or(Value::Null);
        self.reply(id, result)
    }

    fn hover(&mut self, id: Option<Value>, params: &Value) -> io::Result<()> {
        let position = params.get("position").and_then(Position::from_value);
        let result = position
            .and_then(|position| {
                self.with_document(params, |_, document| {
                    features::hover(&document.analysis, position)
                })
            })
            .unwrap_or(Value::Null);
        self.reply(id, result)
    }

    fn with_document<T>(
        &self,
        params: &Value,
        feature: impl FnOnce(&str, &Document) -> T,
    ) -> Option<T> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let document = self.documents.get(uri)?;
        Some(feature(uri, document))
    }

    fn reply(&mut self, id: Option<Value>, result: Value) -> io::Result<()> {
        let Some(id) = id else {
            return Ok(());
        };
        write_message(
            &mut self.writer,
            &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        )
    }

    fn reply_error(
        &mut self,
        id: Option<Value>,
        code: i64,
        message: impl Into<String>,
    ) -> io::Result<()> {
        let Some(id) = id else {
            return Ok(());
        };
        write_message(
            &mut self.writer,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": code, "message": message.into() },
            }),
        )
    }

    fn notify(&mut self, method: &str, params: Value) -> io::Result<()> {
        write_message(
            &mut self.writer,
            &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
        )
    }
}

fn capabilities() -> Value {
    json!({
        "textDocumentSync": { "openClose": true, "change": 2 },
        "documentFormattingProvider": true,
        "definitionProvider": true,
        "hoverProvider": true,
    })
}

fn diagnostic_value(text: &TextDocument, diagnostic: &Diagnostic) -> Value {
    let mut range = text.range(diagnostic.span);
    if range.start == range.end {
        range.end = widen(text, range.start);
    }
    json!({
        "range": range.to_value(),
        "severity": 1,
        "source": "slipfork",
        "message": format!("{}\nnext: {}", diagnostic.message, diagnostic.next),
    })
}

fn widen(text: &TextDocument, position: Position) -> Position {
    let offset = text.offset(position);
    let next = text.text()[offset..]
        .chars()
        .next()
        .map(|character| offset + character.len_utf8())
        .unwrap_or(offset);
    text.position(next)
}
