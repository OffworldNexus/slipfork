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
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slipfork_world::{
    BtrfsEngine, Dirtiness, LogStore, STORE_DIR, StoreLayout, WorldEngine, WorldName,
    is_subvolume_path,
};

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
  env {
    GREETING = "hello"
    S3_ACCESS = uploads.access_key
    S3_SECRET = uploads.secret_key
  }
  pre-deploy: sh -c 'printf pre > order.txt && echo pre-deploy hook ran'
  post-deploy: sh -c 'for i in $(seq 50); do grep -q svc order.txt && break; sleep 0.1; done; grep -q svc order.txt && printf post >> order.txt && echo post-deploy hook ran'
  service api {
    port 8000
    route /
    run: @uv run python app.py
  }
  service worker {
    run: @uv run python worker.py
  }
  service probe {
    run: @uv run python probe.py
  }
}
s3 uploads
s3 assets {
  policy public-read
  cdn static-cdn
}
postgres pg {
  database app_db
}
redis cache
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

print("api ready", flush=True)
HTTPServer(("127.0.0.1", int(os.environ["PORT"])), Handler).serve_forever()
"#;

const WORKER_PY: &str =
    "import time\n\nprint(\"worker ready\", flush=True)\nwhile True:\n    time.sleep(3600)\n";

const BROKEN_FORKFILE: &str = r#"# forkfile: 0.1
image app . {
  dockerfile Dockerfile
  service broken {
    run: @uv run python -c "raise SystemExit(7)"
  }
}
"#;

// The run-catalog journey: one image whose `RUN` fails before any pod can start.
const FAIL_BUILD_DOCKERFILE: &str = "\
FROM python:3.13-slim
RUN sh -c 'echo out-line; echo err-line >&2; exit 3'
WORKDIR /project
CMD [\"python3\", \"--version\"]
";

const CATALOG_FORKFILE: &str = r#"# forkfile: 0.1
image app . {
  dockerfile Dockerfile
  service api {
    port 8000
    route /
    run: @uv run python app.py
  }
}
"#;

// Engine-executed instruction failures: the instruction and its one-line cause must survive.
const MISSING_STAGE_DOCKERFILE: &str = "\
FROM python:3.13-slim
COPY --from=missing-stage /x /y
";

// A `FROM` the engine cannot pull: the pull, not a child process, is the failing command.
const UNPULLABLE_FROM_DOCKERFILE: &str = "\
FROM 127.0.0.1:1/nope:latest
CMD [\"true\"]
";

// An invalid `from:` reference: the Forkfile's pull is the failing step, and its record is `FROM`.
const INVALID_FROM_FORKFILE: &str = r#"# forkfile: 0.1
image app from /bad:latest {
  service api {
    port 8000
    route /
    run: @uv run python app.py
  }
}
"#;

// A failure before any instruction ran: no step row exists, so the verdict carries the reason.
const PARSE_FAIL_DOCKERFILE: &str = "\
COPY /x /y
FROM scratch
";

fn fail_closed(message: &str) -> ! {
    panic!("fail closed: {message}");
}

/// CI and `make live-test` always set `SLIPFORK_TEST_ROOT`; there is no ambient fallback.
fn scratch_root() -> PathBuf {
    let Some(root) = std::env::var_os("SLIPFORK_TEST_ROOT").filter(|root| !root.is_empty()) else {
        fail_closed(
            "SLIPFORK_TEST_ROOT is unset or empty.\nnext: SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test",
        )
    };
    PathBuf::from(root)
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
        "fail closed: `{}` is RAM-backed tmpfs and has no Btrfs ioctls.\nnext: SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test",
        scratch.display()
    );
    if let Err(source) = fs::create_dir_all(&scratch) {
        fail_closed(&format!(
            "cannot create `{}`: {source}\nnext: sudo chown -R \"$(id -u):$(id -g)\" {}",
            scratch.display(),
            scratch.display()
        ));
    }
    assert!(
        fs_magic(&scratch) == BTRFS_SUPER_MAGIC,
        "fail closed: `{}` is not on Btrfs.\nnext: SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test",
        scratch.display()
    );
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let run = scratch.join(format!("journey-{}-{nanos}", std::process::id()));
    if let Err(source) = fs::create_dir_all(&run) {
        fail_closed(&format!(
            "cannot create `{}`: {source}\nnext: sudo chown -R \"$(id -u):$(id -g)\" {}",
            run.display(),
            run.display()
        ));
    }
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

/// The longest mount containing `path`, exactly as the daemon's store-root resolver sees it.
fn mount_for(path: &Path) -> PathBuf {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

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
}

/// The store root the CLI resolves for `run`: `<mount>/.slipfork-store` on a writable non-`/`
/// mount, else `$XDG_DATA_HOME/slipfork/store` — the same `LogStore::root_for` rule.
fn expected_store_root(run: &Path, data_home: &Path) -> PathBuf {
    let mount = mount_for(run);
    let probe = mount.join(format!(".slipfork-write-probe-{}", std::process::id()));
    let writable = fs::write(&probe, b"").is_ok();
    let _ = fs::remove_file(&probe);
    LogStore::root_for(std::slice::from_ref(&mount), run, data_home, writable)
}

/// The scratch root plus its projects: `Drop` always tears both worlds down and removes the root.
struct Journey {
    run: PathBuf,
    runtime: PathBuf,
    data_home: PathBuf,
    store_root: PathBuf,
    projects: RefCell<Vec<PathBuf>>,
}

impl Journey {
    fn new() -> Self {
        let run = live_run_root();
        let data_home = run.join("data");
        let store_root = expected_store_root(&run, &data_home);
        let engine = BtrfsEngine::new(StoreLayout::at(&store_root));
        for world in [
            "good",
            "baked",
            "broken",
            "logs",
            "buildfail",
            "stagefail",
            "fromfail",
            "parsefail",
            "badfrom",
        ] {
            if let Ok(world) = WorldName::parse(world) {
                let _ = engine.drop_world(&world);
            }
        }
        Self {
            run,
            runtime: require_tmpfs_runtime_dir(),
            data_home,
            store_root,
            projects: RefCell::new(Vec::new()),
        }
    }

    fn project(&self, name: &str, forkfile: &str) -> PathBuf {
        let dir = self.run.join(name);
        fs::create_dir_all(&dir).expect("create project directory");
        let dockerfile = DOCKERFILE.replace(
            "WORKDIR /project",
            &format!(
                "RUN echo slipfork-build-{}\nWORKDIR /project",
                std::process::id()
            ),
        );
        fs::write(dir.join("Dockerfile"), dockerfile).expect("write Dockerfile");
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
            .env("XDG_DATA_HOME", &self.data_home)
            .output()
            .expect("the slipfork binary must be runnable")
    }

    /// Runs one verb with a piped stdin, as a script would.
    fn slipfork_input(&self, project: &Path, args: &[&str], input: &str) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_slipfork"))
            .args(args)
            .current_dir(project)
            .env("NO_COLOR", "1")
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("XDG_STATE_HOME", self.run.join("state"))
            .env("XDG_DATA_HOME", &self.data_home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the slipfork binary must be runnable");
        child
            .stdin
            .as_mut()
            .expect("piped stdin")
            .write_all(input.as_bytes())
            .expect("write the piped input");
        drop(child.stdin.take());
        child.wait_with_output().expect("collect the piped run")
    }

    /// Spawns `slipfork` on a real kernel PTY owned by the test harness.
    fn pty(&self, project: &Path, args: &[&str], cols: u16, rows: u16) -> Pty {
        Pty::spawn(
            project,
            &self.runtime,
            &self.run.join("state"),
            &self.data_home,
            args,
            cols,
            rows,
        )
    }
}

/// A real kernel PTY driven directly by the test harness (never a mock, never `script`).
struct Pty {
    master: File,
    slave: File,
    child: std::process::Child,
    seen: String,
}

impl Pty {
    fn spawn(
        project: &Path,
        runtime: &Path,
        state: &Path,
        data_home: &Path,
        args: &[&str],
        cols: u16,
        rows: u16,
    ) -> Self {
        // SAFETY: every call below acts on descriptors this function just created or on the
        // child's stdio immediately after `fork`.
        unsafe {
            let master_fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            assert!(
                master_fd >= 0,
                "posix_openpt: {}",
                io::Error::last_os_error()
            );
            assert_eq!(libc::grantpt(master_fd), 0, "grantpt");
            assert_eq!(libc::unlockpt(master_fd), 0, "unlockpt");
            let name = libc::ptsname(master_fd);
            assert!(!name.is_null(), "ptsname returned no slave path");
            let slave_path = std::ffi::CStr::from_ptr(name)
                .to_string_lossy()
                .into_owned();
            let winsize = libc::winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            assert_eq!(libc::ioctl(master_fd, libc::TIOCSWINSZ, &winsize), 0);
            // SAFETY: `master_fd` is the live PTY master created just above.
            let flags = libc::fcntl(master_fd, libc::F_GETFL);
            assert!(flags >= 0, "F_GETFL: {}", io::Error::last_os_error());
            assert_eq!(
                libc::fcntl(master_fd, libc::F_SETFL, flags | libc::O_NONBLOCK),
                0,
                "F_SETFL O_NONBLOCK: {}",
                io::Error::last_os_error()
            );
            let master = File::from_raw_fd(master_fd);
            let slave = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&slave_path)
                .expect("open the harness PTY slave");
            let mut command = Command::new(env!("CARGO_BIN_EXE_slipfork"));
            command
                .args(args)
                .current_dir(project)
                .env("NO_COLOR", "1")
                .env("XDG_RUNTIME_DIR", runtime)
                .env("XDG_STATE_HOME", state)
                .env("XDG_DATA_HOME", data_home)
                .stdin(Stdio::from(slave.try_clone().expect("clone the slave")))
                .stdout(Stdio::from(slave.try_clone().expect("clone the slave")))
                .stderr(Stdio::from(slave.try_clone().expect("clone the slave")));
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
            let child = command.spawn().expect("spawn slipfork on the harness PTY");
            Self {
                master,
                slave,
                child,
                seen: String::new(),
            }
        }
    }

    fn send(&mut self, text: &str) {
        let fd = self.master.as_raw_fd();
        let normalized = text.replace('\n', "\r");
        let mut bytes = normalized.as_bytes();
        while !bytes.is_empty() {
            // SAFETY: `bytes` is initialized for its length and `fd` is the live master.
            let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
            if written > 0 {
                bytes = &bytes[written as usize..];
                continue;
            }
            let error = io::Error::last_os_error();
            assert_eq!(
                error.kind(),
                io::ErrorKind::WouldBlock,
                "write to the harness PTY: {error}"
            );
            let mut pollfd = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            // SAFETY: one valid `pollfd` lives across the call.
            unsafe { libc::poll(&mut pollfd, 1, 1000) };
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let winsize = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: `TIOCSWINSZ` only writes the given geometry onto this live PTY pair; the kernel
        // signals `SIGWINCH` to the terminal's foreground process group.
        assert_eq!(
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &winsize) },
            0
        );
    }

    fn expect(&mut self, needle: &str, timeout: Duration) -> String {
        if self.seen.contains(needle) {
            return self.seen.clone();
        }
        let deadline = Instant::now() + timeout;
        let mut chunk = [0_u8; 4096];
        while Instant::now() < deadline {
            match self.master.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    self.seen.push_str(&String::from_utf8_lossy(&chunk[..read]));
                    if self.seen.contains(needle) {
                        return self.seen.clone();
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("read the harness PTY: {error}"),
            }
        }
        panic!("the PTY never printed `{needle}`; saw:\n{}", self.seen);
    }

    /// Drains PTY output until `needle` appears or the bounded window expires.
    fn wait_for(&mut self, needle: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut chunk = [0_u8; 4096];
        while Instant::now() < deadline && !self.seen.contains(needle) {
            match self.master.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => self.seen.push_str(&String::from_utf8_lossy(&chunk[..read])),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("read the harness PTY: {error}"),
            }
        }
        self.seen.contains(needle)
    }

    /// Reads PTY output for the whole `window`, never short-circuiting on text already in `seen`.
    fn drain(&mut self, window: Duration) {
        let deadline = Instant::now() + window;
        let mut chunk = [0_u8; 4096];
        while Instant::now() < deadline {
            match self.master.read(&mut chunk) {
                Ok(0) => std::thread::sleep(Duration::from_millis(10)),
                Ok(read) => self.seen.push_str(&String::from_utf8_lossy(&chunk[..read])),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("read the harness PTY: {error}"),
            }
        }
    }

    /// Waits for the child (bounded), restores-drain, and asserts cooked mode came back.
    fn finish(mut self) -> (i32, String) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("poll the PTY child") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let status = self.child.wait().expect("reap the PTY child");
                panic!(
                    "the PTY child did not exit within 10s (killed: {status}); saw:\n{}",
                    self.seen
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut chunk = [0_u8; 4096];
        while Instant::now() < deadline {
            match self.master.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => self.seen.push_str(&String::from_utf8_lossy(&chunk[..read])),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
        // SAFETY: `tcgetattr` only reads termios for the live slave descriptor.
        let mut termios: libc::termios = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(self.slave.as_raw_fd(), &mut termios) },
            0,
            "read the PTY termios"
        );
        assert!(
            termios.c_lflag & libc::ICANON != 0,
            "cooked mode must be restored on exit"
        );
        (status.code().unwrap_or(-1), self.seen)
    }
}

impl Drop for Journey {
    fn drop(&mut self) {
        if self.runtime.join("daemon.sock").exists() {
            for project in self.projects.borrow().iter() {
                let _ = Command::new(env!("CARGO_BIN_EXE_slipfork"))
                    .arg("down")
                    .current_dir(project)
                    .env("NO_COLOR", "1")
                    .env("XDG_RUNTIME_DIR", &self.runtime)
                    .env("XDG_STATE_HOME", self.run.join("state"))
                    .env("XDG_DATA_HOME", &self.data_home)
                    .output();
            }
            self.shutdown_daemon();
        }
        let runtime_scope = self.runtime.as_os_str().as_bytes();
        let _ = wait_until(
            || {
                processes_mentioning(b"__daemon").into_iter().all(|pid| {
                    fs::read(format!("/proc/{pid}/environ")).is_ok_and(|env| {
                        !env.windows(runtime_scope.len()).any(|w| w == runtime_scope)
                    })
                })
            },
            Duration::from_secs(10),
        );
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
        let store_engine = BtrfsEngine::new(StoreLayout::at(&self.store_root));
        for world in [
            "good",
            "baked",
            "broken",
            "logs",
            "buildfail",
            "stagefail",
            "fromfail",
            "parsefail",
            "badfrom",
        ] {
            if let Ok(world) = WorldName::parse(world) {
                let _ = store_engine.drop_world(&world);
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

/// Every entry name directly under `dir`, sorted, minus the journey's isolated `state` dir.
fn sorted_listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read the scratch root")
        .map(|entry| {
            entry
                .expect("read a scratch entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name != "state")
        .collect();
    names.sort();
    names
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

/// Hands one accepted `:80` descriptor to `ingress.accept` and returns the raw HTTP response.
fn ingress_exchange(runtime: &Path, request: &str) -> String {
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
        .write_all(request.as_bytes())
        .expect("write the ingress request");
    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .expect("read the ingress response");
    response
}

/// A real `:80` connection is handed to `ingress.accept`, routed, and answered by the world pod.
fn assert_ingress(runtime: &Path, world: &str) {
    let response = ingress_exchange(
        runtime,
        &format!("GET / HTTP/1.1\r\nHost: {world}.localhost\r\nConnection: close\r\n\r\n"),
    );
    assert!(
        response.starts_with("HTTP/1.1 200") && response.contains("ok"),
        "the world's own bytes must return through `dial_tcp`: {response}"
    );
}

#[test]
fn world_up_restart_down_journey() {
    for (label, source) in [("good", GOOD_FORKFILE), ("baked", BAKED_FORKFILE)] {
        let doc =
            slipfork_forkfile::parse(source).unwrap_or_else(|_| panic!("`{label}` must parse"));
        assert!(
            slipfork_forkfile::check(&doc).is_ok(),
            "`{label}` must pass static validation"
        );
    }
    let journey = Journey::new();
    let crun = require_tool("crun");
    let unshare = require_tool("unshare");
    require_userns(&unshare);
    require_registry_egress();

    let good = journey.project("good", GOOD_FORKFILE);
    fs::write(good.join("probe.py"), S3_PROBE).expect("write probe.py");

    // The owner's regression: `up` must never convert the checkout into a subvolume, adopt a
    // sibling, or replace a single inode. Record the identity before the first boot.
    assert!(
        !is_subvolume_path(&good).unwrap_or(true),
        "the fixture must be a plain directory, not a subvolume: {}",
        good.display()
    );
    let parent = good.parent().expect("scratch root").to_path_buf();
    let project_before = fs::metadata(&good).expect("stat the project");
    let app_before = fs::metadata(good.join("app.py")).expect("stat app.py");
    let forkfile_before = fs::metadata(good.join("Forkfile")).expect("stat Forkfile");
    let listing_before = sorted_listing(&parent);

    // 1. First boot: exactly one deduplicated `uv sync`, then hooks and both services.
    let first = journey.slipfork(&good, &["up"]);
    let first_out = stdout(&first);
    let first_err = stderr(&first);
    assert!(
        first.status.success(),
        "{}",
        up_failure("first `up` failed", &good, &first)
    );

    // `up` is read-only over its own checkout: identical inodes, an unchanged parent listing, no
    // `.slipfork-adopt` sibling, and still a plain directory (never a subvolume).
    let project_after = fs::metadata(&good).expect("restat the project after `up`");
    assert_eq!(
        (project_after.ino(), project_after.dev()),
        (project_before.ino(), project_before.dev()),
        "`up` must never replace the project directory with a subvolume"
    );
    let app_after = fs::metadata(good.join("app.py")).expect("restat app.py");
    assert_eq!(
        (app_after.ino(), app_after.dev()),
        (app_before.ino(), app_before.dev()),
        "`up` must leave `app.py` in place"
    );
    let forkfile_after = fs::metadata(good.join("Forkfile")).expect("restat Forkfile");
    assert_eq!(
        (forkfile_after.ino(), forkfile_after.dev()),
        (forkfile_before.ino(), forkfile_before.dev()),
        "`up` must leave `Forkfile` in place"
    );
    assert_eq!(
        fs::read_to_string(good.join("app.py")).expect("read app.py after `up`"),
        APP_PY,
        "`up` must never rewrite the checkout's own files"
    );
    assert!(
        !is_subvolume_path(&good).unwrap_or(true),
        "`up` must never convert a plain checkout into a subvolume"
    );
    let listing_after = sorted_listing(&parent);
    assert_eq!(
        listing_after, listing_before,
        "`up` must not touch the project's parent directory"
    );
    assert!(
        !listing_after
            .iter()
            .any(|name| name.contains(".slipfork-adopt")),
        "`up` must never adopt a sibling: {listing_after:?}"
    );
    assert!(
        first_out.contains("✓ Ready -> http://good.localhost"),
        "ready line missing:\n{first_out}"
    );
    assert_ingress(&journey.runtime, "good");

    // A clean, successfully booted world has no failed record: bare `-r` refuses with exactly
    // one `✗` verdict plus one `next:` remedy, and `--json` mirrors that as `no-failed-run`.
    let no_failed = journey.slipfork(&good, &["logs", "-r"]);
    assert!(
        !no_failed.status.success(),
        "bare `logs -r` in a clean world must exit non-zero"
    );
    assert!(
        stdout(&no_failed).is_empty(),
        "a refusal must not replay a header:\n{}",
        stdout(&no_failed)
    );
    let no_failed_err = stderr(&no_failed);
    let lines: Vec<&str> = no_failed_err.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "a refusal is exactly one verdict + one remedy:\n{no_failed_err}"
    );
    assert!(
        lines[0].starts_with("✗ no failed run in this world"),
        "the verdict names the missing failed run:\n{no_failed_err}"
    );
    assert_eq!(lines[1], "next: slipfork up", "{no_failed_err}");
    let no_failed_json = journey.slipfork(&good, &["logs", "-r", "--json"]);
    assert!(!no_failed_json.status.success());
    let no_failed_json_text = stdout(&no_failed_json);
    let error: serde_json::Value =
        serde_json::from_str(no_failed_json_text.trim()).unwrap_or_else(|err| {
            panic!("clean-world `--json` is one object ({err}):\n{no_failed_json_text}")
        });
    assert_eq!(error["error"]["code"], "no-failed-run", "{error}");
    assert_eq!(
        error["error"]["message"], "no failed run in this world (see slipfork logs --runs)",
        "{error}"
    );
    assert_eq!(error["error"]["next"], "slipfork up", "{error}");
    assert!(
        !no_failed_json_text.contains('\u{1b}'),
        "clean-world `--json` stays ANSI-free"
    );

    assert!(
        first_out.contains("→ [1/"),
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

    // 1a. Multi-S3 object roundtrips on a private bucket (`PUT`/auth `GET` 200, anon `GET`/`PUT`/
    // `DELETE` and tampered SigV4 403) and a public-read bucket (`PUT` 200, anon `GET` 200).
    let probe = good.join("s3.codes");
    let probed = wait_until(
        || {
            fs::read_to_string(&probe)
                .is_ok_and(|codes| codes == "200 200:priv 403 403 403 403 200 200:cdn hello")
        },
        Duration::from_secs(30),
    );
    assert!(
        probed,
        "the S3 probe never wrote `200 200:priv 403 403 403 403 200 200:cdn hello`: {:?}",
        fs::read_to_string(&probe)
    );
    let cdn_get = ingress_exchange(
        &journey.runtime,
        "GET /x HTTP/1.1\r\nHost: assets.good.localhost\r\nConnection: close\r\n\r\n",
    );
    assert!(
        cdn_get.starts_with("HTTP/1.1 200") && cdn_get.ends_with("cdn"),
        "public-read bucket must serve anonymous GET 200 on assets.<world>.localhost: {cdn_get}"
    );
    let cdn_put = ingress_exchange(
        &journey.runtime,
        "PUT /x HTTP/1.1\r\nHost: assets.good.localhost\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope",
    );
    assert!(
        cdn_put.starts_with("HTTP/1.1 403"),
        "public-read bucket must refuse anonymous PUT with 403 on assets.<world>.localhost: {cdn_put}"
    );

    // 1b. Captured logs and ad-hoc exec against the real pod. Capture is asynchronous, so poll
    // the follow-less read until the pod's own startup line has landed.
    let mut log_text = String::new();
    let captured = wait_until(
        || {
            log_text = stdout(&journey.slipfork(&good, &["logs", "api"]));
            log_text.contains("api ready")
        },
        Duration::from_secs(15),
    );
    assert!(
        captured,
        "the api pod never wrote its startup line: {log_text}"
    );
    assert!(
        log_text.contains("── api started ──"),
        "logs must draw the capture divider: {log_text}"
    );
    assert!(
        log_text.lines().any(|line| {
            line.starts_with("api") && line.contains('│') && line.trim_end().ends_with("api ready")
        }),
        "logs must render an aligned gutter per pod: {log_text}"
    );
    assert!(!log_text.contains('\u{1b}'), "NO_COLOR logs stay plain");
    let bare_logs = stdout(&journey.slipfork(&good, &["logs"]));
    assert!(
        bare_logs.contains("api") && !bare_logs.contains("uploads"),
        "bare `logs` must stream services only and exclude S3 resources:\n{bare_logs}"
    );
    let all_logs = journey.slipfork(&good, &["logs", "-a"]);
    assert!(all_logs.status.success(), "{}", stderr(&all_logs));
    let all_logs = stdout(&all_logs);
    assert!(
        all_logs.contains("api"),
        "`logs -a` streams pod-backed resources:\n{all_logs}"
    );
    assert!(
        !all_logs.contains("uploads"),
        "native S3 has no pod logs, so `logs -a` never targets a bucket:\n{all_logs}"
    );
    let bucket_logs = journey.slipfork(&good, &["logs", "uploads"]);
    assert!(
        !bucket_logs.status.success(),
        "a bucket has no pod log stream"
    );
    let bucket_text = format!("{}{}", stdout(&bucket_logs), stderr(&bucket_logs));
    assert!(
        bucket_text.contains("native S3 bucket 'uploads' has no pod logs")
            && bucket_text.contains("next:"),
        "bucket logs must fail with the one-line remedy:\n{bucket_text}"
    );

    let logs_json = journey.slipfork(&good, &["logs", "--json", "api", "worker", "--since", "5m"]);
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
    for frame in &frames {
        if matches!(frame["kind"].as_str(), Some("line" | "divider" | "eof")) {
            assert!(
                frame["ts_ms"].is_u64(),
                "every piped `--json` log frame carries `ts_ms`: {frame:?}"
            );
        }
    }
    assert!(
        !stdout(&logs_json).contains('\u{1b}'),
        "`--json` stays plain"
    );

    // The same window on a real kernel PTY: aligned timestamp gutter, one row per capture entry,
    // no escape leakage, and no unexpected stderr text merged into the scrollback.
    let (pty_code, scrollback) = journey
        .pty(
            &good,
            &["logs", "api", "worker", "--since", "5m", "-t"],
            140,
            40,
        )
        .finish();
    assert_eq!(
        pty_code, 0,
        "`logs api worker --since 5m -t` on a PTY must exit 0:\n{scrollback}"
    );
    assert!(
        !scrollback.contains('\u{1b}'),
        "NO_COLOR PTY scrollback stays plain:\n{scrollback}"
    );
    assert!(
        !scrollback.contains("error:"),
        "unexpected stderr merged into the scrollback:\n{scrollback}"
    );
    let rows: Vec<&str> = scrollback
        .lines()
        .filter(|line| line.contains('│'))
        .collect();
    assert!(rows.len() >= 2, "both targets render rows:\n{scrollback}");
    assert!(
        rows.iter().any(|row| row.contains("api")),
        "the api gutter is missing:\n{scrollback}"
    );
    assert!(
        rows.iter().any(|row| row.contains("worker")),
        "the worker gutter is missing:\n{scrollback}"
    );
    let columns: Vec<usize> = rows
        .iter()
        .map(|row| row.find('│').expect("every row has a gutter"))
        .collect();
    assert!(
        columns.windows(2).all(|pair| pair[0] == pair[1]),
        "the timestamp and pod gutters stay aligned:\n{scrollback}"
    );
    for row in &rows {
        let stamp = row.as_bytes();
        assert!(stamp.len() >= 20, "row is too short: {row:?}");
        assert!(
            stamp[4] == b'-'
                && stamp[7] == b'-'
                && stamp[10] == b' '
                && stamp[13] == b':'
                && stamp[16] == b':',
            "expected a `YYYY-MM-DD HH:MM:SS` gutter: {row:?}"
        );
        assert_eq!(
            stamp[19], b' ',
            "the timestamp gutter ends before the pod: {row:?}"
        );
        assert_eq!(
            row.matches('│').count(),
            1,
            "one capture entry renders one row: {row:?}"
        );
    }

    let future = journey.slipfork(&good, &["logs", "api", "--since", "2999-01-01T00:00:00Z"]);
    assert!(future.status.success(), "{}", stderr(&future));
    assert!(
        !stdout(&future).contains("api ready"),
        "an RFC 3339 future window must empty the replay"
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

    // 1c. One unified `shell`: kind dispatch lives in the daemon, exact refusals on stderr, and a
    // real PTY with `SIGWINCH` forwarding plus cooked-mode restoration.
    let sql = journey.slipfork_input(&good, &["shell", "app_db"], "select 1;\n");
    assert_eq!(
        sql.status.code(),
        Some(0),
        "piped `shell app_db` must succeed:\nstdout={}\nstderr={}",
        stdout(&sql),
        stderr(&sql)
    );
    assert!(
        stdout(&sql).contains('1'),
        "piped psql must print the row: {}",
        stdout(&sql)
    );
    // The declared `database app_db` must exist as a real database, not merely be reachable
    // through a fallback connection to the `postgres` default database.
    let database = journey.slipfork(
        &good,
        &[
            "exec", "pg", "--", "psql", "-d", "app_db", "-tAc", "select 1",
        ],
    );
    assert_eq!(
        database.status.code(),
        Some(0),
        "`psql -d app_db` must succeed after `up`:\nstdout={}\nstderr={}",
        stdout(&database),
        stderr(&database)
    );
    assert!(
        stdout(&database).contains('1'),
        "the declared database must answer a query: {}",
        stdout(&database)
    );
    let cache = journey.slipfork_input(&good, &["shell", "cache"], "ping\n");
    assert_eq!(
        cache.status.code(),
        Some(0),
        "piped `shell cache` must succeed: {}",
        stderr(&cache)
    );
    assert!(
        stdout(&cache).contains("PONG"),
        "piped redis-cli must answer PING: {}",
        stdout(&cache)
    );

    let bucket = journey.slipfork(&good, &["shell", "uploads"]);
    assert_eq!(bucket.status.code(), Some(1));
    let refused = stderr(&bucket);
    assert!(
        refused.contains("✗ 'uploads' is an s3 bucket, not a shell"),
        "{refused}"
    );
    assert!(
        refused.contains("\nnext: slipfork shell api, worker, probe, pg, cache"),
        "the refusal must list every shellable target at column 0: {refused}"
    );
    assert!(
        !refused.contains("\n  next:"),
        "the remedy never carries a residual indent: {refused}"
    );
    assert!(!refused.contains('\u{1b}'), "NO_COLOR refusals stay plain");
    let unknown = journey.slipfork(&good, &["shell", "apii"]);
    assert_eq!(unknown.status.code(), Some(1));
    let refused = stderr(&unknown);
    assert!(
        refused.contains("✗ No shellable target 'apii'"),
        "{refused}"
    );
    assert!(
        refused.contains("\nnext: slipfork shell api"),
        "the remedy starts at column 0: {refused}"
    );

    let mut api = journey.pty(&good, &["shell", "api"], 120, 40);
    api.expect("(good:api)", Duration::from_secs(20));
    // The inner shell must be a real pts, with stdin and stdout both terminals.
    api.send("tty; test -t 0 && test -t 1 && echo TTY-OK\n");
    api.expect("/dev/pts/", Duration::from_secs(10));
    api.expect("TTY-OK", Duration::from_secs(10));
    api.send("stty size\n");
    api.expect("40 120", Duration::from_secs(10));
    api.resize(100, 30);
    // SIGWINCH delivery is racy, so re-ask until the kernel reports the new geometry.
    let resized = wait_until(
        || {
            api.send("stty size\n");
            api.wait_for("30 100", Duration::from_millis(500))
        },
        Duration::from_secs(10),
    );
    assert!(
        resized,
        "the resized geometry never reached the PTY; saw:\n{}",
        api.seen
    );
    api.send("exit\n");
    let (code, seen) = api.finish();
    assert_eq!(code, 0, "`shell api` must exit 0:\n{seen}");
    assert!(
        !seen.contains("can't access tty; job control turned off"),
        "a real PTY must never degrade to `sh`:\n{seen}"
    );

    let mut pg = journey.pty(&good, &["shell", "pg"], 120, 40);
    pg.expect("=#", Duration::from_secs(20));
    pg.send("select 1;\n");
    pg.expect("(1 row)", Duration::from_secs(10));
    pg.send("\\q\n");
    let (code, seen) = pg.finish();
    assert_eq!(code, 0, "`shell pg` must exit 0:\n{seen}");

    let mut redis = journey.pty(&good, &["shell", "cache"], 120, 40);
    redis.expect("6379>", Duration::from_secs(20));
    redis.send("ping\n");
    redis.expect("PONG", Duration::from_secs(10));
    redis.send("quit\n");
    let (code, seen) = redis.finish();
    assert_eq!(code, 0, "`shell cache` must exit 0:\n{seen}");

    // `exec` auto-attaches a real kernel PTY on a terminal, while `--no-interactive` keeps the
    // child on a plain pipe whose stdin hits EOF without hanging the relay.
    let mut exec_tty = journey.pty(
        &good,
        &[
            "exec",
            "api",
            "--",
            "sh",
            "-c",
            "tty; test -t 0 && test -t 1 && echo EXEC-TTY-OK",
        ],
        120,
        40,
    );
    exec_tty.expect("/dev/pts/", Duration::from_secs(20));
    exec_tty.expect("EXEC-TTY-OK", Duration::from_secs(10));
    let (code, seen) = exec_tty.finish();
    assert_eq!(code, 0, "`exec` on a real PTY must exit 0:\n{seen}");

    let forwarded = journey
        .pty(&good, &["exec", "api", "--", "sh", "-c", "exit 7"], 120, 40)
        .finish();
    assert_eq!(
        forwarded.0, 7,
        "`exec` must forward the child's exit code over a PTY:\n{}",
        forwarded.1
    );

    let mut managed = journey.pty(
        &good,
        &[
            "exec",
            "-i",
            "api",
            "--",
            "sh",
            "-c",
            "test -t 0 && echo IN-TTY-OK",
        ],
        120,
        40,
    );
    managed.expect("IN-TTY-OK", Duration::from_secs(20));
    let (code, seen) = managed.finish();
    assert_eq!(code, 0, "`exec -i` on a PTY must exit 0:\n{seen}");

    let mut exec_pipe = journey.pty(
        &good,
        &[
            "exec",
            "--no-interactive",
            "api",
            "--",
            "sh",
            "-c",
            "test -t 0 && echo IN-TTY; cat; echo CAT-EOF",
        ],
        120,
        40,
    );
    exec_pipe.expect("CAT-EOF", Duration::from_secs(20));
    let (code, seen) = exec_pipe.finish();
    assert_eq!(code, 0, "`exec --no-interactive` must exit 0:\n{seen}");
    assert!(
        !seen.contains("IN-TTY"),
        "`--no-interactive` must never allocate a PTY:\n{seen}"
    );

    let no_tty = journey.slipfork_input(&good, &["exec", "-i", "api", "--", "true"], "");
    assert_eq!(no_tty.status.code(), Some(1));
    let refused = stderr(&no_tty);
    assert!(refused.contains("--interactive"), "{refused}");
    assert_eq!(refused.matches("next:").count(), 1, "{refused}");

    let piped = journey.slipfork_input(&good, &["exec", "api", "--", "cat"], "piped-exec\n");
    assert_eq!(
        piped.status.code(),
        Some(0),
        "piped `exec` must succeed:\nstdout={}\nstderr={}",
        stdout(&piped),
        stderr(&piped)
    );
    assert!(
        stdout(&piped).contains("piped-exec"),
        "piped stdin must reach the child: {}",
        stdout(&piped)
    );

    // 1d. Image build logs replay straight from `<store>/logs/<world>/<image>.log`.
    let image_log = journey.store_root.join("logs").join("good").join("app.log");
    assert!(
        wait_until(|| image_log.is_file(), Duration::from_secs(20)),
        "the image builder must record `{}`",
        image_log.display()
    );
    let image_logs = journey.slipfork(&good, &["logs", "app"]);
    assert!(
        image_logs.status.success(),
        "`logs app` must read the capture file: {}",
        stderr(&image_logs)
    );
    assert!(
        !stdout(&image_logs).trim().is_empty(),
        "`logs app` must replay the build output"
    );

    // 2. An image's baked `CMD` runs when the service declares no `run:`.
    let baked = journey.project("baked", BAKED_FORKFILE);
    fs::write(baked.join("Dockerfile"), BAKED_DOCKERFILE).expect("write baked Dockerfile");
    let baked_up = journey.slipfork(&baked, &["up"]);
    assert!(
        baked_up.status.success(),
        "{}",
        up_failure("a baked entrypoint must boot", &baked, &baked_up)
    );
    assert_eq!(
        fs::read_to_string(baked.join("entrypoint.txt")).unwrap_or_default(),
        "entrypoint",
        "the image's `CMD` must be exec'd by crun, not replaced by `true`"
    );
    let baked_down = journey.slipfork(&baked, &["down"]);
    assert!(baked_down.status.success(), "{}", stderr(&baked_down));

    // 3. A failed service keeps the world running; `down` must still work.
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
        failed_out.contains("→ [") || failed_out.contains('✗'),
        "failure must keep a phase header or the ✗ tail:\nstdout={failed_out}\nstderr={failed_err}"
    );
    // v3 splits the failure copy: the narrative (`✗ start broken …` phase row and its gutter
    // output) stays on stdout, while the verdict and the one catalog-derived remedy go to stderr.
    // `BROKEN_FORKFILE` exits 7, so the verdict carries that exact code.
    let stderr_lines: Vec<&str> = failed_err
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(
        stderr_lines.len(),
        2,
        "a service failure prints one verdict and one remedy on stderr:\nstdout={failed_out}\nstderr={failed_err}"
    );
    assert_eq!(
        stderr_lines[0], "✗ start broken failed (exit 7)",
        "the verdict names the phase verb, target, and exit code:\nstderr={failed_err}"
    );
    assert!(
        stderr_lines[1].starts_with("next: slipfork logs -r ")
            && stderr_lines[1].len() > "next: slipfork logs -r ".len(),
        "the one remedy replays a catalog run:\nstderr={failed_err}"
    );
    let has_start_broken = failed_out.lines().any(|line| {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        tokens.len() >= 4
            && matches!(tokens[0], "→" | "✗")
            && tokens[1].starts_with('[')
            && tokens[2] == "start"
            && tokens[3] == "broken"
    });
    assert!(
        has_start_broken && !failed_out.contains("failed (exit 7)"),
        "the narrative stays on stdout and the verdict does not repeat there:\nstdout={failed_out}"
    );
    assert!(
        !failed_out
            .lines()
            .any(|line| line.matches("uv sync").count() > 1),
        "a `setup` row whose command equals its target must never repeat `uv sync`:\nstdout={failed_out}"
    );
    let failure_copy = format!("{failed_out}\n{failed_err}");
    let remedies: Vec<&str> = failure_copy
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("next: slipfork logs -r "))
        .collect();
    assert_eq!(
        remedies.len(),
        1,
        "a service failure prints exactly one `-r` remedy across both streams:\n{failure_copy}"
    );
    assert!(
        !remedies[0].is_empty(),
        "the one remedy must replay a run:\n{failure_copy}"
    );
    let run_id = remedies[0].to_owned();
    assert_eq!(failure_copy.matches("next:").count(), 1, "{failure_copy}");
    for forbidden in ["error:", "check logs with:", "Forkfile:"] {
        assert!(
            !failure_copy.contains(forbidden),
            "a service failure must not print `{forbidden}`:\n{failure_copy}"
        );
    }
    let replay = journey.slipfork(&broken, &["logs", "-r", &run_id]);
    assert!(
        replay.status.success(),
        "the printed `-r {run_id}` must replay the failed service run: {}",
        stderr(&replay)
    );
    let by_target = journey.slipfork(&broken, &["logs", "broken"]);
    assert!(
        by_target.status.success(),
        "`logs broken` must read the failed service capture: {}",
        stderr(&by_target)
    );
    assert!(
        broken.join(".slipfork/world.json").is_file(),
        "the world must stay up after a failed service"
    );
    let listed = journey.slipfork(&broken, &["status"]);
    let listed_text = stdout(&listed);
    assert!(
        listed.status.success() && listed_text.contains("broken"),
        "the failed world must stay listed by status:\n{listed_text}"
    );
    assert!(
        listed_text.lines().any(|line| {
            let row = line.trim_end();
            row.contains("broken") && row.ends_with("failed")
        }),
        "the failed service row must render `broken` with `failed` as its last column:\n{listed_text}"
    );
    // Re-running `up` on the live failed world takes `Heal` (`Request::Restart`), refuses `Ready`
    // when `broken` exits again, and leaves `down` able to reap the restarted pod cleanly.
    let heal_failed = journey.slipfork(&broken, &["up"]);
    assert!(
        !heal_failed.status.success(),
        "heal of a permanently broken service must fail:\n{}",
        up_failure("second `up` on broken succeeded", &broken, &heal_failed)
    );
    assert!(
        !stdout(&heal_failed).contains("✓ Ready")
            && stderr(&heal_failed).contains("next: slipfork logs broken"),
        "heal failure must name `next: slipfork logs broken`:\nstdout={}\nstderr={}",
        stdout(&heal_failed),
        stderr(&heal_failed)
    );
    let down = journey.slipfork(&broken, &["down"]);
    assert!(
        down.status.success(),
        "down after failure: {}",
        stderr(&down)
    );
    assert!(
        stdout(&down).contains("→ [1/1] stopping broken"),
        "down must stream its phase header:\n{}",
        stdout(&down)
    );

    // 3a. The failed service run is still the newest record: a real `down` (never a later
    // successful rebuild) keeps `logs broken`, `logs -r <id>` and `--runs` readable.
    for args in [
        &["logs", "broken"][..],
        &["logs", "-r", run_id.as_str()][..],
        &["logs", "--runs"][..],
    ] {
        let replay = journey.slipfork(&broken, args);
        assert!(
            replay.status.success(),
            "`{args:?}` must work after `down`: {}",
            stderr(&replay)
        );
    }
    let catalog = journey.slipfork(&broken, &["logs", "--runs"]);
    assert!(
        stdout(&catalog)
            .lines()
            .any(|line| line.contains(&format!("#{run_id}")) && line.contains("failed")),
        "the failed service run survives `down`:\n{}",
        stdout(&catalog)
    );

    // 3b. A failing image `RUN` is a durable catalog run: `logs <image>` and `logs -r <id>` replay
    // the build output, `--runs` lists it, and a later successful boot plus `down` keeps all three.
    let buildfail = journey.project("buildfail", CATALOG_FORKFILE);
    fs::write(buildfail.join("Dockerfile"), FAIL_BUILD_DOCKERFILE)
        .expect("write the failing Dockerfile");
    let failed_build = journey.slipfork(&buildfail, &["up"]);
    assert_eq!(
        failed_build.status.code(),
        Some(1),
        "{}",
        up_failure(
            "a failing image RUN must fail `up`",
            &buildfail,
            &failed_build
        )
    );
    let build_failure = format!("{}{}", stdout(&failed_build), stderr(&failed_build));
    assert_eq!(
        build_failure.matches("next:").count(),
        1,
        "an image failure prints exactly one `next:`:\n{build_failure}"
    );
    let build_run = build_failure
        .lines()
        .find_map(|line| line.trim().strip_prefix("next: slipfork logs -r "))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| panic!("the image failure must name its run:\n{build_failure}"))
        .to_owned();
    let image_log = journey
        .store_root
        .join("logs")
        .join("buildfail")
        .join("app.log");
    assert!(
        image_log.is_file(),
        "the failed build must leave `{}`",
        image_log.display()
    );
    assert!(
        !buildfail.join(".slipfork/world.json").exists(),
        "a cold image build failure with zero pods must tear its empty world down immediately"
    );
    let by_image = journey.slipfork(&buildfail, &["logs", "app"]);
    assert!(
        by_image.status.success(),
        "`logs app` after a failed build: {}",
        stderr(&by_image)
    );
    assert!(
        stdout(&by_image).contains("out-line"),
        "`logs app` must replay the failed RUN output:\n{}",
        stdout(&by_image)
    );
    let by_run = journey.slipfork(&buildfail, &["logs", "-r", &build_run]);
    assert!(
        by_run.status.success(),
        "`logs -r {build_run}`: {}",
        stderr(&by_run)
    );
    assert!(
        stdout(&by_run).contains("out-line"),
        "`logs -r {build_run}` must replay the failed RUN output:\n{}",
        stdout(&by_run)
    );
    // The replay is a real capture view: a `#<id>  RUN  app  sh -c …  <started>` header, then the
    // v3 stream gutter (`│` stdout, `┃` stderr) with a UTC clock, then the record's verdict.
    let is_clock = |text: &str| {
        let bytes = text.as_bytes();
        bytes.len() == 12
            && bytes[2] == b':'
            && bytes[5] == b':'
            && bytes[8] == b'.'
            && [0, 1, 3, 4, 6, 7, 9, 10, 11]
                .iter()
                .all(|index| bytes[*index].is_ascii_digit())
    };
    for (label, output) in [
        ("logs app", stdout(&by_image)),
        ("logs -r <id>", stdout(&by_run)),
    ] {
        let header = output
            .lines()
            .find(|line| line.starts_with(&format!("#{build_run}")))
            .unwrap_or_else(|| panic!("`{label}` must print a `#<id>` run header:\n{output}"));
        let fields: Vec<&str> = header.split("  ").collect();
        assert!(
            fields.len() >= 5
                && fields[0] == format!("#{build_run}")
                && fields[1] == "RUN"
                && fields[2] == "app"
                && fields[3] == "sh -c 'echo out-line; echo err-line >&2; exit 3'",
            "`{label}` header must name `#<id>  RUN  app  <command as written>`:\n{header:?}"
        );
        assert_eq!(
            header.matches("app").count(),
            1,
            "`{label}` must never print the image id twice:\n{header:?}"
        );
        let started = fields.last().copied().unwrap_or_default();
        assert!(
            started.contains('-') && started.contains(':'),
            "`{label}` header must end with a date+time, saw {started:?}:\n{header:?}"
        );
        let gutter_clock = |needle: &str, glyph: char| {
            output.lines().find_map(|line| {
                let (clock, rest) = line.trim_start().split_once(' ')?;
                (rest.starts_with(glyph) && rest.contains(needle)).then_some(clock)
            })
        };
        let out_clock = gutter_clock("out-line", '│').unwrap_or_else(|| {
            panic!("`{label}` must replay stdout under the `│` gutter:\n{output}")
        });
        let err_clock = gutter_clock("err-line", '┃').unwrap_or_else(|| {
            panic!("`{label}` must replay stderr under the `┃` gutter:\n{output}")
        });
        for clock in [out_clock, err_clock] {
            assert!(
                is_clock(clock),
                "`{label}` timestamps each replay row, saw {clock:?}:\n{output}"
            );
        }
        assert!(
            !output.contains('\u{1b}'),
            "`{label}` stays ANSI-free:\n{output}"
        );
        assert!(
            !output.contains("out out-line") && !output.contains("err err-line"),
            "`{label}` never prints the removed `out`/`err` words:\n{output}"
        );
        assert!(
            !output.contains("build `app` failed")
                && !output.contains("failed:")
                && !output.contains("next:")
                && !output.contains("✗ Failed")
                && !output.contains("Failed to "),
            "the failure summary stays outside the capture:\n{output}"
        );
        assert!(
            output.trim_end().ends_with("✗ RUN app failed (exit 3)"),
            "`{label}` must end with the record's verdict:\n{output}"
        );
    }

    let replay_json = journey.slipfork(&buildfail, &["logs", "-r", &build_run, "--json"]);
    assert!(
        replay_json.status.success(),
        "`logs -r {build_run} --json`: {}",
        stderr(&replay_json)
    );
    let replay_json_text = stdout(&replay_json);
    let replay_events: Vec<serde_json::Value> = replay_json_text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|error| {
                panic!("`logs -r --json` must emit one JSON object per line ({error}): {line:?}")
            })
        })
        .collect();
    assert!(
        !replay_json_text.contains('\u{1b}'),
        "`logs -r --json` stays ANSI-free:\n{replay_json_text}"
    );
    assert!(
        replay_events.iter().any(|event| {
            event["line"] == "out-line" && event["stream"] == "stdout" && event["ts_ms"].is_u64()
        }),
        "`--json` must carry the stdout stream and its timestamp: {replay_events:?}"
    );
    assert!(
        replay_events.iter().any(|event| {
            event["line"] == "err-line" && event["stream"] == "stderr" && event["ts_ms"].is_u64()
        }),
        "`--json` must carry the stderr stream and its timestamp: {replay_events:?}"
    );
    assert!(
        replay_json_text.contains("\"stream\":\"stderr\""),
        "`logs -r --json` must carry the stderr stream: {replay_json_text}"
    );
    let replay_run = replay_events
        .iter()
        .find(|event| event["run_id"].as_str() == Some(build_run.as_str()))
        .unwrap_or_else(|| {
            panic!("`logs -r --json` must emit the run record bare: {replay_events:?}")
        });
    assert_eq!(
        replay_events
            .first()
            .and_then(|event| event["run_id"].as_str()),
        Some(build_run.as_str()),
        "`logs -r --json` emits the record bare as its first line: {replay_events:?}"
    );
    assert_eq!(replay_run["phase"], "run", "{replay_run}");
    assert_eq!(
        replay_run["instruction"], "sh -c 'echo out-line; echo err-line >&2; exit 3'",
        "{replay_run}"
    );
    assert_eq!(replay_run["source"], "Dockerfile:2", "{replay_run}");
    assert_eq!(replay_run["exit_code"], 3, "{replay_run}");

    // 3c. Bare `-r` answers "what just broke"; `last` names the newest record, and `--json`
    // mirrors every form with the same run id.
    let bare_replay = journey.slipfork(&buildfail, &["logs", "-r"]);
    assert!(
        bare_replay.status.success(),
        "bare `logs -r` must replay the failing RUN: {}",
        stderr(&bare_replay)
    );
    let bare_text = stdout(&bare_replay);
    assert!(
        bare_text.contains(&format!("#{build_run}"))
            && bare_text.contains("sh -c 'echo out-line; echo err-line >&2; exit 3'")
            && bare_text.trim_end().ends_with("✗ RUN app failed (exit 3)"),
        "bare `logs -r` must replay the failed RUN header, command and verdict:\n{bare_text}"
    );
    // A newer successful boot moves `last`; the failed exit-3 RUN stays the `failed` selector.
    fs::write(buildfail.join("Dockerfile"), DOCKERFILE).expect("write the succeeding Dockerfile");
    let rebuilt = journey.slipfork(&buildfail, &["up"]);
    assert!(
        rebuilt.status.success(),
        "{}",
        up_failure("the fixed Dockerfile must boot", &buildfail, &rebuilt)
    );
    let runs_json = journey.slipfork(&buildfail, &["logs", "--runs", "--json"]);
    assert!(
        runs_json.status.success(),
        "`logs --runs --json`: {}",
        stderr(&runs_json)
    );
    let success_run = stdout(&runs_json)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|record| record["run_id"].as_str() != Some(build_run.as_str()))
        .max_by_key(|record| record["started_ms"].as_u64())
        .and_then(|record| record["run_id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| {
            panic!(
                "the successful boot must record a newer run:\n{}",
                stdout(&runs_json)
            )
        });
    assert_ne!(
        success_run, build_run,
        "`last` must be distinguishable from `failed`"
    );
    let last_replay = journey.slipfork(&buildfail, &["logs", "-r", "last"]);
    assert!(
        last_replay.status.success(),
        "`logs -r last`: {}",
        stderr(&last_replay)
    );
    assert!(
        stdout(&last_replay).contains(&format!("#{success_run}")),
        "`logs -r last` must return the newest successful record `{success_run}`:\n{}",
        stdout(&last_replay)
    );
    let bare_after = journey.slipfork(&buildfail, &["logs", "-r"]);
    assert!(
        bare_after.status.success() && stdout(&bare_after).contains(&format!("#{build_run}")),
        "bare `logs -r` must keep replaying the failed record:\n{}",
        stderr(&bare_after)
    );
    for (args, expected) in [
        (&["logs", "-r", "--json"][..], build_run.as_str()),
        (
            &["logs", "-r", build_run.as_str(), "--json"][..],
            build_run.as_str(),
        ),
        (&["logs", "-r", "last", "--json"][..], success_run.as_str()),
    ] {
        let json_replay = journey.slipfork(&buildfail, args);
        assert!(
            json_replay.status.success(),
            "`{args:?}`: {}",
            stderr(&json_replay)
        );
        let text = stdout(&json_replay);
        let first: serde_json::Value = serde_json::from_str(
            text.lines()
                .next()
                .unwrap_or_else(|| panic!("`{args:?}` emits no record:\n{text}")),
        )
        .unwrap_or_else(|error| panic!("`{args:?}` first line is JSON ({error}):\n{text}"));
        assert_eq!(
            first["run_id"].as_str(),
            Some(expected),
            "`{args:?}` must mirror `{expected}`:\n{text}"
        );
        assert!(!text.contains('\u{1b}'), "`{args:?}` stays ANSI-free");
    }
    let down = journey.slipfork(&buildfail, &["down"]);
    assert!(
        down.status.success(),
        "down after the successful rebuild: {}",
        stderr(&down)
    );
    let catalog = journey.slipfork(&buildfail, &["logs", "--runs"]);
    assert!(
        catalog.status.success(),
        "`logs --runs`: {}",
        stderr(&catalog)
    );
    let catalog_text = stdout(&catalog);
    assert!(
        catalog_text
            .lines()
            .any(|line| line.contains(&format!("#{build_run}")) && line.contains("failed")),
        "`--runs` must list the failed image run:\n{catalog_text}"
    );
    let catalog_json = journey.slipfork(&buildfail, &["logs", "--runs", "--json"]);
    assert!(
        catalog_json.status.success(),
        "`logs --runs --json`: {}",
        stderr(&catalog_json)
    );
    let catalog_json_text = stdout(&catalog_json);
    assert!(
        !catalog_json_text.contains('\u{1b}'),
        "`--runs --json` stays escape-free:\n{catalog_json_text}"
    );
    let record = catalog_json_text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| value["run_id"].as_str() == Some(build_run.as_str()))
        .unwrap_or_else(|| panic!("no JSON record for {build_run}:\n{catalog_json_text}"));
    assert_eq!(record["kind"], "image", "{record}");
    assert_eq!(record["target"], "app", "{record}");
    assert_eq!(record["state"], "failed", "{record}");
    assert_eq!(record["phase"], "run", "{record}");
    assert_eq!(
        record["instruction"], "sh -c 'echo out-line; echo err-line >&2; exit 3'",
        "{record}"
    );
    assert_eq!(record["source"], "Dockerfile:2", "{record}");
    assert_eq!(record["exit_code"], 3, "{record}");
    assert!(record["started_ms"].is_u64(), "{record}");
    assert!(record["finished_ms"].is_u64(), "{record}");
    assert!(record["duration_ms"].is_u64(), "{record}");
    assert_eq!(record["world"], "buildfail", "{record}");
    assert!(
        record["project"]
            .as_str()
            .is_some_and(|project| project.ends_with("buildfail")),
        "{record}"
    );

    // The bare run id is a first-class target, not only an `-r` spec.
    let bare_run = journey.slipfork(&buildfail, &["logs", &build_run]);
    assert!(
        bare_run.status.success(),
        "`logs {build_run}` must replay the run: {}",
        stderr(&bare_run)
    );
    assert!(
        stdout(&bare_run).contains("out-line"),
        "`logs {build_run}` must replay the captured output:\n{}",
        stdout(&bare_run)
    );

    // 3c. Every failing instruction explains itself. An engine-executed step (`COPY --from` a
    // missing stage, a `FROM` that cannot be pulled) names the instruction in the replay header,
    // writes its raw one-line cause as that step's `┃` output, and still ends with a verdict naming
    // the verb. A failure before any step puts the reason in the verdict instead of an empty block.
    let stagefail = journey.project("stagefail", CATALOG_FORKFILE);
    fs::write(stagefail.join("Dockerfile"), MISSING_STAGE_DOCKERFILE)
        .expect("write the missing-stage Dockerfile");
    let stage_build = journey.slipfork(&stagefail, &["up"]);
    assert_eq!(
        stage_build.status.code(),
        Some(1),
        "{}",
        up_failure(
            "a missing `COPY --from` stage must fail `up`",
            &stagefail,
            &stage_build
        )
    );
    let stage_block = format!("{}{}", stdout(&stage_build), stderr(&stage_build));
    assert_eq!(
        stage_block.matches("next:").count(),
        1,
        "an engine-step failure prints exactly one `next:`:\n{stage_block}"
    );
    assert!(
        stage_block.contains("✗ COPY app failed"),
        "`up` names the failing instruction in its verdict:\n{stage_block}"
    );
    assert!(
        stage_block
            .lines()
            .any(|line| line.contains("┃ no build stage named `missing-stage`")),
        "`up` streams the failing step's `┃` reason line:\n{stage_block}"
    );
    let stage_run = stage_block
        .lines()
        .find_map(|line| line.trim().strip_prefix("next: slipfork logs -r "))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| panic!("the COPY failure must name its run:\n{stage_block}"))
        .to_owned();
    let stage_replay = journey.slipfork(&stagefail, &["logs", "-r", &stage_run]);
    assert!(
        stage_replay.status.success(),
        "`logs -r {stage_run}`: {}",
        stderr(&stage_replay)
    );
    let stage_text = stdout(&stage_replay);
    let stage_header = stage_text
        .lines()
        .find(|line| line.starts_with(&format!("#{stage_run}")))
        .unwrap_or_else(|| panic!("`logs -r` must print a run header:\n{stage_text}"));
    let stage_fields: Vec<&str> = stage_header.split("  ").collect();
    assert!(
        stage_fields.len() >= 5
            && stage_fields[1] == "COPY"
            && stage_fields[2] == "app"
            && stage_fields[3].contains("--from=missing-stage")
            && stage_fields[3].contains("/x /y"),
        "the replay header names the instruction that ran:\n{stage_header:?}"
    );
    assert_eq!(
        stage_text
            .matches("┃ no build stage named `missing-stage`")
            .count(),
        1,
        "the engine's reason is written exactly once:\n{stage_text}"
    );
    for absent in ["build `app` failed", "Dockerfile:2:", "next:", "crun"] {
        assert!(
            !stage_text.contains(absent),
            "the capture never carries `{absent}`:\n{stage_text}"
        );
    }
    assert!(
        !stage_text.contains('\u{1b}'),
        "NO_COLOR keeps the replay escape-free:\n{stage_text}"
    );
    assert!(
        stage_text.trim_end().ends_with("✗ COPY app failed"),
        "the replay verdict names the instruction:\n{stage_text}"
    );

    let stage_json = journey.slipfork(&stagefail, &["logs", "-r", &stage_run, "--json"]);
    assert!(
        stage_json.status.success(),
        "`logs -r {stage_run} --json`: {}",
        stderr(&stage_json)
    );
    let stage_json_text = stdout(&stage_json);
    assert!(
        !stage_json_text.contains('\u{1b}'),
        "`--json` stays escape-free:\n{stage_json_text}"
    );
    let stage_events: Vec<serde_json::Value> = stage_json_text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let stage_record = stage_events
        .iter()
        .find(|event| event["run_id"].as_str() == Some(stage_run.as_str()))
        .unwrap_or_else(|| panic!("`logs -r --json` must emit the record: {stage_events:?}"));
    assert_eq!(stage_record["phase"], "copy", "{stage_record}");
    assert!(
        stage_record["instruction"]
            .as_str()
            .is_some_and(|text| text.contains("--from=missing-stage")),
        "{stage_record}"
    );
    assert_eq!(stage_record["source"], "Dockerfile:2", "{stage_record}");
    assert!(
        stage_record["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("no build stage named `missing-stage`")),
        "{stage_record}"
    );
    assert_eq!(stage_record["state"], "failed", "{stage_record}");
    assert!(
        stage_record
            .get("exit_code")
            .is_none_or(serde_json::Value::is_null),
        "an engine step carries no child exit code: {stage_record}"
    );

    // The same story for `FROM`: the pull is the step, and its failure is a `┃` line.
    let fromfail = journey.project("fromfail", CATALOG_FORKFILE);
    fs::write(fromfail.join("Dockerfile"), UNPULLABLE_FROM_DOCKERFILE)
        .expect("write the unpullable FROM Dockerfile");
    let from_build = journey.slipfork(&fromfail, &["up"]);
    assert_eq!(
        from_build.status.code(),
        Some(1),
        "{}",
        up_failure(
            "an unpullable `FROM` must fail `up`",
            &fromfail,
            &from_build
        )
    );
    let from_block = format!("{}{}", stdout(&from_build), stderr(&from_build));
    assert_eq!(
        from_block.matches("next:").count(),
        1,
        "a failed `FROM` prints exactly one `next:`:\n{from_block}"
    );
    assert!(
        from_block.contains("✗ FROM app failed"),
        "`up` names the failing instruction in its verdict:\n{from_block}"
    );
    assert!(
        from_block.lines().any(|line| line.contains('┃')),
        "`up` streams the failed `FROM` step's `┃` reason line:\n{from_block}"
    );
    let from_run = from_block
        .lines()
        .find_map(|line| line.trim().strip_prefix("next: slipfork logs -r "))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| panic!("the `FROM` failure must name its run:\n{from_block}"))
        .to_owned();
    let from_replay = journey.slipfork(&fromfail, &["logs", "-r", &from_run]);
    assert!(
        from_replay.status.success(),
        "`logs -r {from_run}`: {}",
        stderr(&from_replay)
    );
    let from_text = stdout(&from_replay);
    let from_header = from_text
        .lines()
        .find(|line| line.starts_with(&format!("#{from_run}")))
        .unwrap_or_else(|| panic!("`logs -r` must print a run header:\n{from_text}"));
    let from_fields: Vec<&str> = from_header.split("  ").collect();
    assert!(
        from_fields.len() >= 5
            && from_fields[1] == "FROM"
            && from_fields[2] == "app"
            && from_fields[3] == "127.0.0.1:1/nope:latest",
        "the replay header names the `FROM` that ran:\n{from_header:?}"
    );
    let from_reason = from_text
        .lines()
        .find_map(|line| line.split_once('┃').map(|(_, reason)| reason.trim()))
        .unwrap_or_default();
    assert!(
        from_reason.contains("127.0.0.1:1") || from_reason.to_ascii_lowercase().contains("refused"),
        "the `┃` reason names the unpullable host, saw {from_reason:?}:\n{from_text}"
    );
    assert!(
        !from_text.contains('\u{1b}'),
        "NO_COLOR keeps the replay escape-free:\n{from_text}"
    );
    assert!(
        from_text.trim_end().ends_with("✗ FROM app failed"),
        "the replay verdict names the `FROM` instruction:\n{from_text}"
    );

    // An invalid `from:` reference is a failure OF the `FROM` step: the row is announced before
    // the reference is parsed, its raw cause is the step's one `┃` line, and both surfaces read
    // `✗ FROM app failed` — never a step-less `✗ image … failed: <reason>`, never a `PULL` verb.
    let badfrom = journey.project("badfrom", INVALID_FROM_FORKFILE);
    let bad_build = journey.slipfork(&badfrom, &["up"]);
    assert_eq!(
        bad_build.status.code(),
        Some(1),
        "{}",
        up_failure(
            "an invalid `from:` reference must fail `up`",
            &badfrom,
            &bad_build
        )
    );
    let bad_block = format!("{}{}", stdout(&bad_build), stderr(&bad_build));
    assert_eq!(
        bad_block.matches("next:").count(),
        1,
        "an invalid `from:` prints exactly one `next:`:\n{bad_block}"
    );
    assert!(
        bad_block.contains("→ [1/1] FROM"),
        "`up` announces the `FROM` row before parsing the reference:\n{bad_block}"
    );
    assert!(
        bad_block
            .lines()
            .any(|line| line.contains('┃') && line.contains("not a valid image reference")),
        "`up` streams the step's one `┃` reason line:\n{bad_block}"
    );
    assert!(
        bad_block.contains("✗ FROM app failed") && !bad_block.contains("✗ image app failed"),
        "`up`'s verdict names the `FROM` step:\n{bad_block}"
    );
    assert!(
        !bad_block.contains("PULL"),
        "`from:` pulls never print a `PULL` verb:\n{bad_block}"
    );
    let bad_run = bad_block
        .lines()
        .find_map(|line| line.trim().strip_prefix("next: slipfork logs -r "))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| panic!("the invalid `from:` must name its run:\n{bad_block}"))
        .to_owned();
    let bad_replay = journey.slipfork(&badfrom, &["logs", "-r", &bad_run]);
    assert!(
        bad_replay.status.success(),
        "`logs -r {bad_run}`: {}",
        stderr(&bad_replay)
    );
    let bad_text = stdout(&bad_replay);
    let bad_header = bad_text
        .lines()
        .find(|line| line.starts_with(&format!("#{bad_run}")))
        .unwrap_or_else(|| panic!("`logs -r` must print a run header:\n{bad_text}"));
    let bad_fields: Vec<&str> = bad_header.split("  ").collect();
    assert!(
        bad_fields.len() >= 5
            && bad_fields[1] == "FROM"
            && bad_fields[2] == "app"
            && bad_fields[3] == "/bad:latest",
        "the header names the `FROM` that ran:\n{bad_header:?}"
    );
    assert_eq!(
        bad_text
            .lines()
            .filter(|line| line.contains('┃') && line.contains("not a valid image reference"))
            .count(),
        1,
        "the step's reason is written exactly once:\n{bad_text}"
    );
    assert!(
        bad_text.trim_end().ends_with("✗ FROM app failed"),
        "the replay verdict names the `FROM` step, with no `: reason` suffix:\n{bad_text}"
    );
    assert!(
        !bad_text.contains("PULL") && !bad_text.contains('\u{1b}'),
        "one `FROM` verb, zero escapes:\n{bad_text}"
    );

    // Before any step exists (a parse error), the verdict itself carries the reason.
    let parsefail = journey.project("parsefail", CATALOG_FORKFILE);
    fs::write(parsefail.join("Dockerfile"), PARSE_FAIL_DOCKERFILE)
        .expect("write the parse-failing Dockerfile");
    let parse_build = journey.slipfork(&parsefail, &["up"]);
    assert_eq!(
        parse_build.status.code(),
        Some(1),
        "{}",
        up_failure("a parse error must fail `up`", &parsefail, &parse_build)
    );
    let parse_block = format!("{}{}", stdout(&parse_build), stderr(&parse_build));
    assert_eq!(
        parse_block.matches("next:").count(),
        1,
        "a step-less failure prints exactly one `next:`:\n{parse_block}"
    );
    assert!(
        parse_block.contains("✗ image app failed: ")
            && parse_block.contains("instruction appears before any `FROM`"),
        "`up` carries the reason in the verdict when no step exists:\n{parse_block}"
    );
    let parse_run = parse_block
        .lines()
        .find_map(|line| line.trim().strip_prefix("next: slipfork logs -r "))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| panic!("the parse failure must name its run:\n{parse_block}"))
        .to_owned();
    let parse_replay = journey.slipfork(&parsefail, &["logs", "-r", &parse_run]);
    assert!(
        parse_replay.status.success(),
        "`logs -r {parse_run}`: {}",
        stderr(&parse_replay)
    );
    let parse_text = stdout(&parse_replay);
    assert!(
        parse_text
            .trim_end()
            .ends_with("✗ image app failed: Dockerfile:1: instruction appears before any `FROM`"),
        "`logs -r` carries the step-less reason in its verdict:\n{parse_text}"
    );
    assert!(
        !parse_text.contains('\u{1b}'),
        "NO_COLOR keeps the replay escape-free:\n{parse_text}"
    );

    // 4. Down and re-up: the stamped `uv sync` is skipped and named as such.
    let down = journey.slipfork(&good, &["down"]);
    assert!(down.status.success(), "down: {}", stderr(&down));
    assert!(
        stdout(&down).contains("→ [1/1] stopping good"),
        "down must stream its phase header:\n{}",
        stdout(&down)
    );

    // 4a. A stopped world still replays its whole history from `<store>/logs/<world>/*.log`:
    // bare, one target, an all-target merge, and a relative window.
    for args in [
        &["logs"][..],
        &["logs", "api"][..],
        &["logs", "--since", "5m"][..],
        &["logs", "-a"][..],
    ] {
        let replay = journey.slipfork(&good, args);
        assert!(
            replay.status.success(),
            "`{args:?}` after down must replay history: {}",
            stderr(&replay)
        );
        let text = stdout(&replay);
        assert!(
            text.contains("api"),
            "`{args:?}` lost the api history:\n{text}"
        );
        assert!(!text.contains('\u{1b}'), "`{args:?}` stays plain:\n{text}");
    }

    // 4b. A follower started while the world is down keeps the canonical captures open and picks
    // up the next boot's lines once `up` runs again.
    let mut follow = journey.pty(&good, &["logs", "-f", "api"], 120, 40);
    follow.expect("api ready", Duration::from_secs(10));
    let before_up = follow.seen.matches("api ready").count();
    let second = journey.slipfork(&good, &["up"]);
    assert!(
        follow.seen.matches("api ready").count() >= before_up,
        "the follower keeps its history while `up` runs"
    );
    let resumed = wait_until(
        || {
            follow.drain(Duration::from_millis(250));
            follow.seen.matches("api ready").count() > before_up
        },
        Duration::from_secs(30),
    );
    assert!(
        resumed,
        "`logs -f` must pick up new lines after `up`:\n{}",
        follow.seen
    );
    follow.send("\u{3}");
    let (follow_code, follow_seen) = follow.finish();
    assert_eq!(
        follow_code, 0,
        "Ctrl-C on a followed log exits 0:\n{follow_seen}"
    );

    let second_out = stdout(&second);
    assert!(
        second.status.success(),
        "{}",
        up_failure("second `up` failed", &good, &second)
    );
    let stamp_line = second_out
        .lines()
        .find(|line| line.contains("uv sync"))
        .unwrap_or_else(|| panic!("no stamped setup line:\n{second_out}"));
    assert!(
        stamp_line.contains("up to date"),
        "stamped step must report its skip: {stamp_line}"
    );

    // 5. Restart one service, then prove teardown leaves nothing behind.
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

    // 5a. The typed `--json` boot stream: every node frame carries its kind, bare subject, a
    // contract state, and its own duration; a hook's live output rides the stream as a `log` line.
    let json_boot = journey.slipfork(&good, &["up", "--json"]);
    assert!(
        json_boot.status.success(),
        "{}",
        up_failure("`up --json` failed", &good, &json_boot)
    );
    let boot_events: Vec<serde_json::Value> = stdout(&json_boot)
        .lines()
        .map(|line| serde_json::from_str(line).expect("each boot line is JSON"))
        .collect();
    for event in &boot_events {
        let Some(kind) = event["kind"].as_str() else {
            continue;
        };
        assert!(
            ["image", "resource", "setup", "hook", "service"].contains(&kind),
            "off-contract boot kind: {event}"
        );
        let name = event["name"]
            .as_str()
            .unwrap_or_else(|| panic!("node frame without a name: {event}"));
        assert!(
            !name.is_empty() && !name.starts_with("Hook ") && !name.starts_with("Setup "),
            "the name column must be the bare subject, never the label: {event}"
        );
        let state = event["state"]
            .as_str()
            .unwrap_or_else(|| panic!("node frame without a state: {event}"));
        assert!(
            [
                "running",
                "cached",
                "pulled",
                "built",
                "ready",
                "up to date",
                "failed"
            ]
            .contains(&state),
            "off-contract boot state: {event}"
        );
        if event["event"].as_str() != Some("started") {
            assert!(
                event["duration_ms"].is_u64(),
                "a finished node must carry its own duration_ms: {event}"
            );
        }
    }
    assert!(
        boot_events.iter().any(|event| {
            event["kind"] == "hook"
                && event["name"]
                    .as_str()
                    .is_some_and(|name| name.ends_with(":pre-deploy"))
                && event["state"] == "up to date"
        }),
        "an unchanged warm `up --json` must reuse the `pre-deploy` stamp (M3.57):\n{}",
        stdout(&json_boot)
    );
    assert!(
        boot_events.iter().any(|event| {
            event["event"] == "log"
                && event["line"]
                    .as_str()
                    .is_some_and(|line| line.contains("post-deploy hook ran"))
        }),
        "the `--json` stream must carry an executing hook's live output line:\n{}",
        stdout(&json_boot)
    );
    let down = journey.slipfork(&good, &["down"]);
    assert!(down.status.success(), "json-boot down: {}", stderr(&down));
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
    for id in ["api", "worker", "probe", "broken"] {
        assert!(
            !listed.contains(id),
            "container `{id}` survived teardown:\n{listed}"
        );
    }
    assert!(!good.join(".slipfork/world.json").exists());
    // The whole up/restart/down lifecycle leaves the checkout inode-identical, never a
    // subvolume, and never adopted into a sibling.
    let project_final = fs::metadata(&good).expect("stat the project after teardown");
    assert_eq!(
        (project_final.ino(), project_final.dev()),
        (project_before.ino(), project_before.dev()),
        "`up`/`restart`/`down` must never replace the project directory"
    );
    assert!(
        !is_subvolume_path(&good).unwrap_or(true),
        "teardown must never convert the checkout into a subvolume"
    );
    assert!(
        !fs::read_dir(&parent)
            .expect("read the scratch root after teardown")
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .contains(".slipfork-adopt")),
        "no `.slipfork-adopt` sibling may survive teardown"
    );
    assert!(!broken.join(".slipfork/world.json").exists());

    // 6. No per-project `.slipfork-store` exists, and no run subvolume survives in the mount store.
    for project in [&good, &broken] {
        assert!(
            !project.join(STORE_DIR).exists(),
            "store must live on the mount, never inside `{}`",
            project.display()
        );
    }
    for world_id in ["good", "broken"] {
        let store = journey.store_root.join("worlds").join(world_id);
        assert!(
            store.is_dir(),
            "world `{world_id}` must exist under `{}`",
            journey.store_root.display()
        );
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

const BAKED_DOCKERFILE: &str = "\
FROM python:3.13-slim
RUN pip install --no-cache-dir uv
WORKDIR /project
CMD [\"sh\", \"-c\", \"printf entrypoint > entrypoint.txt; exec python3 -m http.server 8010\"]
";

const BAKED_FORKFILE: &str = r#"# forkfile: 0.1
image app . {
  dockerfile Dockerfile
  service web {
    port 8010
    route /
  }
}
"#;

// Real SigV4 PUT + auth/anon GET roundtrips on `uploads` (private) and `assets` (cdn public-read).
const S3_PROBE: &str = r#"import datetime, hashlib, hmac, os, pathlib, time, urllib.error, urllib.request

def sign(key, msg):
    return hmac.new(key, msg.encode(), hashlib.sha256).digest()

def s3(method, path, body=b"", auth=True, tamper=False):
    req = urllib.request.Request(
        "http://127.0.0.1:8333" + path,
        data=body if method == "PUT" else None,
        method=method,
    )
    if auth:
        now = datetime.datetime.now(datetime.timezone.utc)
        amz = now.strftime("%Y%m%dT%H%M%SZ")
        date = now.strftime("%Y%m%d")
        payload = hashlib.sha256(body if method == "PUT" else b"").hexdigest()
        host = "127.0.0.1:8333"
        headers = f"host:{host}\nx-amz-content-sha256:{payload}\nx-amz-date:{amz}\n"
        signed = "host;x-amz-content-sha256;x-amz-date"
        canon = f"{method}\n{path}\n\n{headers}\n{signed}\n{payload}"
        scope = f"{date}/us-east-1/s3/aws4_request"
        sts = f"AWS4-HMAC-SHA256\n{amz}\n{scope}\n" + hashlib.sha256(canon.encode()).hexdigest()
        secret = ("AWS4" + os.environ["S3_SECRET"]).encode()
        key = sign(sign(sign(sign(secret, date), "us-east-1"), "s3"), "aws4_request")
        sig = hmac.new(key, sts.encode(), hashlib.sha256).hexdigest()
        if tamper:
            sig = sig[:-1] + ("0" if sig[-1] != "0" else "1")
        access = os.environ["S3_ACCESS"]
        req.add_header("Host", host)
        req.add_header("x-amz-date", amz)
        req.add_header("x-amz-content-sha256", payload)
        req.add_header(
            "Authorization",
            f"AWS4-HMAC-SHA256 Credential={access}/{scope}, SignedHeaders={signed}, Signature={sig}",
        )
    try:
        with urllib.request.urlopen(req, timeout=10) as reply:
            return reply.status, reply.read().decode()
    except urllib.error.HTTPError as error:
        return error.code, ""

put_priv, _ = s3("PUT", "/uploads/x", b"priv", auth=True)
get_priv, body_priv = s3("GET", "/uploads/x", auth=True)
anon_priv, _ = s3("GET", "/uploads/x", auth=False)
anon_put, _ = s3("PUT", "/uploads/y", b"nope", auth=False)
anon_del, _ = s3("DELETE", "/uploads/x", auth=False)
tampered, _ = s3("GET", "/uploads/x", auth=True, tamper=True)
put_cdn, _ = s3("PUT", "/assets/x", b"cdn", auth=True)
anon_cdn, body_cdn = s3("GET", "/assets/x", auth=False)
pathlib.Path("s3.codes").write_text(
    f"{put_priv} {get_priv}:{body_priv} {anon_priv} {anon_put} {anon_del} {tampered} {put_cdn} {anon_cdn}:{body_cdn} {os.getenv('GREETING')}"
)
time.sleep(600)
"#;
