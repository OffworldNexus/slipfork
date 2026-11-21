//! Byte-for-byte guard that the shipped `--manpage` output equals `packaging/man/slipfork.1`.
//!
//! The real binary is invoked, so a change to the `clap` tree (or the renderer's page-name
//! rewriting) can never leave a stale installed manpage behind.

use std::process::Command;

#[test]
fn the_shipped_manpage_matches_the_checked_in_page() {
    let output = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--manpage")
        .env("NO_COLOR", "1")
        .output()
        .expect("the slipfork binary must be runnable");
    assert!(
        output.status.success(),
        "`slipfork --manpage` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shipped = String::from_utf8(output.stdout).expect("roff manpages are UTF-8");
    let checked_in = include_str!("../../../packaging/man/slipfork.1");
    assert_eq!(
        shipped, checked_in,
        "packaging/man/slipfork.1 drifted; regenerate it with `slipfork --manpage > packaging/man/slipfork.1`"
    );
}
