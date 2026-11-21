//! Protected live journey for `slipfork fork` and `slipfork drop`.
//!
//! A real Btrfs scratch project (`SLIPFORK_TEST_ROOT`), a real daemon, and a real `crun` world:
//! `fork` must snapshot both subvolumes, reuse the per-mount `.slipfork-store/images` cache with
//! zero re-pull, switch the checkout's branch, boot the fork behind its own `.localhost` host, and
//! `drop` must refuse dirty work, then remove both snapshots. Every missing host capability FAILS
//! CLOSED.

use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slipfork_ingress::IngressHandoff;
use slipfork_world::{
    BtrfsEngine, Dirtiness, STORE_DIR, StoreLayout, WorldEngine, WorldName, is_subvolume_path,
};

const BTRFS_SUPER_MAGIC: u64 = 0x9123_683E;
const TMPFS_MAGIC: u64 = 0x0102_1994;

const DOCKERFILE: &str = "\
FROM python:3.13-slim
RUN printf cached > /etc/slipfork-fork-cache
WORKDIR /project
CMD [\"python3\", \"-m\", \"http.server\", \"8000\"]
";

const FORKFILE: &str = r#"# forkfile: 0.1
image app . {
  dockerfile Dockerfile
  service api {
    port 8000
    route /
    run: python3 -m http.server 8000
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
        let mount = unmangle(raw);
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

fn unmangle(raw: &str) -> PathBuf {
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
                ((bytes[i + 1] - b'0') << 6) | ((bytes[i + 2] - b'0') << 3) | (bytes[i + 3] - b'0'),
            );
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    PathBuf::from(OsString::from_vec(out))
}

fn require_tool(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| fail_closed(&format!("`{name}` is missing; install it and retry")))
}

fn wait_until(mut condition: impl FnMut() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Every `__daemon` whose environment names this lab's runtime directory or whose argv mentions it.
fn daemons_for(runtime: &Path) -> Vec<u32> {
    use std::os::unix::ffi::OsStrExt;
    let scope = runtime.as_os_str().as_bytes();
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let is_target = cmdline.split(|byte| *byte == 0).any(|arg| {
            arg == b"__daemon" || arg == b"__supervise" || arg.ends_with(b"slipfork-world-init")
        });
        if !is_target {
            continue;
        }
        let names_the_scope = cmdline.windows(scope.len()).any(|window| window == scope);
        let Ok(environ) = fs::read(entry.path().join("environ")) else {
            continue;
        };
        if names_the_scope || environ.windows(scope.len()).any(|window| window == scope) {
            found.push(pid);
        }
    }
    found
}

/// A real `:80` connection is handed to `ingress.accept`, routed by `Host`, and answered by the
/// fork pod, so the assertion proves both the fork world's route table and its own files.
fn assert_fork_ingress(runtime: &Path, world: &str, marker: &str) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the ingress probe listener");
    let mut client = TcpStream::connect(listener.local_addr().expect("probe address"))
        .expect("connect the ingress probe");
    let (accepted, _) = listener.accept().expect("accept the probe connection");
    let daemon = UnixStream::connect(runtime.join("daemon.sock")).expect("connect daemon.sock");
    IngressHandoff::new(1)
        .send(&daemon, accepted.as_fd())
        .expect("hand the accepted :80 descriptor to the daemon");
    drop(accepted);
    client
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("configure the ingress probe");
    client
        .write_all(
            format!("GET / HTTP/1.1\r\nHost: {world}.localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .expect("write the ingress request");
    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .expect("read the ingress response");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "the fork world must answer on its own host: {response}"
    );
    assert!(
        response.contains(marker),
        "the fork pod's own files must return: {response}"
    );
}

struct Lab {
    project: PathBuf,
    runtime: PathBuf,
    store_root: PathBuf,
}

impl Lab {
    fn new() -> Self {
        let Some(test_root) = std::env::var_os("SLIPFORK_TEST_ROOT").map(PathBuf::from) else {
            fail_closed(
                "SLIPFORK_TEST_ROOT is unset.\nnext: SLIPFORK_TEST_ROOT=/mnt/slipfork-live cargo test -p slipfork --features live --test critical_fork_live",
            );
        };
        assert_eq!(
            fs_magic(&test_root),
            BTRFS_SUPER_MAGIC,
            "fail closed: `{}` is not Btrfs.\nnext: mount a loop-backed Btrfs file there",
            test_root.display()
        );
        let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) else {
            fail_closed("XDG_RUNTIME_DIR is unset.\nnext: export XDG_RUNTIME_DIR=/run/user/<uid>");
        };
        assert_eq!(
            fs_magic(&runtime),
            TMPFS_MAGIC,
            "fail closed: `{}` is not tmpfs.\nnext: export XDG_RUNTIME_DIR=/run/user/<uid>",
            runtime.display()
        );
        for tool in ["crun", "unshare", "git"] {
            let _ = require_tool(tool);
        }
        let store_root = mount_store_for(&test_root);
        let world = WorldName::parse("demo").expect("valid world");
        let _ = BtrfsEngine::new(StoreLayout::at(&store_root)).drop_world(&world);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock reads after the epoch")
            .as_nanos();
        let project = test_root.join(format!("fork-live-{nanos}")).join("demo");
        fs::create_dir_all(&project).expect("create the scratch project");
        fs::write(project.join(".dockerignore"), ".git/\n").expect("write .dockerignore");
        fs::write(project.join("Dockerfile"), DOCKERFILE).expect("write Dockerfile");
        fs::write(project.join("Forkfile"), FORKFILE).expect("write Forkfile");
        let git = require_tool("git");
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "test@slipfork.local"][..],
            &["config", "user.name", "Slipfork Test"][..],
            &["add", "-A"][..],
            &["commit", "-q", "-m", "seed"][..],
        ] {
            let status = Command::new(&git)
                .arg("-C")
                .arg(&project)
                .args(args)
                .status()
                .expect("run git");
            assert!(status.success(), "git {args:?} failed");
        }
        Self {
            project,
            runtime,
            store_root,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_slipfork"))
            .args(args)
            .current_dir(&self.project)
            .env("NO_COLOR", "1")
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("TERM", "dumb")
            .output()
            .expect("run slipfork")
    }

    fn shutdown_daemon(&self) {
        let socket = self.runtime.join("daemon.sock");
        if let Ok(mut stream) = UnixStream::connect(socket) {
            let _ = stream.write_all(b"{\"v\":1,\"id\":\"bye\",\"method\":\"shutdown\"}\n");
            let mut reply = [0_u8; 256];
            let _ = stream.read(&mut reply);
        }
    }

    fn fork_project(&self) -> PathBuf {
        self.project.join(".forks/feat-auth")
    }

    fn fork_world(&self) -> PathBuf {
        self.store_root.join("worlds/feat-auth--demo")
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        if self.runtime.join("daemon.sock").exists() {
            let _ = self.run(&["drop", "feat-auth", "-f"]);
            let _ = self.run(&["down"]);
            self.shutdown_daemon();
        }
        let _ = wait_until(
            || daemons_for(&self.runtime).is_empty(),
            Duration::from_secs(10),
        );
        let engine = BtrfsEngine::new(StoreLayout::at(&self.store_root));
        if let Ok(world) = WorldName::parse("demo") {
            let _ = engine.drop_world(&world);
        }
        let root = self.project.parent().unwrap_or(&self.project).to_path_buf();
        let mut subvolumes = Vec::new();
        collect_subvolumes(&root, &mut subvolumes);
        subvolumes.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        for subvolume in subvolumes {
            let _ = engine.destroy(&subvolume, Dirtiness::Clean, true);
        }
        let _ = fs::remove_dir_all(&root);
    }
}

/// Collects every real Btrfs subvolume below `directory` (nested ones included).
fn collect_subvolumes(directory: &Path, found: &mut Vec<PathBuf>) {
    if !directory.is_dir() {
        return;
    }
    if is_subvolume_path(directory).unwrap_or(false) {
        found.push(directory.to_path_buf());
    }
    for entry in fs::read_dir(directory).into_iter().flatten().flatten() {
        let path = entry.path();
        if entry
            .file_type()
            .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
        {
            collect_subvolumes(&path, found);
        }
    }
}

#[test]
fn fork_and_drop_a_live_world_without_leaving_snapshots() {
    let lab = Lab::new();
    let up = lab.run(&["up", "--json"]);
    assert!(
        up.status.success(),
        "up failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&up.stdout),
        String::from_utf8_lossy(&up.stderr)
    );
    assert!(
        !lab.project.join(STORE_DIR).exists(),
        "the store must live at `<mount>/.slipfork-store`, never inside the project subvolume"
    );
    assert!(
        is_subvolume_path(&lab.store_root.join("worlds/demo")).unwrap_or(false),
        "parent world data must live under `<mount>/.slipfork-store/worlds/demo`"
    );

    let fork = lab.run(&["fork", "feat-auth", "--json"]);
    assert!(
        fork.status.success(),
        "fork failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&fork.stdout),
        String::from_utf8_lossy(&fork.stderr)
    );
    let stdout = String::from_utf8_lossy(&fork.stdout);
    let reply: serde_json::Value = serde_json::from_str(stdout.trim()).expect("fork --json object");
    assert_eq!(reply["fork"], "feat-auth", "{stdout}");
    assert_eq!(reply["url"], "http://feat-auth--demo.localhost", "{stdout}");
    assert!(reply["elapsed_ms"].as_u64().unwrap_or(0) > 0, "{stdout}");
    assert!(
        reply["snapshot_ms"].as_u64().unwrap_or(u64::MAX) < 50,
        "the CoW snapshot must stay under 50ms: {stdout}"
    );

    let fork_project = lab.fork_project();
    let fork_world = lab.fork_world();
    assert!(
        !fork_project.join(STORE_DIR).exists(),
        "fork snapshot must not recreate a nested `.slipfork-store`"
    );
    assert!(
        is_subvolume_path(&fork_project).unwrap_or(false),
        "`{}` must be a Btrfs subvolume",
        fork_project.display()
    );
    assert!(
        is_subvolume_path(&fork_world).unwrap_or(false),
        "`{}` must be a Btrfs subvolume",
        fork_world.display()
    );
    let fork_log = fs::read_to_string(fork_project.join(".slipfork/supervisor.log"))
        .expect("read fork supervisor.log");
    assert!(
        fork_log.contains("dockerfile app: 2 cached, 0 built"),
        "fork must reuse the shared image cache without rebuilding: {fork_log}"
    );
    assert!(
        !fork_log.contains("streaming") && !fork_log.contains("pull "),
        "fork must not re-download base image layers: {fork_log}"
    );
    let branch = Command::new(require_tool("git"))
        .arg("-C")
        .arg(&fork_project)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .expect("read the fork branch");
    assert_eq!(
        String::from_utf8_lossy(&branch.stdout).trim(),
        "feat-auth",
        "the fork checkout must switch branches"
    );

    // The fork's own pod must serve the fork's files, and the parent must not see them.
    fs::write(fork_project.join("fork-only.txt"), "fork bytes\n").expect("write the fork marker");
    assert_fork_ingress(&lab.runtime, "feat-auth--demo", "fork-only.txt");
    assert!(
        !lab.project.join("fork-only.txt").exists(),
        "the parent checkout must stay untouched"
    );

    // Dirty work is refused before anything stops, with exactly one remedy.
    let dirty = lab.run(&["drop", "feat-auth"]);
    assert_eq!(dirty.status.code(), Some(1), "{dirty:?}");
    let stderr = String::from_utf8_lossy(&dirty.stderr);
    assert!(stderr.contains("uncommitted changes"), "{stderr}");
    assert!(stderr.contains("slipfork drop feat-auth -f"), "{stderr}");
    assert_eq!(stderr.matches("next:").count(), 1, "{stderr}");
    assert!(
        is_subvolume_path(&fork_project).unwrap_or(false),
        "a refused drop must keep the fork alive"
    );

    let drop = lab.run(&["drop", "feat-auth", "-f", "--json"]);
    assert!(
        drop.status.success(),
        "drop failed: {}",
        String::from_utf8_lossy(&drop.stderr)
    );
    let stdout = String::from_utf8_lossy(&drop.stdout);
    let reply: serde_json::Value = serde_json::from_str(stdout.trim()).expect("drop --json object");
    assert_eq!(reply["dropped"], "feat-auth", "{stdout}");
    assert!(
        !fork_project.exists(),
        "the project snapshot must be removed"
    );
    assert!(
        !fork_world.exists(),
        "the fork world snapshot must be removed"
    );

    let missing = lab.run(&["drop", "feat-auth"]);
    assert_eq!(missing.status.code(), Some(1), "{missing:?}");
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("no fork named"),
        "dropping a fork twice must be typed"
    );

    let down = lab.run(&["down"]);
    assert!(
        down.status.success(),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
    lab.shutdown_daemon();
    assert!(
        UnixStream::connect(lab.runtime.join("daemon.sock")).is_err(),
        "daemon.sock must stop answering"
    );
    assert!(
        wait_until(
            || daemons_for(&lab.runtime).is_empty(),
            Duration::from_secs(10)
        ),
        "no `__daemon` or supervisor may survive: {:?}",
        daemons_for(&lab.runtime)
    );
}
