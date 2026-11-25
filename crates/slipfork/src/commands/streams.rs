//! `logs`, `shell`, and `exec`: streaming verbs rendered from typed events.
//!
//! The daemon sends `{"kind","pod","stream","line"}` events plus one `roster` event that fixes the
//! gutter width. [`StreamRenderer`] owns dividers, instruction rows, `--json`, and the final
//! `{"exit":n}`. A terminal `shell` receives the world's real `/dev/pts/N` master and relays
//! raw bytes untouched; a pipe relays stdin as base64 input frames, so `psql`, `redis-cli`, and
//! `sh` work over a plain pipe.

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use crate::commands::{Session, emit};
use crate::daemon::{DaemonClient, DaemonError};
use crate::ui::report::{StreamKind, StreamRenderer};

use super::runs::{RunRow, render_runs};

/// Set by the SIGINT handler so a followed stream exits `0` on Ctrl-C.
pub(crate) static INTERRUPTED: AtomicBool = AtomicBool::new(false);

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
    /// Tail only the last N rendered lines (`--last`); `-f` implies 20 unless explicit.
    pub last: Option<String>,
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
        let last = match args.last.as_deref() {
            Some(raw) => match parse_last(raw) {
                Some(count) => Some(count),
                None => {
                    return self.report(
                        "bad-last",
                        "✗ --last needs a positive line count",
                        "use `--last 1` or omit it",
                        args.json,
                    );
                }
            },
            None if args.follow => Some(20),
            None => None,
        };
        if args.runs {
            return self.logs_runs(args);
        }
        if let Some(run) = &args.run {
            return self.logs_run(args, run, last);
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
            "tail": last.is_some(),
        });
        let mut renderer = StreamRenderer::logs(self.renderer(), args.timestamps).tail(last);
        match client.call_streaming("logs", params, &mut |event| renderer.event(event)) {
            Ok(_) => {
                renderer.finish(0);
                write_status(&renderer)
            }
            Err(_) if INTERRUPTED.load(Ordering::SeqCst) => {
                renderer.finish(0);
                write_status(&renderer)
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
                let mut failed = false;
                if args.json {
                    // The daemon already matched `RunRecord`'s 10 documented keys; forward verbatim.
                    for record in &catalog {
                        let line = serde_json::to_string(record).unwrap_or_default();
                        failed |= matches!(emit(&format!("{line}\n")), ExitCode::FAILURE);
                    }
                    if skipped > 0 {
                        failed |= matches!(
                            emit(&format!("{}\n", json!({ "skipped": skipped }))),
                            ExitCode::FAILURE
                        );
                    }
                } else {
                    if let Some(notice) = empty_runs_notice(records.len(), skipped) {
                        failed |= matches!(emit(&format!("{notice}\n")), ExitCode::FAILURE);
                    } else if !records.is_empty() {
                        for line in render_runs(&records, self.color(), self.clock()) {
                            failed |= matches!(emit(&format!("{line}\n")), ExitCode::FAILURE);
                        }
                    }
                    if let Some(notice) = skipped_notice(skipped) {
                        let _ = self.renderer().emit_notice(StreamKind::Stderr, &notice);
                    }
                }
                if failed {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                }
            }
            Err(error) => self.stream_fail(&error, "logs", args.json),
        }
    }

    /// `slipfork logs -r <id>`: replay one recorded step run through the indexed reader.
    fn logs_run(&self, args: &LogsArgs, run: &str, last: Option<usize>) -> ExitCode {
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
        let mut renderer = StreamRenderer::replay(self.renderer(), args.timestamps).tail(last);
        match client.call_streaming("logs", params, &mut |event| renderer.event(event)) {
            Ok(_) => {
                renderer.finish(0);
                write_status(&renderer)
            }
            Err(_) if INTERRUPTED.load(Ordering::SeqCst) => {
                renderer.finish(0);
                write_status(&renderer)
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

/// A finished render succeeds unless a write failed for a reason other than a closed pipe.
fn write_status(renderer: &StreamRenderer) -> ExitCode {
    if renderer.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `--last` accepts digits only and at least one line; every other spelling is our own refusal.
fn parse_last(raw: &str) -> Option<usize> {
    let digits = !raw.is_empty() && raw.bytes().all(|byte| byte.is_ascii_digit());
    digits
        .then(|| raw.parse::<usize>().ok())
        .flatten()
        .filter(|count| *count > 0)
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
        "no-failed-run" => "slipfork up".to_owned(),
        "no-run" => "slipfork up".to_owned(),
        "unknown-run" => "slipfork logs --runs".to_owned(),
        "world-booting" => "wait for `slipfork up` to finish, then retry".to_owned(),
        _ => "run `slipfork status` to inspect the world".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{empty_runs_notice, parse_last, skipped_notice};

    /// `--last` is digits only and at least one; `+5`, ` 5`, and `0` are our refusal.
    #[test]
    fn last_accepts_only_positive_digit_counts() {
        assert_eq!(parse_last("1"), Some(1));
        assert_eq!(parse_last("25"), Some(25));
        assert_eq!(parse_last("0"), None);
        assert_eq!(parse_last(""), None);
        assert_eq!(parse_last("+5"), None);
        assert_eq!(parse_last(" 5"), None);
        assert_eq!(parse_last("5 "), None);
        assert_eq!(parse_last("five"), None);
    }

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
