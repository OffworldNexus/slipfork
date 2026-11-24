//! Protected live journey for `slipfork fork` and `slipfork drop`.
//!
//! A real Btrfs scratch project (`SLIPFORK_TEST_ROOT`), a real daemon, and a real `crun` world
//! running the shipped eight-component demo (`support/demo`): PostgreSQL, Redis, two S3 buckets
//! behind one native server, the `backend` image (FastAPI under `uvicorn --reload` for `api` plus
//! the `worker`), the SvelteKit `frontend` image (`vite dev` behind `arg VITE_CDN_URL`), and the
//! OCI `mailpit` service. A fork must reuse every sealed parent `image` subvolume (`cached`, no
//! Dockerfile re-evaluation), re-sync nothing (`uv sync`/`pnpm install`/`*:pre-deploy` stay
//! `up to date`), snapshot every resource clone after one `<5ms` direct-TCP pre-fork `CHECKPOINT`,
//! survive live PG/Redis/S3 writes, and keep Slipfork's own CoW snapshot `< 500ms` while the full
//! 8-pod spawn-to-first-`200` median stays `< 8.0s` (measured `~5.5s` on the 4-vCPU droplet)
//! giving 2-vCPU GitHub Actions loop-Btrfs headroom (one discarded warm-up + three samples).
//! Every missing host capability FAILS CLOSED.

use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
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

/// Both journeys own the one daemon socket and the `demo` world name, so this binary serialises
/// them instead of letting two labs race on `XDG_RUNTIME_DIR`.
static LAB_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// A per-run token so an inserted image instruction always changes the step digest; a constant
/// `RUN true` would report `cached` on the second identical test run.
fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{}-{nanos}", std::process::id())
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

/// Every process whose argv or environment names `scope`; a leaked supervisor or pod shows up here.
fn processes_naming(scope: &Path) -> Vec<u32> {
    use std::os::unix::ffi::OsStrExt;
    let needle = scope.as_os_str().as_bytes();
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
        if pid == std::process::id() {
            continue;
        }
        let named = ["cmdline", "environ"].iter().any(|file| {
            fs::read(entry.path().join(file))
                .is_ok_and(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
        });
        if named {
            found.push(pid);
        }
    }
    found
}

/// Makes one directory read-only and restores its original mode on drop, so a failed assertion
/// cannot leave the isolated registry locked for the lab teardown.
struct UnwritableDir {
    path: PathBuf,
    mode: u32,
}

impl UnwritableDir {
    fn new(path: &Path) -> Self {
        let mode = fs::metadata(path)
            .unwrap_or_else(|error| {
                fail_closed(&format!(
                    "stat the isolated registry `{}`: {error}",
                    path.display()
                ))
            })
            .permissions()
            .mode()
            & 0o7777;
        fs::set_permissions(path, fs::Permissions::from_mode(0o555)).unwrap_or_else(|error| {
            fail_closed(&format!(
                "lock the isolated registry `{}`: {error}",
                path.display()
            ))
        });
        Self {
            path: path.to_path_buf(),
            mode,
        }
    }
}

impl Drop for UnwritableDir {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(self.mode));
    }
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

fn demo_files() -> &'static [&'static str] {
    &[
        "Forkfile",
        ".gitignore",
        "README.md",
        "api/.dockerignore",
        "api/Dockerfile",
        "api/pyproject.toml",
        "api/uv.lock",
        "api/app/__init__.py",
        "api/app/db.py",
        "api/app/mail.py",
        "api/app/main.py",
        "api/app/migrate.py",
        "api/app/s3.py",
        "api/app/worker.py",
        "api/app/world.py",
        "front/.dockerignore",
        "front/Dockerfile",
        "front/package.json",
        "front/pnpm-lock.yaml",
        "front/pnpm-workspace.yaml",
        "front/svelte.config.js",
        "front/vite.config.ts",
        "front/src/app.html",
        "front/src/routes/+layout.server.ts",
        "front/src/routes/+page.svelte",
    ]
}

struct Lab {
    project: PathBuf,
    runtime: PathBuf,
    store_root: PathBuf,
    /// Isolated `$XDG_STATE_HOME`: the daemon's `worlds.json`, lock, and log live under it.
    state_home: PathBuf,
}

impl Lab {
    fn new() -> Self {
        // SAFETY: `geteuid` has no preconditions and only reads this process's credentials.
        assert_ne!(
            unsafe { libc::geteuid() },
            0,
            "the rollback journey must run as the unprivileged slipfork lab user; root bypasses 0o555"
        );
        let Some(test_root) =
            std::env::var_os("SLIPFORK_TEST_ROOT").filter(|root| !root.is_empty())
        else {
            fail_closed(
                "SLIPFORK_TEST_ROOT is unset or empty.\nnext: SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test",
            )
        };
        let test_root = PathBuf::from(test_root);
        if let Err(source) = fs::create_dir_all(&test_root) {
            fail_closed(&format!(
                "cannot create `{}`: {source}\nnext: sudo chown -R \"$(id -u):$(id -g)\" {}",
                test_root.display(),
                test_root.display()
            ));
        }
        assert_eq!(
            fs_magic(&test_root),
            BTRFS_SUPER_MAGIC,
            "fail closed: `{}` is not Btrfs.\nnext: SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test",
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
        // This heavy fixture stays an explicit subvolume: its hundreds of MB of `uv`/`pnpm`
        // artifacts gate the `<500ms` CoW snapshot, and a copied fork would destroy that gate.
        fs::create_dir_all(project.parent().expect("scratch root"))
            .expect("create the scratch root");
        BtrfsEngine::new(StoreLayout::at(&store_root))
            .create_subvolume(&project)
            .expect("create the scratch project subvolume");
        let state_home = project.parent().expect("scratch root").join("state");
        fs::create_dir_all(state_home.join("slipfork")).expect("create the isolated state dir");
        let support = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../support/demo");
        for relative in demo_files() {
            let target = project.join(relative);
            fs::create_dir_all(target.parent().expect("fixture parent")).expect("create dirs");
            fs::copy(support.join(relative), &target).unwrap_or_else(|error| {
                fail_closed(&format!(
                    "copy `{}` into the lab: {error}",
                    support.join(relative).display()
                ))
            });
        }
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
            state_home,
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
            .env("XDG_STATE_HOME", &self.state_home)
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

    /// Every NDJSON frame a `--json` verb streamed before its final summary line.
    fn frames(&self, output: &Output, label: &str) -> Vec<serde_json::Value> {
        assert!(
            output.status.success(),
            "{label} failed: stdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|error| panic!("{label} must emit NDJSON: {error}"))
            })
            .collect()
    }

    /// The last non-empty stdout line: every verb streams NDJSON frames, then one summary.
    fn json(&self, output: &Output, label: &str) -> serde_json::Value {
        self.frames(output, label)
            .pop()
            .unwrap_or_else(|| panic!("{label} emitted no JSON"))
    }

    fn branch(&self, project: &Path, name: &str) {
        let status = Command::new(require_tool("git"))
            .arg("-C")
            .arg(project)
            .args(["branch", "-f", name])
            .status()
            .expect("create the fork branch");
        assert!(status.success(), "git branch {name} failed");
    }

    /// One timed `fork` sample: `spawn` -> the fork's first `200 OK`, then `drop -f`.
    fn sample(&self, name: &str) -> Duration {
        self.branch(&self.project, name);
        let started = Instant::now();
        let fork = self.run(&["fork", name, "--json"]);
        let reply = self.json(&fork, &format!("fork {name}"));
        let world = reply["world"].as_str().expect("fork world").to_owned();
        let snapshot_ms = reply["snapshot_ms"]
            .as_u64()
            .expect("fork --json must report `snapshot_ms`");
        assert!(
            snapshot_ms < 500,
            "Slipfork's own CoW snapshot must stay < 500ms, got {snapshot_ms}ms: {reply}"
        );
        let response = self.get(&format!("{world}.localhost"), "/");
        assert_object(&response, "Slipfork demo");
        let elapsed = started.elapsed();
        // After the timed GET, every shipped row must already have finished, not just `front`.
        let frames = self.frames(&fork, &format!("fork {name}"));
        for row in [
            "pg", "cache", "uploads", "assets", "api", "worker", "front", "mailpit",
        ] {
            assert!(
                frames
                    .iter()
                    .any(|frame| frame["event"] == "finished" && frame["name"] == row),
                "fork `{name}` must finish `{row}` before publishing: {frames:?}"
            );
        }
        // Zero rebuilds and zero re-syncs: every shipped image is adopted `cached`, and no setup or
        // hook command executes again. The warm rows must surface as `up to date`, not vanish.
        for frame in &frames {
            if frame["event"] != "finished" {
                continue;
            }
            let kind = frame["kind"].as_str().unwrap_or_default();
            assert!(
                kind != "setup" && kind != "hook",
                "fork `{name}` must re-run no setup/hook step: {frame}"
            );
            if kind == "image" {
                assert_eq!(
                    frame["state"], "cached",
                    "every fork image must be adopted `cached`: {frame}"
                );
            }
        }
        for name in ["uv sync", "pnpm install"] {
            assert!(
                frames.iter().any(|frame| {
                    frame["event"] == "up-to-date"
                        && frame["kind"] == "setup"
                        && frame["name"] == name
                }),
                "the warm `{name}` setup row must report `up to date`: {frames:?}"
            );
        }
        assert!(
            frames.iter().any(|frame| {
                frame["event"] == "up-to-date"
                    && frame["kind"] == "hook"
                    && frame["name"]
                        .as_str()
                        .is_some_and(|name| name.ends_with(":pre-deploy"))
            }),
            "the warm `*:pre-deploy` hook row must report `up to date`: {frames:?}"
        );
        let status = self.get(&format!("{world}.localhost"), "/api/status");
        assert!(
            status.contains("\"counts\":"),
            "the forked API must serve `/api/status`: {status}"
        );
        println!("fork sample `{name}`: {elapsed:?} (snapshot_ms={snapshot_ms})");
        let drop = self.run(&["drop", name, "-f", "--json"]);
        let _ = self.json(&drop, &format!("drop {name}"));
        elapsed
    }

    fn fork_project(&self) -> PathBuf {
        self.project.join(".forks/feat-auth")
    }

    fn fork_world(&self) -> PathBuf {
        self.store_root.join("worlds/feat-auth--demo")
    }

    fn shutdown_daemon(&self) {
        let socket = self.runtime.join("daemon.sock");
        if let Ok(mut stream) = UnixStream::connect(socket) {
            let _ = stream.write_all(b"{\"v\":1,\"id\":\"bye\",\"method\":\"shutdown\"}\n");
            let mut reply = [0_u8; 256];
            let _ = stream.read(&mut reply);
        }
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        // A panicked rollback journey may leave the isolated registry read-only.
        let _ = fs::set_permissions(
            self.state_home.join("slipfork"),
            fs::Permissions::from_mode(0o755),
        );
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

/// M3.123: a fork's prep frames and its merged `plan` frame share one denominator.
///
/// The renderer derives `[i/N]` from the plan's row positions, so `--json` must never expose a
/// narrower `total` on a prep frame: every planned row that echoes a counter uses the plan total.
fn assert_one_fork_denominator(frames: &[serde_json::Value], label: &str) {
    let plan = frames
        .iter()
        .find(|frame| frame["event"] == "plan")
        .unwrap_or_else(|| panic!("{label} must announce one plan: {frames:?}"));
    let rows = plan["rows"].as_array().expect("plan rows");
    let total = plan["total"].as_u64().expect("plan total");
    assert_eq!(
        total,
        u64::try_from(rows.len()).expect("plan rows fit u64"),
        "{label}: one merged total: {plan}"
    );
    assert!(
        total >= 5,
        "{label}: the fork plan is prep + world rows: {plan}"
    );
    for kind in ["fork", "git"] {
        assert!(
            rows.iter().any(|row| row["kind"] == kind),
            "{label}: `{kind}` prep row in the plan: {plan}"
        );
    }
    let mut echoed = 0;
    for frame in frames
        .iter()
        .filter(|frame| matches!(frame["event"].as_str(), Some("finished" | "up-to-date")))
    {
        let Some(step) = frame["step"].as_u64() else {
            continue;
        };
        let kind = frame["kind"].as_str().unwrap_or_default();
        let name = frame["name"].as_str().unwrap_or_default();
        let Some(position) = rows
            .iter()
            .position(|row| row["kind"] == kind && row["name"] == name)
        else {
            continue;
        };
        assert_eq!(
            step,
            u64::try_from(position).expect("row fits u64") + 1,
            "{label}: `{kind} {name}` must echo its merged plan position: {frame}"
        );
        assert_eq!(
            frame["total"].as_u64(),
            Some(total),
            "{label}: `{kind} {name}` must share the plan denominator: {frame}"
        );
        echoed += 1;
    }
    assert!(
        echoed >= 2,
        "{label}: the prep frames must echo the merged plan: {frames:?}"
    );
}

#[test]
fn fork_and_drop_a_live_world_without_leaving_snapshots() {
    let _guard = LAB_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let lab = Lab::new();

    // `up` must never convert or replace the checkout it boots.
    let project_before = fs::metadata(&lab.project).expect("stat the project before `up`");

    // Cold boot: the eight shipped components (S3 buckets coalesce into one native server node).
    let up = lab.run(&["up", "--json"]);
    let reply = lab.json(&up, "up");
    let project_after = fs::metadata(&lab.project).expect("restat the project after `up`");
    assert_eq!(
        (project_after.ino(), project_after.dev()),
        (project_before.ino(), project_before.dev()),
        "`up` must never replace the checkout directory it boots"
    );
    let siblings: Vec<String> = fs::read_dir(lab.project.parent().expect("scratch root"))
        .expect("read the scratch root")
        .map(|entry| {
            entry
                .expect("read a scratch entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        !siblings.iter().any(|name| name.contains(".slipfork-adopt")),
        "`up` must never adopt a sibling: {siblings:?}"
    );
    let rows = reply["nodes"].as_array().expect("up --json nodes");
    let shipped = [
        "pg", "cache", "uploads", "assets", "api", "worker", "front", "mailpit",
    ];
    for row in shipped {
        assert!(
            rows.iter().any(|node| {
                node["name"] == row
                    && matches!(node["kind"].as_str(), Some("resource") | Some("service"))
                    && node["state"] == "ready"
            }),
            "`{row}` must be ready before `up` returns: {reply}"
        );
    }
    assert_eq!(
        rows.iter()
            .filter(|node| matches!(node["kind"].as_str(), Some("resource") | Some("service")))
            .count(),
        8,
        "exactly the eight shipped rows must finish once: {reply}"
    );
    assert!(
        rows.iter()
            .any(|node| node["kind"] == "hook" && node["name"] == "api:pre-deploy"),
        "the cold `up` must run the `pre-deploy` seed: {reply}"
    );
    // `cdn <id>` is cloud-only (`specs/01` §4): locally the public bucket keeps its own
    // `<bucket>.<world>.localhost` authority, and the retired `static-cdn` host must not route.
    assert_object(
        &lab.get("assets.demo.localhost", "/badge.svg"),
        "public-read 200",
    );
    assert!(
        lab.get("static-cdn.demo.localhost", "/badge.svg")
            .starts_with("HTTP/1.1 404"),
        "`cdn` is cloud-only; no `static-cdn` authority may exist locally"
    );
    assert!(
        lab.get("uploads.demo.localhost", "/audit.json")
            .starts_with("HTTP/1.1 403"),
        "the private `uploads` bucket must deny anonymous GET"
    );
    assert_object(&lab.get("demo.localhost", "/"), "Slipfork demo");
    let status = lab.get("demo.localhost", "/api/status");
    assert!(
        status.contains("\"health\":{\"postgres\":true,\"redis\":true,\"s3\":true}"),
        "the status endpoint must report every backing service healthy: {status}"
    );
    assert!(
        status.contains("\"counts\":{\"items\":2,\"events\":1}"),
        "the seeded demo rows must be visible on the status endpoint: {status}"
    );

    // Warm boot with the paired stamps current: services spawn in parallel with `pg`.
    let down = lab.run(&["down"]);
    assert!(
        down.status.success(),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
    let warm = lab.run(&["up", "--json"]);
    let warm_reply = lab.json(&warm, "warm up");
    // A warm reboot still reports exactly one completed row per shipped component.
    assert_eq!(
        warm_reply["nodes"]
            .as_array()
            .expect("warm up nodes")
            .iter()
            .filter(|node| matches!(node["kind"].as_str(), Some("resource") | Some("service")))
            .count(),
        8,
        "the warm summary must carry the eight shipped rows: {warm_reply}"
    );

    // Seed live state, then fork it.
    assert_eq!(
        lab.exec(
            &lab.project,
            "pg",
            &[
                "psql",
                "-U",
                "postgres",
                "-d",
                "app_db",
                "-tAc",
                "INSERT INTO items (title, done) VALUES ('parent-only', false)",
            ],
        ),
        "INSERT 0 1"
    );
    assert_eq!(
        lab.exec(
            &lab.project,
            "cache",
            &["redis-cli", "SET", "cache:key", "parent-value"]
        ),
        "OK"
    );

    // The shipped pre-fork wire CHECKPOINT must visibly advance the parent's checkpointer stats.
    let checkpoints_before: u64 = lab
        .exec(
            &lab.project,
            "pg",
            &[
                "psql",
                "-U",
                "postgres",
                "-d",
                "postgres",
                "-tAc",
                "SELECT num_timed + num_requested FROM pg_stat_checkpointer",
            ],
        )
        .parse()
        .expect("pg_stat_checkpointer before the fork");
    let pg_clone = lab.store_root.join("worlds/demo.pg.pg");
    let version = pg_clone.join("PG_VERSION");
    let version_mtime = fs::metadata(&version)
        .expect("PG_VERSION")
        .modified()
        .expect("mtime");

    lab.branch(&lab.project, "feat-auth");
    let fork_started = Instant::now();
    let fork = lab.run(&["fork", "feat-auth", "--json"]);
    let fork_reply = lab.json(&fork, "fork feat-auth");
    let fork_frames = lab.frames(&fork, "fork feat-auth");
    assert_one_fork_denominator(&fork_frames, "fork feat-auth");
    let checkpoints_after: u64 = lab
        .exec(
            &lab.project,
            "pg",
            &[
                "psql",
                "-U",
                "postgres",
                "-d",
                "postgres",
                "-tAc",
                "SELECT num_timed + num_requested FROM pg_stat_checkpointer",
            ],
        )
        .parse()
        .expect("pg_stat_checkpointer after the fork");
    assert!(
        checkpoints_after > checkpoints_before,
        "the pre-fork wire CHECKPOINT must run in the parent world ({checkpoints_before} -> {checkpoints_after})"
    );
    assert_eq!(
        fork_reply["url"], "http://feat-auth--demo.localhost",
        "{fork_reply}"
    );
    assert_object(
        &lab.get("feat-auth--demo.localhost", "/"),
        "feat-auth--demo",
    );
    println!(
        "measured spawn -> fork first 200: {:?}",
        fork_started.elapsed()
    );

    // Warm-up + three samples. Slipfork's own CoW snapshot is `< 500ms` per sample; the honest
    // end-to-end gate for the 8-pod spawn-to-first-200 is `< 8.0s` (measured `~5.5s` median on the
    // 4-vCPU droplet) giving 2-vCPU GitHub Actions loop-Btrfs headroom now that the
    // `@uv run`/`@pnpm exec` wrappers are gone. Every sample waits for its 200.
    let warmup = lab.sample("warmup");
    println!("discarded warm-up: {warmup:?}");
    let mut samples = vec![
        lab.sample("sample-1"),
        lab.sample("sample-2"),
        lab.sample("sample-3"),
    ];
    samples.sort_unstable();
    let median = samples[1];
    println!("fork samples: {samples:?} (median {median:?})");
    assert!(
        median < Duration::from_secs_f64(8.0),
        "the 8-component fork median must stay < 8.0s (measured ~5.5s on the 4-vCPU droplet; \
         2-vCPU GitHub Actions loop-Btrfs headroom) (samples {samples:?})"
    );

    let fork_project = lab.fork_project();
    let fork_world = lab.fork_world();
    assert!(
        is_subvolume_path(&fork_project).unwrap_or(false),
        "fork checkout"
    );
    assert!(
        is_subvolume_path(&fork_world).unwrap_or(false),
        "fork world"
    );
    let parent_stamp: serde_json::Value = serde_json::from_slice(
        &fs::read(
            lab.store_root
                .join("worlds/demo/.slipfork-world-stamp.json"),
        )
        .expect("parent stamp"),
    )
    .expect("parent stamp JSON");
    let fork_stamp: serde_json::Value = serde_json::from_slice(
        &fs::read(fork_world.join(".slipfork-world-stamp.json")).expect("fork stamp"),
    )
    .expect("fork stamp JSON");
    assert_eq!(
        parent_stamp["world_epoch"], fork_stamp["world_epoch"],
        "a fork must reuse the parent's `world_epoch`"
    );
    assert!(
        parent_stamp["pre_deploy"]
            .as_object()
            .is_some_and(|map| !map.is_empty()),
        "the world stamp must persist the `pre-deploy` digest"
    );
    assert!(
        !fork_frames
            .iter()
            .any(|frame| { frame["event"] == "finished" && frame["name"] == "api:pre-deploy" }),
        "a matching epoch must not re-run `pre-deploy`: {fork_frames:?}"
    );
    // Parent image subvolumes are adopted: neither Dockerfile is re-evaluated or re-sealed, and
    // `arg VITE_CDN_URL = static-cdn.url` keeps the parent digest.
    assert!(
        !fork_frames.iter().any(|frame| {
            let text = frame["line"].as_str().unwrap_or_default();
            text.contains("dockerfile backend")
                || text.contains("dockerfile frontend")
                || text.contains("streaming")
                || text.contains("manifest ")
        }),
        "a fork must not rebuild or re-download image layers: {fork_frames:?}"
    );
    // Spawn overlap on a stamps-current boot is proven from the live frame stream: a service's
    // `started` frame arrives before `pg` finishes. `nodes` keeps only completed rows.
    let api_spawn = fork_frames
        .iter()
        .position(|frame| {
            frame["event"] == "started" && frame["kind"] == "service" && frame["name"] == "api"
        })
        .expect("a service `started` frame");
    let pg_ready = fork_frames
        .iter()
        .position(|frame| {
            frame["event"] == "finished" && frame["kind"] == "resource" && frame["name"] == "pg"
        })
        .expect("a `pg` ready frame");
    assert!(
        api_spawn < pg_ready,
        "a service must start before `pg` is ready on a current epoch: {fork_frames:?}"
    );

    // Positive adoption evidence, not merely the absence of rebuild lines.
    for image in ["backend", "frontend"] {
        assert!(
            fork_frames.iter().any(|frame| {
                frame["kind"] == "image" && frame["name"] == image && frame["state"] == "cached"
            }),
            "a fork with a current epoch must report `{image}` as `cached`: {fork_frames:?}"
        );
    }
    let branch = Command::new(require_tool("git"))
        .arg("-C")
        .arg(&fork_project)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .expect("read the fork branch");
    assert_eq!(String::from_utf8_lossy(&branch.stdout).trim(), "feat-auth");

    // Live PG rows, Redis keys, and S3 objects survive the CoW snapshot.
    assert_eq!(
        lab.exec(
            &fork_project,
            "pg",
            &[
                "psql",
                "-U",
                "postgres",
                "-d",
                "app_db",
                "-tAc",
                "SELECT count(*) FROM items",
            ],
        ),
        "3",
        "the seeded PG rows must exist inside the fork"
    );
    assert_eq!(
        lab.exec(&fork_project, "cache", &["redis-cli", "GET", "cache:key"]),
        "parent-value"
    );
    assert_object(
        &lab.get("assets.feat-auth--demo.localhost", "/badge.svg"),
        "public-read 200",
    );

    // Post-fork parent writes stay on the parent's side of the CoW boundary.
    assert_eq!(
        lab.exec(
            &lab.project,
            "pg",
            &[
                "psql",
                "-U",
                "postgres",
                "-d",
                "app_db",
                "-tAc",
                "INSERT INTO items (title, done) VALUES ('late', false)",
            ],
        ),
        "INSERT 0 1"
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
                "app_db",
                "-tAc",
                "SELECT count(*) FROM items",
            ],
        ),
        "3",
        "a parent INSERT after `fork` must not appear inside the fork"
    );
    let late = lab.signed_put(
        "assets.demo.localhost",
        "/assets/parent-only.txt",
        "/parent-only.txt",
        "late object",
    );
    assert!(late.starts_with("HTTP/1.1 200"), "{late}");
    assert!(
        lab.get("assets.feat-auth--demo.localhost", "/parent-only.txt")
            .starts_with("HTTP/1.1 404"),
        "a parent S3 PUT after `fork` must not appear inside the fork"
    );

    assert_eq!(
        fs::metadata(&version)
            .expect("PG_VERSION")
            .modified()
            .expect("mtime"),
        version_mtime,
        "fork must never rewrite `PG_VERSION`"
    );

    // Breaking the paired epoch re-runs `pre-deploy`, and a real `front/Dockerfile` input change
    // forces the re-evaluated build to miss at least one step instead of reporting `cached`.
    let layout = StoreLayout::at(&lab.store_root);
    let demo = WorldName::parse("demo").expect("valid world");
    let feat = ForkName::parse("feat-auth").expect("valid fork");
    let fork_pg = layout.fork_world_clone(&demo, &feat, ResourceKind::Postgres, "pg");
    let pg_stamp = fork_pg.join(slipfork_world::PG_STAMP_FILE);
    let mut stamp: serde_json::Value =
        serde_json::from_slice(&fs::read(&pg_stamp).expect("fork PG stamp")).expect("stamp JSON");
    stamp["world_epoch"] = serde_json::Value::String("mismatched".to_owned());
    fs::write(&pg_stamp, serde_json::to_vec(&stamp).expect("encode")).expect("break the epoch");
    // A stamp-only rewrite leaves every step digest intact, so the engine may legitimately report
    // `cached`. Insert a run-unique `RUN` instruction inside the `dev` stage the Forkfile actually
    // selects (`on dev: stage dev`); an EOF `RUN` would only edit an unused later stage, and a
    // constant instruction would stay `cached` on a second identical run.
    let front_dockerfile = fork_project.join("front/Dockerfile");
    let document =
        fs::read_to_string(&front_dockerfile).expect("read the fork `frontend` Dockerfile");
    let Some(build_stage) = document.find("FROM dev AS build") else {
        panic!("the demo `frontend` Dockerfile must define `FROM dev AS build`: {document:?}");
    };
    let mut changed = document[..build_stage].to_owned();
    if !changed.ends_with('\n') {
        changed.push('\n');
    }
    changed.push_str(&format!("RUN echo {}\n", unique_suffix()));
    changed.push_str(&document[build_stage..]);
    fs::write(&front_dockerfile, changed).expect("change a real `frontend` Dockerfile input");
    let down = lab.run_in(&fork_project, &["down"]);
    assert!(
        down.status.success(),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
    let reup = lab.run_in(&fork_project, &["up", "--json"]);
    let reup_reply = lab.json(&reup, "mismatched re-up");
    assert!(
        reup_reply["nodes"].as_array().is_some_and(|nodes| nodes
            .iter()
            .any(|node| { node["name"] == "frontend" && node["state"] == "built" })),
        "a mismatched epoch plus a changed `front/Dockerfile` must rebuild `frontend`: {reup_reply}"
    );
    assert!(
        reup_reply["nodes"].as_array().is_some_and(|nodes| nodes
            .iter()
            .any(|node| node["kind"] == "hook" && node["name"] == "api:pre-deploy")),
        "a mismatched epoch must re-run `pre-deploy`: {reup_reply}"
    );

    // Dirty work is refused before anything stops, with exactly one remedy.
    fs::write(fork_project.join("untracked.txt"), "dirty\n").expect("write dirty work");
    let dirty = lab.run(&["drop", "feat-auth"]);
    assert_eq!(dirty.status.code(), Some(1), "{dirty:?}");
    let stderr = String::from_utf8_lossy(&dirty.stderr);
    assert!(stderr.contains("uncommitted changes"), "{stderr}");
    assert!(stderr.contains("slipfork drop feat-auth -f"), "{stderr}");
    assert_eq!(stderr.matches("next:").count(), 1, "{stderr}");

    let drop = lab.run(&["drop", "feat-auth", "-f", "--json"]);
    let dropped = lab.json(&drop, "drop feat-auth");
    assert_eq!(dropped["dropped"], "feat-auth");
    assert!(
        !fork_project.exists(),
        "the project snapshot must be removed"
    );
    assert!(
        !fork_world.exists(),
        "the fork world snapshot must be removed"
    );
    assert!(!fork_pg.exists(), "the fork PG clone must be removed");
    assert!(
        !layout
            .world_pods(&StoreLayout::fork_world_name(&demo, &feat))
            .exists(),
        "the fork's ephemeral pod clones must be removed"
    );
    let missing = lab.run(&["drop", "feat-auth"]);
    assert_eq!(missing.status.code(), Some(1), "{missing:?}");
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no fork named"));

    // A fork that fails after its snapshots must roll back every trace (project snapshot, world
    // clone, pod rootfs clones, tmpfs bundles) so the name is reusable immediately.
    let git = require_tool("git");
    let parent_branch = String::from_utf8_lossy(
        &Command::new(&git)
            .arg("-C")
            .arg(&lab.project)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .expect("read the parent branch")
            .stdout,
    )
    .trim()
    .to_owned();
    let git_run = |args: &[&str]| {
        let status = Command::new(&git)
            .arg("-C")
            .arg(&lab.project)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    };
    git_run(&["branch", "-f", "feat-auth"]);
    git_run(&["switch", "-q", "feat-auth"]);
    fs::write(lab.project.join("guard.txt"), "committed\n").expect("write the branch guard");
    git_run(&["add", "guard.txt"]);
    git_run(&["commit", "-q", "-m", "guard"]);
    git_run(&["switch", "-q", &parent_branch]);
    // An untracked file whose path `feat-auth` tracks makes the fork's branch switch refuse.
    fs::write(lab.project.join("guard.txt"), "untracked\n").expect("write the conflicting file");
    let refused = lab.run(&["fork", "feat-auth"]);
    assert!(!refused.status.success(), "the conflicting fork must fail");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("next:"), "{stderr}");
    assert!(
        !lab.fork_project().exists(),
        "a failed fork must remove its project snapshot: {stderr}"
    );
    assert!(
        !lab.fork_world().exists(),
        "a failed fork must remove its world clone: {stderr}"
    );
    assert!(
        !lab.store_root.join("pods/feat-auth--demo").exists(),
        "a failed fork must remove its pod rootfs clones: {stderr}"
    );
    assert!(
        !lab.runtime.join("slipfork/pods/feat-auth--demo").exists(),
        "a failed fork must remove its tmpfs bundles and stale `crun` state: {stderr}"
    );
    fs::remove_file(lab.project.join("guard.txt")).expect("drop the conflicting file");
    let retry = lab.run(&["fork", "feat-auth", "--json"]);
    assert!(
        retry.status.success(),
        "the same fork name must be reusable immediately: stdout={}\nstderr={}",
        String::from_utf8_lossy(&retry.stdout),
        String::from_utf8_lossy(&retry.stderr)
    );
    assert!(
        is_subvolume_path(&lab.fork_project()).unwrap_or(false),
        "the retried fork must materialise its project snapshot"
    );
    let cleanup = lab.run(&["drop", "feat-auth", "-f"]);
    assert!(
        cleanup.status.success(),
        "dropping the retried fork failed: {}",
        String::from_utf8_lossy(&cleanup.stderr)
    );
    let _ = Command::new(&git)
        .arg("-C")
        .arg(&lab.project)
        .args(["branch", "-D", "feat-auth"])
        .status();

    let down = lab.run(&["down"]);
    assert!(
        down.status.success(),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
    lab.shutdown_daemon();
    assert!(
        wait_until(
            || daemons_for(&lab.runtime).is_empty(),
            Duration::from_secs(10)
        ),
        "no `__daemon` or supervisor may survive: {:?}",
        daemons_for(&lab.runtime)
    );
}

/// M3.51.2 gate proof, independent of the long CoW journey: a fork that reaches `ready` but whose
/// registry `publish()` cannot persist must roll back atomically. On HEAD this leaks — `publish()`
/// fails *after* inserting the live entry and taking the supervisor child, so the old
/// `discard_fork` purged the tmpfs pods and unlinked the snapshots under two live pods. The
/// `/proc` scan below is the assertion that catches that leak.
#[test]
fn failed_fork_publish_rolls_back_without_leaking() {
    let _guard = LAB_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let lab = Lab::new();
    let up = lab.run(&["up", "--json"]);
    let _ = lab.json(&up, "up");
    let git = require_tool("git");
    let git_run = |args: &[&str]| {
        let status = Command::new(&git)
            .arg("-C")
            .arg(&lab.project)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    };
    const ROLLBACK_FORKFILE: &str = concat!(
        "# forkfile: 0.1\n",
        "domain main\n",
        "image backend ./api {\n",
        "  service api {\n",
        "    run: sleep 3600\n",
        "  }\n",
        "  service worker {\n",
        "    run: sleep 3600\n",
        "  }\n",
        "}\n",
    );
    fs::write(lab.project.join("Forkfile"), ROLLBACK_FORKFILE).expect("write the rollback fixture");
    git_run(&["add", "Forkfile"]);
    git_run(&["commit", "-q", "-m", "rollback fixture"]);
    let canonical_parent = fs::canonicalize(&lab.project).expect("canonical parent project");
    let rollback_project = canonical_parent.join(".forks/rollback");
    let rollback_world = lab.store_root.join("worlds/rollback--demo");
    let state_dir = lab.state_home.join("slipfork");
    // An unwritable registry makes the fork's own `ready` publish fail for real: no mocks.
    let locked = UnwritableDir::new(&state_dir);
    let failed = lab.run(&["fork", "rollback", "--json"]);
    // `--json` reports the daemon failure as a final `{"error": …}` frame on stdout.
    let transcript = format!(
        "{}{}",
        String::from_utf8_lossy(&failed.stdout),
        String::from_utf8_lossy(&failed.stderr)
    );
    assert!(
        !failed.status.success(),
        "an unwritable registry must fail the fork: {transcript}"
    );
    assert!(
        transcript.contains("worlds.json"),
        "the fork must fail writing the registry, not during boot: {transcript}"
    );
    assert!(
        !transcript.contains("exited with"),
        "the `api`/`worker` services must stay healthy: {transcript}"
    );
    // The streamed frames prove both sibling pods were live before the publish failed.
    let booted: Vec<serde_json::Value> = String::from_utf8_lossy(&failed.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str(line.trim()).ok())
        .collect();
    for service in ["api", "worker"] {
        assert!(
            booted.iter().any(|frame| {
                frame["event"] == "started"
                    && frame["kind"] == "service"
                    && frame["name"] == service
            }),
            "`{service}` must already be live when the registry write fails: {booted:?}"
        );
    }
    assert!(
        !booted.iter().any(|frame| frame["event"] == "ready"),
        "a rolled-back fork must never announce `ready` to the CLI: {booted:?}"
    );
    assert!(
        !rollback_project.exists(),
        "the failed fork must remove its project snapshot"
    );
    assert!(
        !is_subvolume_path(&rollback_world).unwrap_or(false) && !rollback_world.exists(),
        "the failed fork must remove its world snapshot"
    );
    let world_leftovers: Vec<String> = fs::read_dir(lab.store_root.join("worlds"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("rollback--demo"))
        .collect();
    assert!(
        world_leftovers.is_empty(),
        "the failed fork must leave no world or `.pg` clone: {world_leftovers:?}"
    );
    assert!(
        !lab.store_root.join("pods/rollback--demo").exists(),
        "the failed fork must remove its pod rootfs clones"
    );
    assert!(
        !lab.runtime.join("slipfork/pods/rollback--demo").exists(),
        "the failed fork must remove its tmpfs bundles and stale `crun` state"
    );
    assert!(
        wait_until(
            || processes_naming(&rollback_project).is_empty(),
            Duration::from_secs(10)
        ),
        "no supervisor or pod may survive a rolled-back fork: {:?}",
        processes_naming(&rollback_project)
    );
    let registry: Vec<serde_json::Value> =
        serde_json::from_slice(&fs::read(state_dir.join("worlds.json")).expect("worlds.json"))
            .expect("worlds.json is JSON");
    assert!(
        !registry
            .iter()
            .any(|entry| entry["project"].as_str() == rollback_project.to_str()),
        "worlds.json must not retain the failed fork: {registry:?}"
    );
    assert!(
        registry
            .iter()
            .any(|entry| entry["project"].as_str() == canonical_parent.to_str()),
        "rolling back a fork must not drop the live parent world: {registry:?}"
    );
    // The daemon's live view must agree: parent present, no rolled-back fork row or nested fork.
    fn world_rows<'a>(rows: &'a [serde_json::Value], out: &mut Vec<&'a serde_json::Value>) {
        for row in rows {
            out.push(row);
            if let Some(forks) = row["forks"].as_array() {
                world_rows(forks, out);
            }
        }
    }
    let status = lab.run(&["status", "--json"]);
    let status_reply = lab.json(&status, "status after the rollback");
    let mut rows = Vec::new();
    world_rows(
        status_reply["worlds"].as_array().expect("status worlds"),
        &mut rows,
    );
    assert!(
        rows.iter().any(|row| row["world"] == "demo"),
        "the daemon must still report the live parent world: {status_reply}"
    );
    assert!(
        !rows
            .iter()
            .any(|row| row["world"] == "rollback--demo" || row["fork"] == "rollback"),
        "the daemon must not report a rolled-back fork: {status_reply}"
    );
    std::mem::drop(locked);
    // With the registry writable again the same name must pass preflight and boot to `ready`.
    let retry = lab.run(&["fork", "rollback", "--json"]);
    let retry_stderr = String::from_utf8_lossy(&retry.stderr);
    assert!(
        retry.status.success(),
        "the same fork name must be reusable immediately: {retry_stderr}"
    );
    let retried = lab.json(&retry, "retried fork");
    assert_eq!(retried["world"], "rollback--demo", "{retried}");
    assert!(
        is_subvolume_path(&rollback_project).unwrap_or(false),
        "the retried fork must materialise its project snapshot"
    );
    let rollback_drop = lab.run(&["drop", "rollback", "-f", "--json"]);
    let _ = lab.json(&rollback_drop, "drop rollback");
}

/// One isolated real-Btrfs scratch world shared by both fork journeys: an owner-shaped checkout,
/// a tmpfs daemon runtime, and an isolated `$XDG_STATE_HOME` (its own `worlds.json` and lock).
/// `plain()` is a checkout without a `Dockerfile` (the `fork` copy path); `root_context()` holds
/// `.git` at the context root, where a `fork` branch switch is a real build-input test.
struct ForkLab {
    root: PathBuf,
    runtime: PathBuf,
    store_root: PathBuf,
    project: PathBuf,
    world: String,
}

const PLAIN_FORKFILE: &str = "# forkfile: 0.1\nimage app from python:3.13-slim {\n  service web {\n    run: python3 -m http.server 8000\n  }\n}\n";

/// The shipped demo builds `api/` and `front/`, so `.git` sits outside their contexts and can
/// never expose this regression. This owner shape builds `image app .` with `COPY . .`.
const ROOT_FORKFILE: &str = "# forkfile: 0.1\ndomain main\nimage app . {\n  service web {\n    port 8000\n    run: python3 -m http.server 8000\n  }\n}\n";
const ROOT_DOCKERFILE: &str = "FROM python:3.13-slim\nWORKDIR /code\nCOPY . .\n";

impl ForkLab {
    fn plain() -> Self {
        Self::with_fixture(
            "plaindemo",
            "plain-fork",
            &[
                ("Forkfile", PLAIN_FORKFILE, None),
                ("marker.txt", "plain checkout\n", None),
                ("run.sh", "#!/bin/sh\necho plain\n", Some(0o755)),
            ],
            "feat-plain",
        )
    }

    fn root_context() -> Self {
        // One per-run context byte keeps the cold boot a real build instead of a step-cache hit
        // left by an earlier run of this journey.
        let notes = format!("noise {}\n", unique_suffix());
        Self::with_fixture(
            "rootctx",
            "root-context",
            &[
                ("Forkfile", ROOT_FORKFILE, None),
                ("Dockerfile", ROOT_DOCKERFILE, None),
                ("app/main.py", "print('parent')\n", None),
                ("notes.txt", notes.as_str(), None),
            ],
            "feat",
        )
    }

    /// Fails closed unless the host gives a real Btrfs scratch root and a tmpfs runtime, writes
    /// `files` into a fresh checkout, and commits it with `fork_branch` ready to switch to.
    fn with_fixture(
        world: &str,
        prefix: &str,
        files: &[(&str, &str, Option<u32>)],
        fork_branch: &str,
    ) -> Self {
        let Some(test_root) =
            std::env::var_os("SLIPFORK_TEST_ROOT").filter(|root| !root.is_empty())
        else {
            fail_closed(
                "SLIPFORK_TEST_ROOT is unset or empty.\nnext: SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test",
            )
        };
        let test_root = PathBuf::from(test_root);
        if let Err(source) = fs::create_dir_all(&test_root) {
            fail_closed(&format!(
                "cannot create `{}`: {source}\nnext: sudo chown -R \"$(id -u):$(id -g)\" {}",
                test_root.display(),
                test_root.display()
            ));
        }
        assert_eq!(
            fs_magic(&test_root),
            BTRFS_SUPER_MAGIC,
            "fail closed: `{}` is not Btrfs.\nnext: SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test",
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
        require_registry_egress();
        let store_root = mount_store_for(&test_root);
        let name = WorldName::parse(world).expect("valid world");
        let _ = BtrfsEngine::new(StoreLayout::at(&store_root)).drop_world(&name);
        let root = test_root.join(format!("{prefix}-{}", unique_suffix()));
        let project = root.join(world);
        fs::create_dir_all(&project).expect("create the checkout");
        fs::create_dir_all(root.join("state/slipfork")).expect("create the isolated state dir");
        for (relative, body, mode) in files {
            let path = project.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create a fixture directory");
            }
            fs::write(&path, body).expect("write a fixture file");
            if let Some(mode) = mode {
                fs::set_permissions(&path, fs::Permissions::from_mode(*mode))
                    .expect("pin a fixture file mode");
            }
        }
        let git = require_tool("git");
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "test@slipfork.local"][..],
            &["config", "user.name", "Slipfork Test"][..],
            &["add", "-A"][..],
            &["commit", "-q", "-m", "seed"][..],
            &["branch", "-f", fork_branch][..],
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
            root,
            runtime,
            store_root,
            project,
            world: world.to_owned(),
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
            .env("TERM", "dumb")
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("XDG_STATE_HOME", self.root.join("state"))
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

    /// Every NDJSON frame a `--json` verb streamed before its final summary line.
    fn frames(&self, output: &Output, label: &str) -> Vec<serde_json::Value> {
        assert!(
            output.status.success(),
            "{label} failed: stdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|error| panic!("{label} must emit NDJSON: {error}"))
            })
            .collect()
    }

    fn json(&self, output: &Output, label: &str) -> serde_json::Value {
        self.frames(output, label)
            .pop()
            .unwrap_or_else(|| panic!("{label} emitted no JSON"))
    }

    fn node_state(&self, reply: &serde_json::Value, name: &str) -> Option<String> {
        reply["nodes"].as_array()?.iter().find_map(|node| {
            (node["kind"] == "image" && node["name"] == name)
                .then(|| node["state"].as_str().unwrap_or_default().to_owned())
        })
    }

    /// Every NDJSON frame on stdout, success or failure: a refused verb still streams its typed
    /// error object as the final line, and its empty plan proves nothing booted.
    fn raw_frames(output: &Output) -> Vec<serde_json::Value> {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    fn stamp(&self, world: &str) -> serde_json::Value {
        serde_json::from_slice(
            &fs::read(
                self.store_root
                    .join(format!("worlds/{world}/.slipfork-world-stamp.json")),
            )
            .unwrap_or_else(|error| panic!("read the `{world}` world stamp: {error}")),
        )
        .unwrap_or_else(|error| panic!("parse the `{world}` world stamp: {error}"))
    }

    /// The sealed read-only rootfs the world stamp recorded for the `app` image.
    fn image_rootfs(&self, world: &str) -> PathBuf {
        let stamp = self.stamp(world);
        PathBuf::from(
            stamp["images"]["app"]["rootfs"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("the `{world}` stamp must record the `app` image rootfs: {stamp}")
                }),
        )
    }
}

impl Drop for ForkLab {
    fn drop(&mut self) {
        let _ = self.run(&["down"]);
        if let Ok(mut stream) = UnixStream::connect(self.runtime.join("daemon.sock")) {
            let _ = stream.write_all(b"{\"v\":1,\"id\":\"bye\",\"method\":\"shutdown\"}\n");
            let mut reply = [0_u8; 256];
            let _ = stream.read(&mut reply);
        }
        let _ = wait_until(
            || daemons_for(&self.runtime).is_empty(),
            Duration::from_secs(10),
        );
        let engine = BtrfsEngine::new(StoreLayout::at(&self.store_root));
        if let Ok(world) = WorldName::parse(&self.world) {
            let _ = engine.drop_world(&world);
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn fork_copies_a_plain_checkout_without_touching_it() {
    let _guard = LAB_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let lab = ForkLab::plain();

    let up = lab.run(&["up"]);
    assert!(
        up.status.success(),
        "up a plain checkout: {}",
        String::from_utf8_lossy(&up.stderr)
    );
    let up_out = String::from_utf8_lossy(&up.stdout);
    assert!(
        up_out.contains("✓ Ready -> http://plaindemo.localhost"),
        "the plain checkout must boot behind its own URL:\n{up_out}"
    );

    let project_before = fs::metadata(&lab.project).expect("stat the plain checkout");
    let marker_before = fs::metadata(lab.project.join("marker.txt")).expect("stat marker.txt");
    let forkfile_before = fs::metadata(lab.project.join("Forkfile")).expect("stat Forkfile");

    let fork = lab.run(&["fork", "feat-plain", "--json"]);
    assert!(
        fork.status.success(),
        "fork a plain checkout: {}",
        String::from_utf8_lossy(&fork.stderr)
    );
    let reply: serde_json::Value = String::from_utf8_lossy(&fork.stdout)
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).expect("fork --json must end with one summary"))
        .expect("fork --json emitted no summary");
    let fork_project = lab.project.join(".forks/feat-plain");
    assert_eq!(
        Path::new(
            reply["project"]
                .as_str()
                .expect("fork summary must report its project")
        ),
        fork_project,
        "{reply}"
    );
    assert!(
        fork_project.is_dir(),
        "a plain checkout must be copied into `.forks`: {reply}"
    );
    assert!(
        !is_subvolume_path(&fork_project).unwrap_or(true),
        "a plain checkout copy is never a subvolume"
    );
    assert!(
        fork_project.join("Forkfile").is_file(),
        "the copy must carry the Forkfile"
    );
    assert_eq!(
        fs::read_to_string(fork_project.join("marker.txt")).expect("read the copied marker"),
        "plain checkout\n",
        "the copy must carry the parent's bytes"
    );
    assert_eq!(
        fs::metadata(fork_project.join("run.sh"))
            .expect("stat the copied run.sh")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "the copy must preserve executable modes"
    );

    let project_after = fs::metadata(&lab.project).expect("restat the plain checkout");
    assert_eq!(
        (project_after.ino(), project_after.dev()),
        (project_before.ino(), project_before.dev()),
        "fork must never replace the parent checkout"
    );
    let marker_after = fs::metadata(lab.project.join("marker.txt")).expect("restat marker.txt");
    assert_eq!(
        (marker_after.ino(), marker_after.dev()),
        (marker_before.ino(), marker_before.dev()),
        "fork must never rewrite `marker.txt`"
    );
    let forkfile_after = fs::metadata(lab.project.join("Forkfile")).expect("restat Forkfile");
    assert_eq!(
        (forkfile_after.ino(), forkfile_after.dev()),
        (forkfile_before.ino(), forkfile_before.dev()),
        "fork must never rewrite `Forkfile`"
    );
    assert_eq!(
        fs::read_to_string(lab.project.join("marker.txt")).expect("reread marker.txt"),
        "plain checkout\n"
    );
    let siblings: Vec<String> = fs::read_dir(lab.project.parent().expect("scratch root"))
        .expect("read the scratch root")
        .map(|entry| {
            entry
                .expect("read a scratch entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        !siblings.iter().any(|name| name.contains(".slipfork-adopt")),
        "fork must never adopt a sibling: {siblings:?}"
    );

    let drop = lab.run(&["drop", "feat-plain", "-f"]);
    assert!(
        drop.status.success(),
        "drop the plain-copied fork: {}",
        String::from_utf8_lossy(&drop.stderr)
    );
    assert!(
        !fork_project.exists(),
        "teardown must remove the copied checkout directory"
    );
}

#[test]
fn fork_adopts_a_root_context_image_cached_across_a_branch_switch() {
    let _guard = LAB_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let lab = ForkLab::root_context();

    let up = lab.run(&["up", "--json"]);
    let cold = lab.json(&up, "cold up");
    assert_eq!(
        lab.node_state(&cold, "app").as_deref(),
        Some("built"),
        "the cold `up` must build the root-context image: {cold}"
    );
    // The sealed image is what `COPY . .` wrote: the source is there, VCS and Slipfork state are
    // not. (A default `dev` boot overlays the live checkout at `/code`, so `exec` sees the host
    // `.git`; the image copy below proves the baked tree itself.)
    let baked = lab.image_rootfs("rootctx");
    assert!(
        baked.join("code/app/main.py").is_file(),
        "the built image must carry the copied source: {}",
        baked.display()
    );
    assert!(
        !baked.join("code/.git").exists(),
        "`.git` must never be copied into the image: {}",
        baked.display()
    );
    assert!(
        !baked.join("code/.slipfork").exists(),
        "`.slipfork` must never be copied into the image"
    );
    assert!(
        !baked.join("code/Dockerfile").exists(),
        "the Dockerfile is metadata, never image content"
    );

    // `feat` sits on the same commit as the parent, so only the branch switch rewrites `.git`.
    let fork_started = Instant::now();
    let fork = lab.run(&["fork", "feat", "--json"]);
    let frames = lab.frames(&fork, "fork feat");
    let reply = lab.json(&fork, "fork feat");
    assert_eq!(reply["world"], "feat--rootctx", "{reply}");
    assert!(
        frames.iter().any(|frame| {
            frame["kind"] == "image" && frame["name"] == "app" && frame["state"] == "cached"
        }),
        "a branch switch must adopt the parent's sealed image: {frames:?}"
    );
    for frame in &frames {
        assert_ne!(
            frame["state"], "built",
            "a fork must never rebuild an image: {frame}"
        );
    }
    assert!(
        !frames
            .iter()
            .any(|frame| frame["event"] == "step" && frame["image"] == "app"),
        "a fork must stream zero build steps: {frames:?}"
    );
    println!("measured root-context fork: {:?}", fork_started.elapsed());
    assert_eq!(
        lab.stamp("rootctx")["world_epoch"],
        lab.stamp("feat--rootctx")["world_epoch"],
        "a fork must inherit the parent's `world_epoch`"
    );

    // A duplicate `fork` of the same name must be refused before any progress row.
    let duplicate = lab.run(&["fork", "feat", "--json"]);
    assert_eq!(duplicate.status.code(), Some(1), "{duplicate:?}");
    let duplicate_frames = ForkLab::raw_frames(&duplicate);
    assert!(
        !duplicate_frames
            .iter()
            .any(|frame| frame["event"] == "plan"),
        "a refused fork must emit zero plan rows: {duplicate_frames:?}"
    );
    assert!(
        !String::from_utf8_lossy(&duplicate.stdout).contains("snapshot"),
        "a refused fork must stream no snapshot row: {}",
        String::from_utf8_lossy(&duplicate.stdout)
    );
    let error = duplicate_frames
        .iter()
        .find(|frame| frame["error"].is_object())
        .unwrap_or_else(|| {
            panic!("a refused fork must emit one error object: {duplicate_frames:?}")
        });
    assert_eq!(error["error"]["code"], "fork-exists", "{error}");
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("already exists")),
        "{error}"
    );
    assert_eq!(error["error"]["next"], "slipfork drop feat -f", "{error}");

    let duplicate_plain = lab.run(&["fork", "feat"]);
    assert_eq!(
        duplicate_plain.status.code(),
        Some(1),
        "{duplicate_plain:?}"
    );
    let stderr = String::from_utf8_lossy(&duplicate_plain.stderr);
    assert!(
        stderr.contains("error: fork \"feat\" already exists"),
        "{stderr}"
    );
    assert_eq!(
        stderr.matches("next:").count(),
        1,
        "exactly one embedded remedy: {stderr}"
    );
    assert!(stderr.contains("next: slipfork drop feat -f"), "{stderr}");
    assert!(
        duplicate_plain.stdout.is_empty(),
        "a refused fork must write no plan or spinner row: {:?}",
        String::from_utf8_lossy(&duplicate_plain.stdout)
    );

    // The advertised remedy really frees the name, and an unchanged re-fork stays cached.
    let drop = lab.run(&["drop", "feat", "-f", "--json"]);
    let dropped = lab.json(&drop, "drop feat before re-fork");
    assert_eq!(dropped["dropped"], "feat");
    let refork = lab.run(&["fork", "feat", "--json"]);
    let refork_frames = lab.frames(&refork, "re-fork feat");
    let refork_reply = lab.json(&refork, "re-fork feat");
    assert_eq!(refork_reply["world"], "feat--rootctx", "{refork_reply}");
    assert!(
        refork_frames.iter().any(|frame| {
            frame["kind"] == "image" && frame["name"] == "app" && frame["state"] == "cached"
        }),
        "an unchanged re-fork must stay cached: {refork_frames:?}"
    );

    // A genuine copied-source edit on the fork branch still rebuilds.
    let fork_project = lab.project.join(".forks/feat");
    assert!(fork_project.is_dir(), "the fork checkout must exist");
    fs::write(fork_project.join("app/main.py"), "print('feat')\n").expect("edit the fork source");
    let git = require_tool("git");
    for args in [
        &["add", "app/main.py"][..],
        &["commit", "-q", "-m", "fork source edit"][..],
    ] {
        let status = Command::new(&git)
            .arg("-C")
            .arg(&fork_project)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }
    let down = lab.run_in(&fork_project, &["down"]);
    assert!(
        down.status.success(),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
    let rebuilt = lab.run_in(&fork_project, &["up", "--json"]);
    let rebuilt_reply = lab.json(&rebuilt, "fork up after the source edit");
    assert_eq!(
        lab.node_state(&rebuilt_reply, "app").as_deref(),
        Some("built"),
        "a real source edit on the fork branch must rebuild: {rebuilt_reply}"
    );
    let rebuilt_image = lab.image_rootfs("feat--rootctx");
    assert!(
        rebuilt_image.join("code/app/main.py").is_file()
            && !rebuilt_image.join("code/.git").exists(),
        "the rebuilt image must carry the edit and never `.git`: {}",
        rebuilt_image.display()
    );

    // ... and the rebuilt image is cached on the next boot, with no repeated build.
    let down = lab.run_in(&fork_project, &["down"]);
    assert!(
        down.status.success(),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
    // One plain (not `--json`) boot reuses the warm path: `NO_COLOR` still renders the human
    // `image … cached` row and must emit zero escape bytes.
    let warm = lab.run_in(&fork_project, &["up"]);
    assert!(
        warm.status.success(),
        "{}",
        String::from_utf8_lossy(&warm.stderr)
    );
    let human = String::from_utf8_lossy(&warm.stdout);
    assert!(
        human.contains("cached"),
        "the rebuilt image must be cached on the next boot: {human}"
    );
    assert!(
        !human.as_bytes().contains(&0x1b),
        "`NO_COLOR` must emit zero escape bytes: {human:?}"
    );

    // The literal owner check: a non-dev (`-F -dev` equivalent) boot keeps the image copy, so
    // `exec … ls -a /code` lists exactly the baked tree — no `.git`.
    let down = lab.run_in(&fork_project, &["down"]);
    assert!(
        down.status.success(),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
    let image_copy = lab.run_in(&fork_project, &["up", "--no-default-features", "--json"]);
    let _ = lab.json(&image_copy, "image-copy fork up");
    let code = lab.exec(&fork_project, "web", &["ls", "-a", "/code"]);
    assert!(
        !code.split_whitespace().any(|name| name == ".git"),
        "the baked `/code` must never carry `.git`: {code}"
    );
    assert!(
        code.split_whitespace().any(|name| name == "app"),
        "the baked `/code` must carry the copied source: {code}"
    );

    let drop = lab.run(&["drop", "feat", "-f", "--json"]);
    let dropped = lab.json(&drop, "drop feat");
    assert_eq!(dropped["dropped"], "feat");
    let down = lab.run(&["down"]);
    assert!(
        down.status.success(),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
}

/// M3.133: `drop` is one four-row plan (`[1/4]`..`[4/4]`) plus one verdict under `NO_COLOR`,
/// `--json` mirrors those steps, and an unreachable control plane surfaces one remedy only.
#[test]
fn drop_prints_one_four_row_plan_and_json_mirrors_it() {
    let _guard = LAB_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let lab = Lab::new();
    // One tiny service keeps this drop-focused journey fast and independent of the eight-pod demo.
    const FORKFILE: &str = concat!(
        "# forkfile: 0.1\n",
        "image app from python:3.13-slim {\n",
        "  service web {\n",
        "    run: python3 -m http.server 8000\n",
        "  }\n",
        "}\n",
    );
    fs::write(lab.project.join("Forkfile"), FORKFILE).expect("write the drop fixture");
    let git = require_tool("git");
    for args in [
        vec!["add", "Forkfile"],
        vec!["commit", "-q", "-m", "drop fixture"],
    ] {
        let status = Command::new(&git)
            .arg("-C")
            .arg(&lab.project)
            .args(&args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }
    let up = lab.run(&["up", "--json"]);
    let _ = lab.json(&up, "up");
    lab.branch(&lab.project, "jello");

    let fork = lab.run(&["fork", "jello", "--json"]);
    let _ = lab.json(&fork, "fork jello");

    let drop = lab.run(&["drop", "jello"]);
    assert!(
        drop.status.success(),
        "{}",
        String::from_utf8_lossy(&drop.stderr)
    );
    assert!(
        !drop.stdout.contains(&0x1b),
        "`NO_COLOR` drop must emit zero escape bytes: {:?}",
        String::from_utf8_lossy(&drop.stdout)
    );
    let stdout = String::from_utf8_lossy(&drop.stdout);
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(lines.len(), 5, "four frames + one verdict: {stdout:?}");
    assert_eq!(lines[0], "→ [1/4] stop jello");
    assert_eq!(lines[2], "→ [3/4] delete jello");
    /// Exactly one real `ui/elapsed` token: `<1ms`, `\d+ms`, or `\d+\.\ds`.
    fn is_timing(token: &str) -> bool {
        if token == "<1ms" {
            return true;
        }
        if let Some(ms) = token.strip_suffix("ms") {
            return !ms.is_empty() && ms.bytes().all(|byte| byte.is_ascii_digit());
        }
        let Some(seconds) = token.strip_suffix('s') else {
            return false;
        };
        let Some((whole, fraction)) = seconds.split_once('.') else {
            return false;
        };
        !whole.is_empty()
            && whole.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.len() == 1
            && fraction.bytes().all(|byte| byte.is_ascii_digit())
    }
    for (line, prefix) in [
        (lines[1], "✓ [2/4] stop    jello  done  pods stopped  "),
        (
            lines[3],
            "✓ [4/4] delete  jello  done  project + world data  ",
        ),
    ] {
        let timing = line
            .strip_prefix(prefix)
            .unwrap_or_else(|| panic!("the settled row shape: {line:?}"));
        let tokens: Vec<&str> = timing.split_whitespace().collect();
        assert_eq!(tokens.len(), 1, "only the timing cell may follow: {line:?}");
        assert!(is_timing(tokens[0]), "real elapsed grammar: {line:?}");
    }
    assert!(
        lines[4].starts_with("✓ Dropped jello (") && lines[4].ends_with(')'),
        "one verdict: {:?}",
        lines[4]
    );

    // The same plan in the machine transcript: one `plan` frame, four stepped frames, summary last.
    let fork = lab.run(&["fork", "jello", "--json"]);
    let _ = lab.json(&fork, "fork jello again");
    let drop = lab.run(&["drop", "jello", "--json"]);
    let frames = lab.frames(&drop, "drop jello --json");
    let summary = frames.last().expect("drop --json summary");
    assert_eq!(summary["dropped"], "jello", "{summary}");
    assert!(summary["elapsed_ms"].as_u64().is_some(), "{summary}");
    let plan = frames
        .iter()
        .find(|frame| frame["event"] == "plan")
        .expect("drop must announce one plan");
    assert_eq!(plan["total"], 4, "{plan}");
    assert_eq!(plan["rows"].as_array().map(Vec::len), Some(4), "{plan}");
    let phases: Vec<&serde_json::Value> = frames
        .iter()
        .filter(|frame| matches!(frame["event"].as_str(), Some("started" | "finished")))
        .collect();
    assert_eq!(phases.len(), 4, "{frames:?}");
    for (frame, (step, kind)) in
        phases
            .iter()
            .zip([(1, "stop"), (2, "stop"), (3, "delete"), (4, "delete")])
    {
        assert_eq!(frame["step"], step, "{frame}");
        assert_eq!(frame["total"], 4, "{frame}");
        assert_eq!(frame["kind"], kind, "{frame}");
        assert_eq!(frame["name"], "jello", "{frame}");
    }

    // An unreachable CLI control plane (the daemon socket path is not a directory) exits 1 with
    // exactly one remedy and opens no rows; this proves the client framing, not a daemon stop.
    let fork = lab.run(&["fork", "jello", "--json"]);
    let _ = lab.json(&fork, "fork jello before the dead control plane");
    let dead = lab.state_home.join("dead-runtime-dir");
    fs::write(&dead, "a regular file, never a runtime directory\n")
        .expect("write the dead runtime placeholder");
    let unreachable_control_plane = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(["drop", "jello"])
        .current_dir(&lab.project)
        .env("NO_COLOR", "1")
        .env("XDG_RUNTIME_DIR", &dead)
        .env("XDG_STATE_HOME", &lab.state_home)
        .env("TERM", "dumb")
        .output()
        .expect("run slipfork drop without a control plane");
    assert_eq!(
        unreachable_control_plane.status.code(),
        Some(1),
        "{unreachable_control_plane:?}"
    );
    let stderr = String::from_utf8_lossy(&unreachable_control_plane.stderr);
    assert_eq!(stderr.matches("next:").count(), 1, "{stderr}");
    let stdout = String::from_utf8_lossy(&unreachable_control_plane.stdout);
    assert!(
        !stdout.contains("delete") && !stdout.contains("ms") && !stdout.contains("Dropped"),
        "an unreachable control plane must open zero rows: {stdout:?}"
    );
    assert!(
        !unreachable_control_plane.stdout.contains(&0x1b),
        "{stdout:?}"
    );

    let drop = lab.run(&["drop", "jello"]);
    assert!(
        drop.status.success(),
        "{}",
        String::from_utf8_lossy(&drop.stderr)
    );
}
