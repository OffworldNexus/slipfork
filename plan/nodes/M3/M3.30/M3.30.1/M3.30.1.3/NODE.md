---
id: M3.30.1.3
title: "Delete single-impl mock traits/doubles & write concrete Constructor DI through both composition roots"
kind: epic
status: done
priority: 3
depends_on: [M3.30.1.2]
paths:
  - Cargo.toml
  - Cargo.lock
  - ARCHITECTURE.md
  - "crates/slipfork-snow/**"
  - "crates/slipfork-dag/**"
  - "crates/slipfork-engine/**"
  - "crates/slipfork-edge/**"
  - "crates/slipfork-ospp/**"
  - "crates/slipfork-vcs/**"
  - "crates/slipfork-world/**"
  - "crates/slipfork-testkit/**"
  - "crates/slipfork/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-18T22:21"
updated: "2026-11-19T00:02"
---

# Goal
Section 4 of `plan/nodes/M3/M3.30/M3.30.1/NODE.md` (read it): delete every single-impl mock-only trait and recording double; inject concrete structs only at the composition roots (`crates/slipfork/src/main.rs`, `crates/slipfork/src/bin/slipforkd.rs`).

# Context (anchors already verified)
- `MonotonicClock` (`crates/slipfork/src/clock.rs:29`); `HandoverClock` (`daemon_unit_install.rs:67`) -> plain `Duration` / `StopBudget` values.
- `FakeClock` x3: `crates/slipfork-engine/src/btrfs_engine.rs:2820`, `crates/slipfork/src/daemon.rs:4626`, `daemon_unit_install.rs:870`.
- `HostProbe`/`FakeHostProbe` + `IngressProbe`/`RecordingIngressProbe` (`crates/slipfork/src/doctor.rs:232,348,578,603`).
- `UnitInstaller`/`RecordingInstaller`/`CapturedInstaller` (`daemon_unit_install.rs:127,681`, `main.rs:453`, `crates/slipfork/tests/critical_cli.rs:1424`).
- `BrowserOpener`/`NoBrowser` (`crates/slipfork/src/browser.rs:82`, `daemon.rs:5195`).
- `UserIdentity`/`FakeIdentity` (`crates/slipfork/src/user.rs`, `daemon_unit_install.rs:604`); `HostIdentity`.
- `OciImageStore` trait + 3 `NoOciStore` stubs (`crates/slipfork-engine/src/rootfs/mod.rs:408`, `rootfs/oci_store.rs:136`).
- `AbiProbe`, `BuildxExporter`, `RecordingEngine` (`daemon.rs:4773`), `StubRootfs` (`daemon.rs:5144`), `FakeDaemon` (`daemon_unit_install.rs:663`), `RecordingDriver` (`btrfs_engine.rs:2603`), `StubWorldEngine` (`crates/slipfork/tests/critical_edge.rs:642`), `FakeBaseImageBuilder` (actual path per implementer audit: `crates/slipfork-engine/tests/critical_rootfs_contract.rs`).
- `TestTreeDriver` in `crates/slipfork-testkit/src/btrfs.rs` — delete the `chmod 0o555` theater. Keep `SubvolumeDriver` only if a genuine second backend exists; otherwise collapse `BtrfsEngine` directly onto `KernelBtrfsDriver`.
- Update `ARCHITECTURE.md` port list to surviving genuine ports (`WorldEngine`, `SecretProvider`, `SubvolumeDriver`, `UnitPort`, `TunnelRegistrar`, `VcsWorkspace`) with concrete-wiring notes.

# Constraints
- Real lab tests only: rewrite `critical_edge` / `critical_rootfs_contract` / `critical_cli` onto real drivers, real disk, real subprocesses. Never re-introduce a fake to keep a test green.
- Known consequence: after `TestTreeDriver` dies, non-Btrfs world suites fail closed on this non-Btrfs host. That is expected; do NOT mask it. Makefile gating lands in M3.30.4.
- Scorched-earth, no aliases, no `#[deprecated]`, no RESULT.md.

# Acceptance
- `! grep -rEn "MemorySecretProvider|LayeredSecretProvider|FakeClock|RecordingInstaller|CapturedInstaller|NoOciStore|StubRootfs|FakeDaemon|TestTreeDriver|RecordingDriver|StubWorldEngine|FakeBaseImageBuilder|NoBrowser|FakeIdentity|RecordingIngressProbe|FakeHostProbe|AbiProbe|BuildxExporter|FakeClock" crates/`
- `! grep -rn "trait MonotonicClock\|trait HandoverClock\|trait UnitInstaller\|trait BrowserOpener\|trait HostProbe\|trait IngressProbe\|trait UserIdentity\|trait OciImageStore" crates/`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork-snow -p slipfork-dag -p slipfork-engine -p slipfork-edge -p slipfork-ospp -p slipfork-vcs -p slipfork-world`
- `cargo test -p slipfork --lib`
- `cargo test -p slipfork-engine --test critical_rootfs_contract`
- `cargo test -p slipfork --test critical_edge` (fail-closed without Btrfs is acceptable and must be reported, not hidden)
