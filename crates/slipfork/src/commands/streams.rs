//! `logs`, `shell`, and `exec`: streaming verbs rendered from typed events.
//!
//! The daemon sends `{"kind","pod","stream","line"}` events plus one `roster` event that fixes the
//! gutter width. This renderer owns dividers, `--json`, and the final `{"exit":n}`. A terminal
//! `shell` receives the world's real `/dev/pts/N` master and relays raw bytes untouched; a pipe
//! relays stdin as base64 input frames, so `psql`, `redis-cli`, and `sh` work over a plain pipe.

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use crate::commands::{Session, emit};
use crate::daemon::{DaemonClient, DaemonError};
use crate::ui::Palette;
use crate::ui::table::{Column, Table};

use super::runs::{RunRow, render_runs};
pub(crate) use super::timeparse::format_ts;

/// Set by the SIGINT handler so a followed stream exits `0` on Ctrl-C.
pub(crate) static INTERRUPTED: AtomicBool = AtomicBool::new(false);

const PALETTE: [&str; 8] = [
    "1;36", "1;35", "1;34", "1;33", "1;32", "1;96", "1;95", "1;31",
];

/// Parsed `slipfork logs` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsArgs {
    /// Keep the stream attached (`-f`).
    pub follow: bool,
    /// Include backing-resource pods (`-a` / `--all`).
    pub all: bool,
    /// Explicit service or resource ids; empty merges the default roster.
    pub targets: Vec<String>,
    /// Only lines at or after this epoch-millisecond timestamp.
    pub since_ms: Option<u64>,
    /// Only lines at or before this epoch-millisecond timestamp.
    pub until_ms: Option<u64>,
    /// Prefix human lines with a timestamp gutter.
    pub timestamps: bool,
    /// Replay one recorded step run (`<id>`, `#<id>`, `last`, `failed`).
    pub run: Option<String>,
    /// List every recorded step run for this world.
    pub runs: bool,
    /// Emit one JSON object per line plus a final `{"exit":0}`.
    pub json: bool,
}

extern "C" fn on_sigint(_signal: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

pub(crate) fn install_sigint() {
    INTERRUPTED.store(false, Ordering::SeqCst);
    // SAFETY: the handler only stores into an atomic. `signal` installs it (with `SA_RESTART`
    // under glibc); the follow-up `sigaction` clears that flag so a blocking read returns `EINTR`.
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
        let mut action: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGINT, std::ptr::null(), &mut action) == 0 {
            action.sa_flags &= !libc::SA_RESTART;
            let _ = libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
        }
    }
}

impl Session {
    /// `slipfork logs [-f] [-a] [service]`.
    pub fn logs(&self, args: &LogsArgs) -> ExitCode {
        install_sigint();
        if args.runs {
            return self.logs_runs(args);
        }
        if let Some(run) = &args.run {
            return self.logs_run(args, run);
        }
        let Some(mut client) = self.existing_daemon(args.json) else {
            return ExitCode::FAILURE;
        };
        let params = json!({
            "project": self.cwd(),
            "follow": args.follow,
            "all": args.all,
            "targets": args.targets,
            "since_ms": args.since_ms,
            "until_ms": args.until_ms,
        });
        let mut renderer = Renderer::new(self.color(), args.json, args.timestamps);
        match client.call_streaming("logs", params, &mut |event| renderer.event(event)) {
            Ok(_) => {
                renderer.finish(0);
                ExitCode::SUCCESS
            }
            Err(_) if INTERRUPTED.load(Ordering::SeqCst) => {
                renderer.finish(0);
                ExitCode::SUCCESS
            }
            Err(error) => self.exact_fail(&error, "logs", args.json),
        }
    }

    /// `slipfork logs --runs`: every recorded step run for this world, newest last.
    fn logs_runs(&self, args: &LogsArgs) -> ExitCode {
        let Some(mut client) = self.existing_daemon(args.json) else {
            return ExitCode::FAILURE;
        };
        let params = json!({ "project": self.cwd(), "runs": true });
        let mut records: Vec<RunRow> = Vec::new();
        let outcome = client.call_streaming("logs", params, &mut |event| {
            if event["event"].as_str() == Some("runs")
                && let Some(rows) = event["rows"].as_array()
            {
                records.extend(rows.iter().filter_map(RunRow::from_value));
            }
        });
        match outcome {
            Ok(_) => {
                if args.json {
                    for record in &records {
                        let line = serde_json::to_string(&record.to_value()).unwrap_or_default();
                        let _ = emit(&format!("{line}\n"));
                    }
                } else if records.is_empty() {
                    println!("no recorded runs yet — `slipfork up` records one per executed step");
                } else {
                    for line in render_runs(&records, self.color()) {
                        println!("{line}");
                    }
                }
                ExitCode::SUCCESS
            }
            Err(error) => self.stream_fail(&error, "logs", args.json),
        }
    }

    /// `slipfork logs -r <id>`: replay one recorded step run through the indexed reader.
    fn logs_run(&self, args: &LogsArgs, run: &str) -> ExitCode {
        let Some(mut client) = self.existing_daemon(args.json) else {
            return ExitCode::FAILURE;
        };
        let params = json!({
            "project": self.cwd(),
            "run": run,
            "follow": false,
            "since_ms": args.since_ms,
            "until_ms": args.until_ms,
        });
        let mut renderer = Renderer::replay(self.color(), args.json, args.timestamps);
        match client.call_streaming("logs", params, &mut |event| renderer.event(event)) {
            Ok(_) => {
                renderer.finish(0);
                ExitCode::SUCCESS
            }
            Err(_) if INTERRUPTED.load(Ordering::SeqCst) => {
                renderer.finish(0);
                ExitCode::SUCCESS
            }
            Err(error) => self.stream_fail(&error, "logs", args.json),
        }
    }

    pub(crate) fn existing_daemon(&self, json: bool) -> Option<DaemonClient> {
        match DaemonClient::connect_existing(self.host()) {
            Ok(Some(client)) => Some(client.with_interrupt(&INTERRUPTED)),
            Ok(None) => {
                let _ = self.report(
                    "no-world",
                    "no world is running in this directory",
                    "run `slipfork up` first",
                    json,
                );
                None
            }
            Err(error) => {
                let _ = self.report(
                    "daemon",
                    &error.to_string(),
                    "check that XDG_RUNTIME_DIR is readable and retry",
                    json,
                );
                None
            }
        }
    }

    pub(crate) fn stream_fail(&self, error: &DaemonError, verb: &str, json: bool) -> ExitCode {
        let next = refusal_next(error, verb);
        self.report(&error.code, &error.message, &next, json)
    }
}

/// The one remedy for a typed streaming refusal.
pub(crate) fn refusal_next(error: &DaemonError, verb: &str) -> String {
    match error.code.as_str() {
        "ambiguous-target" => {
            format!("pass one of the ids above, e.g. `slipfork {verb} <id>`")
        }
        "no-target" => "run `slipfork up` or pass a service".to_owned(),
        "no-world" => "run `slipfork up` first".to_owned(),
        "world-booting" => "wait for `slipfork up` to finish, then retry".to_owned(),
        _ => "run `slipfork status` to inspect the world".to_owned(),
    }
}

/// Renders typed daemon events as gutters, dividers, or NDJSON.
pub(crate) struct Renderer {
    palette: Palette,
    json: bool,
    /// `logs` prefixes every line with its pod gutter; `exec` stays verbatim like `docker exec`.
    gutter: bool,
    /// Prefix a human timestamp before the pod gutter (`-t`).
    timestamps: bool,
    pods: BTreeMap<String, &'static str>,
    width: usize,
}

impl Renderer {
    fn new(color: bool, json: bool, timestamps: bool) -> Self {
        Self::with_gutter(color, json, true, timestamps)
    }

    /// The `exec`/`shell` renderer: the command's own stdout and stderr, byte for byte.
    pub(crate) fn exec(color: bool, json: bool) -> Self {
        Self::with_gutter(color, json, false, false)
    }

    /// The `logs -r <id>` renderer: one recorded run's lines, no per-pod gutter.
    fn replay(color: bool, json: bool, timestamps: bool) -> Self {
        Self::with_gutter(color, json, false, timestamps)
    }

    fn with_gutter(color: bool, json: bool, gutter: bool, timestamps: bool) -> Self {
        Self {
            palette: Palette::new(color),
            json,
            gutter,
            timestamps,
            pods: BTreeMap::new(),
            width: 0,
        }
    }

    pub(crate) fn event(&mut self, event: &Value) {
        let pod = event.get("pod").and_then(Value::as_str).unwrap_or_default();
        let kind = event.get("kind").and_then(Value::as_str).unwrap_or("line");
        let stream = event
            .get("stream")
            .and_then(Value::as_str)
            .unwrap_or("stdout");
        let line = event
            .get("line")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let ts_ms = event.get("ts_ms").and_then(Value::as_u64);
        if kind == "roster" {
            self.roster(event);
            return;
        }
        if self.json {
            let mut payload = json!({
                "pod": pod,
                "stream": stream,
                "line": line,
                "kind": kind,
            });
            if let Some(ts_ms) = ts_ms {
                payload["ts_ms"] = json!(ts_ms);
            }
            let _ = emit(&format!("{payload}\n"));
            return;
        }
        match kind {
            "eof" => {}
            "divider" => {
                let _ = emit(&format!("{}\n", self.palette.tint(line, "2;36")));
            }
            _ if !self.gutter => {
                let stamp = if self.timestamps {
                    ts_ms
                        .map(|ts| format!("{} ", self.palette.dim(&format_ts(ts))))
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let _ = emit(&format!("{stamp}{line}\n"));
            }
            _ => {
                let stamp = if self.timestamps {
                    ts_ms
                        .map(|ts| format!("{} ", self.palette.dim(&format_ts(ts))))
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let code = self.color_for(pod);
                let columns = [
                    Column::left().at_least(self.width),
                    Column::left(),
                    Column::left(),
                ];
                let gutter = Table::new(&columns).gap(" ").indent(&stamp);
                let cells = [
                    self.palette.tint(pod, code),
                    self.palette.divider(),
                    line.to_owned(),
                ];
                // Measure the real row, line included: the table engine skips a zero-width column,
                // and measuring an empty body would silently drop every log line.
                let widths = gutter.widths(&[cells.to_vec()]);
                let row = gutter.line(&widths, &cells);
                let _ = emit(&format!("{row}\n"));
            }
        }
    }

    /// Fixes the gutter width from the daemon's pod roster; it never grows mid-stream.
    fn roster(&mut self, event: &Value) {
        if self.json {
            return;
        }
        let pods: Vec<&str> = event
            .get("pods")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        self.width = pods
            .iter()
            .map(|pod| pod.chars().count())
            .max()
            .unwrap_or(0);
        for pod in pods {
            let _ = self.color_for(pod);
        }
    }

    pub(crate) fn finish(&self, code: i64) {
        if self.json {
            let _ = emit(&format!("{}\n", json!({ "exit": code })));
        }
    }

    fn color_for(&mut self, pod: &str) -> &'static str {
        let index = self.pods.len();
        self.pods
            .entry(pod.to_owned())
            .or_insert(PALETTE[index % PALETTE.len()])
    }
}
