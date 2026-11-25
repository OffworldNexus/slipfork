//! Real `openpty` captures of the append-only boot renderer at 110×30 and 80×24.
//!
//! The visible tests re-execute this same test binary with `--exact boot_render_fixture
//! --ignored` under a sized PTY; the fixture drives the shipped [`BootTui`] with production wire
//! frames (`plan`, `started`, `step`, `log`, `finished`, `failed`) and writes to the slave.

#![allow(clippy::too_many_arguments)]

#[path = "support/vt.rs"]
mod vt;

use std::fs;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use slipfork::boot::BootTui;
use slipfork::ui::UiMode;

const PROMPT: &str = "slipfork@slipfork-dev-vm:~/demo$ slipfork up";
const RUN_TIMEOUT: Duration = Duration::from_secs(240);

struct PtyRun {
    output: String,
    status: ExitStatus,
    /// Byte offset at which the mid-run `TIOCSWINSZ` narrowed the terminal.
    resize_offset: Option<usize>,
}

/// A local, configurable `openpty` runner (fixed 120×40 lives in `tests/support/pty.rs`).
fn run_pty(
    mut command: Command,
    rows: u16,
    cols: u16,
    resize: Option<(Duration, u16, u16)>,
) -> PtyRun {
    let mut master = 0;
    let mut slave = 0;
    let winsize = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `openpty` fills both descriptors and reads one valid `winsize`.
    let allocated = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &winsize,
        )
    };
    assert_eq!(allocated, 0, "openpty must allocate a real PTY");
    // SAFETY: both descriptors are freshly owned by this process.
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    // SAFETY: both descriptors are freshly owned by this process.
    let master = unsafe { fs::File::from_raw_fd(master) };
    command
        .stdin(Stdio::from(slave.try_clone().expect("clone PTY slave")))
        .stdout(Stdio::from(slave.try_clone().expect("clone PTY slave")))
        .stderr(Stdio::from(slave));
    // SAFETY: the closure only performs `setsid(2)`/`ioctl(TIOCSCTTY)` between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("spawn the PTY child");
    drop(command);
    let finished = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&finished);
    let reader = thread::spawn(move || {
        let mut master = master;
        let mut out = Vec::new();
        let started = Instant::now();
        let mut resized = false;
        let mut resize_offset = None;
        let mut chunk = [0_u8; 8192];
        loop {
            if let Some((delay, new_rows, new_cols)) = resize
                && !resized
                && started.elapsed() >= delay
            {
                let size = libc::winsize {
                    ws_row: new_rows,
                    ws_col: new_cols,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };
                // SAFETY: the master descriptor stays open until this reader thread finishes.
                unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
                resized = true;
                resize_offset = Some(out.len());
            }
            let mut poll = libc::pollfd {
                fd: master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid `pollfd` lives across the call; the tick bounds the wait.
            let ready = unsafe { libc::poll(&mut poll, 1, 25) };
            if ready < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            if ready == 0 {
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                continue;
            }
            match master.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    out.extend_from_slice(&chunk[..read]);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    continue;
                }
                Err(_) => break,
            }
        }
        (out, resize_offset)
    });
    let status = wait_bounded(&mut child);
    finished.store(true, Ordering::Relaxed);
    let (bytes, resize_offset) = reader.join().expect("join the PTY reader");
    PtyRun {
        output: String::from_utf8_lossy(&bytes).into_owned(),
        status,
        resize_offset,
    }
}

fn wait_bounded(child: &mut std::process::Child) -> ExitStatus {
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {}
            Err(error) => panic!("cannot wait for the PTY child: {error}"),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "the PTY child did not finish within {}s",
                RUN_TIMEOUT.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(25));
    }
}

// ---------------------------------------------------------------------------
// The shipped renderer, driven by production wire frames.
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn boot_render_fixture() {
    let fixture = std::env::var("SLIPFORK_BOOT_FIXTURE").expect("SLIPFORK_BOOT_FIXTURE");
    let width = std::env::var("SLIPFORK_BOOT_WIDTH")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(80usize);
    let pace = std::env::var("SLIPFORK_BOOT_PACE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let mut tui = BootTui::boot(UiMode::Interactive { color: true }, width, "boot", false);
    for frame in frames(&fixture) {
        tui.observe(&frame);
        if pace > 0 {
            thread::sleep(Duration::from_millis(pace));
        }
    }
    tui.finish();
}

const INSTRUCTIONS: &[(&str, &str)] = &[
    ("FROM", "python:3.14-trixie"),
    ("COPY", "--from=ghcr.io/astral-sh/uv:latest /uv /uvx /bin/"),
    ("RUN", "apt-get update && apt-get install -y curl"),
    ("COPY", ". /app"),
    ("RUN", "pip install -r requirements.txt"),
    ("ENV", "PYTHONUNBUFFERED=1"),
    ("RUN", "python -m compileall /app"),
    ("COPY", "templates /app/templates"),
    ("RUN", "pytest -q"),
    ("WORKDIR", "/app"),
    ("CMD", "uv run blastit"),
];

fn plan(rows: &[(&str, &str)], total: u64) -> Value {
    let rows: Vec<Value> = rows
        .iter()
        .map(|(kind, name)| json!({ "kind": kind, "name": name }))
        .collect();
    json!({ "event": "plan", "total": total, "rows": rows })
}

fn image_started(step: u64, total: u64) -> Value {
    json!({
        "event": "started",
        "step": step,
        "total": total,
        "kind": "image",
        "name": "api",
    })
}

fn step(image: &str, index: u64, total: u64, instruction: &str, args: &str, state: &str) -> Value {
    json!({
        "event": "step",
        "image": image,
        "index": index,
        "total": total,
        "instruction": instruction,
        "args": args,
        "state": state,
        "elapsed_ms": if state == "done" { 4 } else { 0 },
        "instruction_width": 7,
    })
}

fn finished(executed: u64) -> Value {
    json!({
        "event": "finished",
        "kind": "image",
        "name": "api",
        "state": "built",
        "executed": executed,
        "duration_ms": 2500,
    })
}

fn service_finished() -> Value {
    json!({
        "event": "finished",
        "step": 2,
        "total": 2,
        "kind": "service",
        "name": "web",
        "state": "running",
        "duration_ms": 1200,
    })
}

fn frames(fixture: &str) -> Vec<Value> {
    match fixture {
        "cached" => vec![
            plan(&[("image", "api"), ("service", "web")], 2),
            image_started(1, 2),
            json!({
                "event": "up-to-date",
                "kind": "image",
                "name": "api",
                "state": "cached",
                "duration_ms": 8,
            }),
        ],
        "fresh" | "resize" => {
            let total = u64::try_from(INSTRUCTIONS.len()).expect("tiny total");
            let mut frames = vec![
                plan(&[("image", "api"), ("service", "web")], 2),
                image_started(1, 2),
                json!({
                    "event": "started",
                    "step": 2,
                    "total": 2,
                    "kind": "service",
                    "name": "web",
                    "command": "uv run blastit dev",
                }),
            ];
            // The parallel `COPY --from` (2) announces before `FROM` (1); every step is announced
            // before the completions, which arrive out of order (2 before 1, 6 before 5).
            for index in [2u64, 1, 3, 4, 5, 6, 7, 8, 9, 10, 11] {
                let (instruction, args) = INSTRUCTIONS[usize::try_from(index - 1).expect("tiny")];
                frames.push(step("api", index, total, instruction, args, "started"));
                if index == 5 {
                    // Only this step and the outer service stream child output, and their lines
                    // interleave: each key still earns its one `→` header.
                    for (event, stream, line) in [
                        (
                            json!({ "event": "log", "image": "api", "index": index, "stream": "stdout" }),
                            "stdout",
                            "Collecting pip",
                        ),
                        (
                            json!({ "event": "log", "kind": "service", "name": "web", "stream": "stderr" }),
                            "stderr",
                            "web listening",
                        ),
                        (
                            json!({ "event": "log", "image": "api", "index": index, "stream": "stdout" }),
                            "stdout",
                            "Successfully installed pip",
                        ),
                        (
                            json!({ "event": "log", "kind": "service", "name": "web", "stream": "stdout" }),
                            "stdout",
                            "blastit dev ready",
                        ),
                    ] {
                        let mut frame = event;
                        frame["stream"] = json!(stream);
                        frame["line"] = json!(line);
                        frames.push(frame);
                    }
                    frames.push(json!({
                        "event": "step",
                        "image": "api",
                        "index": index,
                        "total": total,
                        "instruction": instruction,
                        "args": args,
                        "state": "tail",
                        "phase": "sealing",
                        "elapsed_ms": 3100,
                    }));
                }
            }
            for index in [2u64, 1, 3, 4, 6, 5, 7, 8, 9, 10, 11] {
                let (instruction, args) = INSTRUCTIONS[usize::try_from(index - 1).expect("tiny")];
                frames.push(step("api", index, total, instruction, args, "done"));
            }
            frames.push(finished(total));
            frames.push(service_finished());
            frames
        }
        "failing" => {
            let mut frames = vec![
                plan(&[("image", "api"), ("service", "web")], 2),
                image_started(1, 2),
            ];
            for (position, (instruction, args)) in INSTRUCTIONS.iter().take(3).enumerate() {
                let index = u64::try_from(position + 1).expect("tiny index");
                let total = u64::try_from(INSTRUCTIONS.len()).expect("tiny total");
                frames.push(step("api", index, total, instruction, args, "started"));
                frames.push(step("api", index, total, instruction, args, "done"));
            }
            let total = u64::try_from(INSTRUCTIONS.len()).expect("tiny total");
            frames.push(step(
                "api",
                4,
                total,
                "RUN",
                "apt-get install -y blastit",
                "started",
            ));
            for (stream, line) in [
                ("stdout", "installing blastit"),
                ("stderr", "E: Unable to locate package blastit"),
            ] {
                frames.push(json!({
                    "event": "log",
                    "image": "api",
                    "index": 4,
                    "stream": stream,
                    "line": line,
                }));
            }
            frames.push(json!({
                "event": "failed",
                "phase": "run",
                "target": "api",
                "exit_code": 1,
                "run_id": "5be421",
                "next": "slipfork logs -r 5be421",
            }));
            frames
        }
        other => panic!("unknown fixture {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Visible PTY captures.
// ---------------------------------------------------------------------------

fn spawn(fixture: &str, rows: u16, cols: u16, resize: Option<(Duration, u16, u16)>) -> PtyRun {
    let exe = std::env::current_exe().expect("the test binary path");
    let mut command = Command::new(exe);
    command.args(["--exact", "boot_render_fixture", "--ignored", "--nocapture"]);
    command
        .env("SLIPFORK_BOOT_FIXTURE", fixture)
        .env("SLIPFORK_BOOT_WIDTH", cols.to_string())
        .env(
            "SLIPFORK_BOOT_PACE_MS",
            if resize.is_some() { "120" } else { "0" },
        )
        .env_remove("NO_COLOR")
        .env("TERM", "xterm-256color");
    run_pty(command, rows, cols, resize)
}

fn assert_prompt(output: &str, cols: u16) {
    let mut screen = vt::Screen::new(200, usize::from(cols));
    screen.place(199, 0, PROMPT);
    // Start the child's stream at the top; the seeded prompt must stay untouched at row 199.
    screen.feed(b"\x1b[1;1H");
    screen.feed(output.as_bytes());
    assert_eq!(
        screen.find_row(PROMPT),
        Some(199),
        "lost the seeded prompt:\n{}",
        screen.text()
    );
    assert_eq!(
        screen.row_text(199),
        PROMPT,
        "the seeded prompt was mutated"
    );
}

fn assert_fixture(fixture: &str, rows: u16, cols: u16, expected: &[&str]) -> PtyRun {
    let run = spawn(fixture, rows, cols, None);
    assert!(
        run.status.success(),
        "fixture {fixture:?} failed: {:?}",
        run.output
    );
    assert!(run.output.ends_with('\n'), "{:?}", run.output);
    assert_append_only(&run.output, usize::from(cols), None);
    assert_prompt(&run.output, cols);
    assert_expected(&run.output, expected);
    if fixture == "fresh" {
        assert_all_inner_rows(&run.output);
    }
    run
}

/// Exactly one verdict per instruction (`n/11`), printed in plan index order with no repeats.
fn assert_verdict_order(output: &str) {
    let text = completed_transcript(output);
    let verdicts: Vec<&str> = text
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            (line.starts_with("✓ ") || line.starts_with("✗ ")) && line.contains("/11")
        })
        .collect();
    assert_eq!(verdicts.len(), 11, "one verdict per instruction:\n{text}");
    for (position, row) in verdicts.iter().enumerate() {
        let counter = format!("{:>2}/11", position + 1);
        assert!(
            row.contains(&counter),
            "row {position} must be {counter}: {row:?}\n{text}"
        );
    }
    let unique: std::collections::BTreeSet<&str> = verdicts.iter().copied().collect();
    assert_eq!(
        unique.len(),
        verdicts.len(),
        "duplicate verdict rows:\n{text}"
    );
}

/// Exactly one `→` header per streamed key in the whole capture, each carrying its identity.
fn assert_stream_headers(output: &str, expected: &[&str]) {
    let text = completed_transcript(output);
    let headers: Vec<&str> = text
        .lines()
        .filter(|line| line.trim_start().starts_with('→'))
        .collect();
    assert_eq!(
        headers.len(),
        expected.len(),
        "one header per streamed key:\n{text}"
    );
    for needle in expected {
        assert_eq!(
            headers
                .iter()
                .filter(|row| row.trim_start() == *needle)
                .count(),
            1,
            "{needle:?} must print exactly one header:\n{text}"
        );
    }
}

/// Every one of the eleven inner rows appears exactly once with its padded `i/11` counter.
fn assert_all_inner_rows(output: &str) {
    let text = completed_transcript(output);
    for index in 1..=11 {
        let needle = format!("  ✓ {index:>2}/11");
        assert_eq!(
            text.matches(&needle).count(),
            1,
            "{needle:?} must appear exactly once:\n{text}"
        );
    }
}

#[test]
fn cached_up_at_110x30_and_80x24_is_append_only() {
    for (rows, cols) in [(30, 110), (24, 80)] {
        let run = assert_fixture("cached", rows, cols, &["[1/2] image  api  cached  8ms"]);
        assert!(
            !completed_transcript(&run.output).contains('→'),
            "a cached step that streams nothing prints no header:\n{}",
            run.output
        );
    }
}

#[test]
fn fresh_eleven_instruction_build_at_110x30_and_80x24_is_append_only() {
    for (rows, cols) in [(30, 110), (24, 80)] {
        let run = assert_fixture(
            "fresh",
            rows,
            cols,
            &[
                "→ [1/2] image  api  running",
                "→  5/11 run      pip install -r requirements.txt",
                "→ [2/2] start  web  running  uv run blastit dev",
                "  ✓  1/11 from",
                "  ✓ 10/11",
                "built 11 steps",
                "✓ [2/2] start  web  running  uv run blastit dev  1.2s",
            ],
        );
        assert_verdict_order(&run.output);
        assert_stream_headers(
            &run.output,
            &[
                "→ [1/2] image  api  running",
                "→  5/11 run      pip install -r requirements.txt",
                "→ [2/2] start  web  running  uv run blastit dev",
            ],
        );
        let text = completed_transcript(&run.output);
        assert_eq!(
            text.matches("✓ [2/2] start  web  running").count(),
            1,
            "the streaming service row settles exactly once:\n{text}"
        );
        assert_eq!(
            text.matches("1.2s").count(),
            1,
            "the service verdict carries its speed once:\n{text}"
        );
    }
}

#[test]
fn failing_build_streams_gutter_lines_and_one_verdict_at_110x30_and_80x24() {
    for (rows, cols) in [(30, 110), (24, 80)] {
        let run = assert_fixture(
            "failing",
            rows,
            cols,
            &[
                "→ [1/2] image  api  running",
                "→  4/11 run      apt-get install -y blastit",
                "    │ installing blastit",
                "    ┃ E: Unable to locate package blastit",
                "  ✗  4/11 run      apt-get install -y blastit",
                "✗ RUN api failed (exit 1)",
                "  next: slipfork logs -r 5be421",
            ],
        );
        let text = completed_transcript(&run.output);
        assert_eq!(
            text.matches("  ✗  4/11").count(),
            1,
            "the failing step prints one `✗` row:\n{text}"
        );
        let lines: Vec<&str> = text.lines().collect();
        let verdict = lines
            .iter()
            .position(|line| line.trim_start().starts_with("✗ ") && line.contains("/11"))
            .expect("the failing instruction row");
        let above = &lines[verdict.saturating_sub(2)..verdict];
        assert!(
            above.iter().any(|line| line.contains('│')),
            "the streamed output sits above the verdict:\n{text}"
        );
    }
}

#[test]
fn a_resize_from_110x30_to_80x24_mid_run_keeps_every_row_and_narrows_the_status_line() {
    let run = spawn(
        "resize",
        30,
        110,
        Some((Duration::from_millis(150), 24, 80)),
    );
    assert!(run.status.success(), "{:?}", run.output);
    assert!(run.output.ends_with('\n'), "{:?}", run.output);
    assert_append_only(
        &run.output,
        110,
        run.resize_offset.map(|offset| (offset, 80)),
    );
    assert_prompt(&run.output, 80);
    assert_expected(
        &run.output,
        &["  ✓  1/11 from", "  ✓ 10/11", "built 11 steps"],
    );
    assert_all_inner_rows(&run.output);
    assert_verdict_order(&run.output);
    assert_stream_headers(
        &run.output,
        &[
            "→ [1/2] image  api  running",
            "→  5/11 run      pip install -r requirements.txt",
            "→ [2/2] start  web  running  uv run blastit dev",
        ],
    );
}

/// Every expected row appears exactly once, as a complete line, in arrival order.
fn assert_expected(output: &str, expected: &[&str]) {
    let text = completed_transcript(output);
    let mut cursor = 0;
    for needle in expected {
        assert_eq!(
            text.matches(needle).count(),
            1,
            "{needle:?} must appear exactly once:\n{text}"
        );
        let found = text[cursor..]
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} is out of order:\n{text}"));
        cursor += found + needle.len();
    }
}

/// Completed rows only: status frames and SGR are removed, `\r\n` becomes `\n`.
fn completed_transcript(output: &str) -> String {
    let mut out = String::new();
    let mut rest = output;
    while let Some(position) = rest.find("\r\x1b[2K") {
        out.push_str(&rest[..position]);
        let after = &rest[position + 5..];
        match (after.find('\r'), after.find('\n')) {
            (Some(cr), _) if after[cr..].starts_with("\r\n") => {
                // A completed row shares the same `\r\x1b[2K` clear prefix before its `\r\n`.
                out.push_str(&after[..cr]);
                out.push('\n');
                rest = &after[cr + 2..];
            }
            (Some(cr), _) => {
                // A status frame ends at the next `\r`; skip its content.
                rest = &after[cr..];
            }
            (_, Some(nl)) => {
                out.push_str(&after[..nl]);
                out.push('\n');
                rest = &after[nl + 1..];
            }
            (None, None) => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    strip_ansi(&out).replace("\r\n", "\n").replace('\r', "\n")
}

/// Fails on any cursor addressing, region, clear, alt-screen, or DSR byte; enforces the
/// `\r\x1b[2K<line>`-only carriage-return contract and the visible status width.
fn assert_append_only(output: &str, cols: usize, post_resize: Option<(usize, usize)>) {
    for forbidden in [
        "\x1b[r",
        "\x1b[2J",
        "\x1b[J",
        "\x1bc",
        "\x1b[?1049",
        "\x1b[?47",
        "\x1b[?1047",
        "\x1b[6n",
    ] {
        assert!(
            !output.contains(forbidden),
            "append-only output carried {forbidden:?}: {output:?}"
        );
    }
    let bytes = output.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\r' {
            if output[index + 1..].starts_with('\n') {
                index += 2;
                continue;
            }
            if output[index + 1..].starts_with("\x1b[2K") {
                let start = index + 5;
                let rest = &output[start..];
                let end = rest.find(['\r', '\n']).unwrap_or(rest.len());
                if !rest[end..].starts_with("\r\x1b[2K") && !rest[end..].is_empty() {
                    // A completed row reuses the clear prefix; only true status frames are sized.
                    index = start;
                    continue;
                }
                let width = console::measure_text_width(&strip_ansi(&rest[..end]));
                let limit = match post_resize {
                    Some((offset, narrow)) if start >= offset => narrow,
                    _ => cols,
                };
                assert!(
                    width <= limit,
                    "status frame is {width} cells for a {limit}-column terminal: {:?}",
                    &rest[..end]
                );
                index = start + end;
                continue;
            }
            panic!(
                "a bare carriage return outside `\\r\\x1b[2K`: {:?}",
                &output[index.saturating_sub(24)..]
            );
        }
        if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'[') {
            let mut end = index + 2;
            while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b';') {
                end += 1;
            }
            if end < bytes.len() && matches!(bytes[end], b'H' | b'f' | b'r' | b'J') {
                panic!("append-only output addressed a screen cell: {output:?}");
            }
            index = end;
            continue;
        }
        index += 1;
    }
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for ch in chars.by_ref() {
                if ('@'..='~').contains(&ch) {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}
