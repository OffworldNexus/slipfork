//! Protected live journey for `slipfork fork` and `slipfork drop`.
//!
//! A real Btrfs scratch project (`SLIPFORK_TEST_ROOT`), a real daemon, and a real `crun` world
//! running a full stack (image + service, PostgreSQL, Redis, and native S3 behind a CDN): `fork`
//! must snapshot the project and every resource clone, reuse the shared image cache with zero
//! re-pull, re-run zero hooks on a matching `world_epoch`, switch the checkout's branch, and prove
//! that live PG rows, Redis keys, and S3 objects survive CoW while later parent writes stay
//! isolated. Breaking the paired epoch must force both hooks to run again. Every missing host
//! capability FAILS CLOSED.

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

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use slipfork_ingress::IngressHandoff;
use slipfork_world::{
    BtrfsEngine, Dirtiness, ForkName, ResourceKind, STORE_DIR, StoreLayout, WorldEngine, WorldName,
    is_subvolume_path,
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
postgres pg { database appdb }
redis cache
s3 assets {
  policy public-read
  cdn assets-cdn
}
image app . {
  before-start: mkdir -p /project/.slipfork && echo before-start >> /project/.slipfork/hooks.log
  pre-deploy: mkdir -p /project/.slipfork && echo pre-deploy >> /project/.slipfork/hooks.log
  service api {
    port 8000
    route /
    run: python3 -m http.server 8000
  }
}
"#;

const S3_ACCESS: &str = "slipfork";
const S3_SECRET: &str = "slipfork-demo-s3";
const S3_REGION: &str = "us-east-1";
const S3_DATE: &str = "20260101";
const S3_AMZ_DATE: &str = "20260101T000000Z";

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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC key");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Signs one request exactly like an AWS SDK: header auth over the given payload digest.
fn signed_headers(
    method: &str,
    path: &str,
    host: &str,
    payload: &str,
) -> Vec<(&'static str, String)> {
    let headers =
        format!("host:{host}\nx-amz-content-sha256:{payload}\nx-amz-date:{S3_AMZ_DATE}\n");
    let signed = "host;x-amz-content-sha256;x-amz-date";
    let canonical = format!("{method}\n{path}\n\n{headers}\n{signed}\n{payload}");
    let scope = format!("{S3_DATE}/{S3_REGION}/s3/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{S3_AMZ_DATE}\n{scope}\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let key = hmac(
        &hmac(
            &hmac(
                &hmac(format!("AWS4{S3_SECRET}").as_bytes(), S3_DATE),
                S3_REGION,
            ),
            "s3",
        ),
        "aws4_request",
    );
    let signature = hex(&hmac(&key, &to_sign));
    vec![
        ("x-amz-date", S3_AMZ_DATE.to_owned()),
        ("x-amz-content-sha256", payload.to_owned()),
        (
            "Authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={S3_ACCESS}/{scope}, SignedHeaders={signed}, Signature={signature}"
            ),
        ),
    ]
}

/// Asserts a `200` plus the expected body text from one ingress exchange.
fn assert_object(response: &str, body: &str) {
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "expected `200 OK` carrying `{body}`: {response}"
    );
    assert!(response.contains(body), "expected `{body}`: {response}");
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
        self.run_in(&self.project, args)
    }

    fn run_in(&self, directory: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_slipfork"))
            .args(args)
            .current_dir(directory)
            .env("NO_COLOR", "1")
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("TERM", "dumb")
            .output()
            .expect("run slipfork")
    }

    /// Runs one command inside a live pod through the real `slipfork exec` control path.
    fn exec(&self, directory: &Path, target: &str, argv: &[&str]) -> String {
        let mut args = vec!["exec", target, "--"];
        args.extend_from_slice(argv);
        let output = self.run_in(directory, &args);
        assert!(
            output.status.success(),
            "exec {target} {argv:?} failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// Hands one accepted `:80` descriptor to the daemon and returns the raw response.
    fn ingress(&self, request: &str) -> String {
        let listener =
            TcpListener::bind(("127.0.0.1", 0)).expect("bind the ingress probe listener");
        let mut client = TcpStream::connect(listener.local_addr().expect("probe address"))
            .expect("connect the ingress probe");
        let (accepted, _) = listener.accept().expect("accept the probe connection");
        let daemon = UnixStream::connect(self.runtime.join("daemon.sock")).expect("daemon.sock");
        IngressHandoff::new(1)
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

    fn get(&self, host: &str, path: &str) -> String {
        self.ingress(&format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
        ))
    }

    /// A real SigV4 `PUT`; `key_path` is the bucket-qualified target the S3 verifier sees after
    /// the CDN rewrite, `send_path` is the bare object path the CDN authority receives.
    fn signed_put(&self, host: &str, key_path: &str, send_path: &str, body: &str) -> String {
        let payload = hex(&Sha256::digest(body.as_bytes()));
        let headers = signed_headers("PUT", key_path, host, &payload);
        let mut request = format!(
            "PUT {send_path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(body);
        self.ingress(&request)
    }

    /// Lines in the ledger a `before-start` or `pre-deploy` hook appended.
    fn hook_lines(&self, project: &Path) -> usize {
        fs::read_to_string(project.join(".slipfork/hooks.log"))
            .map(|text| text.lines().count())
            .unwrap_or(0)
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
    let parent_hooks = lab.hook_lines(&lab.project);
    assert_eq!(
        parent_hooks, 2,
        "`up` must run the declared `before-start` and `pre-deploy` hooks exactly once"
    );

    // Seed the full stack through the real CLI/control plane before forking.
    lab.exec(
        &lab.project,
        "pg",
        &[
            "psql",
            "-U",
            "postgres",
            "-d",
            "postgres",
            "-tAc",
            "CREATE TABLE seed (id int PRIMARY KEY, note text); INSERT INTO seed VALUES (1, 'parent')",
        ],
    );
    assert_eq!(
        lab.exec(
            &lab.project,
            "cache",
            &["redis-cli", "SET", "cache:key", "parent-value"]
        ),
        "OK",
        "the parent redis must accept the seeded key"
    );
    let seeded = lab.signed_put(
        "assets-cdn.demo.localhost",
        "/assets/seed.txt",
        "/seed.txt",
        "parent object",
    );
    assert!(
        seeded.starts_with("HTTP/1.1 200"),
        "seeding the bucket must succeed: {seeded}"
    );

    // Wall clock starts at the `Command::spawn` boundary; the daemon's own `elapsed_ms` is only
    // secondary evidence because the ceiling is `spawn` -> the fork's first `200 OK` (`specs/03` §3).
    let started = std::time::Instant::now();
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
    assert_eq!(reply["world"], "feat-auth--demo", "{stdout}");
    assert_eq!(reply["url"], "http://feat-auth--demo.localhost", "{stdout}");
    assert!(reply["elapsed_ms"].as_u64().unwrap_or(0) > 0, "{stdout}");
    assert!(
        reply["snapshot_ms"].as_u64().unwrap_or(u64::MAX) < 150,
        "the CoW snapshot batch must stay under 150ms (five concurrent `SNAP_CREATE_V2` ioctls \
         share one transaction commit; the kernel floor on this network-attached volume is \
         ~50ms, while a byte copy of the same tree costs seconds): {stdout}"
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
    // The fork snapshot carries the parent's `world_epoch` checkpoint, so stamped work is reused.
    let parent_stamp = lab
        .store_root
        .join("worlds/demo/.slipfork-world-stamp.json");
    let fork_stamp = fork_world.join(".slipfork-world-stamp.json");
    assert!(
        parent_stamp.is_file() && fork_stamp.is_file(),
        "both the parent and the fork must carry `.slipfork-world-stamp.json`"
    );
    let parent_epoch: serde_json::Value =
        serde_json::from_slice(&fs::read(&parent_stamp).expect("read parent stamp"))
            .expect("parent stamp JSON");
    let fork_epoch: serde_json::Value =
        serde_json::from_slice(&fs::read(&fork_stamp).expect("read fork stamp"))
            .expect("fork stamp JSON");
    assert_eq!(
        parent_epoch["world_epoch"], fork_epoch["world_epoch"],
        "a fork must reuse the parent's epoch, so hooks stay at 0ms"
    );
    assert!(
        parent_epoch["before_start"]
            .as_object()
            .is_some_and(|map| !map.is_empty()),
        "world stamp must persist `before_start` hook digests: {parent_epoch}"
    );
    assert!(
        parent_epoch["pre_deploy"]
            .as_object()
            .is_some_and(|map| !map.is_empty()),
        "world stamp must persist `pre_deploy` hook digests: {parent_epoch}"
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

    // A matched epoch means zero hook re-runs.
    assert_eq!(
        lab.hook_lines(&fork_project),
        parent_hooks,
        "a matching `world_epoch` must not re-run `before-start`/`pre-deploy`"
    );

    // The fork's own pod must serve the fork's files, and the parent must not see them.
    fs::write(fork_project.join("fork-only.txt"), "fork bytes\n").expect("write the fork marker");
    assert_object(&lab.get("feat-auth--demo.localhost", "/"), "fork-only.txt");
    // The protected ceiling: `spawn` -> the fork's own first `200 OK` from the ingress.
    let wall = started.elapsed();
    assert!(
        wall < Duration::from_secs(1),
        "fork must serve its first 200 within 1.0s of spawn (took {wall:?})"
    );
    println!(
        "measured spawn -> fork first 200: {:?} (snapshot_ms={})",
        wall, reply["snapshot_ms"]
    );
    assert!(
        !lab.project.join("fork-only.txt").exists(),
        "the parent checkout must stay untouched"
    );

    // Live PostgreSQL rows, Redis keys, and S3 objects all survive the CoW snapshot.
    assert_eq!(
        lab.exec(
            &fork_project,
            "pg",
            &[
                "psql",
                "-U",
                "postgres",
                "-d",
                "postgres",
                "-tAc",
                "SELECT note FROM seed WHERE id = 1",
            ],
        ),
        "parent",
        "the seeded PG row must exist inside the fork"
    );
    assert_eq!(
        lab.exec(&fork_project, "cache", &["redis-cli", "GET", "cache:key"]),
        "parent-value",
        "the seeded redis key must exist inside the fork"
    );
    assert_object(
        &lab.get("assets-cdn.feat-auth--demo.localhost", "/seed.txt"),
        "parent object",
    );

    // Post-fork parent writes must stay on the parent's side of the CoW boundary.
    lab.exec(
        &lab.project,
        "pg",
        &[
            "psql",
            "-U",
            "postgres",
            "-d",
            "postgres",
            "-tAc",
            "INSERT INTO seed VALUES (2, 'late')",
        ],
    );
    assert_eq!(
        lab.exec(
            &fork_project,
            "pg",
            &[
                "psql",
                "-U",
                "postgres",
                "-d",
                "postgres",
                "-tAc",
                "SELECT count(*) FROM seed",
            ],
        ),
        "1",
        "a parent INSERT after `fork` must not appear inside the fork"
    );
    assert_eq!(
        lab.exec(
            &lab.project,
            "cache",
            &["redis-cli", "SET", "cache:key", "mutated"],
        ),
        "OK"
    );
    assert_eq!(
        lab.exec(&fork_project, "cache", &["redis-cli", "GET", "cache:key"]),
        "parent-value",
        "a parent redis write after `fork` must not appear inside the fork"
    );
    let late = lab.signed_put(
        "assets-cdn.demo.localhost",
        "/assets/parent-only.txt",
        "/parent-only.txt",
        "late object",
    );
    assert!(
        late.starts_with("HTTP/1.1 200"),
        "the parent bucket must accept the late object: {late}"
    );
    assert!(
        lab.get("assets-cdn.feat-auth--demo.localhost", "/parent-only.txt")
            .starts_with("HTTP/1.1 404"),
        "a parent S3 PUT after `fork` must not appear inside the fork"
    );

    // Breaking the paired `world_epoch` forces both hooks to run again; the data still survives.
    let layout = StoreLayout::at(&lab.store_root);
    let demo = WorldName::parse("demo").expect("valid world");
    let feat = ForkName::parse("feat-auth").expect("valid fork");
    let fork_pg = layout.fork_world_clone(&demo, &feat, ResourceKind::Postgres, "pg");
    let pg_stamp = fork_pg.join(slipfork_world::PG_STAMP_FILE);
    let mut stamp: serde_json::Value =
        serde_json::from_slice(&fs::read(&pg_stamp).expect("read the fork's paired PG stamp"))
            .expect("PG stamp JSON");
    stamp["world_epoch"] = serde_json::Value::String("mismatched".to_owned());
    fs::write(
        &pg_stamp,
        serde_json::to_vec(&stamp).expect("encode the altered PG stamp"),
    )
    .expect("break the fork's paired epoch");
    let down = lab.run_in(&fork_project, &["down"]);
    assert!(
        down.status.success(),
        "down failed: {}",
        String::from_utf8_lossy(&down.stderr)
    );
    let reup = lab.run_in(
        &fork_project,
        &["up", "--world", "feat-auth--demo", "--json"],
    );
    assert!(
        reup.status.success(),
        "re-up failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&reup.stdout),
        String::from_utf8_lossy(&reup.stderr)
    );
    assert_eq!(
        lab.hook_lines(&fork_project),
        parent_hooks + 2,
        "a mismatched paired `world_epoch` must re-run both hooks"
    );
    assert_eq!(
        lab.exec(
            &fork_project,
            "pg",
            &[
                "psql",
                "-U",
                "postgres",
                "-d",
                "postgres",
                "-tAc",
                "SELECT note FROM seed WHERE id = 1",
            ],
        ),
        "parent",
        "the fork's PG data must survive a reboot"
    );
    assert_eq!(
        lab.exec(&fork_project, "cache", &["redis-cli", "GET", "cache:key"]),
        "parent-value",
        "the fork's redis key must survive a reboot"
    );
    assert_object(
        &lab.get("assets-cdn.feat-auth--demo.localhost", "/seed.txt"),
        "parent object",
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
    assert!(
        !fork_pg.exists(),
        "the fork's flat `<fork>--<world>.pg.<pod>` clone must be removed"
    );
    assert!(
        !layout
            .world_pods(&StoreLayout::fork_world_name(&demo, &feat))
            .exists(),
        "the fork's ephemeral pod clones must be removed"
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
