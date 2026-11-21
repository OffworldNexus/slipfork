//! The versioned NEWLINE-delimited JSON frames of `<runtime_dir>/daemon.sock`.
//!
//! One request produces either one success frame, one typed error frame, or a stream of `event`
//! frames followed by the final success frame. Unknown versions and methods answer a typed error
//! and leave the connection open.

use serde::Deserialize;
use serde_json::{Value, json};

/// Control-plane version; both peers reject anything else.
pub const VERSION: u32 = 1;

/// One client request.
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub v: u32,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// One daemon reply: exactly one of `ok`, `error`, or `event` is present.
#[derive(Debug, Clone, Deserialize)]
pub struct Response {
    #[serde(default)]
    pub v: u32,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub ok: Option<Value>,
    #[serde(default)]
    pub error: Option<Failure>,
    #[serde(default)]
    pub event: Option<Value>,
}

/// The typed failure payload of an error frame.
#[derive(Debug, Clone, Deserialize)]
pub struct Failure {
    pub code: String,
    pub message: String,
}

/// One client input frame for a live `exec`; `input` is base64 or empty for EOF.
#[derive(Debug, Clone, Deserialize)]
pub struct InputFrame {
    #[serde(default)]
    pub input: Option<String>,
}

/// A success frame: `{"v":1,"id":<id>,"ok":<object>}`.
#[must_use]
pub fn ok(id: &str, value: Value) -> Vec<u8> {
    frame(json!({ "v": VERSION, "id": id, "ok": value }))
}

/// An error frame: `{"v":1,"id":<id>,"error":{"code":..,"message":..}}`.
#[must_use]
pub fn error(id: &str, code: &str, message: &str) -> Vec<u8> {
    frame(json!({
        "v": VERSION,
        "id": id,
        "error": { "code": code, "message": message },
    }))
}

/// A streaming progress frame: `{"v":1,"id":<id>,"event":<object>}`.
#[must_use]
pub fn event(id: &str, value: Value) -> Vec<u8> {
    frame(json!({ "v": VERSION, "id": id, "event": value }))
}

/// Encodes one request frame for a client.
#[must_use]
pub fn request(id: &str, method: &str, params: Value) -> Vec<u8> {
    frame(json!({ "v": VERSION, "id": id, "method": method, "params": params }))
}

/// Encodes one client `input` frame for a live `exec`.
#[must_use]
pub fn input(id: &str, data: &str) -> Vec<u8> {
    frame(json!({ "v": VERSION, "id": id, "input": data }))
}

fn frame(value: Value) -> Vec<u8> {
    let mut bytes = value.to_string().into_bytes();
    bytes.push(b'\n');
    bytes
}
