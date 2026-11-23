---
id: M3.30.1
title: "Purge dead weight: forked snow resolver, 14 mock/fake doubles, support-matrix bureaucracy & duplicated helpers"
kind: epic
status: done
priority: 3
depends_on: [M3.27]
paths:
  - Cargo.toml
  - Cargo.lock
  - Makefile
  - "crates/**"
  - "support/**"
  - "tools/**"
  - "docs/support-matrix.md"
  - zensical.toml
  - ARCHITECTURE.md
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-18T22:19"
updated: "2026-11-19T00:02"
---

# Goal
Ship only code that runs and is executed by tests. Delete the forked `snow` engine inside `daemon`, all `0.00%`-covered bureaucracy, every single-impl mock-only trait / recording double, and all copy-pasted helper forks — leaving one canonical owner module per concern and strict Constructor IoC of concrete structs.

# Context
Audit-verified anchors (HEAD). Do NOT re-research, do NOT keep aliases or shims (pre-`1.0` scorched earth).

## 1. Forked `snow` engine → single canonical resolver
- `crates/slipfork/src/daemon.rs:4185` `render_claimed` (~80 LOC) — delete.
- `crates/slipfork/src/daemon.rs:4109` `resolve_mode_local` (~76 LOC) — delete.
- Extend the canonical owner `crates/slipfork-snow/src/faceless.rs` and `crates/slipfork-snow/src/model.rs` with the partial/claimed evaluation entry point, then make `daemon` call it. `${{ }}` parsing, secret taint, and normalization stay exclusively inside `slipfork-snow`.

## 2. `0.00%`-executed bureaucracy (delete files + wiring)
- `crates/slipfork-dag/src/support.rs`, `crates/slipfork-dag/tests/critical_support.rs`
- `support/integrations.toml`, `tools/check_support.py`, `docs/support-matrix.md`
- Remove from `zensical.toml` nav and drop the `support-check` / `support-sync` / `rs-test` targets from `Makefile`.
- `Commands::Support` variant at `crates/slipfork/src/cli.rs:213` + dispatch/render at `crates/slipfork/src/main.rs:1355-1399`.

## 3. Dead provider/conformance/event code
- `MemorySecretProvider` `crates/slipfork-snow/src/provider.rs:216`; `LayeredSecretProvider` `crates/slipfork-snow/src/provider.rs:465` (0%-covered fallback).
- Prod-compiled conformance modules: `crates/slipfork-engine/src/conformance.rs`, `crates/slipfork-engine/src/rootfs/conformance.rs`, `crates/slipfork-snow/src/conformance.rs` — delete; move any assertion worth keeping into the owning crate's `tests/`.
- Unemitted `BootEvent::Secrets` (`crates/slipfork-edge/src/daemon.rs` producer enum, rendered at `crates/slipfork/src/main.rs:1476`) — delete variant + arm.

## 4. Single-impl mock-only traits & doubles (keep concrete DI!)
Delete the trait AND its doubles; replace with the concrete struct injected at the composition root (`crates/slipfork/src/main.rs`, `crates/slipfork/src/bin/slipforkd.rs`):
- `MonotonicClock` `crates/slipfork/src/clock.rs:29`; `HandoverClock` `crates/slipfork/src/daemon_unit_install.rs:67` → plain `Duration` / `StopBudget` values.
- `FakeClock` x3: `crates/slipfork-engine/src/btrfs_engine.rs:2820`, `crates/slipfork/src/daemon.rs:4626`, `crates/slipfork/src/daemon_unit_install.rs:870`.
- `HostProbe`/`FakeHostProbe` `crates/slipfork/src/doctor.rs:232,348`; `IngressProbe`/`RecordingIngressProbe` `crates/slipfork/src/doctor.rs:578,603`.
- `UnitInstaller`/`RecordingInstaller`/`CapturedInstaller` `crates/slipfork/src/daemon_unit_install.rs:127,681`, `crates/slipfork/src/main.rs:453`, `crates/slipfork/tests/critical_cli.rs:1424`.
- `BrowserOpener`/`NoBrowser` `crates/slipfork/src/browser.rs:82`, `crates/slipfork/src/daemon.rs:5195`.
- `UserIdentity`/`FakeIdentity` `crates/slipfork/src/user.rs`, `crates/slipfork/src/daemon_unit_install.rs:604`; `HostIdentity`.
- `OciImageStore` trait + 3 `NoOciStore` stubs: `crates/slipfork-engine/src/rootfs/mod.rs:408`, `crates/slipfork-engine/src/rootfs/oci_store.rs:136`.
- `AbiProbe`, `BuildxExporter`, `RecordingEngine` `crates/slipfork/src/daemon.rs:4773`, `StubRootfs` `crates/slipfork/src/daemon.rs:5144`, `FakeDaemon` `crates/slipfork/src/daemon_unit_install.rs:663`, `RecordingDriver` `crates/slipfork-engine/src/btrfs_engine.rs:2603`, `StubWorldEngine` `crates/slipfork/tests/critical_edge.rs:642`, `FakeBaseImageBuilder` `crates/slipfork/tests/critical_rootfs_contract.rs:373`.
- `TestTreeDriver` in `crates/slipfork-testkit/src/btrfs.rs` — delete the `chmod 0o555` theater. Keep `SubvolumeDriver` only if a genuine second backend exists; otherwise collapse `BtrfsEngine` directly onto `KernelBtrfsDriver`.

## 5. Duplicated helper forks (one canonical owner each)
- Merkle glob hashing x3: `crates/slipfork-dag/src/lib.rs:350`, `crates/slipfork-dag/src/fs.rs:30`, `crates/slipfork-engine/src/rootfs/input.rs:20`.
- Identical 16 KiB TCP relay loops: `crates/slipfork-edge/src/relay.rs:57` == `crates/slipfork-engine/src/bin/slipfork-world-init.rs:433`.
- ALTCHA x2: `crates/slipfork-ospp/src/pairing.rs:133` vs `crates/slipfork-ospp-testkit/src/reference_server.rs:723`.
- `quoted()` `crates/slipfork-dag/src/world.rs:223` vs `crates/slipfork-engine/src/rootfs/mod.rs:717`; `world_state_label()` `crates/slipfork/src/commands/status.rs:142` vs `crates/slipfork/src/commands/ps.rs:111`; `absolute_env()` `crates/slipfork/src/main.rs:209` vs `crates/slipfork/src/bin/git-remote-slipfork.rs:100`.
- Remove the `static AtomicU64` at `crates/slipfork-engine/src/rootfs/mod.rs:725`.

# Deliverables
- ~1.5k+ LOC of production code and all mock doubles gone; `slipfork-snow` is the only `snow` evaluator.
- `Cargo.toml` / `Cargo.lock` pruned of crates that only existed for deleted test doubles.
- `ARCHITECTURE.md` port-trait list updated to the surviving genuine ports (`WorldEngine`, `SecretProvider`, `SubvolumeDriver`, `UnitPort`, `TunnelRegistrar`, `VcsWorkspace`) with concrete-wiring notes.
- `Makefile` targets `support-check` / `support-sync` / `rs-test` removed; `zensical.toml` nav no longer lists `support-matrix.md`.

# Constraints
- Keep strict Constructor IoC: composition root only, no globals, no `OnceLock`, no `std::env::var` in library crates.
- Delete atomically — no `#[deprecated]`, no alias modules, no "kept for tests" comments.
- Never weaken protected `critical_*` suites; only the `critical_support.rs` file above and the enumerated doubles are authorized deletions.
- Do not touch `docs/**` prose beyond removing the support-matrix nav/file (docs rewrite is `M3.30.5`).

# Acceptance
- `! test -e crates/slipfork-dag/src/support.rs && ! test -e support/integrations.toml && ! test -e docs/support-matrix.md && ! test -e tools/check_support.py`
- `! grep -rn "render_claimed\|resolve_mode_local" crates/slipfork/src/daemon.rs`
- `! grep -rEn "MemorySecretProvider|LayeredSecretProvider|FakeClock|RecordingInstaller|CapturedInstaller|NoOciStore|StubRootfs|FakeDaemon|TestTreeDriver|RecordingDriver|StubWorldEngine" crates/`
- `! grep -rn "support-matrix\|check_support" zensical.toml Makefile`
- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork-snow -p slipfork-dag -p slipfork-engine -p slipfork-edge -p slipfork-ospp`
- `cargo test -p slipfork --test critical_edge --test critical_rootfs_contract`
