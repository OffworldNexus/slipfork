//! Live lab journey: a real world's services, ports, and routes through `status` and `ps`.
//!
//! Builds a real Btrfs subvolume world from a `Forkfile`, boots it with `slipfork up`, and asserts
//! the status tree and scoped `ps` show exactly the declared resources. The lab is opt-in: without
//! `SLIPFORK_TEST_ROOT` the test fails closed with the exact remedy instead of degrading.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

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

/// Stops the world on every exit path, including a failed assertion.
struct WorldGuard {
    project: PathBuf,
    runtime: PathBuf,
}

impl Drop for WorldGuard {
    fn drop(&mut self) {
        let _ = run(&self.project, &self.runtime, &["down"]);
    }
}

#[test]
fn live_status_and_ps_show_the_real_world() {
    let root = test_root();
    let runtime = runtime_dir();
    fs::create_dir_all(&runtime).expect("create the live runtime directory");
    let project = root.join("status-live-demo");
    let _ = fs::remove_dir_all(&project);
    fs::create_dir_all(&project).expect("create the live project");
    fs::write(project.join("Forkfile"), FORKFILE).expect("write the live Forkfile");
    let _guard = WorldGuard {
        project: project.clone(),
        runtime: runtime.clone(),
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
    let _ = fs::remove_dir_all(&project);
}
