---
id: M3.30.1.3.3.4
title: Concrete unit installer + delete last recording/stub doubles
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.30.1.3.3.3]
paths:
  - Cargo.toml
  - Cargo.lock
  - ARCHITECTURE.md
  - "docs/design/dx-0.1.0-ux.md"
  - "crates/slipfork/**"
  - "crates/slipfork-edge/**"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-18T23:13"
updated: "2026-11-18T23:52"
---

# Goal
Split 4/4 of `plan/nodes/M3/M3.30/M3.30.1/M3.30.1.3/M3.30.1.3.3/NODE.md`: the last doubles leave the tree.

# Work
- `UnitInstaller`/`RecordingInstaller`/`CapturedInstaller` -> one concrete installer struct constructed with the real unit-directory path (production `$XDG_CONFIG_HOME/systemd/user`; tests a real disk temp dir under `~/.cache/slipfork`), writing real unit files with correct modes. Live `systemctl --user` legs fail closed with one clear line where absent — never skip green.
- Delete `RecordingEngine`, `StubRootfs` (daemon.rs tests), `FakeDaemon` (`daemon_unit_install.rs`), `StubWorldEngine` (actual path: `crates/slipfork-edge/tests/critical_edge.rs:11`), any `FakeBaseImageBuilder` remnant.
- Rewrite `crates/slipfork/tests/critical_cli.rs`, `critical_l0_wiring.rs`, `crates/slipfork-edge/tests/critical_edge.rs` onto concrete types; keep every protected assertion and the existing loud Btrfs fail-closed gate (sib. M3.30.4 consolidates later).

# Constraints
No traits/mocks/`#[cfg(test)]` hatches; both roots remain the only ambient readers; no aliases. No RESULT.md.

# Acceptance
- `! grep -rEn "RecordingInstaller|CapturedInstaller|FakeDaemon|trait UnitInstaller" crates/`
- Test-only `RecordingEngine`/`StubRootfs`/`StubWorldEngine` deletions are carried over to `M3.30.4` (logged in its `NODE.md`); they are confined to `#[cfg(test)]`/`tests/**` here.
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork --lib`
- `cargo test -p slipfork --test critical_cli` (off-Btrfs fails closed; report the line)
