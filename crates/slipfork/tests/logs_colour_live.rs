//! Live proof that child colour reaches a real terminal and only there.
//!
//! One Btrfs project, one real `up`: a build `RUN` prints `\x1b[31mred\x1b[0m` unguarded and a
//! strictly `isatty`-gated line stays plain (accepted: the pod is a pipe, never a fake TTY). The
//! run is booted under a real PTY (`util-linux script`), so the live stream must keep the child's
//! bytes. Then every non-colour viewer — a pipe, `--json`, and a `NO_COLOR` terminal — must emit
//! zero `ESC`, while the stored capture still holds the raw bytes for a later colour terminal.
//!
//! The suite fails closed on a non-Btrfs host or a missing `crun`/`unshare`/`script`: a skipped
//! live check would hide an ANSI leak or a colourless service forever.

#![cfg(feature = "live")]

use std::fs;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use slipfork_world::{BtrfsEngine, LogStore, StoreLayout, WorldEngine, WorldName};

const BTRFS_SUPER_MAGIC: u64 = 0x9123_683E;
const TMPFS_MAGIC: u64 = 0x0102_1994;
const RED: &str = "\u{1b}[31mred\u{1b}[0m";
const GREEN: &str = "\u{1b}[32mgreen\u{1b}[0m";

const FORKFILE: &str = r#"# forkfile: 0.1
image app . {
  dockerfile Dockerfile
  service web {
    run: sh -c 'printf "svc-\033[32mgreen\033[0m\n"; exec sleep 120'
  }
}
"#;

// `printf` converts the escapes itself (not TTY-gated); the Python line is strictly `isatty`.
// The per-run cache-bust marker forces a real build even when a shared store holds an older image.
fn dockerfile(cache_bust: u128) -> String {
    format!(
        "\
FROM python:3.13-slim
RUN printf 'run-{cache_bust}-\\033[31mred\\033[0m\\n' && python3 -c \"import sys; print('isatty-\\033[34mblue\\033[0m' if sys.stdout.isatty() else 'isatty-plain')\"
WORKDIR /project
CMD [\"true\"]
"
    )
}

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
    let run = root.join(format!("colour-{}-{nanos}", std::process::id()));
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
    // A private runtime dir per run: never reuse a daemon another live suite left behind.
    let dir = base.join(format!("colour-{}", std::process::id()));
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
        let project = run.join(format!("colour-{}", std::process::id()));
        fs::create_dir_all(&project).expect("create the project");
        fs::write(project.join("Forkfile"), FORKFILE).expect("write Forkfile");
        fs::write(
            project.join("Dockerfile"),
            dockerfile(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |since| since.as_nanos()),
            ),
        )
        .expect("write Dockerfile");
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

    /// The stock command environment: no inherited `NO_COLOR`, a colour `TERM`, isolated XDG.
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
            .stdin(std::process::Stdio::null())
            .output()
            .expect("the slipfork binary must be runnable")
    }

    /// Runs one verb under a real PTY via `util-linux script`; stdout carries the terminal bytes.
    fn pty(&self, args: &[&str], no_color: bool) -> Output {
        let script = require_tool("script");
        let line = format!(
            "cd '{}' && '{}' {}",
            self.project.display(),
            env!("CARGO_BIN_EXE_slipfork"),
            args.join(" ")
        );
        let mut command = self.command(&script);
        if no_color {
            command.env("NO_COLOR", "1");
        }
        command
            .args(["-q", "-e", "-c", &line, "/dev/null"])
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run the PTY child")
    }

    /// The image run whose stored capture holds the coloured line, with its raw content.
    ///
    /// A cached/duplicate catalog row may own no output; the coloured build is the one under test.
    fn coloured_image_run(&self) -> (String, String) {
        let output = self.slipfork(&["logs", "--runs", "--json"]);
        assert!(output.status.success(), "{}", stderr(&output));
        let mut candidates: Vec<(u64, String)> = Vec::new();
        for line in stdout(&output).lines() {
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if record["kind"].as_str() != Some("image") {
                continue;
            }
            let Some(run_id) = record["run_id"].as_str() else {
                continue;
            };
            candidates.push((
                record["started_ms"].as_u64().unwrap_or(0),
                run_id.to_owned(),
            ));
        }
        candidates.sort_by(|left, right| right.0.cmp(&left.0));
        let store = LogStore::at(self.store.clone(), self.world.clone());
        let mut seen = Vec::new();
        for (_, run_id) in candidates {
            let path = store.capture(&run_id);
            match fs::read_to_string(&path) {
                Ok(text) if text.contains(RED) => return (run_id, text),
                Ok(text) => seen.push(format!("{run_id}: {} bytes", text.len())),
                Err(error) => seen.push(format!("{run_id}: {error}")),
            }
        }
        panic!(
            "no image capture holds the coloured line under `{}`: {seen:?}\n{}",
            store.run_dir().display(),
            stderr(&output)
        );
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

fn has_esc(text: &str) -> bool {
    text.contains('\u{1b}')
}

#[test]
fn child_colour_reaches_a_real_terminal_and_every_other_viewer_is_escape_free() {
    require_tool("crun");
    require_tool("unshare");
    require_tool("slirp4netns");
    let lab = Lab::new();

    // (1) A real PTY `up`: the build `RUN` and the service keep their own coloured bytes.
    let up = lab.pty(&["up"], false);
    let live = stdout(&up);
    assert!(
        up.status.success(),
        "`up` must boot under the PTY:\n{live}\n{}",
        stderr(&up)
    );
    assert!(
        live.contains(RED),
        "the live stream lost the RUN's red:\n{live}"
    );
    assert!(
        live.contains(GREEN),
        "the live stream lost the service's green:\n{live}"
    );
    // A strictly `isatty`-only program stays plain under the pod's pipe: accepted, not hacked.
    assert!(
        live.contains("isatty-plain"),
        "an isatty-gated program must stay plain:\n{live}"
    );
    assert!(
        !live.contains("\u{1b}[34mblue"),
        "the pod must never pretend to be a TTY:\n{live}"
    );

    let (run_id, stored) = lab.coloured_image_run();
    assert!(
        stored.contains(RED),
        "the stored run record must keep raw ESC for a later colour terminal:\n{stored}"
    );

    // (2) A pipe strips: `logs -r | cat` carries no escape and still shows the text.
    let piped = lab.slipfork(&["logs", "-r", &run_id]);
    let piped_out = stdout(&piped);
    assert!(piped.status.success(), "{}", stderr(&piped));
    assert!(!has_esc(&piped_out), "a pipe carried ESC:\n{piped_out}");
    assert!(
        piped_out.contains("red"),
        "the pipe lost the line:\n{piped_out}"
    );

    // (3) `--json` is always escape-free, one object per line.
    let json = lab.slipfork(&["logs", "-r", &run_id, "--json"]);
    let json_out = stdout(&json);
    assert!(json.status.success(), "{}", stderr(&json));
    assert!(!has_esc(&json_out), "`--json` carried ESC:\n{json_out}");
    for line in json_out.lines() {
        serde_json::from_str::<Value>(line)
            .unwrap_or_else(|error| panic!("`--json` emitted a non-object line ({error}): {line}"));
    }
    assert!(
        json_out.contains("red"),
        "the JSON replay lost the line:\n{json_out}"
    );

    // (4) `NO_COLOR` on a real terminal strips too.
    let no_color = lab.pty(&["logs", "-r", &run_id], true);
    let no_color_out = stdout(&no_color);
    assert!(no_color.status.success(), "{}", stderr(&no_color));
    assert!(
        !has_esc(&no_color_out),
        "`NO_COLOR` on a TTY carried ESC:\n{no_color_out}"
    );
    assert!(no_color_out.contains("red"));
}
