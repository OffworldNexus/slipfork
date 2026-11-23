//! Protected end-to-end journey: the real standalone `slipfork-lsp` child over
//! stdio.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use slipfork_forkfile::{keywords, self_properties};

const BAD_URI: &str = "file:///lab/Bad.Forkfile";
const BAD_SOURCE: &str = "# forkfile: 0.1\n\nimage app . {\n  service web {\n    port 8080\n    route /🚀 exact /a rewrite /b\n  }\n}\n";
const FORMAT_URI: &str = "file:///lab/Format.Forkfile";
const FORMAT_SOURCE: &str =
    "# forkfile: 0.1\n\nimage app . {\nservice web {\nport 8080\nrun: @uv run main.py\n}\n}\n";
const HOVER_URI: &str = "file:///lab/Hover.Forkfile";
const HOVER_SOURCE: &str = "# forkfile: 0.1\nfeatures {\n  dev\n}\ndomain mail\npostgres pg {\n  database db\n}\nimage app . {\n  arg BUILD_DOMAIN = mail.name\n  env {\n    GREETING = \"hello\"\n    LOGIN_URL = \"${self.url}/login\"\n    PRIMARY = self.domain\n    SCHEME = self.scheme\n    DB_PASSWORD = db.password\n    SECRET_KEY = secret(SECRET_KEY, random_string(50))\n  }\n  service web {\n    port 8080\n    run: @uv run main.py\n    before-start: echo $env.GREETING\n    on dev {\n      run: @uv run main.py --reload\n    } else {\n      run: @uv run main.py\n    }\n  }\n}\n";
const DOMAIN_URI: &str = "file:///lab/Domain.Forkfile";
const DOMAIN_SOURCE: &str = "# forkfile: 0.1\ndomain main\nenv common {\n  URL = main.url\n  SECRET_KEY = secret(SECRET_KEY) ?? random_string(50)\n}\n";
const POLICY_URI: &str = "file:///lab/Policy.Forkfile";
const POLICY_SOURCE: &str = "# forkfile: 0.1\n\ns3 uploads\ns3 assets { policy public-read }\n\nenv common {\n  PRIVATE_URL = uploads.url\n  PUBLIC_URL = assets.url\n  SECRET = uploads.secret_key\n}\n";

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl Client {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_slipfork-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn `slipfork-lsp`");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    fn send(&mut self, message: &Value) {
        let body = serde_json::to_vec(message).expect("encode");
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len()).expect("header");
        self.stdin.write_all(&body).expect("body");
        self.stdin.flush().expect("flush");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        loop {
            let message = self.read();
            if message.get("id").and_then(Value::as_i64) == Some(id) {
                return message.get("result").cloned().unwrap_or(Value::Null);
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
    }

    fn notification(&mut self, method: &str) -> Value {
        loop {
            let message = self.read();
            if message.get("method").and_then(Value::as_str) == Some(method) {
                return message.get("params").cloned().unwrap_or(Value::Null);
            }
        }
    }

    fn read(&mut self) -> Value {
        let mut length = None;
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).expect("stdout line");
            assert!(read > 0, "server closed stdout before replying");
            let header = line.trim_end_matches(['\r', '\n']);
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':')
                && name.eq_ignore_ascii_case("Content-Length")
            {
                length = Some(value.trim().parse::<usize>().expect("length"));
            }
        }
        let length = length.expect("Content-Length");
        let mut body = vec![0_u8; length];
        self.stdout.read_exact(&mut body).expect("body");
        serde_json::from_slice(&body).expect("json")
    }

    fn finish(mut self) {
        assert_eq!(self.request("shutdown", Value::Null), Value::Null);
        self.notify("exit", Value::Null);
        let _ = self.stdin.flush();
        drop(self.stdin);
        for _ in 0..100 {
            if let Some(status) = self.child.try_wait().expect("wait") {
                assert!(status.success(), "`slipfork-lsp` exited with {status}");
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let mut stderr = String::new();
        if let Some(mut pipe) = self.child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        panic!("`slipfork-lsp` did not exit after `exit`; stderr:\n{stderr}");
    }
}

#[test]
fn live_stdio_journey_publishes_diagnostics_hover_definition_and_formatting() {
    let mut client = Client::spawn();

    let initialize = client.request(
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    assert_eq!(
        initialize["capabilities"]["documentFormattingProvider"],
        json!(true)
    );
    assert_eq!(
        initialize["capabilities"]["definitionProvider"],
        json!(true)
    );
    assert_eq!(initialize["capabilities"]["hoverProvider"], json!(true));
    client.notify("initialized", json!({}));

    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": BAD_URI, "languageId": "forkfile", "version": 1, "text": BAD_SOURCE },
        }),
    );
    let diagnostics = client.notification("textDocument/publishDiagnostics");
    assert_eq!(diagnostics["uri"], json!(BAD_URI));
    let first = &diagnostics["diagnostics"][0];
    assert_eq!(first["severity"], json!(1));
    assert_eq!(first["range"]["start"]["line"], json!(5));
    assert_eq!(
        first["range"]["start"]["character"],
        json!(23),
        "astral emoji must shift the caret by UTF-16 code units"
    );

    let emoji = position_of(BAD_SOURCE, "🚀", 0);
    let emoji_end = json!({
        "line": emoji["line"],
        "character": emoji["character"].as_u64().unwrap() + 1,
    });
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": BAD_URI, "version": 2 },
            "contentChanges": [{ "range": { "start": emoji, "end": emoji_end }, "text": "x" }],
        }),
    );
    let diagnostics = client.notification("textDocument/publishDiagnostics");
    assert_eq!(
        diagnostics["diagnostics"][0]["range"]["start"]["line"],
        json!(5)
    );
    assert_eq!(
        diagnostics["diagnostics"][0]["range"]["start"]["character"],
        json!(22),
        "a ranged edit must republish UTF-16 columns"
    );

    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": FORMAT_URI, "languageId": "forkfile", "version": 1, "text": FORMAT_SOURCE },
        }),
    );
    let diagnostics = client.notification("textDocument/publishDiagnostics");
    assert_eq!(diagnostics["diagnostics"], json!([]));

    let edits = client.request(
        "textDocument/formatting",
        json!({ "textDocument": { "uri": FORMAT_URI }, "options": { "tabSize": 2, "insertSpaces": true } }),
    );
    let new_text = edits[0]["newText"].as_str().expect("formatted text");
    assert!(new_text.contains("\n  service web {\n"), "{new_text}");
    assert!(new_text.contains("\n    port 8080\n"), "{new_text}");

    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": FORMAT_URI, "version": 2 },
            "contentChanges": [{ "text": new_text }],
        }),
    );
    let diagnostics = client.notification("textDocument/publishDiagnostics");
    assert_eq!(diagnostics["diagnostics"], json!([]));
    let edits = client.request(
        "textDocument/formatting",
        json!({ "textDocument": { "uri": FORMAT_URI }, "options": { "tabSize": 2, "insertSpaces": true } }),
    );
    assert_eq!(edits, json!([]), "formatting must be idempotent");

    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": FORMAT_URI, "version": 3 },
            "contentChanges": [{ "text": BAD_SOURCE }],
        }),
    );
    let diagnostics = client.notification("textDocument/publishDiagnostics");
    assert!(!diagnostics["diagnostics"].as_array().unwrap().is_empty());

    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": HOVER_URI, "languageId": "forkfile", "version": 1, "text": HOVER_SOURCE },
        }),
    );
    let diagnostics = client.notification("textDocument/publishDiagnostics");
    assert_eq!(diagnostics["diagnostics"], json!([]));

    let hover = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": HOVER_URI },
            "position": position_of(HOVER_SOURCE, "@uv", 1),
        }),
    );
    let contents = hover["contents"]["value"].as_str().expect("hover markdown");
    assert!(contents.contains("uv sync"), "{contents}");
    assert!(contents.contains("uv.lock"), "{contents}");

    let secret = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": HOVER_URI },
            "position": position_of(HOVER_SOURCE, "db.password", 0),
        }),
    );
    let contents = secret["contents"]["value"].as_str().expect("secret hover");
    assert!(contents.contains("Secret-tainted"), "{contents}");
    assert!(contents.contains("database"), "{contents}");

    for (needle, expected) in [
        ("self.url", "primary URL with a scheme"),
        ("self.domain", "bare hostname"),
        ("self.scheme", "http"),
        ("random_string", "secret generator"),
    ] {
        let hover = client.request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": HOVER_URI },
                "position": position_of(HOVER_SOURCE, needle, 0),
            }),
        );
        let contents = hover["contents"]["value"]
            .as_str()
            .unwrap_or_else(|| panic!("hover for {needle}"));
        assert!(contents.contains(expected), "{needle}: {contents}");
    }

    let domain_line = position_of(HOVER_SOURCE, "domain mail", 0)["line"].as_u64();
    let definition = client.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": HOVER_URI },
            "position": position_of(HOVER_SOURCE, "mail.name", 0),
        }),
    );
    assert_eq!(definition[0]["uri"], json!(HOVER_URI));
    assert_eq!(
        definition[0]["range"]["start"]["line"].as_u64(),
        domain_line
    );
    assert_eq!(definition[0]["range"]["start"]["character"], json!(7));

    let secret_definition = client.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": HOVER_URI },
            "position": position_of(HOVER_SOURCE, "secret(SECRET_KEY", 7),
        }),
    );
    assert_eq!(
        secret_definition[0]["range"]["start"]["line"].as_u64(),
        position_of(HOVER_SOURCE, "SECRET_KEY = secret", 0)["line"].as_u64()
    );

    let self_items = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": HOVER_URI },
            "position": position_of(HOVER_SOURCE, "self.domain", 5),
        }),
    );
    let expected_self = self_properties()
        .iter()
        .map(|property| (*property).to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        completion_labels(&self_items),
        expected_self,
        "LSP self properties drifted from the compiler facts"
    );

    let keywords_result = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": HOVER_URI },
            "position": { "line": 0, "character": 0 },
        }),
    );
    let mut expected_keywords = keywords()
        .iter()
        .map(|keyword| (*keyword).to_owned())
        .collect::<Vec<_>>();
    expected_keywords.push("self".to_owned());
    assert_eq!(
        completion_labels(&keywords_result),
        expected_keywords,
        "LSP keywords drifted from the compiler facts"
    );

    let domain_items = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": HOVER_URI },
            "position": position_of(HOVER_SOURCE, "mail.name", 5),
        }),
    );
    assert_eq!(completion_labels(&domain_items), ["name"]);

    let env_definition = client.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": HOVER_URI },
            "position": position_of(HOVER_SOURCE, "$env.GREETING", 5),
        }),
    );
    assert_eq!(
        env_definition[0]["range"]["start"]["line"].as_u64(),
        position_of(HOVER_SOURCE, "GREETING = ", 0)["line"].as_u64()
    );

    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": DOMAIN_URI, "languageId": "forkfile", "version": 1, "text": DOMAIN_SOURCE },
        }),
    );
    let diagnostics = client.notification("textDocument/publishDiagnostics");
    let messages = diagnostics["diagnostics"]
        .as_array()
        .expect("domain diagnostics")
        .iter()
        .map(|diagnostic| {
            diagnostic["message"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert!(
        messages
            .iter()
            .any(|message| message.contains("is not a domain property")),
        "{messages:?}"
    );
    assert!(
        messages.iter().any(|message| message.contains("generator")),
        "{messages:?}"
    );

    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": POLICY_URI, "languageId": "forkfile", "version": 1, "text": POLICY_SOURCE },
        }),
    );
    let diagnostics = client.notification("textDocument/publishDiagnostics");
    assert_eq!(diagnostics["diagnostics"], json!([]));

    let policy = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": POLICY_URI },
            "position": position_of(POLICY_SOURCE, "policy", 0),
        }),
    );
    let contents = policy["contents"]["value"].as_str().expect("policy hover");
    assert!(contents.contains("private"), "{contents}");
    assert!(contents.contains("public-read"), "{contents}");
    assert!(contents.contains("cdn"), "{contents}");

    for (needle, expected) in [
        ("uploads.url", "Secret-tainted"),
        ("assets.url", "Non-secret"),
        ("uploads.secret_key", "Secret-tainted"),
    ] {
        let hover = client.request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": POLICY_URI },
                "position": position_of(POLICY_SOURCE, needle, 0),
            }),
        );
        let contents = hover["contents"]["value"]
            .as_str()
            .unwrap_or_else(|| panic!("hover for {needle}"));
        assert!(contents.contains(expected), "{needle}: {contents}");
    }

    client.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": BAD_URI } }),
    );

    client.finish();
}

fn completion_labels(result: &Value) -> Vec<String> {
    result["items"]
        .as_array()
        .expect("completion items")
        .iter()
        .map(|item| item["label"].as_str().expect("completion label").to_owned())
        .collect()
}

fn position_of(source: &str, needle: &str, extra: usize) -> Value {
    let offset = source.find(needle).expect("needle in source") + extra;
    let before = &source[..offset];
    let line = before.matches('\n').count();
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character: usize = source[line_start..offset]
        .chars()
        .map(char::len_utf16)
        .sum();
    json!({ "line": line, "character": character })
}
