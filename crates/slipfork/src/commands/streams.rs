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
use super::timeparse::format_clock;
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
        let mut renderer = Renderer::new(self.color(), args.json, args.timestamps, args.follow);
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
        let mut catalog: Vec<Value> = Vec::new();
        let mut skipped = 0_usize;
        let outcome = client.call_streaming("logs", params, &mut |event| {
            if event["event"].as_str() == Some("runs") {
                if let Some(rows) = event["rows"].as_array() {
                    records.extend(rows.iter().filter_map(RunRow::from_value));
                    catalog.extend(rows.iter().cloned());
                }
                skipped = usize::try_from(event["skipped"].as_u64().unwrap_or(0)).unwrap_or(0);
            }
        });
        match outcome {
            Ok(_) => {
                if args.json {
                    // The daemon already matched `RunRecord`'s 10 documented keys; forward verbatim.
                    for record in &catalog {
                        let line = serde_json::to_string(record).unwrap_or_default();
                        let _ = emit(&format!("{line}\n"));
                    }
                    if skipped > 0 {
                        let _ = emit(&format!("{}\n", json!({ "skipped": skipped })));
                    }
                } else {
                    if let Some(notice) = empty_runs_notice(records.len(), skipped) {
                        println!("{notice}");
                    } else if !records.is_empty() {
                        for line in render_runs(&records, self.color()) {
                            println!("{line}");
                        }
                    }
                    if let Some(notice) = skipped_notice(skipped) {
                        eprintln!("{notice}");
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
                    &error.code,
                    &error.message,
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

/// The bare-catalog hint; a record-bearing or skipped-line-bearing catalog never prints it.
fn empty_runs_notice(records: usize, skipped: usize) -> Option<&'static str> {
    (records == 0 && skipped == 0)
        .then_some("no recorded runs yet — `slipfork up` records one per executed step")
}

/// The one human notice for catalog lines written by an older schema.
fn skipped_notice(skipped: usize) -> Option<String> {
    let (noun, verb) = match skipped {
        0 => return None,
        1 => ("entry", "is"),
        _ => ("entries", "are"),
    };
    Some(format!(
        "{skipped} {noun} from an older schema {verb} not listed"
    ))
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

/// The short human stream label: `out` for stdout and merged captures, `err` for stderr.
fn stream_label(stream: &str) -> &'static str {
    match stream {
        "stderr" => "err",
        _ => "out",
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
    /// Replay rows carry a `HH:MM:SS.mmm` clock.
    clock: bool,
    /// Human rows carry an `out`/`err` label.
    stream_labels: bool,
    pods: BTreeMap<String, &'static str>,
    width: usize,
}

impl Renderer {
    fn new(color: bool, json: bool, timestamps: bool, follow: bool) -> Self {
        Self {
            palette: Palette::new(color),
            json,
            gutter: true,
            timestamps,
            clock: false,
            stream_labels: follow,
            pods: BTreeMap::new(),
            width: 0,
        }
    }

    /// The `exec`/`shell` renderer: the command's own stdout and stderr, byte for byte.
    pub(crate) fn exec(color: bool, json: bool) -> Self {
        Self {
            palette: Palette::new(color),
            json,
            gutter: false,
            timestamps: false,
            clock: false,
            stream_labels: false,
            pods: BTreeMap::new(),
            width: 0,
        }
    }

    /// The `logs -r <id>` renderer: one recorded run's timestamped, stream-labeled lines.
    fn replay(color: bool, json: bool, timestamps: bool) -> Self {
        Self {
            palette: Palette::new(color),
            json,
            gutter: false,
            timestamps,
            clock: true,
            stream_labels: true,
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
            // `logs -r --json` mirrors the catalog record before its captured lines.
            if self.json
                && let Some(run) = event.get("run")
            {
                let _ = emit(&format!("{run}\n"));
            }
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
                if self.clock {
                    let stamp = ts_ms.map_or_else(String::new, |ts| {
                        format!("{}  ", self.palette.dim(&format_clock(ts)))
                    });
                    let label = if self.stream_labels {
                        format!("{}  ", self.palette.dim(stream_label(stream)))
                    } else {
                        String::new()
                    };
                    let _ = emit(&format!("{stamp}{label}{line}\n"));
                } else {
                    let stamp = if self.timestamps {
                        ts_ms
                            .map(|ts| format!("{} ", self.palette.dim(&format_ts(ts))))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    let _ = emit(&format!("{stamp}{line}\n"));
                }
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
                    Column::left(),
                ];
                let gutter = Table::new(&columns).gap(" ").indent(&stamp);
                let label = if self.stream_labels {
                    self.palette.dim(stream_label(stream))
                } else {
                    String::new()
                };
                let cells = [
                    self.palette.tint(pod, code),
                    self.palette.divider(),
                    label,
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
        if let Some(run) = event.get("run") {
            // A roster carrying a catalog record is a capture replay, so every following line takes
            // the same clock-stamped `out`/`err` path as `logs -r`.
            self.gutter = false;
            self.clock = true;
            self.stream_labels = true;
            self.print_header(run);
        }
    }

    /// One replay header row: `#<run_id>  <kind>  <target>  <command>  <started_at>`.
    fn print_header(&self, run: &Value) {
        let run_id = run["run_id"].as_str().unwrap_or_default();
        let kind = run["kind"].as_str().unwrap_or_default();
        let target = run["target"].as_str().unwrap_or_default();
        let command = run["command"].as_str().unwrap_or_default();
        let mut row = format!(
            "{}  {}  {}  {}",
            self.palette.key(&format!("#{run_id}")),
            self.palette.dim(kind),
            self.palette.label(target),
            self.palette.dim(command),
        );
        if let Some(started) = run["started_ms"].as_u64() {
            row.push_str(&format!("  {}", self.palette.dim(&format_ts(started))));
        }
        let _ = emit(&format!("{row}\n"));
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

#[cfg(test)]
mod tests {
    use super::{empty_runs_notice, skipped_notice};

    /// An older-schema line is reported in words, and singular/plural read correctly.
    #[test]
    fn the_skipped_notice_counts_old_schema_lines() {
        assert_eq!(skipped_notice(0), None);
        assert_eq!(
            skipped_notice(1).as_deref(),
            Some("1 entry from an older schema is not listed")
        );
        assert_eq!(
            skipped_notice(2).as_deref(),
            Some("2 entries from an older schema are not listed")
        );
    }

    /// A record-bearing or skipped-line-bearing catalog never claims nothing was recorded.
    #[test]
    fn the_empty_notice_only_shows_for_a_truly_empty_catalog() {
        assert!(empty_runs_notice(0, 0).is_some());
        assert_eq!(empty_runs_notice(1, 0), None);
        assert_eq!(empty_runs_notice(0, 1), None);
    }
}
