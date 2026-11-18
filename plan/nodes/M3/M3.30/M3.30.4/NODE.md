---
id: M3.30.4
title: "Lab-first test consolidation: 1 Btrfs journey + 1 OSPP journey + pure algorithm tests & single-build Makefile"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.30.3]
paths: [Makefile, Cargo.toml, Cargo.lock, "crates/**", "tools/**", "poc/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-18T22:19"
updated: "2026-11-18T22:19"
---

# Goal
Collapse 37 redundant daemon/world spawns into two rich end-to-end journeys plus focused pure-algorithm tests, and stop `make check` from re-linking the workspace 22 times.

# Context
Audit-verified anchors (HEAD). Do NOT re-research.

## 1. Test harness speed
- `crates/slipfork/tests/support/mod.rs:105-145` runs a nested `cargo build` inside tests → delete it and use Cargo's `CARGO_BIN_EXE_*` env vars.
- `Makefile`: `make check` currently re-compiles/re-links the workspace ~22 times. Compile test targets once, run them, keep `make check` fast.

## 2. Consolidate spawns
- `crates/slipfork/tests/critical_cli.rs` — 21 world boots.
- `crates/slipfork/tests/critical_ospp_daemon.rs` — 11 boots.
- `crates/slipfork/tests/critical_l0_wiring.rs` — 4 boots.
- `crates/slipfork/tests/critical_slug.rs` — 5 boots.

### Journey A (one rich full-stack Btrfs World E2E suite)
`up` (idempotent second `up`), bare `status`, `list`, `ps`, `logs`, `exec`, checkout edit + `restart`, no-arg `fork` with git config unset, duplicate-fork refusal, unbooted-restart refusal, `open --remote`, `drop`, `down`, `prune` — and assert every subvolume and `-pgid` process group was reaped.

### Journey B (one rich OSPP v1 loopback reference-server E2E suite)
`127.0.0.1:0` reference server from `slipfork-ospp-testkit`: `slipfork auth <ws-url>` `#handle=` pairing → `<state_dir>/ospp.json` written `0600` → `slipforkd` reboot with NO `--ospp-endpoint` flag survives via the store → encrypted `subscribe` with local-vault `claims` → assert zero secret bytes on disk → live `env_updated` auto-restarts `api` → streaming HPKE `dump_ready` restore.

### Pure unit/integration tests (algorithms only)
Faceless template rendering + secret taint, DAG cycle/wave planning, HPKE/wire framing, `.localhost` HTTP/WebSocket proxy routing, git/jj workspace diffs, ANSI/`NO_COLOR`/width calculation.

## 3. Btrfs gating
- Gate the real Btrfs world-booting E2E binary so off-Btrfs hosts run fast pure/loopback tests and **fail closed** (clear message + non-zero) if the Btrfs suite is invoked directly without Btrfs — never silently skip.
- `tools/do_vm.py test` runs the full Btrfs E2E suite on the lab VM.

## 4. Stale PoC reference
- `poc/run_benchmark.py` still calls the deleted `slipfork dag plan` command — remove that call (and any dead parse of its output).

# Deliverables
- `critical_cli.rs` / `critical_ospp_daemon.rs` / `critical_l0_wiring.rs` / `critical_slug.rs` restructured into the two journeys + pure algorithm tests, with world reap assertions.
- `Makefile` `check` target: one workspace build, targeted test binaries; no nested `cargo build` from tests.
- `tools/do_vm.py test` documented path for full Btrfs E2E.

# Constraints
- Zero mocks/fakes: real disk, real git/jj subprocesses, real Btrfs, real loopback server.
- Protected `critical_*` suites may be restructured within this node's paths but their assertions must not be weakened or dropped.
- No `RESULT.md`; journey evidence goes to `LOG.md` via `plan.py log`.

# Carried over from `M3.30.1.3.3.4`
- Replace the test-only `RecordingEngine`/`StubRootfs` (`crates/slipfork/src/daemon.rs` `#[cfg(test)]`) and `StubWorldEngine` (`crates/slipfork-edge/tests/critical_edge.rs`) with the real lab drivers inside the two consolidated journeys. They are confined to test code, not production mock traits.
- Engine fixtures must create their state dir before the `statfs` probe so the off-Btrfs fail-closed line reads `BtrfsRequired`, not `statfs ... errno 2`.

# Acceptance
- `! grep -rn "Command::new(\"cargo\")\|cargo\", \"build\"" crates/slipfork/tests/support/mod.rs`
- `! grep -rn "dag plan" poc/`
- `make -n check` shows a single `cargo test`/`cargo build` compile invocation per target set.
- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork-dag -p slipfork-snow -p slipfork-ospp -p slipfork-engine --lib`
- `cargo test -p slipfork --test critical_edge --test critical_l0_wiring`
- Full Btrfs journeys are proven on the VM in `M3.30.6`; locally they must fail closed with a clear message.
