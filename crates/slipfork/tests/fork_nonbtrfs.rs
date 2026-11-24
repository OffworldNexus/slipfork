//! `fork`/`drop` with no running world: fail closed with one typed remedy line.
//!
//! The scratch project lives on `/var/tmp` (disk, never RAM `/tmp`) and is deliberately never
//! booted. `fork` refuses with `no-world` before touching the checkout, and `drop` refuses with
//! `no-fork` for a fork this world has never created. No engine is faked and no world is booted.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const ANSI: &str = "\u{1b}[";

fn scratch() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock reads after the epoch")
        .as_nanos();
    let root =
        PathBuf::from("/var/tmp").join(format!("slipfork-fork-{}-{nanos}", std::process::id()));
    fs::create_dir_all(root.join("demo")).expect("create the scratch project");
    root
}

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(args)
        .current_dir(root.join("demo"))
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("XDG_STATE_HOME", root.join("state"))
        .output()
        .expect("the slipfork binary must be runnable")
}

fn shutdown(root: &Path) {
    let socket = root.join("run/daemon.sock");
    if let Ok(mut stream) = UnixStream::connect(socket) {
        let _ = stream.write_all(b"{\"v\":1,\"id\":\"bye\",\"method\":\"shutdown\"}\n");
        let mut reply = [0_u8; 256];
        let _ = stream.read(&mut reply);
    }
}

#[test]
fn fork_and_drop_without_a_running_world_fail_closed() {
    let root = scratch();
    let project = root.join("demo");

    // A `fork` with no running world must name the missing prerequisite and snapshot nothing.
    let fork = run(&root, &["fork", "feat-auth"]);
    assert_eq!(fork.status.code(), Some(1), "{fork:?}");
    let stderr = String::from_utf8_lossy(&fork.stderr);
    assert_eq!(stderr.matches("next:").count(), 1, "{stderr}");
    assert!(stderr.contains("next: run `slipfork up` first"), "{stderr}");
    assert!(!stderr.contains(ANSI), "piped errors stay plain: {stderr}");
    assert!(
        !project.join(".forks").exists(),
        "a refusal must not snapshot the project"
    );

    let fork_json = run(&root, &["fork", "feat-auth", "--json"]);
    assert_eq!(fork_json.status.code(), Some(1), "{fork_json:?}");
    let stdout = String::from_utf8_lossy(&fork_json.stdout);
    assert!(stdout.contains("\"code\":\"no-world\""), "{stdout}");
    assert!(!stdout.contains(ANSI), "--json emits zero ANSI: {stdout}");
    assert!(
        !project.join(".forks").exists(),
        "`--json` must not snapshot the project either"
    );

    // `drop` for a fork this world never created must name the missing fork, not a missing world.
    let drop = run(&root, &["drop", "feat-auth"]);
    assert_eq!(drop.status.code(), Some(1), "{drop:?}");
    let stderr = String::from_utf8_lossy(&drop.stderr);
    assert_eq!(stderr.matches("next:").count(), 1, "{stderr}");
    assert!(
        stderr.contains("next: run `slipfork ps` to list this world's forks"),
        "{stderr}"
    );
    assert!(!stderr.contains(ANSI), "piped errors stay plain: {stderr}");

    let drop_json = run(&root, &["drop", "feat-auth", "--json"]);
    assert_eq!(drop_json.status.code(), Some(1), "{drop_json:?}");
    let stdout = String::from_utf8_lossy(&drop_json.stdout);
    assert!(stdout.contains("\"code\":\"no-fork\""), "{stdout}");
    assert!(!stdout.contains(ANSI), "--json emits zero ANSI: {stdout}");
    assert!(
        !project.join(".forks").exists(),
        "a refused drop must not create a checkout"
    );

    shutdown(&root);
    let _ = fs::remove_dir_all(&root);
}
