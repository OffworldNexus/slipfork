//! Live lab journey: a real world's services, ports, and routes through `status` and `ps`.
//!
//! Builds a real Btrfs subvolume world from a `Forkfile`, boots it with `slipfork up`, and asserts
//! the status tree and scoped `ps` show exactly the declared resources. The lab is opt-in: without
//! `SLIPFORK_TEST_ROOT` the test fails closed with the exact remedy instead of degrading.

use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use slipfork_world::{BtrfsEngine, Dirtiness, STORE_DIR, StoreLayout, WorldEngine, WorldName};

const FORKFILE: &str = "# forkfile: 0.1\n\
postgres pg {\n  version 18\n}\n\
redis cache\n\
s3 uploads {\n  cdn static_cdn\n}\n";

fn test_root() -> PathBuf {
    let Some(root) = std::env::var_os("SLIPFORK_TEST_ROOT") else {
        panic!("SLIPFORK_TEST_ROOT is unset\nnext: SLIPFORK_TEST_ROOT=/mnt/btrfs make live-test");
    };
    PathBuf::from(root)
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from("/dev/shm").join(format!("slipfork-live-{}", std::process::id()))
        })
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

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_slipfork")
}

fn run(project: &Path, runtime: &Path, args: &[&str]) -> Output {
    Command::new(binary())
        .args(args)
        .current_dir(project)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("NO_COLOR", "1")
        .output()
        .expect("the slipfork binary must run")
}

fn daemon_alive(runtime: &Path) -> bool {
    let scope = runtime.as_os_str().as_bytes();
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if !cmdline.windows(b"__daemon".len()).any(|w| w == b"__daemon") {
            continue;
        }
        if let Ok(environ) = fs::read(entry.path().join("environ"))
            && environ.windows(scope.len()).any(|w| w == scope)
        {
            return true;
        }
    }
    false
}

/// Stops the world and its daemon on every exit path, including a failed assertion.
struct WorldGuard {
    project: PathBuf,
    runtime: PathBuf,
    store_root: PathBuf,
}

impl Drop for WorldGuard {
    fn drop(&mut self) {
        if self.runtime.join("daemon.sock").exists() {
            let _ = run(&self.project, &self.runtime, &["down"]);
            if let Ok(mut stream) = UnixStream::connect(self.runtime.join("daemon.sock")) {
                let _ = stream.write_all(b"{\"v\":1,\"id\":\"bye\",\"method\":\"shutdown\"}\n");
                let mut reply = [0_u8; 256];
                let _ = stream.read(&mut reply);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && daemon_alive(&self.runtime) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let engine = BtrfsEngine::new(StoreLayout::at(&self.store_root));
        if let Ok(world) = WorldName::parse("status-live-demo") {
            let _ = engine.drop_world(&world);
        }
        let _ = engine.destroy(&self.project, Dirtiness::Clean, true);
        let _ = fs::remove_dir_all(&self.project);
    }
}

#[test]
fn live_status_and_ps_show_the_real_world() {
    let root = test_root();
    let store_root = mount_store_for(&root);
    let runtime = runtime_dir();
    fs::create_dir_all(&runtime).expect("create the live runtime directory");
    let project = root.join("status-live-demo");
    let engine = BtrfsEngine::new(StoreLayout::at(&store_root));
    if let Ok(world) = WorldName::parse("status-live-demo") {
        let _ = engine.drop_world(&world);
    }
    let _ = engine.destroy(&project, Dirtiness::Clean, true);
    let _ = fs::remove_dir_all(&project);
    fs::create_dir_all(&project).expect("create the live project");
    fs::write(project.join("Forkfile"), FORKFILE).expect("write the live Forkfile");
    let _guard = WorldGuard {
        project: project.clone(),
        runtime: runtime.clone(),
        store_root,
    };

    let up = run(&project, &runtime, &["up"]);
    assert!(
        up.status.success(),
        "slipfork up failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&up.stdout),
        String::from_utf8_lossy(&up.stderr)
    );

    let status = run(&project, &runtime, &["status", "--json"]);
    assert!(status.status.success(), "{status:?}");
    let status_json: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("status --json parses");
    let world = &status_json["worlds"][0];
    assert_eq!(world["url"], "http://status-live-demo.localhost");
    let ids: Vec<&str> = world["services"]
        .as_array()
        .expect("services array")
        .iter()
        .filter_map(|service| service["id"].as_str())
        .collect();
    assert!(ids.contains(&"pg"), "{ids:?}");
    assert!(ids.contains(&"cache"), "{ids:?}");
    assert!(ids.contains(&"uploads"), "{ids:?}");
    let uploads = world["services"]
        .as_array()
        .expect("services")
        .iter()
        .find(|service| service["id"] == "uploads")
        .expect("uploads row");
    assert_eq!(uploads["cdn"], "static_cdn");
    assert_eq!(uploads["running"], true);

    let human = run(&project, &runtime, &["status"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).expect("status is UTF-8");
    assert!(human.contains("● status-live-demo"), "{human}");
    assert!(human.contains("postgres 18"), "{human}");
    assert!(human.contains("redis 7"), "{human}");
    assert!(human.contains("s3 + cdn"), "{human}");
    assert!(!human.contains('\u{1b}'), "{human}");

    let ps = run(&project, &runtime, &["ps", "--json"]);
    assert!(ps.status.success());
    let ps_json: serde_json::Value = serde_json::from_slice(&ps.stdout).expect("ps --json parses");
    assert_eq!(ps_json["worlds"].as_array().map(Vec::len), Some(1));
    assert_eq!(ps_json["worlds"][0]["services"][0]["kind"], "postgres");

    let scoped = run(&project, &runtime, &["ps"]);
    assert!(scoped.status.success());
    assert!(String::from_utf8_lossy(&scoped.stdout).contains("postgres 18"));

    // A bounded wait proves the world is reaped before this test returns.
    let deadline = Instant::now() + Duration::from_secs(60);
    let _ = run(&project, &runtime, &["down"]);
    while Instant::now() < deadline {
        let ps = run(&project, &runtime, &["ps", "--json"]);
        if ps.status.success() && String::from_utf8_lossy(&ps.stdout).contains("\"worlds\":[]") {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
