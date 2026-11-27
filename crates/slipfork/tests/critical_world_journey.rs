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
use std::io::{self, BufRead, BufReader, Read, Write};
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
    BtrfsEngine, Dirtiness, LEASE_FILE, Lease, LogStore, STORE_DIR, StoreLayout, WorldEngine,
    WorldName, is_subvolume_path,
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

// A service whose own stderr marker must survive into `logs -r`, and whose exit code must name
// the verdict, without waiting out the 10s readiness window.
const CRASH_FORKFILE: &str = r#"# forkfile: 0.1
image app . {
  dockerfile Dockerfile
  service web {
    port 8000
    route /
    run: @uv run python crash.py
  }
}
"#;

const CRASH_PY: &str = r#"import sys
print("crash-marker", file=sys.stderr, flush=True)
raise SystemExit(1)
"#;

// The run-catalog journey: one image whose `RUN` fails before any pod can start. Exactly three
// instructions, so the replay is a real `1/4`, `2/4`, `3/4`, `4/4` build sequence. The `RUN` prints more
// than a pipe buffer before failing, so a `| head` reader provably closes a still-blocked writer.
const FAIL_BUILD_RUN: &str = "\
sh -c 'i=1; while [ $i -le 6000 ]; do echo \"bulk-$i-xxxxxxxxxxxxxxxxxxxxxxxxxxxx\"; i=$((i+1)); done; echo out-line; echo err-line >&2; exit 3'";

fn fail_build_dockerfile() -> String {
    format!("FROM python:3.13-slim\nCOPY . /app\nWORKDIR /app\nRUN {FAIL_BUILD_RUN}\n")
}

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
COPY --from=Missing-Stage /x /y
WORKDIR /project
";
// A `FROM` the engine cannot pull: the pull, not a child process, is the failing command.
const UNPULLABLE_FROM_DOCKERFILE: &str = "\
FROM 127.0.0.1:1/nope:latest
WORKDIR /project
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

// M3.115: every image build input (the `Dockerfile`, a copied context file) must change the head
// so `up`/`restart` rebuild instead of serving a stale image.
const CONVERGE_FORKFILE: &str = r#"# forkfile: 0.1
image app . {
  dockerfile Dockerfile
  service web {
    run: sleep 3600
  }
}
"#;

const CONVERGE_FORKFILE_TWO: &str = r#"# forkfile: 0.1
image app . {
  dockerfile Dockerfile
  service web {
    run: sleep 3600
  }
  service worker {
    run: sleep 3600
  }
}
"#;

const CONVERGE_DOCKERFILE_ONE: &str = "\
FROM python:3.13-slim
COPY seed.txt /seed.txt
RUN sh -c 'cat /seed.txt; echo built-one'
WORKDIR /project
CMD [\"python3\", \"--version\"]
";

const CONVERGE_DOCKERFILE_FAIL: &str = "\
FROM python:3.13-slim
COPY seed.txt /seed.txt
RUN sh -c 'cat /seed.txt; echo built-two; exit 9'
WORKDIR /project
CMD [\"python3\", \"--version\"]
";

const CONVERGE_DOCKERFILE_THREE: &str = "\
FROM python:3.13-slim
COPY seed.txt /seed.txt
RUN sh -c 'cat /seed.txt; echo built-three'
WORKDIR /project
CMD [\"python3\", \"--version\"]
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
            "crash",
            "converge",
            "devroot",
            "devcode",
            "devkeep",
            "devbaked",
            "devbakedroot",
            "legacy",
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
        // Every journey shares one image store, so the content salt must be unique per journey
        // *and* project: a process-wide salt let an earlier journey seal this exact image and
        // serve a later `up` as `cached`, hiding the cold-build contract the journey asserts.
        let journey = self.run.file_name().map_or_else(
            || std::process::id().to_string(),
            |token| token.to_string_lossy().into_owned(),
        );
        let dockerfile = DOCKERFILE.replace(
            "WORKDIR /project",
            &format!("RUN echo slipfork-build-{journey}-{name}\nWORKDIR /project"),
        );
        fs::write(dir.join("Dockerfile"), dockerfile).expect("write Dockerfile");
        fs::write(dir.join("Forkfile"), forkfile).expect("write Forkfile");
        fs::write(dir.join("pyproject.toml"), PYPROJECT).expect("write pyproject.toml");
        fs::write(dir.join("uv.lock"), UV_LOCK).expect("write uv.lock");
        fs::write(dir.join(".dockerignore"), ".venv/\n__pycache__/\n")
            .expect("write .dockerignore");
        fs::write(dir.join("app.py"), APP_PY).expect("write app.py");
        fs::write(dir.join("crash.py"), CRASH_PY).expect("write crash.py");
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

    /// Spawns one verb with a piped stdout, reads exactly one line, then closes the pipe and reaps
    /// the child: a `| head` reader proves a closed stdout exits quietly.
    ///
    /// `blocked` asserts the writer is still parked on a full pipe after that one line, which only
    /// a capture replay (thousands of output rows) can guarantee; a catalog listing is smaller than
    /// the pipe buffer and may already have exited cleanly.
    fn slipfork_head(&self, project: &Path, args: &[&str], blocked: bool) -> (Option<i32>, String) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_slipfork"))
            .args(args)
            .current_dir(project)
            .env("NO_COLOR", "1")
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("XDG_STATE_HOME", self.run.join("state"))
            .env("XDG_DATA_HOME", &self.data_home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the slipfork binary must be runnable");
        {
            let stdout = child.stdout.take().expect("piped stdout");
            let mut reader = BufReader::new(stdout);
            let mut first = String::new();
            reader
                .read_line(&mut first)
                .expect("read one record from the piped stream");
            assert!(
                !first.trim().is_empty(),
                "a `--json` stream must print its first record"
            );
            if blocked {
                assert!(
                    child.try_wait().expect("poll the piped child").is_none(),
                    "the writer must still be blocked on the full pipe after one line"
                );
            }
        }
        let output = child.wait_with_output().expect("reap the closed-pipe run");
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
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
            "crash",
            "converge",
            "devroot",
            "devcode",
            "devkeep",
            "devbaked",
            "devbakedroot",
            "legacy",
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

/// The one run id a pre-plan image failure's `next:` remedy names (`slipfork logs -r <id>`).
fn failed_build_run(block: &str) -> String {
    block
        .lines()
        .filter_map(|line| line.trim().strip_prefix("next: "))
        .find_map(|remedy| {
            remedy
                .rsplit_once("slipfork logs -r ")
                .map(|(_, id)| id.trim().trim_end_matches('`').to_owned())
        })
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| panic!("the failure must name its recorded run:\n{block}"))
}

/// The shipped pre-plan image failure copy: one `error: … failed:` envelope and one `next:`
/// remedy. The typed `✗ RUN … failed` verdict belongs to the run's replay.
fn assert_image_failure_envelope(block: &str, cause: &str) {
    assert!(
        block
            .lines()
            .any(|line| line.trim_start().starts_with("error: ")
                && line.contains("failed:")
                && line.contains(cause)),
        "`up` carries the image failure envelope naming `{cause}`:\n{block}"
    );
}

/// The newest image-build run id recorded for `project`, or a panic naming the failed listing.
fn latest_image_run(journey: &Journey, project: &Path) -> String {
    let runs = journey.slipfork(project, &["logs", "--runs", "--json"]);
    let text = stdout(&runs);
    assert!(
        runs.status.success(),
        "`logs --runs --json` failed: {}",
        stderr(&runs)
    );
    text.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|record| record["kind"] == "image" && record["target"] == "app")
        .max_by_key(|record| record["started_ms"].as_u64())
        .and_then(|record| record["run_id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("no image build run recorded:\n{text}"))
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
        first_out.lines().any(|line| {
            line.strip_prefix("→ [")
                .is_some_and(|rest| rest.trim_start().starts_with("1/") && rest.contains("] image"))
        }),
        "progress lines must carry a real `→ [i/n]` prefix (the index pads to the plan width):\n{first_out}"
    );
    assert!(
        first_out
            .lines()
            .any(|line| line.starts_with("  ✓ 1/") && line.contains("from") && !line.contains('[')),
        "inner Dockerfile steps render indented `n/N` without brackets:\n{first_out}"
    );
    assert!(
        first_out.lines().any(|line| {
            line.starts_with("✓ [")
                && line.contains("image")
                && line.contains("app")
                && line.contains("built ")
        }),
        "settled image builds render the outer `✓ [i/n] image app built N steps` row:\n{first_out}"
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

    // 1d. Image build output replays from the run catalog: the daemon seals every executed
    // build as one run capture (`<store>/logs/<world>/runs/<run_id>.log`) and `logs <image>`
    // resolves that newest record, so nothing is written to a pod-shaped `<image>.log` path.
    let image_run = latest_image_run(&journey, &good);
    let image_log = journey
        .store_root
        .join("logs")
        .join("good")
        .join("runs")
        .join(format!("{image_run}.log"));
    assert!(
        wait_until(|| image_log.is_file(), Duration::from_secs(20)),
        "the image builder must record its run capture `{}`",
        image_log.display()
    );
    let image_logs = journey.slipfork(&good, &["logs", "app"]);
    assert!(
        image_logs.status.success(),
        "`logs app` must read the run capture: {}",
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
    // A non-image run keeps its own command in the header: `#<id>  service  broken  <command>  <when>`.
    let replay_text = stdout(&replay);
    let header = replay_text
        .lines()
        .find(|line| line.starts_with(&format!("#{run_id}")))
        .unwrap_or_else(|| {
            panic!("`logs -r {run_id}` must print a `#<id>` header:\n{replay_text}")
        });
    let fields: Vec<&str> = header.split("  ").collect();
    assert!(
        fields.len() == 5
            && fields[0] == format!("#{run_id}")
            && fields[1] == "service"
            && fields[2] == "broken"
            && fields[3] == "@uv run python -c \"raise SystemExit(7)\""
            && fields[4].contains('-')
            && fields[4].contains(':'),
        "the service replay header must be `#<id>  service  broken  <command>  <started>`:\n{header:?}"
    );
    let by_target = journey.slipfork(&broken, &["logs", "broken"]);
    assert!(
        by_target.status.success(),
        "`logs broken` must read the failed service capture: {}",
        stderr(&by_target)
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
            let Some(state) = row.find(" failed ") else {
                return false;
            };
            let Some(command) = row.find("@uv run python -c \"raise SystemExit(7)\"") else {
                return false;
            };
            row.contains("broken") && state < command
        }),
        "the failed service row must carry `broken`, its own `failed` state, then the run command:\n{listed_text}"
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
        stdout(&down).lines().any(|line| line.starts_with('✓')
            && line.contains("pod")
            && line.contains("broken")
            && line.contains("stopped"))
            && stdout(&down).contains("✓ Stopped broken ("),
        "down must settle the pod it stopped, then its verdict:\n{}",
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

    // 3a2. A service that dies while starting must fail fast (never poll the full 10s), record the
    // child exit on the run, and tee the service's own stderr into `logs -r` with its gutter.
    let crash = journey.project("crash", CRASH_FORKFILE);
    let crash_started = Instant::now();
    let crashed = journey.slipfork(&crash, &["up"]);
    let crash_elapsed = crash_started.elapsed();
    assert_eq!(
        crashed.status.code(),
        Some(1),
        "{}",
        up_failure("a crashing service must fail `up`", &crash, &crashed)
    );
    assert!(
        crash_elapsed < Duration::from_secs(10),
        "a crash during start must fail fast, not wait out the 10s readiness window (took {crash_elapsed:?}):\n{}",
        up_failure("the crashing service was slow to fail", &crash, &crashed)
    );
    let crash_err = stderr(&crashed);
    assert!(
        !crash_err.contains("never opened :8000 within 10s"),
        "a crash is detected as an exit, never a readiness timeout:\n{crash_err}"
    );
    let crash_lines: Vec<&str> = crash_err
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(
        crash_lines.len(),
        2,
        "a crash prints one verdict and one remedy on stderr:\nstdout={}\nstderr={crash_err}",
        stdout(&crashed)
    );
    assert_eq!(
        crash_lines[0], "✗ start web failed (exit 1)",
        "the verdict carries the service's own exit code:\nstderr={crash_err}"
    );
    assert_eq!(
        crash_err.matches("next:").count(),
        1,
        "a crash prints exactly one remedy:\nstderr={crash_err}"
    );
    let crash_run = crash_lines[1]
        .strip_prefix("next: slipfork logs -r ")
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| panic!("the crash must name one `-r` run remedy:\nstderr={crash_err}"))
        .to_owned();
    let crash_replay = journey.slipfork(&crash, &["logs", "-r", &crash_run]);
    assert!(
        crash_replay.status.success(),
        "`logs -r {crash_run}`: {}",
        stderr(&crash_replay)
    );
    let crash_text = stdout(&crash_replay);
    assert!(
        crash_text.contains("crash-marker"),
        "the service's own stderr must reach the run capture:\n{crash_text}"
    );
    // The replay glyph follows the recorded wire stream: `│` for stdout/combined (a merged pod
    // capture) and `┃` for a real stderr. Ask `--json` which stream the tee recorded, then prove
    // the human row carries exactly that one gutter.
    let crash_json = journey.slipfork(&crash, &["logs", "-r", &crash_run, "--json"]);
    assert!(
        crash_json.status.success(),
        "`logs -r {crash_run} --json`: {}",
        stderr(&crash_json)
    );
    let marker_stream = stdout(&crash_json)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("log JSON"))
        .find(|frame| frame["line"] == "crash-marker")
        .and_then(|frame| frame["stream"].as_str().map(str::to_owned))
        .unwrap_or_else(|| {
            panic!(
                "`logs -r {crash_run} --json` must carry the crash-marker stream:\n{}",
                stdout(&crash_json)
            )
        });
    let glyph = match marker_stream.as_str() {
        "stderr" => '┃',
        "stdout" | "combined" => '│',
        other => panic!("off-contract wire stream {other:?}"),
    };
    let marker_row = crash_text
        .lines()
        .find(|line| line.trim_end().ends_with("crash-marker"))
        .unwrap_or_else(|| panic!("the crash-marker row is missing:\n{crash_text}"));
    assert!(
        marker_row.contains(glyph)
            && marker_row.matches('│').count() + marker_row.matches('┃').count() == 1,
        "the captured `{marker_stream}` line must carry exactly one `{glyph}` gutter:\n{crash_text}"
    );
    assert!(
        !crash_text.contains('\u{1b}'),
        "NO_COLOR keeps the crash replay escape-free:\n{crash_text}"
    );
    assert!(
        crash_text.contains("(exit 1)"),
        "the replay verdict carries the child's exit code:\n{crash_text}"
    );
    let crash_store = LogStore::at(
        journey.store_root.clone(),
        WorldName::parse("crash").expect("world"),
    );
    let crash_record = crash_store
        .records()
        .expect("read the crash catalog")
        .records
        .into_iter()
        .find(|record| record.run_id == crash_run)
        .unwrap_or_else(|| panic!("the crash must leave a catalog record for {crash_run}"));
    assert_eq!(
        crash_record.exit_code,
        Some(1),
        "the catalog records the child exit: {crash_record:?}"
    );
    assert!(
        crash_record
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("exited (exit 1)")),
        "the catalog reason names the exit: {crash_record:?}"
    );
    assert!(
        crash_record.duration_ms < 10_000,
        "fail-fast must beat the 10s readiness window: {crash_record:?}"
    );
    let crash_target = journey.slipfork(&crash, &["logs", "web"]);
    assert!(
        crash_target.status.success() && stdout(&crash_target).contains("crash-marker"),
        "`logs web` must still read the pod capture: {}",
        stderr(&crash_target)
    );
    let crash_down = journey.slipfork(&crash, &["down"]);
    assert!(
        crash_down.status.success(),
        "crash down: {}",
        stderr(&crash_down)
    );

    // 3b. A failing image `RUN` is a durable catalog run: `logs <image>` and `logs -r <id>` replay
    // the build output, `--runs` lists it, and a later successful boot plus `down` keeps all three.
    let buildfail = journey.project("buildfail", CATALOG_FORKFILE);
    fs::write(buildfail.join("Dockerfile"), fail_build_dockerfile())
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
    let build_run = failed_build_run(&build_failure);
    let image_log = journey
        .store_root
        .join("logs")
        .join("buildfail")
        .join("runs")
        .join(format!("{build_run}.log"));
    assert!(
        image_log.is_file(),
        "the failed build must leave its run capture `{}`",
        image_log.display()
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
    // Every instruction is exactly one clocked replay row, in build order, before the verdict.
    let step_needles = [
        "1/4 FROM python:3.13-slim".to_owned(),
        "2/4 COPY . /app".to_owned(),
        "3/4 WORKDIR /app".to_owned(),
        format!("4/4 RUN {FAIL_BUILD_RUN}"),
    ];
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
            fields.len() == 4
                && fields[0] == format!("#{build_run}")
                && fields[1] == "image"
                && fields[2] == "app",
            "`{label}` header must name the run identity only (`#<id>  image  app  <started>`):\n{header:?}"
        );
        assert!(
            !header.contains("RUN") && !header.contains("sh -c"),
            "`{label}` header must not repeat an instruction:\n{header:?}"
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
        // Every instruction is exactly one clocked row, in build order, ahead of the verdict.
        let verdict_at = output
            .find("✗ RUN app failed (exit 3)")
            .unwrap_or_else(|| panic!("`{label}` must print the final verdict:\n{output}"));
        let mut last_seen = 0;
        for needle in &step_needles {
            let rows: Vec<&str> = output
                .lines()
                .filter(|line| line.trim_end().ends_with(needle.as_str()))
                .collect();
            assert_eq!(
                rows.len(),
                1,
                "`{label}` must replay `{needle}` exactly once:\n{output}"
            );
            let row = rows[0];
            let at = output
                .find(needle.as_str())
                .unwrap_or_else(|| panic!("`{label}` must replay `{needle}`:\n{output}"));
            assert!(
                at >= last_seen && at < verdict_at,
                "`{label}` replays instructions in build order before the verdict:\n{output}"
            );
            last_seen = at;
            let (clock, _) = row.trim_start().split_once("  ").unwrap_or_default();
            assert!(
                is_clock(clock),
                "`{label}` clocks the `{needle}` step row, saw {row:?}:\n{output}"
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
        assert_eq!(
            output
                .lines()
                .filter(|line| line.trim_end() == "✗ RUN app failed (exit 3)")
                .count(),
            1,
            "`{label}` prints exactly one final verdict line:\n{output}"
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
    let mut last_step = None;
    for needle in &step_needles {
        let rows: Vec<usize> = replay_events
            .iter()
            .enumerate()
            .filter(|(_, event)| event["line"].as_str() == Some(needle.as_str()))
            .map(|(position, _)| position)
            .collect();
        assert_eq!(
            rows.len(),
            1,
            "`logs -r --json` must carry `{needle}` exactly once: {replay_events:?}"
        );
        let position = rows[0];
        assert!(
            last_step.is_none_or(|last| position > last),
            "`logs -r --json` orders the instruction rows in build order: {replay_events:?}"
        );
        last_step = Some(position);
        let event = &replay_events[position];
        assert_eq!(
            event["kind"], "step",
            "`{needle}` must be a `step` row: {event}"
        );
        assert!(
            event["ts_ms"].is_u64(),
            "`{needle}` must carry a numeric timestamp: {event}"
        );
    }
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
    assert_eq!(replay_run["instruction"], FAIL_BUILD_RUN, "{replay_run}");
    assert_eq!(replay_run["source"], "Dockerfile:4", "{replay_run}");
    assert_eq!(replay_run["exit_code"], 3, "{replay_run}");

    // A reader that stops early (`| head`) closes stdout: both catalog streams must exit 0 with an
    // empty, ANSI-free stderr instead of printing `error: failed to write output: Broken pipe`.
    for (args, blocked) in [
        (vec!["logs", "-r", build_run.as_str(), "--json"], true),
        (vec!["logs", "--runs", "--json"], false),
    ] {
        let (code, head_stderr) = journey.slipfork_head(&buildfail, &args, blocked);
        assert_eq!(
            code,
            Some(0),
            "`{}` must exit 0 on a closed pipe: {head_stderr:?}",
            args.join(" ")
        );
        assert!(
            head_stderr.is_empty(),
            "`{}` must stay silent on a closed pipe: {head_stderr:?}",
            args.join(" ")
        );
    }

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
            && bare_text.contains(FAIL_BUILD_RUN)
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
    assert_eq!(record["instruction"], FAIL_BUILD_RUN, "{record}");
    assert_eq!(record["source"], "Dockerfile:4", "{record}");
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
    // missing stage, a `FROM` that cannot be pulled) gets one clocked step row, writes its raw
    // one-line cause as that step's `┃` output, and still ends with a verdict naming the verb. The
    // replay header is run identity only (`#<id>  image  <target>  <started>`). A failure before
    // any step puts the reason in the verdict instead of an empty block.
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
    assert_image_failure_envelope(&stage_block, "no build stage named `Missing-Stage`");
    assert!(
        stage_block
            .lines()
            .any(|line| line.contains("┃ no build stage named `Missing-Stage`")),
        "`up` streams the failing step's `┃` reason line:\n{stage_block}"
    );
    let stage_run = failed_build_run(&stage_block);
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
        stage_fields.len() == 4
            && stage_fields[0] == format!("#{stage_run}")
            && stage_fields[1] == "image"
            && stage_fields[2] == "app",
        "the replay header names the run identity only (`#<id>  image  app  <started>`):\n{stage_header:?}"
    );
    assert!(
        stage_fields[3].contains('-') && stage_fields[3].contains(':'),
        "the replay header must end with a date+time, saw {:?}:\n{stage_header:?}",
        stage_fields[3]
    );
    assert!(
        !stage_header.contains("COPY"),
        "the replay header must not repeat an instruction:\n{stage_header:?}"
    );
    assert_eq!(
        stage_header.matches("app").count(),
        1,
        "the replay must never print the image id twice:\n{stage_header:?}"
    );
    // Exactly one clocked step row names the failing instruction, ahead of the verdict.
    let stage_step = "2/3 COPY --from=Missing-Stage /x /y";
    assert_eq!(
        stage_text
            .lines()
            .filter(|line| line.trim_end().ends_with(stage_step))
            .count(),
        1,
        "exactly one step row must name `{stage_step}`:\n{stage_text}"
    );
    let stage_step_at = stage_text
        .rfind(stage_step)
        .unwrap_or_else(|| panic!("the failing step row is missing:\n{stage_text}"));
    let stage_verdict_at = stage_text
        .rfind("✗ COPY app failed")
        .unwrap_or_else(|| panic!("the replay verdict is missing:\n{stage_text}"));
    assert!(
        stage_step_at < stage_verdict_at,
        "the failing step row must precede the verdict:\n{stage_text}"
    );
    assert_eq!(
        stage_text
            .matches("┃ no build stage named `Missing-Stage`")
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
            .is_some_and(|text| text.contains("--from=Missing-Stage")),
        "{stage_record}"
    );
    assert_eq!(stage_record["source"], "Dockerfile:2", "{stage_record}");
    assert!(
        stage_record["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("no build stage named `Missing-Stage`")),
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
    assert_image_failure_envelope(&from_block, "127.0.0.1:1");
    assert!(
        from_block.lines().any(|line| line.contains('┃')),
        "`up` streams the failed `FROM` step's `┃` reason line:\n{from_block}"
    );
    let from_run = failed_build_run(&from_block);
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
        from_fields.len() == 4
            && from_fields[0] == format!("#{from_run}")
            && from_fields[1] == "image"
            && from_fields[2] == "app",
        "the replay header names the run identity only (`#<id>  image  app  <started>`):\n{from_header:?}"
    );
    assert!(
        from_fields[3].contains('-') && from_fields[3].contains(':'),
        "the replay header must end with a date+time, saw {:?}:\n{from_header:?}",
        from_fields[3]
    );
    assert!(
        !from_header.contains("FROM"),
        "the replay header must not repeat an instruction:\n{from_header:?}"
    );
    assert_eq!(
        from_header.matches("app").count(),
        1,
        "the replay must never print the image id twice:\n{from_header:?}"
    );
    // Exactly one clocked step row names the failing instruction, ahead of the verdict.
    let from_step = "FROM 127.0.0.1:1/nope:latest";
    assert_eq!(
        from_text
            .lines()
            .filter(|line| line.trim_end().ends_with(from_step))
            .count(),
        1,
        "exactly one step row must name `{from_step}`:\n{from_text}"
    );
    let from_step_at = from_text
        .rfind(from_step)
        .unwrap_or_else(|| panic!("the failing step row is missing:\n{from_text}"));
    let from_verdict_at = from_text
        .rfind("✗ FROM app failed")
        .unwrap_or_else(|| panic!("the replay verdict is missing:\n{from_text}"));
    assert!(
        from_step_at < from_verdict_at,
        "the failing step row must precede the verdict:\n{from_text}"
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
    let bad_doc = slipfork_forkfile::parse(INVALID_FROM_FORKFILE).expect("parse badfrom");
    let bad_total = slipfork_world::world_plan(&bad_doc, &badfrom)
        .expect("plan badfrom")
        .steps
        .len();
    assert!(
        bad_block.contains(&format!("→ [1/{bad_total}] image"))
            && bad_block.contains("  → 1/1 from")
            && !bad_block.contains("[1/1] from"),
        "`up` opens the outer `[1/{bad_total}] image` row then the indented `1/1 FROM` row before parsing:\n{bad_block}"
    );
    assert!(
        bad_block
            .lines()
            .any(|line| line.contains('┃') && line.contains("not a valid image reference")),
        "`up` streams the step's one `┃` reason line:\n{bad_block}"
    );
    assert_image_failure_envelope(&bad_block, "not a valid image reference");
    assert!(
        !bad_block.contains("PULL"),
        "`from:` pulls never print a `PULL` verb:\n{bad_block}"
    );
    let bad_run = failed_build_run(&bad_block);
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
        bad_fields.len() == 4
            && bad_fields[0] == format!("#{bad_run}")
            && bad_fields[1] == "image"
            && bad_fields[2] == "app",
        "the replay header names the run identity only (`#<id>  image  app  <started>`):\n{bad_header:?}"
    );
    assert!(
        bad_fields[3].contains('-') && bad_fields[3].contains(':'),
        "the replay header must end with a date+time, saw {:?}:\n{bad_header:?}",
        bad_fields[3]
    );
    assert!(
        !bad_header.contains("FROM"),
        "the replay header must not repeat an instruction:\n{bad_header:?}"
    );
    assert_eq!(
        bad_header.matches("app").count(),
        1,
        "the replay must never print the image id twice:\n{bad_header:?}"
    );
    // Exactly one clocked step row names the failing instruction, ahead of the verdict.
    let bad_step = "FROM /bad:latest";
    assert_eq!(
        bad_text
            .lines()
            .filter(|line| line.trim_end().ends_with(bad_step))
            .count(),
        1,
        "exactly one step row must name `{bad_step}`:\n{bad_text}"
    );
    let bad_step_at = bad_text
        .rfind(bad_step)
        .unwrap_or_else(|| panic!("the failing step row is missing:\n{bad_text}"));
    let bad_verdict_at = bad_text
        .rfind("✗ FROM app failed")
        .unwrap_or_else(|| panic!("the replay verdict is missing:\n{bad_text}"));
    assert!(
        bad_step_at < bad_verdict_at,
        "the failing step row must precede the verdict:\n{bad_text}"
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
    assert_image_failure_envelope(&parse_block, "instruction appears before any `FROM`");
    let parse_run = failed_build_run(&parse_block);
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
        stdout(&down)
            .lines()
            .any(|line| line.starts_with('✓') && line.contains("pod") && line.contains("stopped"))
            && stdout(&down).contains("✓ Stopped good ("),
        "down must settle the pods it stopped, then its verdict:\n{}",
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
        stdout(&restart).lines().any(|line| line.starts_with('✓')
            && line.contains("restarting")
            && line.contains("api")),
        "restart must settle its one recycling row:\n{}",
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
fn changed_build_inputs_rebuild_and_restart_converges() {
    for (label, source) in [
        ("converge", CONVERGE_FORKFILE),
        ("converge-two", CONVERGE_FORKFILE_TWO),
    ] {
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

    // The copied content is unique per process: a previous run's sealed step must never be
    // mistaken for this run's rebuild, even on a warm shared store.
    let salt = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after the epoch")
            .as_nanos()
    );
    let seed_one = format!("one-{salt}");
    let seed_two = format!("two-{salt}");
    let seed_three = format!("three-{salt}");

    let project = journey.project("converge", CONVERGE_FORKFILE);
    fs::write(project.join("seed.txt"), &seed_one).expect("write the copied context");
    fs::write(project.join("Dockerfile"), CONVERGE_DOCKERFILE_ONE).expect("write the Dockerfile");

    // 1. Cold boot builds the image.
    let first = journey.slipfork(&project, &["up"]);
    assert!(
        first.status.success(),
        "{}",
        up_failure("first `up` failed", &project, &first)
    );
    let first_run = latest_image_run(&journey, &project);

    // 2. An unchanged `up` is idle: no rebuild and no new image build record.
    let cached = journey.slipfork(&project, &["up"]);
    assert!(
        cached.status.success(),
        "{}",
        up_failure("unchanged `up` failed", &project, &cached)
    );
    assert!(
        !stdout(&cached).contains("built"),
        "an unchanged `up` must not rebuild:\n{}",
        stdout(&cached)
    );
    assert_eq!(
        latest_image_run(&journey, &project),
        first_run,
        "an unchanged `up` must not rebuild the image"
    );

    // 3. The owner's reproduction: editing the `RUN` to a failing command must actually run it.
    fs::write(project.join("Dockerfile"), CONVERGE_DOCKERFILE_FAIL).expect("edit the Dockerfile");
    let failing = journey.slipfork(&project, &["up"]);
    assert!(
        !failing.status.success(),
        "the edited failing RUN must fail the boot: {}",
        up_failure("edited `up` unexpectedly succeeded", &project, &failing)
    );
    let failed_output = format!(
        "{}{}{}",
        stdout(&failing),
        stderr(&failing),
        supervisor_tail(&project)
    );
    assert!(
        failed_output.contains("built-two"),
        "the edited `RUN` must execute instead of serving the cached image:\n{failed_output}"
    );
    assert!(
        failed_output.contains("exit 9"),
        "the edited command's exit code must surface:\n{failed_output}"
    );
    // A failed build must never detach the live world: the old image still serves immediately.
    let still_serving = journey.slipfork(&project, &["exec", "web", "--", "cat", "/seed.txt"]);
    assert!(still_serving.status.success(), "{}", stderr(&still_serving));
    assert_eq!(
        stdout(&still_serving).trim(),
        seed_one,
        "a failed `up` must leave the old world serving its last image"
    );
    let ps = journey.slipfork(&project, &["ps", "--json"]);
    assert!(ps.status.success(), "{}", stderr(&ps));
    let reply: serde_json::Value = serde_json::from_slice(&ps.stdout).expect("ps --json");
    let world = reply["worlds"]
        .as_array()
        .and_then(|worlds| worlds.iter().find(|world| world["world"] == "converge"))
        .unwrap_or_else(|| panic!("`ps --json` must list the live world: {reply}"));
    let web = world["services"]
        .as_array()
        .and_then(|services| services.iter().find(|service| service["id"] == "web"))
        .unwrap_or_else(|| panic!("`ps --json` must list the `web` service: {reply}"));
    assert_eq!(
        web["running"], true,
        "a failed `up` must leave the live pod running: {reply}"
    );

    // 4. A succeeding Dockerfile edit rebuilds with a new image run id, then stays cached.
    fs::write(project.join("Dockerfile"), CONVERGE_DOCKERFILE_THREE)
        .expect("repair the Dockerfile");
    let rebuilt = journey.slipfork(&project, &["up"]);
    assert!(
        rebuilt.status.success(),
        "{}",
        up_failure("the repaired Dockerfile must boot", &project, &rebuilt)
    );
    assert!(
        stdout(&rebuilt).contains("built-three"),
        "the repaired Dockerfile must run:\n{}",
        stdout(&rebuilt)
    );
    let dockerfile_run = latest_image_run(&journey, &project);
    assert_ne!(
        first_run, dockerfile_run,
        "a Dockerfile edit must produce a new image run id"
    );
    let cached_again = journey.slipfork(&project, &["up"]);
    assert!(cached_again.status.success());
    assert!(
        !stdout(&cached_again).contains("built"),
        "a warm unchanged `up` must not rebuild:\n{}",
        stdout(&cached_again)
    );
    assert_eq!(latest_image_run(&journey, &project), dockerfile_run);

    // 4b. `up --no-cache` ignores the sealed step cache: every instruction executes again with a
    // new image run id and real timings, then a plain `up` adopts the fresh stamps and is cached.
    let forced = journey.slipfork(&project, &["up", "--no-cache", "--json"]);
    assert!(
        forced.status.success(),
        "{}",
        up_failure("`up --no-cache` failed", &project, &forced)
    );
    let forced_text = stdout(&forced);
    let forced_frames: Vec<serde_json::Value> = forced_text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("each --no-cache line is JSON"))
        .collect();
    let verdict = forced_frames
        .last()
        .expect("`up --json` ends with one object");
    assert_eq!(
        verdict["no_cache"], true,
        "the final `up --json` object must carry the flag: {verdict}"
    );
    assert!(
        forced_frames.iter().any(|frame| {
            frame["event"] == "log"
                && frame["line"]
                    .as_str()
                    .is_some_and(|line| line.contains("built-three"))
        }),
        "`--no-cache` must re-execute the cached `RUN`:\n{forced_text}"
    );
    assert!(
        forced_frames.iter().any(|frame| {
            frame["event"] == "step"
                && frame["image"] == "app"
                && frame["instruction"] == "RUN"
                && frame["elapsed_ms"].is_u64()
        }),
        "`--no-cache` must stream real per-instruction rows with timings:\n{forced_text}"
    );
    let forced_run = latest_image_run(&journey, &project);
    assert_ne!(
        dockerfile_run, forced_run,
        "`--no-cache` must record a new image run id instead of reusing the cached one"
    );
    // A stopped world never takes the daemon's Heal path, so this plain `up` must consult the
    // image stamps the `--no-cache` boot sealed: adopting them is the cache really being warm.
    let stopped = journey.slipfork(&project, &["down"]);
    assert!(
        stopped.status.success(),
        "down after `--no-cache` failed: {}",
        stderr(&stopped)
    );
    let cached_after_force = journey.slipfork(&project, &["up"]);
    assert!(
        cached_after_force.status.success(),
        "{}",
        up_failure(
            "`up` after `--no-cache` failed",
            &project,
            &cached_after_force
        )
    );
    assert!(
        !stdout(&cached_after_force).contains("built"),
        "a cold boot must adopt the fresh stamps instead of rebuilding:\n{}",
        stdout(&cached_after_force)
    );
    assert_eq!(
        latest_image_run(&journey, &project),
        forced_run,
        "a plain `up` after `--no-cache` must be cached again"
    );

    // 5. A copied context edit changes the canonical context digest, rebuilds, and the new
    // content is what the image actually holds.
    fs::write(project.join("seed.txt"), &seed_two).expect("edit the copied context");
    let context_edit = journey.slipfork(&project, &["up"]);
    assert!(
        context_edit.status.success(),
        "{}",
        up_failure(
            "the context-edited build must boot",
            &project,
            &context_edit
        )
    );
    assert!(
        stdout(&context_edit).contains(&seed_two),
        "the edited COPY input must execute in the build:\n{}",
        stdout(&context_edit)
    );
    let context_run = latest_image_run(&journey, &project);
    assert_ne!(
        dockerfile_run, context_run,
        "a copied context edit must produce a new image run id"
    );
    let shown = journey.slipfork(&project, &["exec", "web", "--", "cat", "/seed.txt"]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    assert_eq!(
        stdout(&shown).trim(),
        seed_two,
        "the rebuilt image must hold the edited COPY content"
    );

    // 5b. A world whose lease was lost (a pre-digest boot or a removed lease file) must still
    // reconcile on a copied-file edit; the old mtime heuristic would serve the stale image.
    let lease_path = journey
        .store_root
        .join("worlds")
        .join("converge")
        .join(LEASE_FILE);
    let lease = Lease::read(&lease_path).expect("the live world writes its lease");
    assert!(
        lease.is_consistent(),
        "the live world's lease head must match its entries"
    );
    fs::remove_file(&lease_path).expect("drop the recorded head");
    fs::write(project.join("seed.txt"), &seed_three).expect("edit the copied context again");
    let lost_head = journey.slipfork(&project, &["up"]);
    assert!(
        lost_head.status.success(),
        "{}",
        up_failure("the lost-head context edit must boot", &project, &lost_head)
    );
    assert!(
        stdout(&lost_head).contains(&seed_three),
        "a lost recorded head must not hide a copied-file edit:\n{}",
        stdout(&lost_head)
    );
    assert_ne!(
        context_run,
        latest_image_run(&journey, &project),
        "a lost recorded head must still rebuild on a copied context edit"
    );
    let shown = journey.slipfork(&project, &["exec", "web", "--", "cat", "/seed.txt"]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    assert_eq!(
        stdout(&shown).trim(),
        seed_three,
        "the rebuilt image must hold the second edited COPY content"
    );

    // 6. A Forkfile edit (add a service) converges on `restart`, not a stale pod restart.
    fs::write(project.join("Forkfile"), CONVERGE_FORKFILE_TWO).expect("add the worker service");
    let restarted = journey.slipfork(&project, &["restart"]);
    assert!(
        restarted.status.success(),
        "{}",
        up_failure(
            "restart after the Forkfile edit failed",
            &project,
            &restarted
        )
    );
    assert!(
        stdout(&restarted).contains("worker"),
        "`restart` must boot the newly declared service:\n{}",
        stdout(&restarted)
    );
    let ps = journey.slipfork(&project, &["ps", "--json"]);
    assert!(ps.status.success(), "{}", stderr(&ps));
    let reply: serde_json::Value = serde_json::from_slice(&ps.stdout).expect("ps --json");
    let services = reply["worlds"][0]["services"]
        .as_array()
        .unwrap_or_else(|| panic!("ps --json carries services: {reply}"));
    let worker = services
        .iter()
        .find(|row| row["id"] == "worker")
        .unwrap_or_else(|| panic!("the added service must appear: {services:?}"));
    assert_eq!(worker["running"], true, "{worker}");
    let converged_run = latest_image_run(&journey, &project);

    // 7. An unchanged `restart` keeps today's in-place restart with no rebuild.
    let again = journey.slipfork(&project, &["restart"]);
    assert!(
        again.status.success(),
        "{}",
        up_failure("unchanged restart failed", &project, &again)
    );
    assert!(
        stdout(&again).contains("restarting"),
        "an unchanged `restart` must restart in place:\n{}",
        stdout(&again)
    );
    assert_eq!(
        latest_image_run(&journey, &project),
        converged_run,
        "an unchanged `restart` must not rebuild the image"
    );

    // 8. Teardown leaves no process or container behind.
    let down = journey.slipfork(&project, &["down"]);
    assert!(down.status.success(), "final down: {}", stderr(&down));
    let _ = wait_until(
        || scoped_leaks(&journey.run).is_empty(),
        Duration::from_secs(10),
    );
    assert!(
        scoped_leaks(&journey.run).is_empty(),
        "processes survived teardown: {:?}",
        scoped_leaks(&journey.run)
    );
    let listed = crun_list(&crun);
    for id in ["web", "worker"] {
        assert!(
            !listed.contains(id),
            "container `{id}` survived teardown:\n{listed}"
        );
    }
}

#[test]
fn dev_mounts_follow_the_dockerfile_workdir_and_refuse_root() {
    let journey = Journey::new();
    require_tool("crun");
    let unshare = require_tool("unshare");
    require_userns(&unshare);
    require_registry_egress();

    // (a) No `WORKDIR`: `/` can never be overlaid, so `up` refuses before any pod exists.
    let root = journey.project("devroot", DEV_FORKFILE);
    fs::write(root.join("Dockerfile"), DEV_ROOT_DOCKERFILE).expect("write the root Dockerfile");
    fs::write(root.join("hello.py"), DEV_HELLO).expect("write hello.py");
    let refused = journey.slipfork(&root, &["up"]);
    assert!(
        !refused.status.success(),
        "a `/` WORKDIR must refuse a dev boot:\n{}{}",
        stdout(&refused),
        stderr(&refused)
    );
    let refusal = format!("{}{}", stdout(&refused), stderr(&refused));
    let lines: Vec<&str> = refusal
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    assert!(
        lines.iter().any(|line| line.contains("dev image `app`")
            && line.contains("Dockerfile")
            && line.contains("WORKDIR")
            && line.contains("`stage 1`")),
        "the verdict names the image, Dockerfile, stage, and missing workdir:\n{refusal}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("next: set WORKDIR")),
        "the refusal must carry the WORKDIR remedy:\n{refusal}"
    );

    // (b) `WORKDIR /code`: the pod runs there over the live checkout, no restart needed.
    let code = journey.project("devcode", DEV_FORKFILE);
    fs::write(code.join("Dockerfile"), DEV_CODE_DOCKERFILE).expect("write the /code Dockerfile");
    fs::write(code.join("hello.py"), DEV_HELLO).expect("write hello.py");
    let up = journey.slipfork(&code, &["up"]);
    assert!(
        up.status.success(),
        "{}",
        up_failure("`WORKDIR /code` must boot", &code, &up)
    );
    let pwd = journey.slipfork(&code, &["exec", "web", "--", "pwd"]);
    assert_eq!(
        stdout(&pwd).trim(),
        "/code",
        "exec must run in the Dockerfile WORKDIR:\n{}",
        stderr(&pwd)
    );
    fs::write(code.join("hello.py"), "print('live edit')\n").expect("edit the host file");
    let live = journey.slipfork(&code, &["exec", "web", "--", "cat", "hello.py"]);
    assert_eq!(
        stdout(&live),
        "print('live edit')\n",
        "host edits under `/code` must be live without a restart:\n{}",
        stderr(&live)
    );

    // (c) An explicit `/project/api` WORKDIR keeps the `/project` bind: `/project/api` maps to
    // `<project>/api`, so host edits there are live.
    let kept = journey.project("devkeep", DEV_FORKFILE);
    fs::write(kept.join("Dockerfile"), DEV_PROJECT_DOCKERFILE)
        .expect("write the /project/api Dockerfile");
    fs::create_dir_all(kept.join("api")).expect("create the api directory");
    fs::write(kept.join("api/hello.py"), DEV_HELLO).expect("write api/hello.py");
    let up = journey.slipfork(&kept, &["up"]);
    assert!(
        up.status.success(),
        "{}",
        up_failure("`WORKDIR /project/api` must boot", &kept, &up)
    );
    let pwd = journey.slipfork(&kept, &["exec", "web", "--", "pwd"]);
    assert_eq!(stdout(&pwd).trim(), "/project/api");
    fs::write(kept.join("api/hello.py"), "print('kept edit')\n").expect("edit api/hello.py");
    let live = journey.slipfork(&kept, &["exec", "web", "--", "cat", "hello.py"]);
    assert_eq!(stdout(&live), "print('kept edit')\n");

    // (d) `-F -dev` is pure Docker: the image copy and its WORKDIR, never the host checkout.
    let baked = journey.project("devbaked", DEV_FORKFILE);
    fs::write(baked.join("Dockerfile"), DEV_CODE_DOCKERFILE).expect("write the /code Dockerfile");
    fs::write(baked.join("hello.py"), DEV_HELLO).expect("write hello.py");
    let up = journey.slipfork(&baked, &["up", "-F", "-dev"]);
    assert!(
        up.status.success(),
        "{}",
        up_failure("`-F -dev` must boot", &baked, &up)
    );
    fs::write(baked.join("hello.py"), "print('host only')\n").expect("edit the host file");
    let baked_read = journey.slipfork(&baked, &["exec", "web", "--", "cat", "hello.py"]);
    assert_eq!(
        stdout(&baked_read),
        DEV_HELLO,
        "`-F -dev` must keep the baked copy:\n{}",
        stderr(&baked_read)
    );
    let pwd = journey.slipfork(&baked, &["exec", "web", "--", "pwd"]);
    assert_eq!(stdout(&pwd).trim(), "/code");

    // (e) A no-WORKDIR image is refused only for `dev`: `-F -dev` still boots its baked copy.
    let baked_root = journey.project("devbakedroot", DEV_FORKFILE);
    fs::write(baked_root.join("Dockerfile"), DEV_ROOT_DOCKERFILE)
        .expect("write the root Dockerfile");
    fs::write(baked_root.join("hello.py"), DEV_HELLO).expect("write hello.py");
    let up = journey.slipfork(&baked_root, &["up", "-F", "-dev"]);
    assert!(
        up.status.success(),
        "{}",
        up_failure("`-F -dev` must boot a no-WORKDIR image", &baked_root, &up)
    );
    let pwd = journey.slipfork(&baked_root, &["exec", "web", "--", "pwd"]);
    assert_eq!(stdout(&pwd).trim(), "/");
    let baked_read = journey.slipfork(&baked_root, &["exec", "web", "--", "cat", "/hello.py"]);
    assert_eq!(
        stdout(&baked_read),
        DEV_HELLO,
        "the image copy must be visible at `/`:\n{}",
        stderr(&baked_read)
    );
    fs::write(baked_root.join("hello.py"), "print('host only')\n").expect("edit the host file");
    let still_baked = journey.slipfork(&baked_root, &["exec", "web", "--", "cat", "/hello.py"]);
    assert_eq!(
        stdout(&still_baked),
        DEV_HELLO,
        "`-F -dev` must never see the host edit"
    );

    for project in [&code, &kept, &baked, &baked_root] {
        let down = journey.slipfork(project, &["down"]);
        assert!(down.status.success(), "final down: {}", stderr(&down));
    }
}

/// A stale `.slipfork/world.json` never wins identity over the ledger row keyed by the checkout.
#[test]
fn stale_world_record_loses_to_the_ledger_identity() {
    let journey = Journey::new();
    require_tool("crun");
    let unshare = require_tool("unshare");
    require_userns(&unshare);
    require_registry_egress();

    let legacy = journey.project("legacy", CATALOG_FORKFILE);
    let record = legacy.join(".slipfork/world.json");
    let stale = "{\"world\":\"feat-other--legacy\",\"socket\":\"/run/stale.sock\",\"project\":\"/srv/legacy\",\"pid\":1}";
    fs::create_dir_all(legacy.join(".slipfork")).expect("create .slipfork");
    fs::write(&record, stale).expect("write the stale world record");

    // 1. Fresh registration: the path-derived world wins; the record is dropped silently.
    let first = journey.slipfork(&legacy, &["up"]);
    assert!(
        first.status.success(),
        "{}",
        up_failure(
            "a stale record must not block a fresh `up`",
            &legacy,
            &first
        )
    );
    let started = format!("{}{}", stdout(&first), stderr(&first));
    assert!(
        started.contains("✓ Ready -> http://legacy.localhost"),
        "the checkout's own world must boot:\n{started}"
    );
    assert!(
        !started.contains("world.json") && !started.contains("feat-other"),
        "a fresh registration must settle the record without scaring the user:\n{started}"
    );
    assert!(!record.exists(), "the stale record must be removed");
    let first_run = latest_image_run(&journey, &legacy);
    assert!(
        journey.run.join("state/slipfork/state.db").is_file(),
        "the daemon must own a real ledger row on disk"
    );

    let ps = journey.slipfork(&legacy, &["ps", "--json"]);
    assert!(ps.status.success(), "ps: {}", stderr(&ps));
    let reply: serde_json::Value = serde_json::from_slice(&ps.stdout).expect("ps --json");
    let world = reply["worlds"]
        .as_array()
        .and_then(|worlds| worlds.iter().find(|world| world["world"] == "legacy"))
        .unwrap_or_else(|| panic!("the path's world must be registered: {reply}"));
    assert_eq!(
        world["state"], "live",
        "the world must live under the path identity: {reply}"
    );

    // 2. Once the ledger owns the path, a disagreeing record fails closed with one remedy.
    fs::write(&record, stale).expect("restore the stale world record");
    let disagree = journey.slipfork(&legacy, &["up"]);
    assert!(
        !disagree.status.success(),
        "a disagreeing record must fail closed:\n{}{}",
        stdout(&disagree),
        stderr(&disagree)
    );
    let refusal = format!("{}{}", stdout(&disagree), stderr(&disagree));
    assert_eq!(
        refusal.matches("next:").count(),
        1,
        "a conflict is one line plus one `next:`:\n{refusal}"
    );
    assert!(
        refusal.contains("next: rm .slipfork/world.json"),
        "the one remedy clears the stale record:\n{refusal}"
    );
    assert!(
        refusal.contains("feat-other--legacy") && refusal.contains("`legacy`"),
        "the refusal names both identities:\n{refusal}"
    );
    assert!(record.is_file(), "a conflict never deletes the evidence");

    let after = journey.slipfork(&legacy, &["ps", "--json"]);
    assert!(after.status.success(), "ps: {}", stderr(&after));
    let reply: serde_json::Value = serde_json::from_slice(&after.stdout).expect("ps --json");
    let rows: Vec<&serde_json::Value> = reply["worlds"]
        .as_array()
        .map(|worlds| {
            worlds
                .iter()
                .filter(|world| world["world"] == "legacy")
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        rows.len(),
        1,
        "a refused `up` must not duplicate the row: {reply}"
    );
    assert_eq!(
        rows[0]["state"], "live",
        "the live world must keep serving: {reply}"
    );

    fs::remove_file(&record).expect("clear the stale record");
    // 3. Cleared evidence: the next `up` is idle — no rebuild and no duplicate ledger row.
    let idle = journey.slipfork(&legacy, &["up"]);
    assert!(
        idle.status.success(),
        "{}",
        up_failure("`up` after clearing the record failed", &legacy, &idle)
    );
    assert!(
        !stdout(&idle).contains("built"),
        "an unchanged `up` must not rebuild:\n{}",
        stdout(&idle)
    );
    assert_eq!(
        latest_image_run(&journey, &legacy),
        first_run,
        "an idle `up` must not rebuild the image"
    );
    let settled = journey.slipfork(&legacy, &["ps", "--json"]);
    assert!(settled.status.success(), "ps: {}", stderr(&settled));
    let reply: serde_json::Value = serde_json::from_slice(&settled.stdout).expect("ps --json");
    assert_eq!(
        reply["worlds"].as_array().map(|worlds| worlds
            .iter()
            .filter(|world| world["world"] == "legacy")
            .count()),
        Some(1),
        "an idle `up` must not duplicate the row: {reply}"
    );

    let down = journey.slipfork(&legacy, &["down"]);
    assert!(down.status.success(), "down: {}", stderr(&down));
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

const DEV_FORKFILE: &str = r#"# forkfile: 0.1
image app . {
  dockerfile Dockerfile
  service web {
    run: python3 -m http.server 8099
  }
}
"#;

// No WORKDIR: dev must refuse instead of overlaying the rootfs root.
const DEV_ROOT_DOCKERFILE: &str = "\
FROM python:3.13-slim
COPY . .
CMD [\"python3\", \"-m\", \"http.server\", \"8099\"]
";

const DEV_CODE_DOCKERFILE: &str = "\
FROM python:3.13-slim
WORKDIR /code
COPY . .
CMD [\"python3\", \"-m\", \"http.server\", \"8099\"]
";

const DEV_PROJECT_DOCKERFILE: &str = "\
FROM python:3.13-slim
WORKDIR /project/api
COPY . .
CMD [\"python3\", \"-m\", \"http.server\", \"8099\"]
";

const DEV_HELLO: &str = "print('baked')\n";

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
