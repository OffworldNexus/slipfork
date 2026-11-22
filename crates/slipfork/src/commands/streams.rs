//! `logs`, `shell`, and `exec`: streaming verbs rendered from typed events.
//!
//! The daemon sends `{"kind","pod","stream","line"}` events plus one `roster` event that fixes the
//! gutter width. This renderer owns dividers, `--json`, and the final `{"exit":n}`. A terminal
//! `shell` receives the world's real `/dev/pts/N` master and relays raw bytes untouched; a pipe
//! relays stdin as base64 input frames, so `psql`, `redis-cli`, and `sh` work over a plain pipe.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::os::fd::AsRawFd;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use slipfork_world::pod_log::now_ms;

use crate::commands::{Session, emit};
use crate::daemon::{DaemonClient, DaemonError};
use crate::ui::Palette;

/// Set by the SIGINT handler so a followed stream exits `0` on Ctrl-C.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

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
    /// Emit one JSON object per line plus a final `{"exit":0}`.
    pub json: bool,
}

/// Parsed `shell` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetArgs {
    /// Required service, image, postgres, database, or redis id.
    pub target: String,
    /// Emit events and a final `{"exit":n}` instead of human output.
    pub json: bool,
}

/// Parsed `slipfork exec` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecArgs {
    /// Explicit service/image id, when given.
    pub target: Option<String>,
    /// Emit events and a final `{"exit":n}` instead of human output.
    pub json: bool,
    /// Command and arguments after `--`.
    pub argv: Vec<String>,
}

extern "C" fn on_sigint(_signal: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

fn install_sigint() {
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

    /// `slipfork shell <target>`: a real PTY on a terminal, a plain pipe otherwise.
    ///
    /// The daemon owns kind dispatch: the CLI sends the target id and the daemon builds argv from
    /// its live roster. A terminal receives the `/dev/pts/N` master and raw bytes pass straight
    /// through; a pipe relays `exec` stdin as base64 frames.
    pub fn shell(&self, args: &TargetArgs) -> ExitCode {
        install_sigint();
        let Some(mut client) = self.existing_daemon(args.json) else {
            return ExitCode::FAILURE;
        };
        let tty = !args.json && self.interactive() && std::io::stdin().is_terminal();
        if tty {
            let (cols, rows) = crate::commands::pty::geometry();
            let params = json!({
                "project": self.cwd(),
                "target": args.target,
                "tty": true,
                "cols": cols,
                "rows": rows,
            });
            let (id, master) = match client.call_pty("shell", params) {
                Ok(received) => received,
                Err(error) => return self.shell_fail(&error, args.json),
            };
            if let Err(error) = crate::commands::pty::attach(&mut client, &id, &master) {
                drop(master);
                return self.report(
                    "pty",
                    &error.to_string(),
                    "retry `slipfork shell` from a real terminal",
                    args.json,
                );
            }
            drop(master);
            return match client.finish_pty(&id, "shell") {
                Ok(value) => {
                    let code = value.get("exit").and_then(Value::as_i64).unwrap_or(0);
                    ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
                }
                Err(error) => self.shell_fail(&error, args.json),
            };
        }
        let params = json!({
            "project": self.cwd(),
            "target": args.target,
            "tty": false,
        });
        let mut renderer = Renderer::exec(self.color(), args.json);
        let stdin = std::io::stdin();
        let input_fd = stdin.as_raw_fd();
        let mut input = stdin.lock();
        let outcome = client.call_duplex("shell", params, &mut input, input_fd, &mut |event| {
            renderer.event(event)
        });
        match outcome {
            Ok(value) => {
                let code = value.get("exit").and_then(Value::as_i64).unwrap_or(0);
                renderer.finish(code);
                ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
            }
            Err(_) if INTERRUPTED.load(Ordering::SeqCst) => {
                renderer.finish(130);
                ExitCode::from(130)
            }
            Err(error) => self.shell_fail(&error, args.json),
        }
    }

    /// Renders a daemon shell refusal; a kind refusal is already the exact `1 line + next:` copy.
    fn shell_fail(&self, error: &DaemonError, json: bool) -> ExitCode {
        self.exact_fail(error, "shell", json)
    }

    /// Renders a `shell-refused` payload verbatim and everything else through [`Self::stream_fail`].
    fn exact_fail(&self, error: &DaemonError, verb: &str, json: bool) -> ExitCode {
        if json {
            let next = refusal_next(error, verb);
            return self.report(&error.code, &error.message, &next, true);
        }
        if error.code == "shell-refused" {
            eprintln!("{}", error.message);
            return ExitCode::FAILURE;
        }
        self.stream_fail(error, verb, false)
    }

    /// `slipfork exec [service|image] -- <cmd>`: forwards the child's exit code and stdin.
    pub fn exec(&self, args: &ExecArgs) -> ExitCode {
        let params = json!({
            "project": self.cwd(),
            "target": args.target,
            "argv": args.argv,
            "stdin": true,
            "select": "any",
        });
        self.exec_like("exec", params, args.json, "exec")
    }

    fn exec_like(&self, method: &str, params: Value, json: bool, verb: &str) -> ExitCode {
        install_sigint();
        let Some(mut client) = self.existing_daemon(json) else {
            return ExitCode::FAILURE;
        };
        let mut renderer = Renderer::exec(self.color(), json);
        let stdin = std::io::stdin();
        let input_fd = stdin.as_raw_fd();
        let mut input = stdin.lock();
        let outcome = client.call_duplex(method, params, &mut input, input_fd, &mut |event| {
            renderer.event(event)
        });
        match outcome {
            Ok(value) => {
                let code = value.get("exit").and_then(Value::as_i64).unwrap_or(0);
                renderer.finish(code);
                ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
            }
            Err(_) if INTERRUPTED.load(Ordering::SeqCst) => {
                renderer.finish(130);
                ExitCode::from(130)
            }
            Err(error) => self.stream_fail(&error, verb, json),
        }
    }

    fn existing_daemon(&self, json: bool) -> Option<DaemonClient> {
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

    fn stream_fail(&self, error: &DaemonError, verb: &str, json: bool) -> ExitCode {
        let next = refusal_next(error, verb);
        self.report(&error.code, &error.message, &next, json)
    }
}

/// The one remedy for a typed streaming refusal.
fn refusal_next(error: &DaemonError, verb: &str) -> String {
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
struct Renderer {
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
    fn exec(color: bool, json: bool) -> Self {
        Self::with_gutter(color, json, false, false)
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

    fn event(&mut self, event: &Value) {
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
                let _ = emit(&format!("{line}\n"));
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
                let pad = " ".repeat(self.width.saturating_sub(pod.chars().count()));
                let gutter = self.palette.tint(pod, code);
                let divider = self.palette.divider();
                let _ = emit(&format!("{stamp}{gutter}{pad} {divider} {line}\n"));
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

    fn finish(&self, code: i64) {
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

/// clap value parser for `--since`: relative (`30s|5m|2h|1d`) or an RFC 3339 timestamp.
pub fn parse_since(text: &str) -> Result<u64, String> {
    parse_time(text)
}

/// clap value parser for `--until`: relative (`30s|5m|2h|1d`) or an RFC 3339 timestamp.
pub fn parse_until(text: &str) -> Result<u64, String> {
    parse_time(text)
}

fn parse_time(text: &str) -> Result<u64, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("empty time value; use `30s|5m|2h|1d` or an RFC 3339 timestamp".to_owned());
    }
    if let Some(delta) = relative_ms(text) {
        return Ok(now_ms().saturating_sub(delta));
    }
    parse_rfc3339_ms(text).ok_or_else(|| {
        format!(
            "`{text}` is not `30s|5m|2h|1d` or RFC 3339 like `2026-11-22T12:00:00Z` \
             (next: `slipfork logs api --since 5m`)"
        )
    })
}

fn relative_ms(text: &str) -> Option<u64> {
    if !text.is_ascii() {
        return None;
    }
    let (number, unit) = text.split_at(text.len().checked_sub(1)?);
    let factor = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    number.parse::<u64>().ok()?.checked_mul(factor)
}

fn parse_rfc3339_ms(text: &str) -> Option<u64> {
    let text = text.trim();
    if !text.is_ascii() || text.len() < 20 {
        return None;
    }
    let year = i64::from(digits(&text[0..4])?);
    if &text[4..5] != "-" || &text[7..8] != "-" {
        return None;
    }
    let month = digits(&text[5..7])?;
    let day = digits(&text[8..10])?;
    if !matches!(text.as_bytes()[10], b'T' | b't' | b' ') {
        return None;
    }
    if &text[13..14] != ":" || &text[16..17] != ":" {
        return None;
    }
    let hour = digits(&text[11..13])?;
    let minute = digits(&text[14..16])?;
    let second = digits(&text[17..19])?;
    if month == 0
        || month > 12
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let mut rest = &text[19..];
    let mut frac = 0_u64;
    if let Some(stripped) = rest.strip_prefix('.') {
        let fraction: String = stripped
            .chars()
            .take_while(|character| character.is_ascii_digit())
            .collect();
        if fraction.is_empty() {
            return None;
        }
        let mut padded = fraction.clone();
        padded.truncate(3);
        while padded.len() < 3 {
            padded.push('0');
        }
        frac = padded.parse().ok()?;
        rest = &stripped[fraction.len()..];
    }
    let offset = match rest {
        "" | "Z" | "z" => 0_i64,
        _ => {
            let bytes = rest.as_bytes();
            let sign = match bytes[0] {
                b'+' => 1_i64,
                b'-' => -1_i64,
                _ => return None,
            };
            let body = &rest[1..];
            let (hours, minutes) = if body.len() == 5 && body.as_bytes()[2] == b':' {
                (&body[0..2], &body[3..5])
            } else if body.len() == 4 {
                (&body[0..2], &body[2..4])
            } else {
                return None;
            };
            let hours = i64::from(digits(hours)?);
            let minutes = i64::from(digits(minutes)?);
            if hours > 23 || minutes > 59 {
                return None;
            }
            sign * (hours * 3600 + minutes * 60)
        }
    };
    let days = days_from_civil(year, month, day);
    let seconds =
        days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second)
            - offset;
    if seconds < 0 {
        return None;
    }
    Some(seconds as u64 * 1000 + frac)
}

fn digits(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let month = i64::from(month);
    let day = i64::from(day);
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = (days - era * 146_097) as u32;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = i64::from(year_of_era) + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// UTC `YYYY-MM-DD HH:MM:SS` gutter for `-t`, independent of the host timezone.
fn format_ts(ts_ms: u64) -> String {
    let seconds = (ts_ms / 1000) as i64;
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        time / 3600,
        time % 3600 / 60,
        time % 60
    )
}
