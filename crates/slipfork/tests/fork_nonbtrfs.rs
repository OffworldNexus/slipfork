//! `fork`/`drop` on a project that is not a Btrfs subvolume: real daemon, typed remedy.
//!
//! The project lives on `/var/tmp` (disk, never RAM `/tmp`), so the engine's own subvolume probe
//! refuses before any snapshot exists. No engine is faked and no world is booted.

use std::fs;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const ANSI: &str = "\u{1b}[";
const BTRFS_SUPER_MAGIC: u64 = 0x9123_683E;

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
fn fork_on_a_non_btrfs_project_fails_closed_with_the_btrfs_remedy() {
    assert_ne!(
        fs_magic(Path::new("/var/tmp")),
        BTRFS_SUPER_MAGIC,
        "fail closed: `/var/tmp` is Btrfs; next: run this suite on a host whose `/var/tmp` is not Btrfs"
    );
    let root = scratch();
    let output = run(&root, &["fork", "feat-auth"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not a Btrfs subvolume"), "{stderr}");
    assert!(stderr.contains("next:"), "{stderr}");
    assert!(stderr.contains("btrfs mount"), "{stderr}");
    assert_eq!(stderr.matches("next:").count(), 1, "{stderr}");
    assert!(!stderr.contains(ANSI), "piped errors stay plain: {stderr}");
    assert!(
        !root.join("demo/.forks").exists(),
        "a refusal must not snapshot the project"
    );

    let json = run(&root, &["fork", "feat-auth", "--json"]);
    assert_eq!(json.status.code(), Some(1), "{json:?}");
    let stdout = String::from_utf8_lossy(&json.stdout);
    assert!(stdout.contains("\"code\":\"not-btrfs\""), "{stdout}");
    assert!(!stdout.contains(ANSI), "--json emits zero ANSI: {stdout}");

    let drop = run(&root, &["drop", "feat-auth"]);
    assert_eq!(drop.status.code(), Some(1), "{drop:?}");
    let stderr = String::from_utf8_lossy(&drop.stderr);
    assert_eq!(stderr.matches("next:").count(), 1, "{stderr}");

    shutdown(&root);
    let _ = fs::remove_dir_all(&root);
}
