//! Protected live E2E journey for world namespaces, `crun` pods, resources, and reap.
//!
//! Everything runs on real Btrfs subvolumes, a real tmpfs runtime directory, real `unshare`,
//! real `crun` pods, and a real SeaweedFS S3 policy. On a host without Btrfs, `crun`, unprivileged
//! user namespaces, or a tmpfs `$XDG_RUNTIME_DIR` this suite FAILS CLOSED with one remedy line
//! instead of skipping. When `SLIPFORK_TEST_{TOOLBOX,PG,REDIS,S3}_ROOTFS` are unset it pulls and
//! seals `python:3.13-slim`, `postgres:18`, `redis:7`, and SeaweedFS into the test store itself.

use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slipfork_image::{Architecture, Credentials, ImageLayout, ImageReference, OciPuller};
use slipfork_world::{
    Bind, BindSource, BtrfsEngine, PodEnv, PodSpec, ResourceKind, S3Bucket, S3Credentials,
    S3Policy, SeaweedfsPlan, StoreLayout, WorldEngine, WorldHost, WorldName, WorldSupervisor,
    is_btrfs_path, is_subvolume_path, postgres, redis, seaweedfs,
};

const SECRET: &str = "slipfork-probe-secret-9d3f1c";
const SVC_PORT: u16 = 18_080;
const MOUNT_REMEDY: &str = "mount -o user_subvol_rm_allowed,noatime <device> <mount>";
const POD_IDS: [&str; 5] = ["m339svc", "m339clienta", "m339pg", "m339cache", "m339s3"];

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
http.server.HTTPServer(('127.0.0.1', 18080), Handler).serve_forever()
"#;

const CLIENT_PY: &str = r#"
import ctypes, json, os, pathlib, socket, sys, time, urllib.request, urllib.error

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

out['svc'] = connect(18080, 5.0)
if role == 'a':
    deadline = time.time() + 90
    while time.time() < deadline and not (connect(5432) and connect(6379) and connect(8333)):
        time.sleep(0.5)
    out['pg'] = connect(5432)
    out['redis'] = connect(6379)
    out['s3'] = connect(8333)
    if out['s3']:
        try:
            request = urllib.request.Request(
                'http://127.0.0.1:8888/buckets/cdn/hello.txt', data=b'hello', method='PUT')
            urllib.request.urlopen(request, timeout=10).read()
            out['upload'] = 'ok'
        except Exception as error:
            out['upload'] = repr(error)
        for bucket in ('private', 'cdn'):
            try:
                response = urllib.request.urlopen(
                    'http://127.0.0.1:8333/%s/hello.txt' % bucket, timeout=10)
                out[bucket] = response.status
            except urllib.error.HTTPError as error:
                out[bucket] = error.code
            except Exception as error:
                out[bucket] = repr(error)

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
    let [toolbox_sealed, pg_sealed, redis_sealed, s3_sealed] = sealed;
    let clone_root = run.join("images");
    fs::create_dir_all(&clone_root).expect("create image clone root");
    let mut clones = Vec::new();
    let images = Images {
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
fn crun_list(crun: &Path) -> String {
    let output = Command::new(crun)
        .arg("list")
        .output()
        .expect("fail closed: could not execute crun list");
    assert!(
        output.status.success(),
        "fail closed: `crun list` exited with {}\nnext: confirm `crun --version` runs and `$XDG_RUNTIME_DIR` is the same tmpfs for launch and list; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

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
            CLIENT_PY.to_owned(),
            role.to_owned(),
            result.to_owned(),
        ],
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

    // 2. One in-namespace loopback server plus the real backing resources.
    let server = PodSpec {
        id: "m339svc".to_owned(),
        rootfs: images.toolbox.clone(),
        workdir: "/project".to_owned(),
        argv: vec!["python3".to_owned(), "-c".to_owned(), SERVER_PY.to_owned()],
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

    supervisor_a
        .launch(&postgres("m339pg", &images.pg, &world_a.data.join("pg")))
        .expect("launch postgres");
    supervisor_a
        .launch(&redis(
            "m339cache",
            &images.redis,
            &world_a.data.join("redis"),
        ))
        .expect("launch redis");
    let plan = SeaweedfsPlan::render(
        &S3Credentials::new("slipfork-probe-key", "slipfork-probe-value"),
        &[
            S3Bucket::new("private", S3Policy::Private).expect("valid bucket"),
            S3Bucket::new("cdn", S3Policy::PublicRead).expect("valid bucket"),
        ],
    )
    .expect("render seaweedfs plan");
    supervisor_a
        .launch(&seaweedfs(
            "m339s3",
            &images.s3,
            &world_a.data.join("s3"),
            &plan,
        ))
        .expect("launch seaweedfs");

    // 3. Identity + loopback + cross-world isolation + System V IPC isolation.
    supervisor_a
        .launch(&client_pod(
            &images.toolbox,
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

    // 5. S3 policy: private -> 403, cdn -> public-read 200.
    assert_eq!(
        a["upload"].as_str(),
        Some("ok"),
        "filer upload must succeed"
    );
    assert_eq!(a["private"], 403, "private bucket denies anonymous GET");
    assert_eq!(a["cdn"], 200, "cdn bucket is public-read");

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
    let leaked = processes_mentioning(SECRET.as_bytes());
    assert!(leaked.is_empty(), "secret leaked into argv of {leaked:?}");
    fs::write(world_a.project.join("probe/a.release"), b"go").expect("release world a client");
    fs::write(world_b.project.join("probe/b.release"), b"go").expect("release world b client");

    // 7. `crun` registers all five pods under the same store the launcher used.
    let listed = crun_list(&crun);
    for id in POD_IDS {
        assert!(
            listed.contains(id),
            "container `{id}` must be listed before teardown; `crun list` said:\n{listed}"
        );
    }

    // 8. Clean teardown: zero pods and zero leaked processes, not just the init.
    supervisor_a.stop().expect("stop world a");
    supervisor_b.stop().expect("stop world b");
    let listed = crun_list(&crun);
    for id in POD_IDS {
        assert!(!listed.contains(id), "container `{id}` survived teardown");
    }
    for needle in [
        b"slipfork-world-init".as_slice(),
        b"postgres".as_slice(),
        b"redis-server".as_slice(),
        b"weed".as_slice(),
    ] {
        let survivors = processes_mentioning(needle);
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
