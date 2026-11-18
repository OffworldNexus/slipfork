---
id: M3.30.1.3.3
title: "CLI roots: concrete identity/browser/probe/installer DI, delete remaining doubles"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.30.1.3.2]
paths:
  - Cargo.toml
  - Cargo.lock
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
created: "2026-11-18T22:46"
updated: "2026-11-18T22:46"
---

# Goal
Slice 3 of `plan/nodes/M3/M3.30/M3.30.1/M3.30.1.3/NODE.md`: the CLI crate is concrete-only and both composition roots wire real structs.

# Work
- `UserIdentity`/`FakeIdentity`/`HostIdentity` -> concrete identity value built in `main.rs`.
- `BrowserOpener`/`NoBrowser` -> one concrete opener; headless/refusal remains a real code path (no fake).
- `HostProbe`/`FakeHostProbe`, `IngressProbe`/`RecordingIngressProbe` -> concrete probes.
- `UnitInstaller`/`RecordingInstaller`/`CapturedInstaller` -> concrete systemd unit installer; tests prove it against a real temp `$XDG_CONFIG_HOME` unit dir (bytes + `0644`/`0600` modes); live `systemctl --user` leg fails closed with a clear message where absent.
- Delete `RecordingEngine`, `StubRootfs` (daemon.rs), `FakeDaemon`, `FakeClock`, `AbiProbe`/`BuildxExporter` remnants in the CLI, `StubWorldEngine` (critical_edge), `FakeBaseImageBuilder` (critical_rootfs_contract if still present).
- Rewrite `crates/slipfork/tests/critical_edge.rs`, `critical_cli.rs`, `critical_l0_wiring.rs` onto real drivers/disk/subprocesses; never weaken protected assertions.

# Constraints
- Both roots (`crates/slipfork/src/main.rs`, `bin/slipforkd.rs`) are the ONLY place env/proc/sockets are read; no globals; no `#[cfg(test)]` injection hatches; no mocks/fakes; no aliases; no RESULT.md.

# Acceptance
- `! grep -rEn "MemorySecretProvider|FakeClock|RecordingInstaller|CapturedInstaller|StubRootfs|FakeDaemon|RecordingEngine|StubWorldEngine|FakeBaseImageBuilder|NoBrowser|FakeIdentity|FakeHostProbe|RecordingIngressProbe" crates/`
- `! grep -rn "trait MonotonicClock\|trait HandoverClock\|trait UnitInstaller\|trait BrowserOpener\|trait HostProbe\|trait IngressProbe\|trait UserIdentity\|trait OciImageStore\|trait AbiProbe\|trait BuildxExporter" crates/`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork --lib`
- `cargo test -p slipfork --test critical_edge` (fail-closed off-Btrfs is acceptable, report it)
