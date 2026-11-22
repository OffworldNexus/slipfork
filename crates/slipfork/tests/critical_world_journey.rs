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
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slipfork_world::{
    BtrfsEngine, Dirtiness, STORE_DIR, StoreLayout, WorldEngine, WorldName, is_subvolume_path,
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
  post-deploy: sh -c 'for i in $(seq 50); do grep -q svc order.txt && break; sleep 0.1; done; grep -q svc order.txt && printf post >> order.txt'
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

// A minimal service-only world: it proves log history and following without a backing database.
const LOGS_FORKFILE: &str = r#"# forkfile: 0.1
image app . {
  dockerfile Dockerfile
  service api {
    run: @uv run python app.py
  }
}
"#;

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

fn mount_store_for(path: &Path) -> PathBuf {
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
        .join(STORE_DIR)
}

/// The scratch root plus its projects: `Drop` always tears both worlds down and removes the root.
struct Journey {
    run: PathBuf,
    runtime: PathBuf,
    store_root: PathBuf,
    projects: RefCell<Vec<PathBuf>>,
}

impl Journey {
    fn new() -> Self {
        let run = live_run_root();
        let store_root = mount_store_for(&run);
        let engine = BtrfsEngine::new(StoreLayout::at(&store_root));
        for world in ["good", "baked", "broken", "logs"] {
            if let Ok(world) = WorldName::parse(world) {
                let _ = engine.drop_world(&world);
            }
        }
        Self {
            run,
            runtime: require_tmpfs_runtime_dir(),
            store_root,
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

    /// Runs one verb with a piped stdin, as a script would.
    fn slipfork_input(&self, project: &Path, args: &[&str], input: &str) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_slipfork"))
            .args(args)
            .current_dir(project)
            .env("NO_COLOR", "1")
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("XDG_STATE_HOME", self.run.join("state"))
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
        let mut bytes = text.as_bytes();
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
        for world in ["good", "baked", "broken", "logs"] {
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

/// True when some `Forkfile:<n>` names a real line number.
fn forkfile_line(text: &str) -> bool {
    text.match_indices("Forkfile:").any(|(index, marker)| {
        text[index + marker.len()..]
            .chars()
            .next()
            .is_some_and(|next| next.is_ascii_digit())
    })
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
        "GET /x HTTP/1.1\r\nHost: static-cdn.good.localhost\r\nConnection: close\r\n\r\n",
    );
    assert!(
        cdn_get.starts_with("HTTP/1.1 200") && cdn_get.ends_with("cdn"),
        "public-read CDN must serve anonymous GET 200 on static-cdn.<world>.localhost: {cdn_get}"
    );
    let cdn_put = ingress_exchange(
        &journey.runtime,
        "PUT /x HTTP/1.1\r\nHost: static-cdn.good.localhost\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope",
    );
    assert!(
        cdn_put.starts_with("HTTP/1.1 403"),
        "public-read CDN must refuse anonymous PUT with 403 on static-cdn.<world>.localhost: {cdn_put}"
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
        refused.contains("  next: slipfork shell api, worker, probe, pg, cache"),
        "the refusal must list every shellable target: {refused}"
    );
    assert!(!refused.contains('\u{1b}'), "NO_COLOR refusals stay plain");
    let unknown = journey.slipfork(&good, &["shell", "apii"]);
    assert_eq!(unknown.status.code(), Some(1));
    let refused = stderr(&unknown);
    assert!(
        refused.contains("✗ No shellable target 'apii'"),
        "{refused}"
    );
    assert!(refused.contains("  next: slipfork shell api"), "{refused}");

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
    assert!(
        forkfile_line(&failed_out) || forkfile_line(&failed_err),
        "the failure tail must name a numbered Forkfile line:\nstdout={failed_out}\nstderr={failed_err}"
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
    let listed_text = stdout(&listed);
    assert!(
        listed.status.success() && listed_text.contains("broken"),
        "the failed world must stay listed by status:\n{listed_text}"
    );
    assert!(
        listed_text
            .lines()
            .any(|line| line.contains("broken") && line.contains("failed")),
        "the failed service row must render its `failed` state:\n{listed_text}"
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
            let _ = follow.wait_for("api ready", Duration::from_millis(250));
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
            event["event"] == "log"
                && event["line"]
                    .as_str()
                    .is_some_and(|line| line.contains("pre-deploy hook ran"))
        }),
        "the `--json` stream must carry a hook's live output line:\n{}",
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
