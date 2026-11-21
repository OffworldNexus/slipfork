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
use std::net::{TcpStream, ToSocketAddrs};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BTRFS_SUPER_MAGIC: u64 = 0x9123_683E;
const TMPFS_MAGIC: u64 = 0x0102_1994;

const DOCKERFILE: &str = "\
FROM python:3.13-slim
RUN pip install --no-cache-dir uv
WORKDIR /project
CMD [\"python3\", \"--version\"]
";

// `pre` and `post` are written by the hooks; `svc` is written by the service itself, so
// `presvcpost` proves the service ran strictly between the two hooks.
const GOOD_FORKFILE: &str = r#"# forkfile: 0.1
image app . {
  dockerfile Dockerfile
  pre-deploy: sh -c 'printf pre > order.txt'
  post-deploy: sh -c 'for i in $(seq 50); do grep -q svc order.txt && break; sleep 0.1; done; grep -q svc order.txt && printf post >> order.txt'
  service api {
    port 8000
    run: @uv run python -c "open('order.txt','a').write('svc'); import time; time.sleep(3600)"
  }
  service worker {
    run: @uv run python -c "import time; time.sleep(3600)"
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
        self.projects.borrow_mut().push(dir.clone());
        dir
    }

    fn slipfork(&self, project: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_slipfork"))
            .args(args)
            .current_dir(project)
            .env("NO_COLOR", "1")
            .env("XDG_RUNTIME_DIR", &self.runtime)
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
                .output();
        }
        let _ = wait_until(
            || processes_mentioning(b"slipfork-world-init").is_empty(),
            Duration::from_secs(10),
        );
        if let Err(error) = fs::remove_dir_all(&self.run) {
            eprintln!(
                "journey cleanup: cannot remove `{}`: {error}",
                self.run.display()
            );
        }
    }
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
        "first `up` failed:\nstdout:\n{first_out}\nstderr:\n{first_err}"
    );
    assert!(
        first_out.contains("✓ Ready -> http://good.localhost"),
        "ready line missing:\n{first_out}"
    );
    assert!(
        first_err.contains("progress: .slipfork/supervisor.log"),
        "human mode must name the supervisor log:\n{first_err}"
    );
    assert!(
        first_out.contains("[1/"),
        "progress lines must carry a real `[i/n]` prefix:\n{first_out}"
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

    // 2. A failed service keeps the world running; `down` must still work.
    let broken = journey.project("broken", BROKEN_FORKFILE);
    let failed = journey.slipfork(&broken, &["up"]);
    assert_eq!(
        failed.status.code(),
        Some(1),
        "a failing service must fail `up`:\n{}",
        stdout(&failed)
    );
    let failed_err = stderr(&failed);
    assert!(failed_err.contains("error:"), "{failed_err}");
    assert!(failed_err.contains("next:"), "{failed_err}");
    assert!(
        broken.join(".slipfork/world.json").is_file(),
        "the world must stay up after a failed service"
    );
    let down = journey.slipfork(&broken, &["down"]);
    assert!(
        down.status.success(),
        "down after failure: {}",
        stderr(&down)
    );

    // 3. Down and re-up: the stamped `uv sync` is skipped and named as such.
    let down = journey.slipfork(&good, &["down"]);
    assert!(down.status.success(), "down: {}", stderr(&down));
    let second = journey.slipfork(&good, &["up"]);
    let second_out = stdout(&second);
    assert!(
        second.status.success(),
        "second `up` failed:\n{second_out}\n{}",
        stderr(&second)
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
        "restart: {}\n{}",
        stdout(&restart),
        stderr(&restart)
    );
    assert!(
        stdout(&restart).contains("Restarted api"),
        "{}",
        stdout(&restart)
    );
    let down = journey.slipfork(&good, &["down"]);
    assert!(down.status.success(), "final down: {}", stderr(&down));
    assert!(
        wait_until(
            || processes_mentioning(b"slipfork-world-init").is_empty(),
            Duration::from_secs(10)
        ),
        "slipfork-world-init survived teardown"
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
