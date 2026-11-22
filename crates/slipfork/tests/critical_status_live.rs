//! Live lab journey: a real world's services, ports, and routes through `status` and `ps`, plus a
//! real PTY boot pane that proves Braille/bar colour appears only when the terminal allows it.
//!
//! Builds a real Btrfs subvolume world from a `Forkfile`, boots it with `slipfork up` under
//! `script -qec`, and asserts the status tree and scoped `ps` show exactly the declared resources.
//! The lab is opt-in: without `SLIPFORK_TEST_ROOT` the test fails closed with the exact remedy
//! instead of degrading.

use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use slipfork_world::{
    BtrfsEngine, Dirtiness, STORE_DIR, StoreLayout, WorldEngine, WorldName, is_btrfs_path,
};

const FORKFILE: &str = "# forkfile: 0.1\n\
postgres pg {\n  version 18\n}\n\
redis cache\n\
s3 uploads {\n  policy public-read\n  cdn assets-cdn\n}\n";

const TAIL_FORKFILE: &str = "# forkfile: 0.1\n\
postgres pg {\n  version 18\n}\n\
redis cache\n\
s3 uploads {\n  policy public-read\n  cdn assets-cdn\n}\n\
image box from python:3.13-slim {\n  pre-deploy: sh -c 'echo rolling-tail-1; sleep 0.3; echo rolling-tail-2'\n  service worker {\n    run: sleep 3600\n  }\n}\n";

const BRAILLE: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

fn test_root() -> PathBuf {
    let Some(root) = std::env::var_os("SLIPFORK_TEST_ROOT").filter(|root| !root.is_empty()) else {
        panic!(
            "SLIPFORK_TEST_ROOT is unset or empty\nnext: SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test"
        )
    };
    let root = PathBuf::from(root);
    if let Err(source) = fs::create_dir_all(&root) {
        panic!(
            "fail closed: cannot create `{}`: {source}\nnext: sudo chown -R \"$(id -u):$(id -g)\" {}",
            root.display(),
            root.display()
        );
    }
    assert!(
        is_btrfs_path(&root).unwrap_or(false),
        "fail closed: `{}` is not on Btrfs.\nnext: SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test",
        root.display()
    );
    root
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from("/dev/shm").join(format!("slipfork-live-{}", std::process::id()))
        })
}

fn mount_store_for(path: &Path) -> PathBuf {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").expect("read /proc/self/mountinfo");
    let mut best: Option<(PathBuf, usize)> = None;
    for line in mountinfo.lines() {
        let Some((head, _)) = line.split_once(" - ") else {
            continue;
        };
        let Some(raw) = head.split_ascii_whitespace().nth(4) else {
            continue;
        };
        let bytes = raw.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\'
                && i + 3 < bytes.len()
                && bytes[i + 1..=i + 3]
                    .iter()
                    .all(|b| (b'0'..=b'7').contains(b))
            {
                out.push(
                    ((bytes[i + 1] - b'0') << 6)
                        | ((bytes[i + 2] - b'0') << 3)
                        | (bytes[i + 3] - b'0'),
                );
                i += 4;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        let mount = PathBuf::from(OsString::from_vec(out));
        if canonical.starts_with(&mount) {
            let depth = mount.components().count();
            if best.as_ref().is_none_or(|(_, prev)| depth >= *prev) {
                best = Some((mount, depth));
            }
        }
    }
    best.map_or_else(|| PathBuf::from("/"), |(mount, _)| mount)
        .join(STORE_DIR)
}

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_slipfork")
}

fn run(project: &Path, runtime: &Path, args: &[&str]) -> Output {
    Command::new(binary())
        .args(args)
        .current_dir(project)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("NO_COLOR", "1")
        .output()
        .expect("the slipfork binary must run")
}

/// Runs `args` under a real 120-column PTY; `script` is a lab requirement, never a mock.
fn run_pty(project: &Path, runtime: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let available = Command::new("script")
        .arg("--version")
        .output()
        .expect("the live PTY journey needs `script`(1)")
        .status
        .success();
    assert!(
        available,
        "`script --version` must succeed for the PTY probe"
    );
    let bin = binary().replace('\'', r"'\''");
    let line = format!(
        "stty cols 120 rows 40 2>/dev/null; '{bin}' {}",
        args.join(" ")
    );
    let mut command = Command::new("script");
    command.args(["-qec", &line, "/dev/null"]);
    command
        .current_dir(project)
        .env("XDG_RUNTIME_DIR", runtime)
        .env_remove("NO_COLOR")
        .env("TERM", "xterm-256color");
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().expect("run the PTY probe")
}

/// Every maximal run of bar cells, so a sheared or multi-width bar cannot pass unnoticed.
fn bar_runs(text: &str) -> Vec<usize> {
    let mut runs = Vec::new();
    let mut run = 0;
    for character in text.chars() {
        if matches!(character, '█' | '░') {
            run += 1;
        } else if run > 0 {
            runs.push(run);
            run = 0;
        }
    }
    if run > 0 {
        runs.push(run);
    }
    runs
}

/// The one width every anchored bar draw must occupy.
const BAR_CELLS: usize = 16;

/// Every anchored summary frame for `title`, normalized to `[<cells>] done/total  title  elapsed`.
///
/// Frames are redrawn in place (`\r` already stripped), so one line can hold several; each `[`
/// that starts a 16-cell bar followed by a counter and the title is one real, complete draw.
fn bar_frames(text: &str, title: &str) -> Vec<String> {
    let mut frames = Vec::new();
    for (open, _) in text.match_indices('[') {
        let rest = &text[open..];
        let Some(close) = rest.find(']') else {
            continue;
        };
        let cells = &rest[1..close];
        if cells.chars().count() != BAR_CELLS
            || !cells.chars().all(|cell| matches!(cell, '█' | '░'))
        {
            continue;
        }
        let Some(after) = rest[close + 1..].strip_prefix(' ') else {
            continue;
        };
        let Some((counter, after)) = after.split_once("  ") else {
            continue;
        };
        if !counter.contains('/')
            || !counter
                .chars()
                .all(|character| character.is_ascii_digit() || character == '/')
        {
            continue;
        }
        let Some(elapsed) = after.strip_prefix(title).and_then(|after| {
            after
                .trim_start()
                .split_whitespace()
                .next()
                .filter(|elapsed| elapsed.ends_with('s'))
        }) else {
            continue;
        };
        frames.push(format!("[{cells}] {counter}  {title}  {elapsed}"));
    }
    frames
}

/// Removes every escape sequence so rows can be asserted exactly as a user reads them.
///
/// A CSI is `ESC [` then parameter/intermediate bytes and only ends on the final byte
/// `0x40..=0x7e`; treating `[` itself as the final byte would leak `0;36m` into the row.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            out.push(character);
            continue;
        }
        if chars.clone().next() == Some('[') {
            let _ = chars.next();
            for byte in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&byte) {
                    break;
                }
            }
        } else {
            // A two-byte escape (`ESC =`, `ESC O …`): drop the introducer payload.
            let _ = chars.next();
        }
    }
    out
}

fn daemon_alive(runtime: &Path) -> bool {
    let scope = runtime.as_os_str().as_bytes();
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if !cmdline.windows(b"__daemon".len()).any(|w| w == b"__daemon") {
            continue;
        }
        if let Ok(environ) = fs::read(entry.path().join("environ"))
            && environ.windows(scope.len()).any(|w| w == scope)
        {
            return true;
        }
    }
    false
}

/// Stops the world and its daemon on every exit path, including a failed assertion.
struct WorldGuard {
    project: PathBuf,
    runtime: PathBuf,
    store_root: PathBuf,
}

impl Drop for WorldGuard {
    fn drop(&mut self) {
        if self.runtime.join("daemon.sock").exists() {
            let _ = run(&self.project, &self.runtime, &["down"]);
            if let Ok(mut stream) = UnixStream::connect(self.runtime.join("daemon.sock")) {
                let _ = stream.write_all(b"{\"v\":1,\"id\":\"bye\",\"method\":\"shutdown\"}\n");
                let mut reply = [0_u8; 256];
                let _ = stream.read(&mut reply);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && daemon_alive(&self.runtime) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let engine = BtrfsEngine::new(StoreLayout::at(&self.store_root));
        if let Ok(world) = WorldName::parse("status-live-demo") {
            let _ = engine.drop_world(&world);
        }
        let _ = engine.destroy(&self.project, Dirtiness::Clean, true);
        let _ = fs::remove_dir_all(&self.project);
    }
}

#[test]
fn live_status_and_ps_show_the_real_world() {
    let root = test_root();
    let store_root = mount_store_for(&root);
    let runtime = runtime_dir();
    fs::create_dir_all(&runtime).expect("create the live runtime directory");
    let project = root.join("status-live-demo");
    let engine = BtrfsEngine::new(StoreLayout::at(&store_root));
    if let Ok(world) = WorldName::parse("status-live-demo") {
        let _ = engine.drop_world(&world);
    }
    let _ = engine.destroy(&project, Dirtiness::Clean, true);
    let _ = fs::remove_dir_all(&project);
    fs::create_dir_all(&project).expect("create the live project");
    fs::write(project.join("Forkfile"), FORKFILE).expect("write the live Forkfile");
    let _guard = WorldGuard {
        project: project.clone(),
        runtime: runtime.clone(),
        store_root,
    };

    // The first boot is observed on a real colour PTY: Braille, one 16-cell bar, and a finished row.
    let boot = run_pty(&project, &runtime, &["up"], &[]);
    assert!(
        boot.status.success(),
        "PTY `slipfork up` failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&boot.stdout),
        String::from_utf8_lossy(&boot.stderr)
    );
    let captured = String::from_utf8_lossy(&boot.stdout).replace('\r', "");
    assert!(
        BRAILLE.chars().any(|glyph| captured.contains(glyph)),
        "a colour PTY must show the Braille spinner: {captured:?}"
    );
    let runs = bar_runs(&captured);
    assert!(
        !runs.is_empty(),
        "a colour PTY must show the bar: {captured:?}"
    );
    assert!(
        runs.iter().all(|cells| *cells == 16),
        "every bar draw is exactly 16 cells: {runs:?} in {captured:?}"
    );
    let readable = plain(&captured);
    let frame = bar_frames(&readable, "Booting status-live-demo");
    assert!(
        frame.iter().any(|line| line.starts_with('[')),
        "the anchored `[████░░] done/total  Booting <world>  <elapsed>` frame must be captured: {frame:?} in {readable}"
    );
    // The world declares no image, so the finished-row proof names the golden PostgreSQL resource
    // the pane actually drew (`\r` is stripped above, so rows can follow bar remnants on a line).
    assert!(
        readable.contains("✓ resource") && readable.contains("pg ready"),
        "the boot must print a finished row for the golden PostgreSQL resource: {readable}"
    );

    let status = run(&project, &runtime, &["status", "--json"]);
    assert!(status.status.success(), "{status:?}");
    let status_json: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("status --json parses");
    let world = &status_json["worlds"][0];
    assert_eq!(world["url"], "http://status-live-demo.localhost");
    let ids: Vec<&str> = world["services"]
        .as_array()
        .expect("services array")
        .iter()
        .filter_map(|service| service["id"].as_str())
        .collect();
    assert!(ids.contains(&"pg"), "{ids:?}");
    assert!(ids.contains(&"cache"), "{ids:?}");
    assert!(ids.contains(&"uploads"), "{ids:?}");
    let uploads = world["services"]
        .as_array()
        .expect("services")
        .iter()
        .find(|service| service["id"] == "uploads")
        .expect("uploads row");
    assert_eq!(uploads["cdn"], "assets-cdn");
    assert_eq!(uploads["policy"], "public-read");
    assert_eq!(uploads["running"], true);

    let human = run(&project, &runtime, &["status"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).expect("status is UTF-8");
    assert!(human.contains("● status-live-demo"), "{human}");
    assert!(human.contains("postgres 18"), "{human}");
    assert!(human.contains("redis 7"), "{human}");
    assert!(human.contains("s3 (public-read)"), "{human}");
    assert!(!human.contains('\u{1b}'), "{human}");

    let ps = run(&project, &runtime, &["ps", "--json"]);
    assert!(ps.status.success());
    let ps_json: serde_json::Value = serde_json::from_slice(&ps.stdout).expect("ps --json parses");
    assert_eq!(ps_json["worlds"].as_array().map(Vec::len), Some(1));
    assert_eq!(ps_json["worlds"][0]["services"][0]["kind"], "postgres");

    let scoped = run(&project, &runtime, &["ps"]);
    assert!(scoped.status.success());
    assert!(String::from_utf8_lossy(&scoped.stdout).contains("postgres 18"));

    // A hook-bearing boot on the colour PTY must also render the rolling `│` tail, while
    // `NO_COLOR=1` and `TERM=dumb` PTY runs emit zero `ESC[` bytes.
    let stop_initial = run(&project, &runtime, &["down"]);
    assert!(stop_initial.status.success(), "{stop_initial:?}");
    fs::write(project.join("Forkfile"), TAIL_FORKFILE).expect("write the hook Forkfile");
    let tail_boot = run_pty(&project, &runtime, &["up"], &[]);
    assert!(
        tail_boot.status.success(),
        "PTY `slipfork up` with hook failed: {}",
        String::from_utf8_lossy(&tail_boot.stderr)
    );
    let tail_readable = plain(&String::from_utf8_lossy(&tail_boot.stdout).replace('\r', ""));
    assert!(
        tail_readable.contains('│') && tail_readable.contains("rolling-tail-"),
        "the colour PTY must render the hook's rolling tail behind its `│` gutter: {tail_readable}"
    );

    let down_for_plain = run(&project, &runtime, &["down"]);
    assert!(down_for_plain.status.success(), "{down_for_plain:?}");
    for (args, envs) in [
        (&["up"][..], &[("NO_COLOR", "1")][..]),
        (&["restart"][..], &[("TERM", "dumb")][..]),
    ] {
        let output = run_pty(&project, &runtime, args, envs);
        assert!(
            output.status.success(),
            "{args:?} on a plain PTY failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("\u{1b}["),
            "{args:?} with {envs:?} emitted an escape: {:?}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    // A bounded wait proves the world is reaped before this test returns.
    let deadline = Instant::now() + Duration::from_secs(60);
    let _ = run(&project, &runtime, &["down"]);
    while Instant::now() < deadline {
        let ps = run(&project, &runtime, &["ps", "--json"]);
        if ps.status.success() && String::from_utf8_lossy(&ps.stdout).contains("\"worlds\":[]") {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
