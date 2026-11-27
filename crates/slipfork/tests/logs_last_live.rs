//! Live proof that `logs --last <N>` tails rendered lines and `-f` implies 20.
//!
//! One Btrfs project, one real `up`, one service that prints 100 historical lines and then waits
//! for a marker file before printing live lines. The journey proves: `--last 25` prints exactly
//! the last 25 historical lines, `-f` starts with the last 20 and then appends live lines,
//! `--last 0` fails closed with one line plus one `next:`, `--json --last 3` emits exactly three
//! line records plus the exit trailer with zero `ESC`, and `logs --last 25 | head` stays quiet.
//!
//! The suite fails closed on a non-Btrfs host or a missing `crun`/`unshare`/`slirp4netns`: a
//! skipped live check would hide a broken tail forever.

#![cfg(feature = "live")]

use std::fs;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use slipfork_world::{BtrfsEngine, LogStore, StoreLayout, WorldEngine, WorldName};

const BTRFS_SUPER_MAGIC: u64 = 0x9123_683E;
const TMPFS_MAGIC: u64 = 0x0102_1994;
/// The service prints 100 history lines, then waits for `.logs-go` before its two live lines.
const HISTORY: u32 = 100;
const FORKFILE: &str = r#"# forkfile: 0.1
image box from python:3.13-slim {
  service web {
    run: sh -c 'i=0; while [ $i -lt 100 ]; do i=$((i+1)); echo "hist-$i"; done; until [ -f /project/.logs-go ]; do sleep 0.1; done; echo live-1; echo live-2; exec sleep 300'
  }
}
"#;

fn fail_closed(message: &str) -> ! {
    panic!("fail closed: {message}");
}

fn fs_magic(path: &Path) -> u64 {
    let Ok(file) = fs::File::open(path) else {
        return 0;
    };
    // SAFETY: `statfs` is plain-old-data and the descriptor stays live across the call.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: the kernel writes one `statfs` through the valid pointer.
    let ret = unsafe { libc::fstatfs(file.as_raw_fd(), &mut stat) };
    if ret == 0 { stat.f_type as u64 } else { 0 }
}

fn scratch_root() -> PathBuf {
    let Some(root) = std::env::var_os("SLIPFORK_TEST_ROOT").filter(|root| !root.is_empty()) else {
        fail_closed(
            "SLIPFORK_TEST_ROOT is unset or empty.\nnext: SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test",
        );
    };
    let root = PathBuf::from(root);
    if fs_magic(&root) != BTRFS_SUPER_MAGIC {
        fail_closed(&format!(
            "`{}` is not on Btrfs.\nnext: mount -o user_subvol_rm_allowed,noatime <device> {}",
            root.display(),
            root.display()
        ));
    }
    root
}

fn live_run_root() -> PathBuf {
    let root = scratch_root();
    assert!(
        !root.starts_with("/tmp"),
        "fail closed: `{}` is RAM-backed tmpfs",
        root.display()
    );
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let run = root.join(format!("last-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&run).expect("create the live run root");
    run
}

fn require_tool(name: &str) -> PathBuf {
    for directory in std::env::var_os("PATH")
        .unwrap_or_default()
        .to_string_lossy()
        .split(':')
    {
        if directory.is_empty() {
            continue;
        }
        let candidate = Path::new(directory).join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    fail_closed(&format!(
        "`{name}` is not on PATH.\nnext: install {name} (crun, util-linux, slirp4netns) and re-run"
    ))
}

fn require_tmpfs_runtime_dir() -> PathBuf {
    let uid = unsafe { libc::geteuid() };
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}")));
    assert!(
        base.is_dir() && fs_magic(&base) == TMPFS_MAGIC,
        "fail closed: `{}` is not a tmpfs runtime directory.\nnext: export XDG_RUNTIME_DIR=/run/user/{uid}",
        base.display()
    );
    let dir = base.join(format!("last-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create the private runtime dir");
    fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .expect("chmod the private runtime dir");
    dir
}

/// One project plus its isolated XDG state; `Drop` stops the world and removes the scratch root.
struct Lab {
    run: PathBuf,
    runtime: PathBuf,
    data_home: PathBuf,
    project: PathBuf,
    world: WorldName,
    store: PathBuf,
}

impl Lab {
    fn new() -> Self {
        let run = live_run_root();
        let runtime = require_tmpfs_runtime_dir();
        let data_home = run.join("data");
        let project = run.join(format!("last-{}", std::process::id()));
        fs::create_dir_all(&project).expect("create the project");
        fs::write(project.join("Forkfile"), FORKFILE).expect("write Forkfile");
        let world = WorldName::parse(
            project
                .file_name()
                .expect("project name")
                .to_str()
                .expect("utf-8 project name"),
        )
        .expect("valid world name");
        let store = LogStore::root_for(
            std::slice::from_ref(&scratch_root()),
            &project,
            &data_home,
            true,
        );
        Self {
            run,
            runtime,
            data_home,
            project,
            world,
            store,
        }
    }

    fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&self.project)
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR")
            .env_remove("CLICOLOR_FORCE")
            .env("TERM", "xterm-256color")
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("XDG_STATE_HOME", self.run.join("state"))
            .env("XDG_DATA_HOME", &self.data_home);
        command
    }

    fn slipfork(&self, args: &[&str]) -> Output {
        self.command(Path::new(env!("CARGO_BIN_EXE_slipfork")))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("the slipfork binary must be runnable")
    }

    fn marker(&self) -> PathBuf {
        self.project.join(".logs-go")
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = self.slipfork(&["down"]);
        let _ = BtrfsEngine::new(StoreLayout::at(&self.store)).drop_world(&self.world);
        let _ = fs::remove_dir_all(&self.run);
        let _ = fs::remove_dir_all(&self.runtime);
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Every `hist-<n>` number in rendered order.
fn history_rows(text: &str) -> Vec<u32> {
    text.lines()
        .filter_map(|line| {
            let (_, rest) = line.split_once("hist-")?;
            rest.split_whitespace().next()?.parse().ok()
        })
        .collect()
}

macro_rules! assert_success {
    ($output:expr, $what:expr) => {
        assert!(
            $output.status.success(),
            "{} must succeed:\n{}\n{}",
            $what,
            stdout(&$output),
            stderr(&$output)
        );
    };
}

#[test]
fn last_tails_history_and_follow_defaults_to_twenty() {
    require_tool("crun");
    require_tool("unshare");
    require_tool("slirp4netns");
    let lab = Lab::new();

    let up = lab.slipfork(&["up"]);
    assert_success!(up, "`up`");

    // (1) `--last 25` prints exactly the last 25 rendered history lines.
    let tail = lab.slipfork(&["logs", "--last", "25"]);
    assert_success!(tail, "`logs --last 25`");
    let tailed = history_rows(&stdout(&tail));
    assert_eq!(
        tailed,
        (HISTORY - 24..=HISTORY).collect::<Vec<_>>(),
        "`--last 25` must print exactly hist-76..hist-100:\n{}",
        stdout(&tail)
    );

    // (2) `--json --last 3` is exactly three records plus the exit trailer, escape-free.
    let json = lab.slipfork(&["logs", "--json", "--last", "3"]);
    assert_success!(json, "`logs --json --last 3`");
    let json_out = stdout(&json);
    assert!(
        !json_out.contains('\u{1b}'),
        "`--json` carried ESC:\n{json_out}"
    );
    let records: Vec<Value> = json_out
        .lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|error| panic!("bad JSON ({error}): {line}"))
        })
        .collect();
    let lines: Vec<&str> = records
        .iter()
        .filter(|record| record["kind"] == "line")
        .filter_map(|record| record["line"].as_str())
        .collect();
    assert_eq!(lines, vec!["hist-98", "hist-99", "hist-100"], "{records:?}");
    assert_eq!(
        records.last(),
        Some(&serde_json::json!({ "exit": 0 })),
        "{records:?}"
    );

    // (3) `-f` starts with the last 20 history lines, then appends live lines.
    let follow = lab
        .command(Path::new(env!("CARGO_BIN_EXE_slipfork")))
        .args(["logs", "-f"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the followed stream");
    std::thread::sleep(Duration::from_secs(2));
    fs::write(lab.marker(), b"go\n").expect("release the live lines");
    std::thread::sleep(Duration::from_secs(2));
    // SAFETY: `kill(2)` never dereferences memory; the child is still alive (service sleeps 300s).
    unsafe {
        libc::kill(follow.id() as i32, libc::SIGINT);
    }
    let followed = follow.wait_with_output().expect("reap the followed stream");
    assert_success!(followed, "`logs -f` after Ctrl-C");
    let followed_out = stdout(&followed);
    assert_eq!(
        history_rows(&followed_out),
        (HISTORY - 19..=HISTORY).collect::<Vec<_>>(),
        "`-f` must start with exactly the last 20 history lines:\n{followed_out}"
    );
    let live_at = followed_out
        .find("live-1")
        .unwrap_or_else(|| panic!("`-f` lost the live lines:\n{followed_out}"));
    let last_history_at = followed_out
        .rfind("hist-100")
        .unwrap_or_else(|| panic!("`-f` lost the history lines:\n{followed_out}"));
    assert!(
        live_at > last_history_at,
        "live lines must follow the flushed history:\n{followed_out}"
    );

    // (4) A closed pipe (`| head`) is a clean stream end, never an error block.
    let piped = lab
        .command(Path::new("sh"))
        .args([
            "-c",
            &format!(
                "'{}' logs --last 25 | head -1",
                env!("CARGO_BIN_EXE_slipfork")
            ),
        ])
        .stdin(Stdio::null())
        .output()
        .expect("run the piped viewer");
    assert!(
        piped.status.success(),
        "`logs --last 25 | head` must stay quiet:\n{}\n{}",
        stdout(&piped),
        stderr(&piped)
    );
    assert_eq!(stdout(&piped).lines().count(), 1);
    assert!(
        !stderr(&piped).contains("failed to write output"),
        "a closed pipe must not print a write failure:\n{}",
        stderr(&piped)
    );

    // (5) `--last 0` fails closed before the daemon: one line plus one `next:`.
    let zero = lab.slipfork(&["logs", "--last", "0"]);
    assert!(!zero.status.success(), "`--last 0` must fail closed");
    let zero_err = stderr(&zero);
    assert!(
        zero_err.contains("--last needs a positive line count"),
        "{zero_err}"
    );
    assert!(
        zero_err.contains("next: use `--last 1` or omit it"),
        "{zero_err}"
    );
    assert!(
        !zero_err.contains("panicked"),
        "a refused count must never panic:\n{zero_err}"
    );
}
