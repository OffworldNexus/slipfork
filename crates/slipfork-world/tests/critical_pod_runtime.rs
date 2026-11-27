//! Protected live E2E journey for world namespaces, `crun` pods, resources, and reap.
//!
//! Everything runs on real Btrfs subvolumes, a real tmpfs runtime directory, real `unshare`, and
//! real `crun` pods. On a host without Btrfs, `crun`, unprivileged user namespaces, or a tmpfs
//! `$XDG_RUNTIME_DIR` this suite FAILS CLOSED with one remedy line instead of skipping. When
//! `SLIPFORK_TEST_{TOOLBOX,PG,REDIS}_ROOTFS` are unset it pulls and seals `python:3.13-slim`,
//! `postgres:18`, and `redis:7` into the test store itself.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slipfork_image::{Architecture, ImageLayout, ImageReference, OciPuller, RegistryCredentials};
use slipfork_world::{
    Bind, BindSource, BtrfsEngine, Dirtiness, ImageFacts, OutputKind, PodEnv, PodHost, PodSpec,
    PostgresLauncher, PostgresTemplate, RedisLauncher, ResourceKind, StoreLayout, Stream,
    TemplateState, WorldEngine, WorldHost, WorldName, WorldSupervisor, WorldTools, is_btrfs_path,
    is_subvolume_path, write_bundle,
};

const SECRET: &str = "slipfork-probe-secret-9d3f1c";
const SVC_PORT: u16 = 18_099;
/// Placeholder both probe scripts replace with [`SVC_PORT`], so the port lives once.
const SVC_PORT_PLACEHOLDER: &str = "__SVC_PORT__";
const MOUNT_REMEDY: &str = "mount -o user_subvol_rm_allowed,noatime <device> <mount>";

const SERVER_PY: &str = r#"
import http.server, pathlib
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b'world-ok'
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass
srv = http.server.HTTPServer(('127.0.0.1', __SVC_PORT__), Handler)
pathlib.Path('/project/probe/svc.ready').write_text('ready')
srv.serve_forever()
"#;

/// The probe server script with [`SVC_PORT`] interpolated.
fn server_py() -> String {
    SERVER_PY.replace(SVC_PORT_PLACEHOLDER, &SVC_PORT.to_string())
}

/// The probe client script with [`SVC_PORT`] interpolated.
fn client_py() -> String {
    CLIENT_PY.replace(SVC_PORT_PLACEHOLDER, &SVC_PORT.to_string())
}

const CLIENT_PY: &str = r#"
import ctypes, datetime, hashlib, hmac, json, os, pathlib, socket, sys, time, urllib.request, urllib.error

role = sys.argv[1]
name = sys.argv[2]
out = {'uid': os.geteuid(), 'pid': os.getpid()}

def connect(port, timeout=1.0):
    sock = socket.socket()
    sock.settimeout(timeout)
    try:
        sock.connect(('127.0.0.1', port))
        return True
    except OSError:
        return False
    finally:
        sock.close()

libc = ctypes.CDLL(None, use_errno=True)
libc.shmat.restype = ctypes.c_void_p
shmid = libc.shmget(90000, 4096, 0o1666)
address = libc.shmat(shmid, None, 0)
if address and address != ctypes.c_void_p(-1).value:
    ctypes.memmove(address, role.encode(), 1)
    time.sleep(1.0)
    out['shm'] = ctypes.string_at(address, 1).decode()
    libc.shmdt(ctypes.c_void_p(address))
else:
    out['shm'] = 'unavailable'

out['svc'] = connect(__SVC_PORT__, 5.0)
if role == 'a':
    deadline = time.time() + 90
    while time.time() < deadline and not connect(5432):
        time.sleep(0.5)
    out['pg'] = connect(5432)
    deadline = time.time() + 90
    while time.time() < deadline and not connect(6379):
        time.sleep(0.5)
    out['redis'] = connect(6379)

expected_path = pathlib.Path('/project/probe/expected_secret')
expected = expected_path.read_text().strip() if expected_path.is_file() else None
value = os.environ.get('SLIPFORK_TEST_SECRET')
out['secret_env'] = value == expected if expected is not None else value is None
out['secret_argv'] = bool(expected) and expected.encode() in pathlib.Path('/proc/self/cmdline').read_bytes()

pathlib.Path('/project/probe/%s.json' % name).write_text(json.dumps(out))

# Stay alive until the host has finished scanning /proc for the planted secret.
release = pathlib.Path('/project/probe/%s.release' % name)
deadline = time.time() + 120
while time.time() < deadline and not release.exists():
    time.sleep(0.1)
"#;

fn fail_closed(message: &str) -> ! {
    panic!("fail closed: {message}");
}

fn scratch_root() -> PathBuf {
    if let Some(root) = std::env::var_os("SLIPFORK_TEST_ROOT") {
        return PathBuf::from(root);
    }
    let home = std::env::var_os("HOME")
        .unwrap_or_else(|| fail_closed("HOME is unset; export SLIPFORK_TEST_ROOT=<btrfs dir>"));
    PathBuf::from(home).join(".cache/slipfork/tests")
}

fn live_run_root() -> PathBuf {
    let scratch = scratch_root();
    assert!(
        !scratch.starts_with("/tmp"),
        "fail closed: `{}` is RAM-backed tmpfs and has no Btrfs ioctls.\nnext: set SLIPFORK_TEST_ROOT to a Btrfs directory such as ~/.cache/slipfork/tests",
        scratch.display()
    );
    fs::create_dir_all(&scratch).expect("create Btrfs scratch root");
    match is_btrfs_path(&scratch) {
        Ok(true) => {}
        Ok(false) => panic!(
            "fail closed: `{}` is not on Btrfs.\nnext: {MOUNT_REMEDY}",
            scratch.display()
        ),
        Err(error) => panic!(
            "fail closed: probing `{}` failed: {error}\nnext: {MOUNT_REMEDY}",
            scratch.display()
        ),
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock reads before the epoch")
        .as_nanos();
    let run = scratch.join(format!("pods-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&run).expect("create run root");
    run
}

fn require_tool(name: &str) -> PathBuf {
    for dir in std::env::var_os("PATH")
        .unwrap_or_default()
        .to_string_lossy()
        .split(':')
    {
        if dir.is_empty() {
            continue;
        }
        let candidate = Path::new(dir).join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    fail_closed(&format!(
        "`{name}` is not on PATH.\nnext: install {name} (crun, util-linux) and re-run"
    ))
}

fn is_tmpfs(path: &Path) -> bool {
    use std::os::fd::AsRawFd;
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    // SAFETY: `statfs` is plain-old-data and the descriptor is live.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: the kernel writes one `statfs` through the valid pointer.
    let ret = unsafe { libc::fstatfs(file.as_raw_fd(), &mut stat) };
    ret == 0 && stat.f_type as u64 == 0x0102_1994
}

fn require_tmpfs_runtime_dir() -> PathBuf {
    let uid = unsafe { libc::geteuid() };
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}")));
    assert!(
        dir.is_dir() && is_tmpfs(&dir),
        "fail closed: `{}` is not a tmpfs runtime directory.\nnext: export XDG_RUNTIME_DIR=/run/user/{uid} (a tmpfs); pod secrets must never reach disk",
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
        "fail closed: unprivileged user namespaces are unavailable.\nnext: `sudo sysctl -w kernel.unprivileged_userns_clone=1` (or enable CONFIG_USER_NS)"
    );
}

struct Images {
    toolbox: PathBuf,
    pg: PathBuf,
    redis: PathBuf,
    toolbox_facts: ImageFacts,
    pg_facts: ImageFacts,
    redis_facts: ImageFacts,
    /// Writable per-pod CoW clones made for this run; destroyed before the leak proof.
    clones: Vec<PathBuf>,
}

fn require_python(rootfs: &Path) {
    let has_python = ["usr/local/bin/python3", "usr/bin/python3", "bin/python3"]
        .iter()
        .any(|relative| rootfs.join(relative).is_file());
    assert!(
        has_python,
        "fail closed: toolbox rootfs `{}` has no python3.\nnext: export SLIPFORK_TEST_TOOLBOX_ROOTFS=<sealed python:3.13-slim subvolume>",
        rootfs.display()
    );
}

/// CoW-clones one sealed image into a writable per-pod rootfs under this run.
fn clone_image(
    engine: &BtrfsEngine,
    sealed: &Path,
    dest: &Path,
    clones: &mut Vec<PathBuf>,
) -> PathBuf {
    engine
        .clone_subvolume(sealed, dest)
        .unwrap_or_else(|error| {
            fail_closed(&format!(
                "could not clone sealed image `{}` to `{}`: {error}\nnext: {}",
                sealed.display(),
                dest.display(),
                MOUNT_REMEDY
            ))
        });
    clones.push(dest.to_path_buf());
    dest.to_path_buf()
}

fn require_images(engine: &BtrfsEngine, run: &Path) -> Images {
    let from_env = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
    };
    if let (Some(toolbox), Some(pg), Some(redis)) = (
        from_env("SLIPFORK_TEST_TOOLBOX_ROOTFS"),
        from_env("SLIPFORK_TEST_PG_ROOTFS"),
        from_env("SLIPFORK_TEST_REDIS_ROOTFS"),
    ) {
        require_python(&toolbox);
        return Images {
            toolbox,
            pg,
            redis,
            // Exported rootfs carry no config document; the pod still gets the default PATH.
            toolbox_facts: ImageFacts::default(),
            pg_facts: ImageFacts::default(),
            redis_facts: ImageFacts::default(),
            clones: Vec::new(),
        };
    }

    // No exported rootfs: pull and seal the upstream images into a Btrfs cache beside the run,
    // then clone each sealed rootfs writable (the `PodSpec::rootfs` contract).
    let cache = scratch_root().join("images");
    let store = StoreLayout::new(&cache);
    let puller = OciPuller::new(
        Arc::new(BtrfsEngine::new(StoreLayout::new(&cache))),
        ImageLayout::new(store.images_root()),
        RegistryCredentials::default(),
        Architecture::from_oci(std::env::consts::ARCH)
            .unwrap_or_else(|| fail_closed("this host architecture has no OCI platform mapping")),
        format!("slipfork-tests/{}", env!("CARGO_PKG_VERSION")),
    );
    let pull = |raw: &str| {
        let reference = ImageReference::parse(raw).unwrap_or_else(|error| {
            fail_closed(&format!("cannot parse image reference `{raw}`: {error}"))
        });
        puller.pull(&reference).unwrap_or_else(|error| {
            fail_closed(&format!(
                "could not pull `{raw}`: {error}\nnext: this suite needs docker.io egress; connect the host and re-run, or export SLIPFORK_TEST_{{TOOLBOX,PG,REDIS}}_ROOTFS"
            ))
        })
    };
    let sealed = [
        pull("python:3.13-slim"),
        pull(
            ResourceKind::Postgres
                .image()
                .expect("postgres:18 has an image"),
        ),
        pull(ResourceKind::Redis.image().expect("redis:7 has an image")),
    ];
    let facts = |image: &slipfork_image::PulledImage| image.facts.clone();
    let [toolbox_sealed, pg_sealed, redis_sealed] = sealed;
    let [toolbox_facts, pg_facts, redis_facts] = [
        facts(&toolbox_sealed),
        facts(&pg_sealed),
        facts(&redis_sealed),
    ];
    let clone_root = run.join(".slipfork-store/pods/probe-images");
    fs::create_dir_all(&clone_root).expect("create image clone root");
    let mut clones = Vec::new();
    let images = Images {
        toolbox_facts,
        pg_facts,
        redis_facts,
        toolbox: clone_image(
            engine,
            &toolbox_sealed.rootfs,
            &clone_root.join("toolbox"),
            &mut clones,
        ),
        pg: clone_image(
            engine,
            &pg_sealed.rootfs,
            &clone_root.join("pg"),
            &mut clones,
        ),
        redis: clone_image(
            engine,
            &redis_sealed.rootfs,
            &clone_root.join("redis"),
            &mut clones,
        ),
        clones,
    };
    require_python(&images.toolbox);
    images
}

/// Runs `crun list` with the same (default) root the pods were launched under.
///
/// `world_init` never passes `--root`, so `list` must not either; a different root would hide
/// every container instead of proving the store is empty.
struct ProbeWorld {
    name: WorldName,
    project: PathBuf,
    data: PathBuf,
}

fn prepare_world(engine: &BtrfsEngine, run: &Path, name: &str) -> ProbeWorld {
    let world = WorldName::parse(name).expect("valid world name");
    let data = engine.create_world(&world).expect("create world subvolume");
    // `redis/` is a plain directory: a nested subvolume would snapshot as an empty `ino == 2`
    // stub, and the AOF must travel with the world on `fork`. `PGDATA` lives flat beside the world.
    fs::create_dir_all(data.join("redis")).expect("create the redis parent directory");
    let project = run.join(format!("project-{name}"));
    engine
        .create_subvolume(&project)
        .expect("create project subvolume");
    fs::create_dir_all(project.join("probe")).expect("seed probe dir");
    ProbeWorld {
        name: world,
        project,
        data,
    }
}

/// A pod-local `/dev/shm` on a real tmpfs, so `dynamic_shared_memory_type=posix` has a backend.
fn shm_dir(tag: &str) -> PathBuf {
    assert!(
        is_tmpfs(Path::new("/dev/shm")),
        "fail closed: /dev/shm is not a tmpfs; postgres needs one for `dynamic_shared_memory_type=posix`"
    );
    let dir = PathBuf::from("/dev/shm").join(format!("slipfork-test-{}-{tag}", std::process::id()));
    fs::create_dir_all(&dir).expect("create the pod-local /dev/shm");
    dir
}

fn project_bind(project: &Path) -> Bind {
    Bind {
        source: BindSource::Host(project.to_path_buf()),
        destination: "/project".to_owned(),
        read_only: false,
    }
}

fn client_pod(
    rootfs: &Path,
    facts: &ImageFacts,
    project: &Path,
    id: &str,
    role: &str,
    result: &str,
    secret: Option<&str>,
) -> PodSpec {
    let mut env = Vec::new();
    if let Some(secret) = secret {
        env.push(PodEnv::new("SLIPFORK_TEST_SECRET", secret));
    }
    PodSpec {
        id: id.to_owned(),
        rootfs: rootfs.to_path_buf(),
        workdir: "/project".to_owned(),
        argv: vec![
            "python3".to_owned(),
            "-c".to_owned(),
            client_py(),
            role.to_owned(),
            result.to_owned(),
        ],
        inherited_env: facts.env.clone(),
        env,
        binds: vec![project_bind(project)],
        files: Vec::new(),
        identity: None,
    }
}

fn wait_for_file(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.is_file() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "fail closed: `{}` never appeared within {timeout:?}",
        path.display()
    );
}

fn wait_json(path: &Path, timeout: Duration) -> serde_json::Value {
    wait_for_file(path, timeout);
    let bytes = fs::read(path).expect("read pod result");
    serde_json::from_slice(&bytes).expect("parse pod result")
}

fn collect_logs(
    supervisor: &WorldSupervisor,
    service: &str,
) -> (Vec<(String, Stream)>, Vec<String>) {
    let mut lines = Vec::new();
    let mut dividers = Vec::new();
    let mut on_output = |event: slipfork_world::control::OutputEvent| match event.kind {
        OutputKind::Divider => dividers.push(event.line),
        OutputKind::Line => lines.push((event.line, event.stream)),
        OutputKind::Eof => {}
    };
    supervisor
        .logs(Some(service), false, &mut on_output)
        .expect("replay captured logs");
    (lines, dividers)
}

/// Pids whose cmdline mentions `needle` **and** belong to this journey.
///
/// A fresh run root is unique, and every world process lives in one of the journey's pid
/// namespaces, so a stray `slipfork-world-init`/`postgres` from an older lab run (with a
/// different run root and namespace) can never fail this journey, while a real leak always does.
fn processes_mentioning(needle: &[u8], run: &Path, worlds: &[PathBuf]) -> Vec<u32> {
    let scope = run.as_os_str().as_bytes();
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
        if !cmdline.windows(needle.len()).any(|window| window == needle) {
            continue;
        }
        let in_run = cmdline.windows(scope.len()).any(|window| window == scope);
        let in_world = fs::read_link(format!("/proc/{pid}/ns/pid"))
            .is_ok_and(|namespace| worlds.iter().any(|world| world == &namespace));
        if in_run || in_world {
            found.push(pid);
        }
    }
    found
}

fn subvolumes_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).expect("read scratch tree");
        for entry in entries {
            let entry = entry.expect("read scratch tree entry");
            if !entry.file_type().expect("stat entry").is_dir() {
                continue;
            }
            let path = entry.path();
            if is_subvolume_path(&path).expect("probe directory") {
                found.push(path.clone());
            }
            stack.push(path);
        }
    }
    found
}

#[test]
fn world_pod_lifecycle() {
    let run = live_run_root();
    let runtime = require_tmpfs_runtime_dir();
    // A unique fresh root must not be named by any live process; fail closed if it is.
    let stale = processes_mentioning(run.as_os_str().as_bytes(), &run, &[]);
    assert!(
        stale.is_empty(),
        "fail closed: `{}` already appears in the cmdline of {stale:?}; a stale journey owns it",
        run.display()
    );
    let crun = require_tool("crun");
    let unshare = require_tool("unshare");
    let slirp4netns = require_tool("slirp4netns");
    require_userns(&unshare);
    let init = PathBuf::from(env!("CARGO_BIN_EXE_slipfork-world-init"));
    assert!(
        init.is_file(),
        "fail closed: `slipfork-world-init` binary missing at `{}`",
        init.display()
    );
    let uid = unsafe { libc::geteuid() };
    let gid = unsafe { libc::getegid() };
    let engine = BtrfsEngine::new(StoreLayout::new(&run));
    engine.ensure_store().expect("create .slipfork-store");
    let images = require_images(&engine, &run);

    let world_a = prepare_world(&engine, &run, "probe-a");
    let world_b = prepare_world(&engine, &run, "probe-b");
    // The client compares its OCI env against this planted value; it never enters argv.
    fs::write(world_a.project.join("probe/expected_secret"), SECRET)
        .expect("seed the expected secret");

    let store_root = StoreLayout::new(&run);
    let host = || {
        let mut host = WorldHost::new(
            WorldTools {
                crun: crun.clone(),
                unshare: unshare.clone(),
                slirp4netns: slirp4netns.clone(),
                world_init: init.clone(),
            },
            runtime.clone(),
            store_root.logs_root(),
            uid,
            gid,
            Some(PathBuf::from("/etc/resolv.conf")),
        );
        host.boot_timeout = Duration::from_secs(30);
        host
    };
    let mut supervisor_a = WorldSupervisor::boot(host(), &world_a.name).expect("boot world a");
    let mut supervisor_b = WorldSupervisor::boot(host(), &world_b.name).expect("boot world b");
    // The two worlds' pid namespaces scope every leak scan to this journey.
    let world_pid_namespaces: Vec<PathBuf> = [supervisor_a.init_pid(), supervisor_b.init_pid()]
        .into_iter()
        .flatten()
        .filter_map(|pid| fs::read_link(format!("/proc/{pid}/ns/pid")).ok())
        .collect();

    // 1. Host-resolvable namespaces: `/proc/<init-pid>/ns/*`, verified by inode identity.
    let table = supervisor_a
        .namespaces()
        .expect("canonical namespace table");
    assert_eq!(table.iter().count(), 6, "six world namespaces");
    for (kind, path) in table.iter() {
        assert!(
            path.exists(),
            "`{kind}` namespace path `{}` must resolve",
            path.display()
        );
        assert!(
            path.starts_with(format!(
                "/proc/{}/ns",
                supervisor_a.init_pid().expect("init pid")
            )),
            "`{kind}` must be stored as /proc/<init-pid>/ns/*"
        );
    }

    // 1b. A run-once step streams both pipes and reports the real exit code.
    let run_once_root = run.join(".slipfork-store/pods/probe-images/run-once");
    engine
        .clone_subvolume(&images.toolbox, &run_once_root)
        .expect("clone a rootfs for the run-once probe");
    let run_once = PodSpec {
        id: "m339runonce".to_owned(),
        rootfs: run_once_root.clone(),
        workdir: "/".to_owned(),
        argv: vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "printf 'only-once\\n'; printf 'boom\\n' >&2; exit 7".to_owned(),
        ],
        inherited_env: images.toolbox_facts.env.clone(),
        env: Vec::new(),
        binds: Vec::new(),
        files: Vec::new(),
        identity: None,
    };
    let mut out_lines = Vec::new();
    let mut err_lines = Vec::new();
    let mut sink = |stream: slipfork_world::Stream, line: &str| match stream {
        slipfork_world::Stream::Stdout => out_lines.push(line.to_owned()),
        slipfork_world::Stream::Stderr => err_lines.push(line.to_owned()),
        slipfork_world::Stream::Combined => {}
    };
    let code = supervisor_a
        .run_once(&run_once, &mut sink)
        .expect("run the one-shot probe");
    assert_eq!(code, 7, "run-once must report the command's exit code");
    assert!(
        out_lines.iter().any(|line| line == "only-once"),
        "stdout must stream live: {out_lines:?}"
    );
    assert!(
        err_lines.iter().any(|line| line == "boom"),
        "stderr must stream live: {err_lines:?}"
    );
    engine
        .destroy(&run_once_root, Dirtiness::Clean, true)
        .expect("destroy the run-once rootfs");

    // 2. One in-namespace loopback server plus the real backing resources.
    let server = PodSpec {
        id: "m339svc".to_owned(),
        rootfs: images.toolbox.clone(),
        workdir: "/project".to_owned(),
        argv: vec!["python3".to_owned(), "-c".to_owned(), server_py()],
        inherited_env: images.toolbox_facts.env.clone(),
        env: Vec::new(),
        binds: vec![project_bind(&world_a.project)],
        files: Vec::new(),
        identity: None,
    };
    supervisor_a
        .launch(&server)
        .expect("launch loopback server");
    wait_for_file(
        &world_a.project.join("probe/svc.ready"),
        Duration::from_secs(60),
    );
    // The daemon reaches a world pod by asking the init for a `Dial`ed descriptor, never by
    // connecting from the host (the service port is not reachable outside the world netns).
    let mut dialed = supervisor_a
        .dial_tcp(SVC_PORT)
        .expect("the init must hand back a world-loopback connection");
    dialed
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("configure the dialed stream");
    dialed
        .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .expect("write the dialed request");
    let mut response = String::new();
    dialed
        .read_to_string(&mut response)
        .expect("read the dialed response");
    assert!(
        response.contains("world-ok"),
        "dial_tcp must return the pod's own bytes: {response:?}"
    );
    let refused = supervisor_a
        .dial_tcp(1)
        .expect_err("a closed world port must be refused");
    assert!(
        matches!(refused, slipfork_world::Error::Protocol { .. }),
        "a refused dial must be typed: {refused}"
    );
    assert!(
        refused.to_string().contains("dial-refused"),
        "a refused dial must carry the init's code: {refused}"
    );

    // 1c. Detached output is captured on disk, replayable, and exec runs in the pod.
    let printer = PodSpec {
        id: "m339print".to_owned(),
        rootfs: images.toolbox.clone(),
        workdir: "/project".to_owned(),
        argv: vec![
            "python3".to_owned(),
            "-u".to_owned(),
            "-c".to_owned(),
            "print('printed', flush=True)\nimport sys\nprint('printed-err', file=sys.stderr, flush=True)\nimport time\ntime.sleep(120)".to_owned(),
        ],
        inherited_env: images.toolbox_facts.env.clone(),
        env: Vec::new(),
        binds: vec![project_bind(&world_a.project)],
        files: Vec::new(),
        identity: None,
    };
    supervisor_a
        .launch(&printer)
        .expect("launch the printer pod");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut printed = Vec::new();
    let mut dividers = Vec::new();
    while Instant::now() < deadline {
        let (lines, seen) = collect_logs(&supervisor_a, "m339print");
        dividers = seen;
        if lines.iter().any(|(line, _)| line == "printed")
            && lines.iter().any(|(line, _)| line == "printed-err")
        {
            printed = lines;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        printed
            .iter()
            .any(|(line, stream)| line == "printed" && *stream == Stream::Stdout),
        "the detached pod's stdout must land in <pods>/logs/m339print.log as Stream::Stdout: {printed:?}"
    );
    assert!(
        printed
            .iter()
            .any(|(line, stream)| line == "printed-err" && *stream == Stream::Stderr),
        "the detached pod's stderr must replay as Stream::Stderr: {printed:?}"
    );
    assert!(
        dividers
            .iter()
            .any(|line| line.contains("m339print") && line.contains("started")),
        "the capture must open with a started divider: {dividers:?}"
    );

    let mut exec_lines = Vec::new();
    let mut on_exec = |event: slipfork_world::control::OutputEvent| {
        if event.kind == OutputKind::Line && matches!(event.stream, Stream::Stdout | Stream::Stderr)
        {
            exec_lines.push(event.line);
        }
    };
    let code = supervisor_a
        .exec(
            "m339svc",
            &[
                "python3".to_owned(),
                "-c".to_owned(),
                "import sys; print('exec-ok'); sys.exit(3)".to_owned(),
            ],
            &mut on_exec,
        )
        .expect("exec inside the live server pod");
    assert_eq!(
        code, 3,
        "exec must forward the child's exit code: {exec_lines:?}"
    );
    assert!(
        exec_lines.iter().any(|line| line == "exec-ok"),
        "exec must stream stdout: {exec_lines:?}"
    );

    supervisor_a
        .restart(Some("m339print"))
        .expect("restart the printer pod");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut restarted = false;
    while Instant::now() < deadline {
        let (_, seen) = collect_logs(&supervisor_a, "m339print");
        if seen
            .iter()
            .any(|line| line.contains("m339print") && line.contains("restarted"))
        {
            restarted = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        restarted,
        "a restart must append a `restarted` divider to the same log"
    );

    // 1d. Golden `postgres:18` template: the real `initdb` runs once inside world a as a
    // run-once crun pod, then the cluster is sealed `BTRFS_SUBVOL_RDONLY` and version-stamped.
    let layout = engine.layout().clone();
    let template = PostgresTemplate::new(layout.clone());
    let mut initdb_output: Vec<String> = Vec::new();
    let created = template
        .ensure(
            &engine,
            &images.pg,
            &mut |spec| {
                let code = supervisor_a
                    .run_once(spec, &mut |_, line| initdb_output.push(line.to_owned()))?;
                if code == 0 {
                    Ok(())
                } else {
                    Err(slipfork_world::Error::PodLaunch {
                        message: format!("initdb exited {code}: {initdb_output:?}"),
                    })
                }
            },
            &mut |_| {},
        )
        .unwrap_or_else(|error| {
            fail_closed(&format!(
                "could not build the golden postgres-18 template: {error}\nnext: boot from a postgres:18 rootfs (it must contain `initdb`)"
            ))
        });
    assert_eq!(
        created,
        TemplateState::Created,
        "the first boot must run the one `initdb`"
    );
    assert!(
        engine
            .is_sealed(&template.path())
            .expect("probe the golden template"),
        "the golden template must be sealed with BTRFS_SUBVOL_RDONLY"
    );
    let denied = fs::write(template.path().join("intruder"), b"x")
        .expect_err("a sealed template must refuse writes");
    assert_eq!(denied.raw_os_error(), Some(libc::EROFS), "{denied:?}");
    let cached = template
        .ensure(
            &engine,
            &images.pg,
            &mut |_| panic!("initdb must never run twice"),
            &mut |_| {},
        )
        .expect("a sealed, version-stamped template is cached");
    assert_eq!(cached, TemplateState::Cached);

    // 1e. `< 1ms` per-world PGDATA clone: a flat sibling beside the world, stale locks stripped.
    let pg_a = PostgresLauncher::new(layout.clone(), world_a.name.clone());
    let pg_b = PostgresLauncher::new(layout.clone(), world_b.name.clone());
    let warm = pg_a
        .clone(&engine, &template.path(), "m339pgwarm")
        .expect("warm the snapshot path");
    engine
        .destroy(&warm.path, Dirtiness::Clean, true)
        .expect("destroy the warm clone");
    let pgdata = pg_a
        .clone(&engine, &template.path(), "m339pg")
        .expect("clone PGDATA for world a");
    assert_eq!(
        pgdata.path,
        run.join(".slipfork-store/worlds/probe-a.pg.m339pg"),
        "PGDATA must be a flat sibling of the world, never nested inside it"
    );
    // One `SNAP_CREATE_V2` ioctl: ~0.3ms local NVMe, 8.6–27.2ms raw cloud volume, 60–110ms on
    // loop-backed CI Btrfs. The ceiling catches added user-space work (a second ioctl, a copy, a
    // syncfs) or an `initdb` re-run (> 1s), never block-device commit latency.
    assert!(
        pgdata.elapsed < Duration::from_millis(250),
        "PGDATA clone took {:?}, budget 250ms (one kernel snapshot ioctl)",
        pgdata.elapsed
    );
    assert!(
        pgdata.path.join("PG_VERSION").is_file(),
        "the clone must carry the curated initdb payload"
    );
    fs::write(pgdata.path.join("postmaster.pid"), b"4242\n").expect("plant a stale lock");
    let reused = pg_a
        .clone(&engine, &template.path(), "m339pg")
        .expect("reuse the clone");
    assert_eq!(
        reused.elapsed,
        Duration::ZERO,
        "a reboot must not pay a second snapshot"
    );
    assert!(
        !pgdata.path.join("postmaster.pid").exists(),
        "a reused PGDATA must never inherit postmaster.pid"
    );
    let pgdata_b = pg_b
        .clone(&engine, &template.path(), "m339pg")
        .expect("clone PGDATA for world b");
    assert_ne!(
        pgdata.path, pgdata_b.path,
        "every world owns its own PGDATA clone"
    );

    // 1f. The launcher runs `postgres` directly with the exact dev flag set, no entrypoint script.
    let shm_a = shm_dir("a");
    let shm_b = shm_dir("b");
    let pg_spec = pg_a
        .spec("m339pg", &images.pg, &shm_a, &images.pg_facts)
        .expect("render the postgres pod");
    assert!(
        pg_spec.argv[0].ends_with("/postgres"),
        "the launcher must exec postgres itself, not docker-entrypoint.sh: {:?}",
        pg_spec.argv[0]
    );
    assert_eq!(
        pg_spec.argv[1..],
        [
            "-D",
            "/var/lib/postgresql/data",
            "-c",
            "fsync=off",
            "-c",
            "synchronous_commit=off",
            "-c",
            "full_page_writes=off",
            "-c",
            "wal_level=minimal",
            "-c",
            "max_wal_senders=0",
            "-c",
            "autovacuum=off",
            "-c",
            "shared_buffers=32MB",
            "-c",
            "dynamic_shared_memory_type=posix",
            "-c",
            "unix_socket_directories=/var/lib/postgresql/data/.sock",
        ],
        "postgres must run the exact dev flag set"
    );
    assert_eq!(
        pg_spec.binds[0].source,
        BindSource::Host(pgdata.path.clone()),
        "PGDATA binds the flat clone"
    );
    assert_eq!(pg_spec.binds[0].destination, "/var/lib/postgresql/data");
    assert_eq!(
        pg_spec.binds[1].destination, "/dev/shm",
        "postgres needs a pod-local /dev/shm for `dynamic_shared_memory_type=posix`"
    );
    assert!(
        pg_spec
            .env
            .iter()
            .any(|env| env.key == "POSTGRES_HOST_AUTH_METHOD" && env.value == "trust"),
        "local auth must stay trust"
    );
    let pg_bundle_dir = run.join("pg-bundle");
    let pg_bundle = write_bundle(
        &pg_spec,
        &PodHost {
            crun: crun.clone(),
            resolv_conf: Some(PathBuf::from("/etc/resolv.conf")),
            uid,
            gid,
        },
        &pg_bundle_dir,
    )
    .expect("render the postgres bundle");
    let pg_config: serde_json::Value =
        serde_json::from_slice(&fs::read(&pg_bundle.config).expect("read the postgres config"))
            .expect("parse the postgres config");
    let pg_env: Vec<&str> = pg_config["process"]["env"]
        .as_array()
        .expect("postgres env")
        .iter()
        .filter_map(|value| value.as_str())
        .collect();
    assert!(
        pg_env.iter().any(|value| value.starts_with("PG_MAJOR=")),
        "postgres must inherit the image's PG_MAJOR: {pg_env:?}"
    );
    assert!(
        pg_env
            .iter()
            .any(|value| value.starts_with("PATH=") && value.contains("/usr/local/bin")),
        "postgres must inherit the image PATH: {pg_env:?}"
    );
    assert!(
        !pg_env.iter().any(|value| value.contains(SECRET)),
        "a world component secret must never reach a backing-resource bundle: {pg_env:?}"
    );
    let _ = fs::remove_dir_all(&pg_bundle_dir);
    supervisor_a.launch(&pg_spec).expect("launch postgres");
    let pg_isready = Path::new(&pg_spec.argv[0])
        .parent()
        .expect("postgres binary has a directory")
        .join("pg_isready")
        .to_string_lossy()
        .into_owned();
    let waited = pg_a
        .await_ready("m339pg", &mut || {
            // The real client binary, against the pod's unix socket: never a bare TCP accept.
            supervisor_a
                .exec(
                    "m339pg",
                    &[
                        pg_isready.clone(),
                        "-h".to_owned(),
                        "/var/lib/postgresql/data/.sock".to_owned(),
                    ],
                    &mut |_| {},
                )
                .is_ok_and(|code| code == 0)
        })
        .expect("pg_isready must accept the pod's unix socket");
    assert!(
        waited <= PostgresLauncher::READINESS_BUDGET,
        "readiness took {waited:?}"
    );
    assert_eq!(
        PostgresLauncher::READINESS_INTERVAL,
        Duration::from_millis(2),
        "readiness must poll every 2ms, never a fixed sleep"
    );

    // 1g. Redis: AOF on `<world>/redis/<id>` (a plain directory inside the world subvolume) so
    // live keys survive `fork`, and `--save "" --appendfsync no` so a dev world never stalls.
    let cache_a = RedisLauncher::new(layout.clone(), world_a.name.clone());
    let cache_b = RedisLauncher::new(layout.clone(), world_b.name.clone());
    let aof_a = cache_a
        .prepare("m339cache")
        .expect("create world a's AOF directory");
    assert_eq!(
        aof_a,
        world_a.data.join("redis/m339cache"),
        "AOF files must live inside the world so a fork carries them"
    );
    assert!(
        !is_subvolume_path(&aof_a).expect("probe the AOF directory"),
        "a nested subvolume would snapshot as an empty stub"
    );
    let redis_spec = cache_a
        .spec("m339cache", &images.redis, &images.redis_facts)
        .expect("render the redis pod");
    assert!(
        redis_spec.argv[0].ends_with("/redis-server"),
        "the launcher must exec redis-server itself: {:?}",
        redis_spec.argv[0]
    );
    assert_eq!(
        redis_spec.argv[1..],
        [
            "--bind",
            "127.0.0.1",
            "--dir",
            "/data",
            "--save",
            "",
            "--appendonly",
            "yes",
            "--appendfsync",
            "no",
        ],
        "redis must never save an RDB or fsync in a dev world"
    );
    assert_eq!(redis_spec.binds[0].source, BindSource::Host(aof_a.clone()));
    let redis_cli = Path::new(&redis_spec.argv[0])
        .parent()
        .expect("redis-server has a directory")
        .join("redis-cli")
        .to_string_lossy()
        .into_owned();
    supervisor_a
        .launch(&redis_spec)
        .expect("launch redis in world a");
    cache_a
        .await_ready(&mut || {
            supervisor_a
                .exec(
                    "m339cache",
                    &[redis_cli.clone(), "PING".to_owned()],
                    &mut |_| {},
                )
                .is_ok_and(|code| code == 0)
        })
        .expect("redis-cli PING must succeed inside world a");
    let aof_b = cache_b
        .prepare("m339cache")
        .expect("create world b's AOF directory");
    assert_ne!(aof_a, aof_b, "each world stamps its own AOF directory");
    let redis_b = cache_b
        .spec("m339cache", &images.redis, &images.redis_facts)
        .expect("render world b's redis pod");
    supervisor_b
        .launch(&redis_b)
        .expect("launch redis in world b");
    cache_b
        .await_ready(&mut || {
            supervisor_b
                .exec(
                    "m339cache",
                    &[redis_cli.clone(), "PING".to_owned()],
                    &mut |_| {},
                )
                .is_ok_and(|code| code == 0)
        })
        .expect("redis-cli PING must succeed inside world b");
    let mut ignore = |_: slipfork_world::control::OutputEvent| {};
    let set = supervisor_a
        .exec(
            "m339cache",
            &[
                "redis-cli".to_owned(),
                "SET".to_owned(),
                "fork-key".to_owned(),
                "world-a".to_owned(),
            ],
            &mut ignore,
        )
        .expect("write a live key through world a's redis");
    assert_eq!(set, 0, "redis-cli SET must succeed");
    let mut seen = Vec::new();
    let mut capture = |event: slipfork_world::control::OutputEvent| {
        if event.kind == OutputKind::Line {
            seen.push(event.line);
        }
    };
    let get = supervisor_b
        .exec(
            "m339cache",
            &[
                "redis-cli".to_owned(),
                "GET".to_owned(),
                "fork-key".to_owned(),
            ],
            &mut capture,
        )
        .expect("read through world b's redis");
    assert_eq!(get, 0, "redis-cli GET must succeed in world b");
    assert!(
        !seen.iter().any(|line| line.contains("world-a")),
        "world b must never see world a's live keys: {seen:?}"
    );
    let appendonly = aof_a.join("appendonlydir");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !appendonly.is_dir() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        appendonly.is_dir(),
        "redis must append its AOF under `<world>/redis/<id>`, never under /tmp"
    );

    // 3. Identity + loopback + cross-world isolation + System V IPC isolation.
    supervisor_a
        .launch(&client_pod(
            &images.toolbox,
            &images.toolbox_facts,
            &world_a.project,
            "m339clienta",
            "a",
            "a",
            Some(SECRET),
        ))
        .expect("launch world a client");
    supervisor_b
        .launch(&client_pod(
            &images.toolbox,
            &images.toolbox_facts,
            &world_b.project,
            "m339clientb",
            "b",
            "b",
            None,
        ))
        .expect("launch world b client");
    let a = wait_json(
        &world_a.project.join("probe/a.json"),
        Duration::from_secs(120),
    );
    let b = wait_json(
        &world_b.project.join("probe/b.json"),
        Duration::from_secs(120),
    );

    assert_eq!(
        a["uid"].as_u64(),
        Some(u64::from(uid)),
        "geteuid() inside the world must equal the host uid"
    );
    assert_eq!(a["svc"].as_bool(), Some(true), "world loopback must work");
    assert_eq!(
        b["svc"].as_bool(),
        Some(false),
        "the same port must not be reachable across worlds"
    );
    let address: SocketAddr = format!("127.0.0.1:{SVC_PORT}").parse().expect("address");
    assert!(
        TcpStream::connect_timeout(&address, Duration::from_millis(300)).is_err(),
        "the world port must not be reachable from the host"
    );
    assert_eq!(a["shm"].as_str(), Some("a"), "world a owns its shm segment");
    assert_eq!(b["shm"].as_str(), Some("b"), "world b owns its shm segment");

    // 4. Real resources answer on the shared world loopback.
    assert_eq!(a["pg"].as_bool(), Some(true), "postgres must accept :5432");
    assert_eq!(a["redis"].as_bool(), Some(true), "redis must accept :6379");

    // 5. The OCI env carried the secret while both clients stayed alive; argv never carries it.
    assert_eq!(
        a["secret_env"].as_bool(),
        Some(true),
        "the OCI env must deliver SLIPFORK_TEST_SECRET"
    );
    assert_eq!(
        a["secret_argv"].as_bool(),
        Some(false),
        "the planted secret must never appear in pod argv"
    );
    assert_eq!(
        b["secret_env"].as_bool(),
        Some(true),
        "world b must not receive the secret"
    );
    assert_eq!(
        b["secret_argv"].as_bool(),
        Some(false),
        "world b argv must stay clean"
    );
    let leaked = processes_mentioning(SECRET.as_bytes(), &run, &world_pid_namespaces);
    assert!(leaked.is_empty(), "secret leaked into argv of {leaked:?}");
    fs::write(world_a.project.join("probe/a.release"), b"go").expect("release world a client");
    fs::write(world_b.project.join("probe/b.release"), b"go").expect("release world b client");

    // 7. The init inside the world reports every tracked pod (host-side `crun list` cannot:
    // containers live in the world's user namespace and answer `kill: Operation not permitted`).
    let pods = supervisor_a.pods().expect("list the world's pods");
    for id in ["m339svc", "m339pg", "m339cache"] {
        let pod = pods
            .iter()
            .find(|pod| pod.id == id)
            .unwrap_or_else(|| panic!("pod `{id}` must be tracked before teardown: {pods:?}"));
        assert!(pod.running, "pod `{id}` must still be running: {pods:?}");
    }

    // 8. Clean teardown: the init refuses `List` once it is gone, and no process survives.
    supervisor_a.stop().expect("stop world a");
    supervisor_b.stop().expect("stop world b");
    let error = supervisor_a
        .pods()
        .expect_err("a stopped world must refuse to report pods");
    assert!(
        matches!(
            &error,
            slipfork_world::Error::Init { message } if message == "world is not running"
        ),
        "a stopped world must fail closed with a typed init error: {error}"
    );
    for needle in [
        b"slipfork-world-init".as_slice(),
        b"postgres".as_slice(),
        b"redis-server".as_slice(),
    ] {
        let survivors = processes_mentioning(needle, &run, &world_pid_namespaces);
        assert!(
            survivors.is_empty(),
            "`{}` survived teardown in {survivors:?}",
            String::from_utf8_lossy(needle)
        );
    }

    for world in [&world_a, &world_b] {
        engine
            .destroy(&world.data, slipfork_world::Dirtiness::Clean, false)
            .expect("destroy world subvolume");
        engine
            .destroy(&world.project, slipfork_world::Dirtiness::Clean, false)
            .expect("destroy project subvolume");
    }
    for clone in [pgdata.path, pgdata_b.path] {
        engine
            .destroy(&clone, slipfork_world::Dirtiness::Clean, true)
            .expect("destroy the per-world PGDATA clone");
    }
    engine
        .destroy(&template.path(), slipfork_world::Dirtiness::Clean, true)
        .expect("destroy the sealed golden template");
    for shm in [&shm_a, &shm_b] {
        fs::remove_dir_all(shm).expect("remove the pod-local /dev/shm");
    }
    for clone in &images.clones {
        engine
            .destroy(clone, slipfork_world::Dirtiness::Clean, false)
            .expect("destroy image rootfs clone");
    }
    if !images.clones.is_empty() {
        fs::remove_dir_all(run.join(".slipfork-store/pods/probe-images"))
            .expect("remove image clone root");
    }
    let leftovers = subvolumes_under(&run);
    assert!(leftovers.is_empty(), "leaked subvolumes: {leftovers:?}");
    fs::remove_dir_all(&run).expect("remove scratch run");
}
