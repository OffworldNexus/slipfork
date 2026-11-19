//! Protected end-to-end contract for the `Forkfile` language server.
//!
//! One cohesive journey spawns the real `slipfork-lsp` binary over real pipes (zero mocks):
//! initialize, invalid diagnostics with a non-BMP emoji UTF-16 column, valid diagnostics, hover,
//! formatting, malformed JSON recovery, and a clean `shutdown` + `exit` with exit code 0.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

const VALID: &str = "\
project demo
features {
    default = [local]
    local
}

postgres pg {
    database app_db
}

env common {
    DATABASE_URL = app_db.url
    SESSION = secret(SESSION_KEY)
}

service api ./api {
    use env common
    port 3000
    run cargo run
}
";

const INVALID: &str = "env emoji {\n    A = \"🔑\" + missing\n}\n";

const URI: &str = "file:///tmp/Forkfile";

struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: ChildStderr,
    next_id: i64,
}

impl Session {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_slipfork-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn slipfork-lsp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let stderr = child.stderr.take().expect("stderr");
        Self {
            child,
            stdin,
            stdout,
            stderr,
            next_id: 1,
        }
    }

    fn send(&mut self, message: &Value) {
        let body = serde_json::to_vec(message).expect("serialize");
        self.send_raw(&body);
    }

    fn send_raw(&mut self, body: &[u8]) {
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len()).expect("header");
        self.stdin.write_all(body).expect("body");
        self.stdin.flush().expect("flush");
    }

    fn recv(&mut self) -> Value {
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).expect("header line");
            assert!(
                !line.is_empty(),
                "the server closed stdout before a complete message"
            );
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                content_length = value.trim().parse().expect("content length");
            }
        }
        let mut body = vec![0u8; content_length];
        self.stdout.read_exact(&mut body).expect("body");
        serde_json::from_slice(&body).expect("JSON body")
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let response = self.recv();
        assert_eq!(response["id"], json!(id));
        response
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn open(&mut self, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": URI, "languageId": "forkfile", "version": 1, "text": text
            }}),
        );
    }

    fn change(&mut self, text: &str, version: i64) {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": URI, "version": version },
                "contentChanges": [{ "text": text }]
            }),
        );
    }
}

fn position_of(text: &str, byte: usize) -> (u32, u32) {
    let before = &text[..byte];
    let line = before.matches('\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = text[line_start..byte]
        .chars()
        .map(|ch| ch.len_utf16() as u32)
        .sum();
    (line, character)
}

#[test]
fn forkfile_language_server_lifecycle() {
    let mut session = Session::start();

    let initialize = session.request(
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    assert_eq!(
        initialize["result"]["capabilities"]["textDocumentSync"]["change"],
        json!(1)
    );
    assert_eq!(
        initialize["result"]["capabilities"]["documentFormattingProvider"],
        json!(true)
    );
    session.notify("initialized", json!({}));

    // Invalid document: every diagnostic is pushed, and the emoji line reports UTF-16 columns.
    session.open(INVALID);
    let published = session.recv();
    assert_eq!(published["method"], "textDocument/publishDiagnostics");
    let diagnostics = published["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics array");
    assert!(!diagnostics.is_empty());
    let target = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["range"]["start"]["line"] == json!(1))
        .expect("a diagnostic on the emoji line");
    assert_eq!(target["range"]["start"]["character"], json!(15));
    assert!(
        target["message"]
            .as_str()
            .expect("message")
            .contains("missing")
    );

    // Full sync to a valid document clears diagnostics.
    session.change(VALID, 2);
    let cleared = session.recv();
    assert_eq!(
        cleared["params"]["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .len(),
        0
    );

    // Hover on a keyword and on a `secret(...)` call argument.
    let (line, character) = position_of(VALID, VALID.find("postgres").expect("keyword"));
    let keyword = session.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": URI }, "position": {
            "line": line, "character": character
        }}),
    );
    assert!(
        keyword["result"]["contents"]["value"]
            .as_str()
            .expect("hover markdown")
            .contains("PostgreSQL cluster")
    );

    let (line, character) = position_of(VALID, VALID.find("SESSION_KEY").expect("secret key") + 1);
    let secret = session.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": URI }, "position": {
            "line": line, "character": character
        }}),
    );
    assert!(
        secret["result"]["contents"]["value"]
            .as_str()
            .expect("hover markdown")
            .contains("0600")
    );

    // Formatting returns one whole-document edit with canonical indentation.
    let messy = "service   api ./api {\nport 3000\nrun cargo run\n}\n";
    session.change(messy, 3);
    let diagnostics = session.recv();
    assert!(
        diagnostics["params"]["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .is_empty()
    );
    let formatting = session.request(
        "textDocument/formatting",
        json!({ "textDocument": { "uri": URI }, "options": {
            "tabSize": 4, "insertSpaces": true
        }}),
    );
    let edits = formatting["result"].as_array().expect("edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(
        edits[0]["newText"],
        json!("service   api ./api {\n    port 3000\n    run cargo run\n}\n")
    );

    // Malformed JSON is answered with a parse error, and the server keeps running.
    session.send_raw(b"{ not json");
    let malformed = session.recv();
    assert_eq!(malformed["error"]["code"], json!(-32700));

    // Reopening an invalid document republishes diagnostics; `didClose` clears them.
    session.open(INVALID);
    let reopened = session.recv();
    assert!(
        !reopened["params"]["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .is_empty()
    );
    session.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": URI } }),
    );
    let closed = session.recv();
    assert_eq!(closed["method"], "textDocument/publishDiagnostics");
    assert_eq!(closed["params"]["uri"], json!(URI));
    assert!(
        closed["params"]["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .is_empty()
    );

    // An unknown method is `-32601`, and the server stays alive.
    let unknown = session.request("workspace/unknownThing", json!({}));
    assert_eq!(unknown["error"]["code"], json!(-32601));

    // Clean shutdown + exit.
    let shutdown = session.request("shutdown", Value::Null);
    assert_eq!(shutdown["result"], Value::Null);
    session.notify("exit", json!({}));
    let status = session.child.wait().expect("wait");
    assert!(status.success(), "exit code: {status}");

    let mut stderr = String::new();
    session
        .stderr
        .read_to_string(&mut stderr)
        .expect("read stderr");
    assert!(stderr.is_empty(), "stderr leaked text: {stderr}");
}

#[test]
fn an_oversized_body_fails_closed_with_empty_stderr() {
    const MAX_BODY_PLUS_ONE: usize = 2 * 1024 * 1024 + 1;

    let mut child = Command::new(env!("CARGO_BIN_EXE_slipfork-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn slipfork-lsp");
    let mut stdin = child.stdin.take().expect("stdin");
    write!(stdin, "Content-Length: {MAX_BODY_PLUS_ONE}\r\n\r\n").expect("header");
    stdin.flush().expect("flush");
    drop(stdin);

    let output = child.wait_with_output().expect("wait");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "stdout: {:?}", output.stdout);
    assert!(
        output.stderr.is_empty(),
        "stderr leaked text: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn exit_without_shutdown_is_a_failure() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_slipfork-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn slipfork-lsp");
    let mut stdin = child.stdin.take().expect("stdin");
    let body = serde_json::to_vec(&json!({ "jsonrpc": "2.0", "method": "exit" })).expect("json");
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len()).expect("header");
    stdin.write_all(&body).expect("body");
    stdin.flush().expect("flush");
    drop(stdin);

    let status = child.wait().expect("wait");
    assert_eq!(status.code(), Some(1));
}
