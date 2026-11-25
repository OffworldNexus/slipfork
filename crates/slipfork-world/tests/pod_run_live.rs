//! Live E2E of the service-pod run identity: the image's `USER` (default container root) rendered
//! through the same single-owner userns mapping and Docker capability set as build steps.
//!
//! One cohesive journey, three pods against a real Btrfs store: (a) container root keeps a
//! `0 → euid` mapping, the 14 Docker caps, `HOME=/root`, and a live `crun` pod's file under a bind
//! mount stays owned by the invoking host uid/gid; (b) a numeric `USER 1000:1000` with a passwd
//! entry renders `{1000,1000}`, an empty cap set, and the image account's `HOME`/`USER`/`LOGNAME`;
//! (c) a `USER` with no passwd entry fails closed with one line plus a `next:` remedy. The live
//! journey also asserts Docker-parity creation modes (`0755`/`0644`) under a hostile inherited
//! `0002` umask, so a pod can never inherit a desktop daemon's group-writable defaults.
//!
//! The suite fails closed on a non-Btrfs host or a missing `crun`/user namespace: a skipped live
//! identity check hides a pod that runs as the wrong uid forever.

#![cfg(feature = "live")]

use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slipfork_world::identity::PROCESS_UMASK;
use slipfork_world::{
    Bind, BindSource, BtrfsEngine, Identity, IdentityError, PodEnv, PodHost, PodSpec, StoreLayout,
    WorldEngine, WorldHost, WorldName, WorldSupervisor, WorldTools, is_btrfs_path, write_bundle,
};

const MOUNT_REMEDY: &str = "mount -o user_subvol_rm_allowed,noatime <device> <mount>";

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
    let run = scratch.join(format!("pod-run-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&run).expect("create run root");
    run
}

fn require_tool(name: &str) -> PathBuf {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| {
            fail_closed(&format!(
                "`{name}` is not on PATH.\nnext: install {name} (crun, util-linux, slirp4netns) and re-run"
            ))
        })
}

fn tmpfs_dir(tag: &str) -> PathBuf {
    let dir = PathBuf::from("/dev/shm").join(format!(
        "slipfork-podrun-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos())
    ));
    fs::create_dir_all(&dir).expect("create the tmpfs runtime root");
    let file = fs::File::open(&dir).expect("open the runtime root");
    // SAFETY: the descriptor is live and `statfs` is plain-old-data.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: the descriptor is live and the kernel writes exactly one `statfs`.
    let ret = unsafe { libc::fstatfs(file.as_raw_fd(), &mut stat) };
    assert_eq!(ret, 0, "fstatfs the runtime root");
    assert_eq!(
        stat.f_type as u64, 0x0102_1994,
        "the runtime root must be tmpfs"
    );
    dir
}

/// Copies one host binary and every shared object `ldd` reports into `rootfs`.
fn copy_bin(rootfs: &Path, binary: &str) {
    let destination = rootfs.join(binary.trim_start_matches('/'));
    fs::create_dir_all(destination.parent().expect("binary parent")).expect("create bin dir");
    fs::copy(binary, &destination).unwrap_or_else(|error| panic!("copy `{binary}`: {error}"));
    let output = std::process::Command::new("ldd")
        .arg(binary)
        .output()
        .unwrap_or_else(|error| panic!("ldd `{binary}`: {error}"));
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        let mut library = fields.next().unwrap_or_default();
        if library == "linux-vdso.so.1" {
            continue;
        }
        if !library.starts_with('/')
            && let Some(next) = fields.next()
            && next == "=>"
        {
            library = fields.next().unwrap_or_default();
        }
        if !library.starts_with('/') {
            continue;
        }
        let target = rootfs.join(library.trim_start_matches('/'));
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).expect("create library dir");
        }
        fs::copy(library, &target).unwrap_or_else(|error| panic!("copy `{library}`: {error}"));
    }
}

/// Copies `sh`, `touch`, `mkdir`, `id`, and `sleep` into `rootfs` and names the in-image `touch`
/// path.
fn assemble_rootfs(rootfs: &Path) -> String {
    for dir in ["bin", "usr/bin", "etc", "tmp", "dev", "proc", "sys", "root"] {
        fs::create_dir_all(rootfs.join(dir)).expect("create rootfs dir");
    }
    copy_bin(rootfs, "/bin/sh");
    let mut touch = None;
    for name in ["touch", "mkdir", "id", "sleep", "grep"] {
        let binary = [format!("/usr/bin/{name}"), format!("/bin/{name}")]
            .into_iter()
            .find(|candidate| Path::new(candidate).is_file())
            .unwrap_or_else(|| panic!("the lab needs a real `{name}` binary"));
        copy_bin(rootfs, &binary);
        if name == "touch" {
            touch = Some(binary);
        }
    }
    touch.expect("touch was copied")
}

/// Runs one command through the same control socket `slipfork exec` uses and returns stdout.
fn pod_exec(pods_root: &Path, pod: &str, argv: &[&str]) -> String {
    let mut output = String::new();
    let status = slipfork_world::control::exec(
        pods_root,
        pod,
        &argv.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
        &mut |event| {
            output.push_str(&event.line);
            output.push('\n');
        },
    )
    .unwrap_or_else(|error| panic!("exec `{}` in `{pod}`: {error}", argv.join(" ")));
    assert_eq!(status, 0, "exec `{}` failed", argv.join(" "));
    output
}

fn env_value(env: &[String], key: &str) -> Option<String> {
    env.iter()
        .find_map(|pair| pair.strip_prefix(&format!("{key}=")))
        .map(str::to_owned)
}

fn read_config(bundle: &slipfork_world::PodBundle) -> serde_json::Value {
    serde_json::from_slice(&fs::read(&bundle.config).expect("read config.json"))
        .expect("parse config.json")
}

fn config_env(json: &serde_json::Value) -> Vec<String> {
    json["process"]["env"]
        .as_array()
        .expect("process.env")
        .iter()
        .map(|value| value.as_str().expect("env string").to_owned())
        .collect()
}

fn caps(json: &serde_json::Value, set: &str) -> usize {
    json["process"]["capabilities"][set]
        .as_array()
        .unwrap_or_else(|| panic!("capabilities.{set}"))
        .len()
}

fn spec(id: &str, rootfs: &Path, argv: &[&str], identity: Identity) -> PodSpec {
    PodSpec {
        id: id.to_owned(),
        rootfs: rootfs.to_path_buf(),
        workdir: "/".to_owned(),
        argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
        inherited_env: vec!["PATH=/bin:/usr/bin".to_owned()],
        env: Vec::new(),
        binds: Vec::new(),
        files: Vec::new(),
        identity: Some(identity),
    }
}

fn wait_for(path: &Path, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    path.exists()
}

/// Installs a hostile inherited umask for the lifetime of the guard and restores the previous value
/// on drop, so a panic mid-journey never leaks the poisoned umask into the rest of the binary.
struct HostileUmask {
    previous: libc::mode_t,
}

impl HostileUmask {
    fn install(value: libc::mode_t) -> Self {
        // SAFETY: `umask` is process-wide state; this guard is the suite's only installer and puts
        // the captured value back on drop.
        let previous = unsafe { libc::umask(value) };
        Self { previous }
    }
}

impl Drop for HostileUmask {
    fn drop(&mut self) {
        // SAFETY: restore exactly the value captured by `install`.
        unsafe { libc::umask(self.previous) };
    }
}

#[test]
fn pod_identity_maps_the_image_user_and_a_live_root_pod_writes_host_owned_files() {
    let run = live_run_root();
    let layout = StoreLayout::new(&run);
    let engine = BtrfsEngine::new(StoreLayout::new(&run));
    engine.ensure_store().expect("create .slipfork-store");
    let world = WorldName::parse("podrun").expect("valid world");
    engine.create_world(&world).expect("create the world store");

    // SAFETY: geteuid/getegid are pure identity reads.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    let crun = require_tool("crun");
    let host = PodHost {
        crun,
        resolv_conf: None,
        uid,
        gid,
    };

    // (a) An image identity of container root: real Btrfs subvolume rootfs, real bundle.
    let rootfs_a = layout.world_pods(&world).join("svc-root");
    fs::create_dir_all(rootfs_a.parent().expect("pod rootfs parent")).expect("create the pods dir");
    engine
        .create_subvolume(&rootfs_a)
        .expect("create the pod rootfs subvolume");
    let touch = assemble_rootfs(&rootfs_a);
    let project = run.join("project-a");
    fs::create_dir_all(&project).expect("create the bound project dir");
    // The pod's *own init* creates the entries under the bind mount, so the assertions below prove
    // the container init honoured `process.umask`, not merely a later `crun exec`.
    let mut root_spec = spec(
        "svc-root",
        &rootfs_a,
        &[
            "/bin/sh",
            "-c",
            "mkdir /project/sub && touch /project/sub/file && exec /usr/bin/sleep 120",
        ],
        Identity::ROOT,
    );
    root_spec.binds = vec![Bind {
        source: BindSource::Host(project.clone()),
        destination: "/project".to_owned(),
        read_only: false,
    }];
    let root_bundle = write_bundle(&root_spec, &host, &run.join("bundle-root"))
        .expect("write the container-root bundle");
    let json = read_config(&root_bundle);
    assert_eq!(json["process"]["user"]["uid"], 0);
    assert_eq!(json["process"]["user"]["gid"], 0);
    // Explicit Docker-parity process state, never an `env` entry.
    assert_eq!(
        json["process"]["umask"].as_u64(),
        Some(u64::from(PROCESS_UMASK)),
        "every pod renders OCI `process.umask` explicitly"
    );
    assert!(
        !config_env(&json)
            .iter()
            .any(|entry| entry.starts_with("UMASK=")),
        "the umask is process state, not an environment variable"
    );
    assert_eq!(json["linux"]["uidMappings"][0]["containerID"], 0);
    assert_eq!(json["linux"]["uidMappings"][0]["hostID"], uid);
    assert_eq!(json["linux"]["uidMappings"][0]["size"], 1);
    assert_eq!(json["linux"]["gidMappings"][0]["containerID"], 0);
    assert_eq!(json["linux"]["gidMappings"][0]["hostID"], gid);
    for set in ["bounding", "effective", "permitted"] {
        assert_eq!(
            caps(&json, set),
            14,
            "container root gets Docker's default caps on {set}"
        );
    }
    assert!(
        !root_bundle.dir.join("passwd").exists(),
        "an image root identity keeps the image's own account database"
    );
    let env = config_env(&json);
    assert_eq!(env_value(&env, "HOME").as_deref(), Some("/root"));
    assert_eq!(env_value(&env, "USER").as_deref(), Some("root"));

    // Live: the pod's container root is the invoking host user, so its file stays host-owned.
    let runtime = tmpfs_dir("rt");
    let world_host = WorldHost::new(
        WorldTools {
            crun: require_tool("crun"),
            unshare: require_tool("unshare"),
            slirp4netns: require_tool("slirp4netns"),
            world_init: PathBuf::from(env!("CARGO_BIN_EXE_slipfork-world-init")),
        },
        runtime.clone(),
        layout.logs_root(),
        uid,
        gid,
        None,
    );
    // A desktop daemon commonly runs with `0002`, and world-init hands its umask down to the
    // `crun run` it spawns. Install the hostile value *before* the world boots so the container's
    // own init inherits it; only the rendered OCI `process.umask` (0022) can keep the pod's
    // creations at Docker's 0755/0644.
    let _umask = HostileUmask::install(0o002);
    let supervisor = WorldSupervisor::boot(world_host, &world).expect("boot a real world");
    let pods_dir = supervisor.pods_dir().to_path_buf();
    write_bundle(
        &root_spec,
        &supervisor.pod_host(),
        &pods_dir.join("svc-root"),
    )
    .expect("write the live OCI bundle");
    slipfork_world::launch_pod(&pods_dir, &root_spec, Duration::from_secs(60))
        .expect("launch the container-root pod");

    // The `slipfork exec` path: the pod's own `config.json` process spec is reused verbatim, so
    // `id` must report container root and the effective set must be Docker's default caps.
    let id = pod_exec(&pods_dir, "svc-root", &["/usr/bin/id"]);
    assert!(
        id.contains("uid=0 gid=0"),
        "exec inside the pod must be container root: {id}"
    );
    let status = pod_exec(
        &pods_dir,
        "svc-root",
        &["/bin/sh", "-c", "grep '^CapEff:' /proc/self/status"],
    );
    assert!(
        status.contains("00000000a80425fb"),
        "container root must hold Docker's 14 default caps (CapEff 0xa80425fb): {status}"
    );
    pod_exec(&pods_dir, "svc-root", &[touch.as_str(), "/project/created"]);
    let created = project.join("created");
    assert!(
        wait_for(&created, Duration::from_secs(30)),
        "the live pod never created `{}`.\nnext: check `crun`/user-namespace support and the world log",
        created.display()
    );
    let meta = fs::metadata(&created).expect("stat the pod-created file");
    assert_eq!(
        (meta.uid(), meta.gid()),
        (uid, gid),
        "a file created by container root in a bind mount must be owned by the invoking user"
    );

    // Same hostile inherited umask, same determinism: `mkdir`/`touch` ran in the container's init
    // argv, so the modes come out 0755/0644 like Docker, never the inherited 0775/0664 that makes
    // `uv`'s `.venv` group-writable.
    let sub = project.join("sub");
    let sub_file = sub.join("file");
    assert!(
        wait_for(&sub_file, Duration::from_secs(30)),
        "the live pod's init argv never created `{}`.\nnext: check `crun`/user-namespace support and the world log",
        sub_file.display()
    );
    let sub_meta = fs::metadata(&sub).expect("stat the pod-created directory");
    assert_eq!(
        sub_meta.mode() & 0o777,
        0o755,
        "a pod-created directory must be 0755 despite the daemon's inherited 0002 umask"
    );
    let file_meta = fs::metadata(&sub_file).expect("stat the pod-created file mode");
    assert_eq!(
        file_meta.mode() & 0o777,
        0o644,
        "a pod-created file must be 0644 despite the daemon's inherited 0002 umask"
    );
    assert_eq!(
        (sub_meta.uid(), sub_meta.gid()),
        (uid, gid),
        "the deterministic modes do not change the host uid/gid mapping"
    );

    // (b) `USER 1000:1000` resolved against an image that ships that passwd entry.
    let rootfs_b = layout.world_pods(&world).join("svc-app");
    fs::create_dir_all(rootfs_b.parent().expect("pod rootfs parent")).expect("create the pods dir");
    engine
        .create_subvolume(&rootfs_b)
        .expect("create the app rootfs subvolume");
    fs::create_dir_all(rootfs_b.join("etc")).expect("seed the image etc");
    fs::write(
        rootfs_b.join("etc/passwd"),
        "root:x:0:0:root:/root:/bin/sh\napp:x:1000:1000:App:/home/app:/bin/sh\n",
    )
    .expect("write the image passwd");
    fs::write(rootfs_b.join("etc/group"), "root:x:0:\napp:x:1000:\n")
        .expect("write the image group");
    assemble_rootfs(&rootfs_b);
    let mut app_spec = spec(
        "svc-app",
        &rootfs_b,
        &["/bin/sh", "-c", "exec /usr/bin/sleep 120"],
        Identity::ROOT,
    );
    app_spec.identity = Some(
        Identity::resolve(&rootfs_b, Some("1000:1000"))
            .expect("a numeric USER resolves as written"),
    );
    assert_eq!(
        app_spec.identity,
        Some(Identity {
            uid: 1000,
            gid: 1000
        })
    );
    let app_bundle =
        write_bundle(&app_spec, &host, &run.join("bundle-app")).expect("write the app bundle");
    let json = read_config(&app_bundle);
    assert_eq!(json["process"]["user"]["uid"], 1000);
    assert_eq!(json["process"]["user"]["gid"], 1000);
    assert_eq!(json["linux"]["uidMappings"][0]["containerID"], 1000);
    assert_eq!(json["linux"]["uidMappings"][0]["hostID"], uid);
    assert_eq!(
        caps(&json, "bounding") + caps(&json, "effective") + caps(&json, "permitted"),
        0,
        "a non-root USER drops Docker's default caps"
    );
    let env = config_env(&json);
    assert_eq!(env_value(&env, "HOME").as_deref(), Some("/home/app"));
    assert_eq!(env_value(&env, "USER").as_deref(), Some("app"));
    assert_eq!(env_value(&env, "LOGNAME").as_deref(), Some("app"));
    assert!(
        !app_bundle.dir.join("passwd").exists(),
        "a named image account keeps the image's own /etc/passwd"
    );
    assert!(
        rootfs_b.join("home/app").is_dir(),
        "the absent account home is materialised for the image identity"
    );

    // Live: the image's `USER 1000:1000` boots as container 1000 with no capabilities, through the
    // same single-owner mapping that keeps its bind-mount writes host-owned.
    write_bundle(&app_spec, &supervisor.pod_host(), &pods_dir.join("svc-app"))
        .expect("write the live app bundle");
    slipfork_world::launch_pod(&pods_dir, &app_spec, Duration::from_secs(60))
        .expect("launch the USER 1000:1000 pod");
    let app_id = pod_exec(&pods_dir, "svc-app", &["/usr/bin/id"]);
    assert!(
        app_id.contains("uid=1000") && app_id.contains("gid=1000"),
        "the image's non-root USER must run as 1000 inside: {app_id}"
    );
    let app_status = pod_exec(
        &pods_dir,
        "svc-app",
        &["/bin/sh", "-c", "grep '^CapEff:' /proc/self/status"],
    );
    assert!(
        app_status.contains("0000000000000000"),
        "a non-root USER must hold zero capabilities: {app_status}"
    );

    // Environment: the pod's own declared entries still win over the account-derived login vars.
    let mut declared = spec("svc-env", &rootfs_b, &["/bin/true"], Identity::ROOT);
    declared.identity = Some(Identity {
        uid: 1000,
        gid: 1000,
    });
    declared.env = vec![PodEnv::new("HOME", "/opt/app"), PodEnv::new("USER", "svc")];
    let declared_bundle = write_bundle(&declared, &host, &run.join("bundle-env"))
        .expect("write the declared-env bundle");
    let env = config_env(&read_config(&declared_bundle));
    assert_eq!(env_value(&env, "HOME").as_deref(), Some("/opt/app"));
    assert_eq!(env_value(&env, "USER").as_deref(), Some("svc"));

    // (c) A `USER` with no passwd entry fails closed, naming the account and its remedy.
    let error = Identity::resolve(&rootfs_b, Some("ghost")).expect_err("no such account");
    assert!(
        matches!(&error, IdentityError::UnknownAccount { file: "passwd", .. }),
        "the missing passwd entry is typed: {error}"
    );
    let rendered = error.to_string();
    assert_eq!(
        rendered.lines().count(),
        2,
        "one cause line plus one `next:` line: {rendered}"
    );
    assert!(rendered.contains("`ghost`"), "{rendered}");
    assert!(
        rendered
            .lines()
            .nth(1)
            .is_some_and(|line| line.starts_with("next: "))
    );
    assert!(
        !rendered.contains(&uid.to_string()),
        "an unknown USER must never fall back to the host uid: {rendered}"
    );

    drop(supervisor);
    // The world's pod rootfs subvolumes are Btrfs, so `remove_dir_all` alone would leak them.
    engine.drop_world(&world).expect("drop the lab world");
    let _ = fs::remove_dir_all(&runtime);
    let _ = fs::remove_dir_all(&run);
}
