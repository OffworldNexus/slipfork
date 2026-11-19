---
id: M3.30.4.2
title: "Journey A: one rich Btrfs world E2E suite; delete daemon/engine test doubles"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.30.4.1]
paths: ["crates/slipfork/tests/**", "crates/slipfork-engine/tests/**", "crates/slipfork-testkit/**", Makefile]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-19T02:19"
updated: "2026-11-19T02:24"
---

# Goal
Section 2 Journey A of `plan/nodes/M3/M3.30/M3.30.4/NODE.md` (read it).

# Work
- Consolidate `critical_cli.rs` (21 boots), `critical_l0_wiring.rs` (4), `critical_slug.rs` (5) and engine live legs into ONE rich Btrfs journey: `up` (idempotent second `up`), bare `status`, `list`, `ps`, `logs`, `exec`, checkout edit + `restart`, no-arg `fork` with git config unset, duplicate-fork refusal, unbooted-restart refusal, `open --remote`, `drop`, `down`, `prune` — asserting every subvolume and `-pgid` process group was reaped.
- Delete `RecordingEngine`/`StubRootfs` (`daemon/tests/engine.rs`) and `StubWorldEngine` once the journey carries their coverage; keep protected assertions.
- Register it in `rs-test-live`.

# Constraints
Zero mocks; protected assertions never weakened; off-Btrfs fails closed loudly; run via `make rs-test-live` / `tools/do_vm.py test`. No RESULT.md.

# Acceptance
- no RecordingEngine/StubRootfs/StubWorldEngine under crates/
- `make -n rs-test-live` includes the journey
- `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork --lib`
