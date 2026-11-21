//! Protected live E2E journey for world namespaces, `crun` pods, resources, and reap.
//!
//! Everything runs on real Btrfs subvolumes, a real tmpfs runtime directory, real `unshare`,
//! real `crun` pods, and a real SeaweedFS S3 policy. On a host without Btrfs, `crun`, unprivileged
//! user namespaces, or a tmpfs `$XDG_RUNTIME_DIR` this suite FAILS CLOSED with one remedy line
//! instead of skipping. When `SLIPFORK_TEST_{TOOLBOX,PG,REDIS,S3}_ROOTFS` are unset it pulls and
//! seals `python:3.13-slim`, `postgres:18`, `redis:7`, and SeaweedFS into the test store itself.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slipfork_image::{Architecture, Credentials, ImageLayout, ImageReference, OciPuller};
use slipfork_world::{
    Bind, BindSource, BtrfsEngine, Dirtiness, ImageFacts, OutputKind, PodEnv, PodHost, PodSpec,
    ResourceKind, S3Bucket, S3Credentials, S3Policy, SeaweedfsPlan, StoreLayout, Stream,
    WorldEngine, WorldHost, WorldName, WorldSupervisor, is_btrfs_path, is_subvolume_path, postgres,
    redis, seaweedfs, write_bundle,
};

const SECRET: &str = "slipfork-probe-secret-9d3f1c";
// SeaweedFS's volume gRPC default is 18080 inside the same world netns; never collide.
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
pathlib.Path('/project/probe/svc.ready').write_text('ready')
http.server.HTTPServer(('127.0.0.1', __SVC_PORT__), Handler).serve_forever()
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

def sign(key, msg):
    return hmac.new(key, msg.encode(), hashlib.sha256).digest()

def s3_call(method, path, body=b'', auth=True):
    url = 'http://127.0.0.1:8333' + path
    req = urllib.request.Request(url, data=body if method == 'PUT' else None, method=method)
    if auth:
        now = datetime.datetime.now(datetime.timezone.utc)
        amz = now.strftime('%Y%m%dT%H%M%SZ')
        date = now.strftime('%Y%m%d')
        payload_hash = hashlib.sha256(body if method == 'PUT' else b'').hexdigest()
        host = '127.0.0.1:8333'
        canon_headers = f'host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz}\n'
        signed_headers = 'host;x-amz-content-sha256;x-amz-date'
        canon_req = f'{method}\n{path}\n\n{canon_headers}\n{signed_headers}\n{payload_hash}'
        scope = f'{date}/us-east-1/s3/aws4_request'
        sts = f'AWS4-HMAC-SHA256\n{amz}\n{scope}\n' + hashlib.sha256(canon_req.encode()).hexdigest()
        k_signing = sign(sign(sign(sign(b'AWS4slipfork-probe-value', date), 'us-east-1'), 's3'), 'aws4_request')
        sig = hmac.new(k_signing, sts.encode(), hashlib.sha256).hexdigest()
        req.add_header('Host', host)
        req.add_header('x-amz-date', amz)
        req.add_header('x-amz-content-sha256', payload_hash)
        req.add_header('Authorization', f'AWS4-HMAC-SHA256 Credential=slipfork-probe-key/{scope}, SignedHeaders={signed_headers}, Signature={sig}')
    try:
        with urllib.request.urlopen(req, timeout=10) as response:
            return response.status, response.read().decode()
    except urllib.error.HTTPError as error:
        return error.code, ''

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
    while time.time() < deadline and not (connect(5432) and connect(6379) and connect(8333)):
        time.sleep(0.5)
    out['pg'] = connect(5432)
    out['redis'] = connect(6379)
    out['s3'] = connect(8333)
    if out['s3']:
        deadline = time.time() + 20
        while time.time() < deadline:
            if s3_call('GET', '/uploads/', auth=False)[0] == 403 and s3_call('GET', '/assets/', auth=False)[0] == 200:
                break
            time.sleep(0.2)
        out['put_uploads'] = s3_call('PUT', '/uploads/x', b'secret-obj', auth=True)[0]
        out['auth_get_uploads'] = list(s3_call('GET', '/uploads/x', auth=True))
        out['anon_get_uploads'] = s3_call('GET', '/uploads/x', auth=False)[0]
        out['put_assets'] = s3_call('PUT', '/assets/x', b'public-obj', auth=True)[0]
        out['anon_get_assets'] = list(s3_call('GET', '/assets/x', auth=False))

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
    s3: PathBuf,
    toolbox_facts: ImageFacts,
    pg_facts: ImageFacts,
    redis_facts: ImageFacts,
    s3_facts: ImageFacts,
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
    if let (Some(toolbox), Some(pg), Some(redis), Some(s3)) = (
        from_env("SLIPFORK_TEST_TOOLBOX_ROOTFS"),
        from_env("SLIPFORK_TEST_PG_ROOTFS"),
        from_env("SLIPFORK_TEST_REDIS_ROOTFS"),
        from_env("SLIPFORK_TEST_S3_ROOTFS"),
    ) {
        require_python(&toolbox);
        return Images {
            toolbox,
            pg,
            redis,
            s3,
            // Exported rootfs carry no config document; the pod still gets the default PATH.
            toolbox_facts: ImageFacts::default(),
            pg_facts: ImageFacts::default(),
            redis_facts: ImageFacts::default(),
            s3_facts: ImageFacts::default(),
            clones: Vec::new(),
        };
    }

    // No exported rootfs: pull and seal the four upstream images into a Btrfs cache beside the
    // run, then clone each sealed rootfs writable (the `PodSpec::rootfs` contract).
    let cache = scratch_root().join("images");
    let store = StoreLayout::new(&cache);
    let puller = OciPuller::new(
        Arc::new(BtrfsEngine::new(StoreLayout::new(&cache))),
        ImageLayout::new(store.images_root()),
        Credentials::Anonymous,
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
                "could not pull `{raw}`: {error}\nnext: this suite needs docker.io egress; connect the host and re-run, or export SLIPFORK_TEST_{{TOOLBOX,PG,REDIS,S3}}_ROOTFS"
            ))
        })
    };
    let sealed = [
        pull("python:3.13-slim"),
        pull(ResourceKind::Postgres.image()),
        pull(ResourceKind::Redis.image()),
        pull(ResourceKind::Seaweedfs.image()),
    ];
    let facts = |image: &slipfork_image::PulledImage| image.facts.clone();
    let [toolbox_sealed, pg_sealed, redis_sealed, s3_sealed] = sealed;
    let [toolbox_facts, pg_facts, redis_facts, s3_facts] = [
        facts(&toolbox_sealed),
        facts(&pg_sealed),
        facts(&redis_sealed),
        facts(&s3_sealed),
    ];
    let clone_root = run.join("images");
    fs::create_dir_all(&clone_root).expect("create image clone root");
    let mut clones = Vec::new();
    let images = Images {
        toolbox_facts,
        pg_facts,
        redis_facts,
        s3_facts,
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
        s3: clone_image(
            engine,
            &s3_sealed.rootfs,
            &clone_root.join("s3"),
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
    for child in ["pg", "redis", "s3"] {
        engine
            .create_subvolume(&data.join(child))
            .expect("create resource data subvolume");
    }
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

fn collect_logs(supervisor: &WorldSupervisor, service: &str) -> (Vec<String>, Vec<String>) {
    let mut lines = Vec::new();
    let mut dividers = Vec::new();
    let mut on_output = |event: slipfork_world::control::OutputEvent| match event.kind {
        OutputKind::Divider => dividers.push(event.line),
        OutputKind::Line => lines.push(event.line),
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
/// namespaces, so a stray `slipfork-world-init`/`postgres`/`weed` from an older lab run (with a
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

    let host = || WorldHost {
        crun: crun.clone(),
        unshare: unshare.clone(),
        world_init: init.clone(),
        runtime_dir: runtime.clone(),
        uid,
        gid,
        resolv_conf: Some(PathBuf::from("/etc/resolv.conf")),
        boot_timeout: Duration::from_secs(30),
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
    let run_once_root = run.join("images/run-once");
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
            "print('printed', flush=True)\nimport time\ntime.sleep(120)".to_owned(),
        ],
        inherited_env: images.toolbox_facts.env.clone(),
        env: Vec::new(),
        binds: vec![project_bind(&world_a.project)],
        files: Vec::new(),
    };
    supervisor_a
        .launch(&printer)
        .expect("launch the printer pod");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut printed = false;
    let mut dividers = Vec::new();
    while Instant::now() < deadline {
        let (lines, seen) = collect_logs(&supervisor_a, "m339print");
        dividers = seen;
        if lines.iter().any(|line| line == "printed") {
            printed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        printed,
        "the detached pod's stdout must land in <pods>/logs/m339print.log"
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

    let pg_spec = postgres(
        "m339pg",
        &images.pg,
        &world_a.data.join("pg"),
        &images.pg_facts,
    );
    assert_eq!(
        pg_spec.argv,
        ["docker-entrypoint.sh", "postgres"],
        "postgres:18's own entrypoint/cmd must drive the pod"
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
    supervisor_a
        .launch(&redis(
            "m339cache",
            &images.redis,
            &world_a.data.join("redis"),
            &images.redis_facts,
        ))
        .expect("launch redis");
    let plan = SeaweedfsPlan::render(
        &S3Credentials::new("slipfork-probe-key", "slipfork-probe-value"),
        &[
            S3Bucket::new("uploads", S3Policy::Private).expect("valid bucket"),
            S3Bucket::new("assets", S3Policy::PublicRead).expect("valid bucket"),
        ],
    )
    .expect("render seaweedfs plan");
    supervisor_a
        .launch(&seaweedfs(
            "m339s3",
            &images.s3,
            &world_a.data.join("s3"),
            &plan,
            &images.s3_facts,
        ))
        .expect("launch seaweedfs");

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
    assert_eq!(a["s3"].as_bool(), Some(true), "seaweedfs must accept :8333");

    // 5. Real multi-bucket S3 roundtrips: `uploads` private (403 anon, 200 auth), `assets` cdn (200 anon),
    // and zero topology/volume-growth failures in the SeaweedFS boot log.
    assert_eq!(a["put_uploads"], 200, "PUT /uploads/x must return 200: {a}");
    assert_eq!(
        a["auth_get_uploads"],
        serde_json::json!([200, "secret-obj"]),
        "authenticated GET /uploads/x must return 200 and payload: {a}"
    );
    assert_eq!(
        a["anon_get_uploads"], 403,
        "anonymous GET /uploads/x must return 403: {a}"
    );
    assert_eq!(a["put_assets"], 200, "PUT /assets/x must return 200: {a}");
    assert_eq!(
        a["anon_get_assets"],
        serde_json::json!([200, "public-obj"]),
        "anonymous GET /assets/x must return 200 and payload: {a}"
    );
    let (s3_lines, _) = collect_logs(&supervisor_a, "m339s3");
    let s3_log = s3_lines.join("\n");
    for forbidden in [
        "topo failed to pick",
        "Not enough data nodes",
        "create 7 volume, created 0",
    ] {
        assert!(
            !s3_log.contains(forbidden),
            "SeaweedFS boot log must not contain `{forbidden}`:\n{s3_log}"
        );
    }

    // 6. The OCI env carried the secret while both clients stayed alive; argv never carries it.
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
    for id in ["m339svc", "m339pg", "m339cache", "m339s3"] {
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
        b"weed".as_slice(),
    ] {
        let survivors = processes_mentioning(needle, &run, &world_pid_namespaces);
        assert!(
            survivors.is_empty(),
            "`{}` survived teardown in {survivors:?}",
            String::from_utf8_lossy(needle)
        );
    }

    for world in [&world_a, &world_b] {
        for child in ["pg", "redis", "s3"] {
            engine
                .destroy(
                    &world.data.join(child),
                    slipfork_world::Dirtiness::Clean,
                    false,
                )
                .expect("destroy data subvolume");
        }
        engine
            .destroy(&world.data, slipfork_world::Dirtiness::Clean, false)
            .expect("destroy world subvolume");
        engine
            .destroy(&world.project, slipfork_world::Dirtiness::Clean, false)
            .expect("destroy project subvolume");
    }
    for clone in &images.clones {
        engine
            .destroy(clone, slipfork_world::Dirtiness::Clean, false)
            .expect("destroy image rootfs clone");
    }
    if !images.clones.is_empty() {
        fs::remove_dir_all(run.join("images")).expect("remove image clone root");
    }
    let leftovers = subvolumes_under(&run);
    assert!(leftovers.is_empty(), "leaked subvolumes: {leftovers:?}");
    fs::remove_dir_all(&run).expect("remove scratch run");
}
