---
id: M3.20
title: "Workspace trait & test decoupling: drop SnapshotMode/StatefulRam and replace FakeIncus in slipfork-edge"
kind: task
status: done
owner: implementer
priority: 2
depends_on: [M3.1, M3.7]
paths:
  - ARCHITECTURE.md
  - "crates/slipfork-engine/src/lib.rs"
  - "crates/slipfork-engine/src/conformance.rs"
  - "crates/slipfork-engine/src/incus.rs"
  - "crates/slipfork-engine/src/reflink_netns.rs"
  - "crates/slipfork-engine/src/env.rs"
  - "crates/slipfork-engine/tests/critical_engine.rs"
  - "crates/slipfork-engine/tests/critical_oci_runtime.rs"
  - "crates/slipfork-edge/src/daemon.rs"
  - "crates/slipfork-edge/tests/critical_edge.rs"
  - crates/slipfork/src/daemon.rs
  - "crates/slipfork-dag/src/support.rs"
  - "packages/opencode-slipfork/tests/coordinator.critical.test.ts"
attempts: 1
max_attempts: 3
escalation: architect
created: "2026-11-16T17:25"
updated: "2026-11-17T03:46"
---

# Goal

Align `trait WorldEngine` with `ADR-0010` / `ARCHITECTURE.md` (`M3.7`) before `BtrfsEngine` lands (`M3.13`), and decouple `crates/slipfork-edge` + non-engine crates/packages from `IncusEngine` and stale `FICLONE` strings — keeping `make check` 100% green at every commit.

# Context

- `reviewer` and `lead` run `make check` on every leaf commit. `crates/slipfork-edge/tests/critical_edge.rs` currently imports `IncusEngine` and `SnapshotMode` (~30 call sites via `FakeIncus`), and `trait WorldEngine` still carries `SnapshotMode` / `EngineError::StatefulRamUnsupported`.
- Removing `SnapshotMode`/`StatefulRamUnsupported` across existing callers and replacing `FakeIncus` with an in-test `StubWorldEngine: WorldEngine` here ensures `BtrfsEngine` (`M3.13`) implements the final `SnapshotMode`-free `WorldEngine` trait cleanly while `make check` stays green throughout `M3.12` -> `M3.21`.

# Design / Invariants

1. Delete `SnapshotMode` and `EngineError::StatefulRamUnsupported` from `crates/slipfork-engine/src/lib.rs` (`create_snapshot` and `freeze_world` take `(&self, source, snapshot)` with no mode flag, matching `ARCHITECTURE.md` §3.3) and update existing callers/tests in `conformance.rs`, `incus.rs`, `reflink_netns.rs`, `critical_engine.rs`, `critical_oci_runtime.rs`, and `crates/slipfork/src/daemon.rs`.
2. In `crates/slipfork-edge/tests/critical_edge.rs`, replace `FakeIncus` + `IncusEngine` with a direct in-test `StubWorldEngine: WorldEngine` (tracking created/destroyed world handles and snapshots in memory without spinning up a fake `/1.0` HTTP socket). Preserve every edge/registry/proxy/share assertion.
3. Scrub stale engine/reflink prose and mock error strings in `crates/slipfork-edge/src/daemon.rs`, `crates/slipfork-dag/src/support.rs`, `crates/slipfork-engine/src/env.rs`, and `packages/opencode-slipfork/tests/coordinator.critical.test.ts` (replace `"FICLONE unsupported"` mock error text with `"BtrfsRequired: state dir must be btrfs"`).

# Acceptance

```bash
! grep -rn 'SnapshotMode\|StatefulRam' crates/
! grep -rniE 'incus|reflink|ficlone|zfs' crates/slipfork-edge/ crates/slipfork-dag/ crates/slipfork-engine/src/env.rs packages/opencode-slipfork/
bun test packages/opencode-slipfork/tests
cargo test -p slipfork-edge --all-targets
make check
```

Expected: both inverted `grep`s print nothing; `bun test`, `slipfork-edge` tests, and full `make check` exit 0.

## Load envelope (owner ruling 2026-11-16, binding)

`make check` must be green at load average ≤ 10 with zero in-repo builds. The pre-`Btrfs` engines are still present until `M3.21`, and their fixed 10 s `WORLD_STOP_GRACE` red-lines world teardown under unrelated foreign load (≈ 40+). When that is the **only** red (`world init pid … is still alive 10s after SIGKILL`), record it verbatim as the deleted engine's residual owned by `M3.13`/`M3.14`/`M3.21` and verify the deterministic gate set instead: `uv run --no-project --with pytest pytest -q tools/tests`, `cargo check --workspace --all-targets`, `cargo clippy --workspace --all-targets -- -D warnings`, `make docs-check`, `guard.py`, `critical_guard.py`, `plan.py validate`, plus the suites this node owns. Never retry-until-green and never hide the red.

# Terminology Rule (OWNER, 2026-11-16 — binding)

Never call the architecture "Option A" (or reference "Option B", "Spike A", "Spike B", or any "we chose X over Y" framing). There was only ever one design: **`BtrfsEngine` over one unified Btrfs subvolume per world + Linux `userns`/`netns`/`ipcns`/`pidns`/`mntns` + `pivot_root` + daemonless static `crun`**. Write it as what Slipfork is and does.

Enforceable gate before Phase 1 completes (owner scope):

```bash
! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/
```

If this node writes any file under `docs/`, `crates/`, or `tools/`, add that inverted grep (scoped to your paths) to your own verification and delete rather than reword any alternative-architecture history.
