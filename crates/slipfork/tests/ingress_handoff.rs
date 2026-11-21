//! CLI-level check for `slipfork __ingress` usage errors.
//!
//! The descriptor handoff itself is proven by the module tests in `src/daemon/handoff.rs`,
//! which call `run` with real loopback sockets and no privileged port.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn scratch(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let root = PathBuf::from("/var/tmp").join(format!(
        "slipfork-ingress-{prefix}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("create the ingress scratch directory on disk");
    root
}

#[test]
fn unknown_arguments_fail_with_exactly_one_error_and_next_line() {
    // Parsing runs before any privilege check or bind, so this holds at every uid.
    let root = scratch("usage");
    let output = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(["__ingress", "--bogus"])
        .env("XDG_RUNTIME_DIR", &root)
        .output()
        .expect("run the ingress usage check");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 2, "{stderr}");
    assert!(lines[0].starts_with("error: "), "{stderr}");
    assert!(lines[1].starts_with("next: "), "{stderr}");
    fs::remove_dir_all(&root).expect("remove the scratch directory");
}
