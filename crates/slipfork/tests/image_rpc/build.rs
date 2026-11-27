//! Cold-to-warm typed frames, typed refusals with `next:`, `--json`, and a broken `| head` pipe.
use crate::common::{Lab, stderr_of};
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Stdio;

/// This lab builds `FROM scratch`, so no `/bin/sh` exists for a noisy `RUN`; one thousand long
/// `ENV` rows stream well past the 64 KiB pipe buffer instead.
const PIPE_ROWS: usize = 1_000;

fn params(lab: &Lab) -> Value {
    json!({ "project": lab.project, "component": "app", "target": null, "no_cache": false })
}

fn write_pipe_dockerfile(lab: &Lab) {
    let mut source = String::from("FROM scratch\n");
    for row in 0..PIPE_ROWS {
        source.push_str(&format!("ENV PIPE_{row:04}={}\n", "x".repeat(96)));
    }
    fs::write(lab.project.join("Dockerfile"), source).expect("write the piped Dockerfile");
}

pub fn cold_then_warm(lab: &Lab) {
    let (cold_events, cold) = lab.call("cold", "image.build", &params(lab));
    let (warm_events, warm) = lab.call("warm", "image.build", &params(lab));
    assert_eq!(cold["exit"], 0, "{cold}");
    assert_eq!(warm["exit"], 0, "{warm}");
    assert!(cold["cache_misses"].as_u64().unwrap_or(0) >= 1, "{cold}");
    assert_eq!(warm["cache_misses"], 0, "{warm}");
    assert!(warm["cache_hits"].as_u64().unwrap_or(0) >= 1, "{warm}");
    for events in [&cold_events, &warm_events] {
        let steps: Vec<&Value> = events
            .iter()
            .filter(|event| event["event"] == "step")
            .collect();
        assert!(!steps.is_empty(), "typed step frames: {events:?}");
        for step in steps {
            for key in [
                "index",
                "total",
                "instruction",
                "args",
                "state",
                "elapsed_ms",
            ] {
                assert!(step.get(key).is_some(), "step `{key}`: {step}");
            }
        }
    }
    let output = lab
        .command()
        .args(["--json", "docker", "build", "app"])
        .output()
        .expect("run the JSON CLI against the warm daemon");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).expect("the JSON stream is UTF-8");
    assert!(
        !text.contains('\u{1b}'),
        "`--json` emits zero escapes: {text:?}"
    );
    let lines: Vec<Value> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("every JSON line parses"))
        .collect();
    assert!(
        lines.iter().any(|line| line["event"] == "step"),
        "{lines:?}"
    );
}

pub fn refusals(lab: &Lab) {
    let mut unknown_params = params(lab);
    unknown_params["component"] = json!("nope");
    let unknown = lab.call_error("unknown", "image.build", &unknown_params);
    assert_refusal(&unknown, "unknown-component");
    fs::write(
        lab.project.join("Forkfile"),
        "# forkfile: 0.1\nimage base from 127.0.0.1:1/library/app:latest {\n}\n",
    )
    .expect("write the from-only Forkfile");
    let pulled = lab.call_error(
        "from-only",
        "image.build",
        &json!({ "project": lab.project, "component": "base" }),
    );
    assert_refusal(&pulled, "not-buildable");
    fs::remove_file(lab.project.join("Forkfile")).expect("remove the Forkfile");
    let missing = lab.call_error("missing", "image.build", &params(lab));
    assert_refusal(&missing, "no-forkfile");
    lab.write_project("scratch");
}

fn assert_refusal(error: &Value, code: &str) {
    assert_eq!(error["code"], code, "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("next:"),
        "`{code}` carries a remedy: {error}"
    );
}

pub fn broken_pipe(lab: &Lab) {
    write_pipe_dockerfile(lab);
    let mut child = lab
        .command()
        .args(["docker", "build", "--no-cache", "app"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the streaming CLI");
    let pid = child.id();
    let stdout = child.stdout.take().expect("the CLI stdout is piped");
    let mut reader = BufReader::new(stdout);
    let mut first = String::new();
    reader
        .read_line(&mut first)
        .expect("read the first CLI line");
    assert!(!first.trim().is_empty(), "the build streams its first row");
    assert!(
        child.try_wait().expect("poll the streaming CLI").is_none(),
        "the build is still parked after its first row"
    );
    // Drop the read end while >64 KiB of rows are still pending: the CLI must see EPIPE, stop
    // reading the daemon, and exit cleanly instead of dying on SIGPIPE.
    drop(reader);
    let status = child.wait().expect("the CLI exits after its reader closes");
    let stderr = stderr_of(child);
    assert!(
        status.success(),
        "a closed reader is not an error: {stderr}"
    );
    assert!(
        stderr.trim().is_empty(),
        "no noise on a closed reader: {stderr}"
    );
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "the CLI child was reaped, not left as a zombie"
    );
    let (_, warm) = lab.call("pipe-warm", "image.build", &params(lab));
    assert_eq!(
        warm["exit"], 0,
        "the daemon stays usable after a closed reader: {warm}"
    );
}
