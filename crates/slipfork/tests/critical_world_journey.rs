//! Protected live journey for `slipfork up`, `down`, and `restart`.
//!
//! Real Btrfs scratch project, real `unshare`/`crun` world, real OCI registry pull, and the
//! native `Dockerfile` builder installing `uv`. The journey proves: one deduplicated `uv sync`
//! node for two services, stamped skips on the second boot, `pre-deploy` -> services ->
//! `post-deploy` ordering with the service's own marker between the hooks, a failing service that
//! leaves the world running, `restart`/`down`, and zero leaked containers, processes, or run
//! subvolumes. Every missing host capability FAILS CLOSED with a remedy instead of skipping.

use std::cell::RefCell;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slipfork_world::{BtrfsEngine, Dirtiness, StoreLayout, WorldEngine, is_subvolume_path};

const BTRFS_SUPER_MAGIC: u64 = 0x9123_683E;
const TMPFS_MAGIC: u64 = 0x0102_1994;

const DOCKERFILE: &str = "\
FROM python:3.13-slim
RUN pip install --no-cache-dir uv
WORKDIR /project
CMD [\"python3\", \"--version\"]
";

// A real `uv` project: `uv sync` creates the venv both services share.
const PYPROJECT: &str = "[project]\nname = \"good\"\nversion = \"0.1.0\"\nrequires-python = \">=3.12\"\ndependencies = []\n";
// A committed lockfile is what makes the stamped `uv sync` skip on the second boot.
const UV_LOCK: &str = "version = 1\nrevision = 5\nrequires-python = \">=3.12\"\n\n[[package]]\nname = \"good\"\nversion = \"0.1.0\"\nsource = { virtual = \".\" }\n";

// `pre` is written by the hook, `svc` by the service's own startup, `post` by the hook that
// waits for it, so `presvcpost` proves the service ran strictly between the two hooks.
const GOOD_FORKFILE: &str = r#"# forkfile: 0.1
image app . {
  dockerfile Dockerfile
  pre-deploy: sh -c 'printf pre > order.txt'
  post-deploy: sh -c 'for i in $(seq 50); do grep -q svc order.txt && break; sleep 0.1; done; grep -q svc order.txt && printf post >> order.txt'
  service api {
    port 8000
    route /
    run: @uv run python app.py
  }
  service worker {
    run: @uv run python worker.py
  }
}
"#;

const APP_PY: &str = r#"import os, pathlib
from http.server import BaseHTTPRequestHandler, HTTPServer

with pathlib.Path("order.txt").open("a") as order:
    order.write("svc")

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"ok"
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass

HTTPServer(("127.0.0.1", int(os.environ["PORT"])), Handler).serve_forever()
"#;

const WORKER_PY: &str = "import time\n\nwhile True:\n    time.sleep(3600)\n";

const BROKEN_FORKFILE: &str = r#"# forkfile: 0.1
image app . {
  dockerfile Dockerfile
  service broken {
    run: @uv run python -c "raise SystemExit(7)"
  }
}
"#;

fn fail_closed(message: &str) -> ! {
    panic!("fail closed: {message}");
}

fn scratch_root() -> PathBuf {
    let home = std::env::var_os("HOME")
        .unwrap_or_else(|| fail_closed("HOME is unset; export HOME before running this suite"));
    PathBuf::from(home).join(".cache/slipfork/tests")
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

fn live_run_root() -> PathBuf {
    let scratch = scratch_root();
    assert!(
        !scratch.starts_with("/tmp"),
        "fail closed: `{}` is RAM-backed tmpfs and has no Btrfs ioctls.\nnext: set HOME to a Btrfs-backed home such as /home/<user>",
        scratch.display()
    );
    fs::create_dir_all(&scratch).expect("create scratch root");
    assert!(
        fs_magic(&scratch) == BTRFS_SUPER_MAGIC,
        "fail closed: `{}` is not on Btrfs.\nnext: mount a Btrfs filesystem at HOME (or a parent) with `-o user_subvol_rm_allowed,noatime`",
        scratch.display()
    );
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let run = scratch.join(format!("journey-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&run).expect("create journey root");
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
        "`{name}` is not on PATH.\nnext: install {name} (crun, util-linux) and re-run"
    ))
}

fn require_tmpfs_runtime_dir() -> PathBuf {
    let uid = unsafe { libc::geteuid() };
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}")));
    assert!(
        dir.is_dir() && fs_magic(&dir) == TMPFS_MAGIC,
        "fail closed: `{}` is not a tmpfs runtime directory.\nnext: export XDG_RUNTIME_DIR=/run/user/{uid} (a tmpfs)",
        dir.display()
    );
    dir
}

fn require_userns(unshare: &Path) {
    let status = Command::new(unshare)
        .args(["--user", "--map-current-user", "--", "true"])
        .status()
        .expect("fail closed: could not execute unshare");
    assert!(
        status.success(),
        "fail closed: unprivileged user namespaces are unavailable.\nnext: `sudo sysctl -w kernel.unprivileged_userns_clone=1`"
    );
}

fn require_registry_egress() {
    let address = ("registry-1.docker.io", 443)
        .to_socket_addrs()
        .ok()
        .and_then(|mut addresses| addresses.next());
    let Some(address) = address else {
        fail_closed(
            "docker.io does not resolve.\nnext: connect the host to the network, then retry",
        );
    };
    assert!(
        TcpStream::connect_timeout(&address, Duration::from_secs(5)).is_ok(),
        "fail closed: no registry egress to registry-1.docker.io:443.\nnext: connect the host to the network, then retry"
    );
}

/// The scratch root plus its projects: `Drop` always tears both worlds down and removes the root.
struct Journey {
    run: PathBuf,
    runtime: PathBuf,
    projects: RefCell<Vec<PathBuf>>,
}

impl Journey {
    fn new() -> Self {
        Self {
            run: live_run_root(),
            runtime: require_tmpfs_runtime_dir(),
            projects: RefCell::new(Vec::new()),
        }
    }

    fn project(&self, name: &str, forkfile: &str) -> PathBuf {
        let dir = self.run.join(name);
        fs::create_dir_all(&dir).expect("create project directory");
        fs::write(dir.join("Dockerfile"), DOCKERFILE).expect("write Dockerfile");
        fs::write(dir.join("Forkfile"), forkfile).expect("write Forkfile");
        fs::write(dir.join("pyproject.toml"), PYPROJECT).expect("write pyproject.toml");
        fs::write(dir.join("uv.lock"), UV_LOCK).expect("write uv.lock");
        fs::write(dir.join(".dockerignore"), ".venv/\n__pycache__/\n")
            .expect("write .dockerignore");
        fs::write(dir.join("app.py"), APP_PY).expect("write app.py");
        fs::write(dir.join("worker.py"), WORKER_PY).expect("write worker.py");
        self.projects.borrow_mut().push(dir.clone());
        dir
    }

    fn slipfork(&self, project: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_slipfork"))
            .args(args)
            .current_dir(project)
            .env("NO_COLOR", "1")
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("XDG_STATE_HOME", self.run.join("state"))
            .output()
            .expect("the slipfork binary must be runnable")
    }
}

impl Drop for Journey {
    fn drop(&mut self) {
        for project in self.projects.borrow().iter() {
            let _ = Command::new(env!("CARGO_BIN_EXE_slipfork"))
                .arg("down")
                .current_dir(project)
                .env("NO_COLOR", "1")
                .env("XDG_RUNTIME_DIR", &self.runtime)
                .env("XDG_STATE_HOME", self.run.join("state"))
                .output();
        }
        self.shutdown_daemon();
        let scope = self.run.as_os_str().as_bytes();
        let survived = !wait_until(
            || {
                processes_mentioning(b"slipfork-world-init")
                    .into_iter()
                    .all(|pid| !belongs_to_run(pid, scope))
            },
            Duration::from_secs(10),
        );
        if survived {
            let leaked: Vec<u32> = processes_mentioning(b"slipfork-world-init")
                .into_iter()
                .filter(|pid| belongs_to_run(*pid, scope))
                .collect();
            let message =
                format!("fail closed: `slipfork-world-init` survived teardown in {leaked:?}");
            if std::thread::panicking() {
                eprintln!("journey cleanup: {message}");
            } else {
                panic!("{message}");
            }
        }
        self.destroy_subvolumes();
        if let Err(error) = fs::remove_dir_all(&self.run) {
            eprintln!(
                "journey cleanup: cannot remove `{}`: {error}",
                self.run.display()
            );
        }
    }
}

impl Journey {
    /// Stops the per-user daemon this journey spawned, so the next run starts clean.
    fn shutdown_daemon(&self) {
        let Ok(mut socket) = UnixStream::connect(self.runtime.join("daemon.sock")) else {
            return;
        };
        let _ = socket
            .write_all(b"{\"v\":1,\"id\":\"cleanup\",\"method\":\"shutdown\",\"params\":{}}\n");
        let _ = socket.flush();
        let mut reply = String::new();
        let _ = socket.read_to_string(&mut reply);
    }

    /// Sealed subvolumes (the project root included) need `SNAP_DESTROY_V2`, not `rmdir`.
    fn destroy_subvolumes(&self) {
        let engine = BtrfsEngine::new(StoreLayout::new(&self.run));
        let mut subvolumes = Vec::new();
        let mut stack = vec![self.run.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if !file_type.is_dir() || file_type.is_symlink() {
                    continue;
                }
                let path = entry.path();
                if is_subvolume_path(&path).expect("fail closed: probe the journey subvolume") {
                    subvolumes.push(path.clone());
                }
                stack.push(path);
            }
        }
        // Deepest first: a child subvolume must be gone before its parent is destroyed.
        subvolumes.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        for path in &subvolumes {
            if let Err(error) = engine.destroy(path, Dirtiness::Clean, true) {
                eprintln!(
                    "journey cleanup: destroy `{}` failed: {error}",
                    path.display()
                );
            }
        }
        let leftovers: Vec<PathBuf> = subvolumes
            .into_iter()
            .filter(|path| {
                // A destroyed subvolume answers ENOENT, which is the success case; any other
                // probe failure must stop the journey instead of being swallowed.
                match is_subvolume_path(path) {
                    Ok(subvolume) => subvolume,
                    Err(slipfork_world::Error::Io { source, .. })
                        if source.kind() == std::io::ErrorKind::NotFound =>
                    {
                        false
                    }
                    Err(error) => {
                        panic!("fail closed: probe the journey subvolume: {error}")
                    }
                }
            })
            .collect();
        if !leftovers.is_empty() {
            let message =
                format!("fail closed: subvolume(s) survived the Btrfs teardown: {leftovers:?}");
            if std::thread::panicking() {
                eprintln!("{message}");
            } else {
                panic!("{message}");
            }
        }
    }
}

/// The last 80 lines of a project's supervisor log, for a failing `up` panic message.
fn supervisor_tail(project: &Path) -> String {
    let log = project.join(".slipfork/supervisor.log");
    let Ok(text) = fs::read_to_string(&log) else {
        return format!("<no {}>", log.display());
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(80);
    lines[start..].join("\n")
}

/// One failing-`up` message carrying stdout, stderr, and the supervisor log tail.
fn up_failure(label: &str, project: &Path, output: &Output) -> String {
    format!(
        "{label}:\nstdout:\n{}\nstderr:\n{}\n--- .slipfork/supervisor.log (tail) ---\n{}",
        stdout(output),
        stderr(output),
        supervisor_tail(project)
    )
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn crun_list(crun: &Path) -> String {
    let output = Command::new(crun)
        .arg("list")
        .output()
        .expect("fail closed: could not execute `crun list`");
    assert!(
        output.status.success(),
        "fail closed: `crun list` failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// True when `pid` or any ancestor's cmdline names this journey's unique run root.
fn belongs_to_run(pid: u32, scope: &[u8]) -> bool {
    let mut current = Some(pid);
    while let Some(pid) = current {
        if let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) {
            if cmdline.windows(scope.len()).any(|window| window == scope) {
                return true;
            }
        }
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        // `pid (comm) state ppid …`; `comm` may contain spaces, so split after the final `)`.
        let Some(rest) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
            return false;
        };
        current = rest
            .split_whitespace()
            .nth(1)
            .and_then(|ppid| ppid.parse::<u32>().ok())
            .filter(|ppid| *ppid > 0);
    }
    false
}

/// Every process this journey leaked: the needle matches and the run root is in the ancestry.
fn scoped_leaks(run: &Path) -> Vec<(String, u32)> {
    let scope = run.as_os_str().as_bytes();
    let mut leaked = Vec::new();
    for needle in [
        b"slipfork-world-init".as_slice(),
        b"postgres".as_slice(),
        b"redis-server".as_slice(),
        b"weed".as_slice(),
        b"uv".as_slice(),
        b"python".as_slice(),
    ] {
        for pid in processes_mentioning(needle) {
            if belongs_to_run(pid, scope) {
                leaked.push((String::from_utf8_lossy(needle).into_owned(), pid));
            }
        }
    }
    leaked
}

fn processes_mentioning(needle: &[u8]) -> Vec<u32> {
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").expect("read /proc") {
        let entry = entry.expect("read /proc entry");
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        if cmdline.windows(needle.len()).any(|window| window == needle) {
            found.push(pid);
        }
    }
    found
}

fn wait_until(mut condition: impl FnMut() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// A real `:80` connection is handed to `ingress.accept`, routed, and answered by the world pod.
fn assert_ingress(runtime: &Path, world: &str) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the ingress probe listener");
    let mut client = TcpStream::connect(listener.local_addr().expect("probe address"))
        .expect("connect the ingress probe");
    let (accepted, _) = listener.accept().expect("accept the probe connection");
    let daemon = UnixStream::connect(runtime.join("daemon.sock")).expect("connect daemon.sock");
    slipfork_ingress::IngressHandoff::new(1)
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
        response.starts_with("HTTP/1.1 200") && response.contains("ok"),
        "the world's own bytes must return through `dial_tcp`: {response}"
    );
}

#[test]
fn world_up_restart_down_journey() {
    let journey = Journey::new();
    let crun = require_tool("crun");
    let unshare = require_tool("unshare");
    require_userns(&unshare);
    require_registry_egress();

    let good = journey.project("good", GOOD_FORKFILE);

    // 1. First boot: exactly one deduplicated `uv sync`, then hooks and both services.
    let first = journey.slipfork(&good, &["up"]);
    let first_out = stdout(&first);
    let first_err = stderr(&first);
    assert!(
        first.status.success(),
        "{}",
        up_failure("first `up` failed", &good, &first)
    );
    assert!(
        first_out.contains("✓ Ready -> http://good.localhost"),
        "ready line missing:\n{first_out}"
    );
    assert_ingress(&journey.runtime, "good");
    assert!(
        first_out.contains("[1/") && first_out.contains("→ [1/"),
        "progress lines must carry a real `→ [i/n]` prefix:\n{first_out}"
    );
    assert!(
        !first_err.contains("error:"),
        "a successful boot must print no error tail:\n{first_err}"
    );
    let sync_lines = first_out
        .lines()
        .filter(|line| line.starts_with('✓') && line.contains("uv sync"))
        .count();
    assert_eq!(
        sync_lines, 1,
        "two services must share exactly one `uv sync` node:\n{first_out}"
    );
    assert!(
        !first_out.contains('\u{1b}'),
        "NO_COLOR output must carry zero escapes: {first_out:?}"
    );
    let order = fs::read_to_string(good.join("order.txt")).expect("hook order file");
    assert_eq!(
        order, "presvcpost",
        "the service must run strictly between its hooks: {order:?}"
    );

    // 1b. Captured logs and ad-hoc exec against the real pod.
    let logs = journey.slipfork(&good, &["logs", "api"]);
    assert!(logs.status.success(), "{}", stderr(&logs));
    let log_text = stdout(&logs);
    assert!(
        log_text.contains("── api started ──"),
        "logs must draw the capture divider: {log_text}"
    );
    assert!(
        log_text.contains("api │"),
        "logs must render a greedy gutter: {log_text}"
    );
    assert!(!log_text.contains('\u{1b}'), "NO_COLOR logs stay plain");

    let logs_json = journey.slipfork(&good, &["logs", "--json", "api"]);
    assert!(logs_json.status.success(), "{}", stderr(&logs_json));
    let mut frames: Vec<serde_json::Value> = stdout(&logs_json)
        .lines()
        .map(|line| serde_json::from_str(line).expect("each logs line is JSON"))
        .collect();
    let exit = frames.pop().expect("logs ends with an exit frame");
    assert_eq!(exit["exit"], 0, "{exit:?}");
    assert!(
        frames.iter().any(|frame| frame["kind"] == "divider"),
        "{frames:?}"
    );

    let exec = journey.slipfork(
        &good,
        &["exec", "api", "--", "sh", "-c", "printf exec-ok; exit 5"],
    );
    assert_eq!(
        exec.status.code(),
        Some(5),
        "exec must forward the pod's exit code: {}",
        stderr(&exec)
    );
    assert!(
        stdout(&exec).contains("exec-ok"),
        "exec must stream the pod's stdout: {}",
        stdout(&exec)
    );

    let missing = journey.slipfork(&good, &["logs", "nope"]);
    assert!(!missing.status.success(), "an unknown pod must fail closed");
    let missing_err = stderr(&missing);
    assert!(missing_err.contains("not running"), "{missing_err}");
    assert!(missing_err.contains("next:"), "{missing_err}");

    // 2. A failed service keeps the world running; `down` must still work.
    let broken = journey.project("broken", BROKEN_FORKFILE);
    let failed = journey.slipfork(&broken, &["up"]);
    assert_eq!(
        failed.status.code(),
        Some(1),
        "{}",
        up_failure("a failing service must fail `up`", &broken, &failed)
    );
    let failed_err = stderr(&failed);
    let failed_out = stdout(&failed);
    assert!(
        failed_out.contains("→ ["),
        "failure must keep a phase header:\n{failed_out}"
    );
    assert!(
        failed_out.contains("Forkfile:"),
        "the failure tail must name the Forkfile line:\n{failed_out}"
    );
    assert!(failed_err.contains("error:"), "{failed_err}");
    assert_eq!(
        failed_err.matches("next:").count(),
        1,
        "a failure prints exactly one next:\n{failed_err}"
    );
    assert!(
        broken.join(".slipfork/world.json").is_file(),
        "the world must stay up after a failed service"
    );
    let listed = journey.slipfork(&broken, &["status"]);
    assert!(
        listed.status.success() && stdout(&listed).contains("broken"),
        "the failed world must stay listed by status:\n{}",
        stdout(&listed)
    );
    let down = journey.slipfork(&broken, &["down"]);
    assert!(
        down.status.success(),
        "down after failure: {}",
        stderr(&down)
    );
    assert!(
        stdout(&down).contains("→ [1/2] stopping broken"),
        "down must stream its phase header:\n{}",
        stdout(&down)
    );

    // 3. Down and re-up: the stamped `uv sync` is skipped and named as such.
    let down = journey.slipfork(&good, &["down"]);
    assert!(down.status.success(), "down: {}", stderr(&down));
    assert!(
        stdout(&down).contains("→ [1/2] stopping good"),
        "down must stream its phase header:\n{}",
        stdout(&down)
    );
    let second = journey.slipfork(&good, &["up"]);
    let second_out = stdout(&second);
    assert!(
        second.status.success(),
        "{}",
        up_failure("second `up` failed", &good, &second)
    );
    let stamp_line = second_out
        .lines()
        .find(|line| line.contains("uv sync") && line.starts_with('✓'))
        .unwrap_or_else(|| panic!("no stamped setup line:\n{second_out}"));
    assert!(
        stamp_line.contains("up to date"),
        "stamped step must report its skip: {stamp_line}"
    );

    // 4. Restart one service, then prove teardown leaves nothing behind.
    let restart = journey.slipfork(&good, &["restart", "api"]);
    assert!(
        restart.status.success(),
        "{}",
        up_failure("restart failed", &good, &restart)
    );
    assert!(
        stdout(&restart).contains("→ [1/1] restarting api"),
        "restart must stream its phase header:\n{}",
        stdout(&restart)
    );
    assert!(
        stdout(&restart).contains("Restarted api"),
        "{}",
        stdout(&restart)
    );
    let down = journey.slipfork(&good, &["down"]);
    assert!(down.status.success(), "final down: {}", stderr(&down));
    let _ = wait_until(
        || scoped_leaks(&journey.run).is_empty(),
        Duration::from_secs(10),
    );
    let leaked = scoped_leaks(&journey.run);
    assert!(
        leaked.is_empty(),
        "processes survived teardown (needle, pid): {leaked:?}"
    );
    let listed = crun_list(&crun);
    for id in ["api", "worker", "broken"] {
        assert!(
            !listed.contains(id),
            "container `{id}` survived teardown:\n{listed}"
        );
    }
    assert!(!good.join(".slipfork/world.json").exists());
    assert!(!broken.join(".slipfork/world.json").exists());

    // 5. No run subvolume survives under either project's managed store.
    for project in [&good, &broken] {
        let store = project.join(".slipfork-store/worlds");
        if !store.exists() {
            continue;
        }
        let mut stack = vec![store];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).expect("walk store") {
                let entry = entry.expect("read store entry");
                if entry.file_type().expect("stat entry").is_dir() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    assert!(
                        !name.starts_with("boot")
                            && !name.starts_with("svc-")
                            && !name.starts_with("res-"),
                        "run subvolume `{}` leaked",
                        entry.path().display()
                    );
                    stack.push(entry.path());
                }
            }
        }
    }
}

#[test]
fn help_lists_the_daily_verbs() {
    let output = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--help")
        .env("NO_COLOR", "1")
        .output()
        .expect("help runs");
    let help = stdout(&output);
    for verb in ["up", "down", "restart"] {
        assert!(
            help.contains(&format!("  {verb} ")),
            "missing `{verb}`:\n{help}"
        );
    }
}
