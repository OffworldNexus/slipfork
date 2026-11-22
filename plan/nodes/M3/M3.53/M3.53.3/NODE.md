---
id: M3.53.3
title: "Align H-013 protected live-test assertions (crisp failed-service remedy, truthful mismatched-epoch rebuild)"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork/tests/critical_*.rs"]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-22T19:20"
updated: "2026-11-22T19:37"
---

# Goal
Make the two H-013 protected live-test assertions match shipped behaviour so `make live-test` passes 8/8: assert the real crisp failed-service remedy, and make the mismatched-epoch case mutate a real build input instead of only a stamp file. **LOAD SKILL `offworld-packaging`** for epic context; this slice is pure Rust test work.

# Context
Anchors verified 2026-11-22 (this is the ONLY active-slice work that compiles Rust — M3.53.1/M3.53.2 are YAML/shell/docs; run loneer crate checks accordingly):
- `crates/slipfork/tests/critical_world_journey.rs:1299-1346`: the failing-service journey. `BROKEN_FORKFILE` at `:104`; `up` must fail closed (`:1300-1305` through `up_failure`); the status assertion at `:1327-1340` currently accepts ANY line containing `broken` and `failed`. The shipped remedy is rendered by `crates/slipfork/src/boot/render.rs:203`: `format!("✗ Failed to {verb} {name} — check logs with: slipfork logs {name}")`. Tighten the assertion to that contract (glyph `✗ Failed to`, em-dash `—`, `check logs with: slipfork logs broken`), keep the `→ [1/1] stopping broken` teardown check at `:1346`, and leave every fail-closed remedy path intact.
- `crates/slipfork/tests/critical_fork_live.rs:971-999`: the "mismatched epoch" branch rewrites only the `PG_VERSION` stamp JSON (`world_epoch = "mismatched"`) then demands `frontend` state `built` plus an `api:pre-deploy` hook run. Untruthful: the step digest is unchanged, so the engine legitimately reports `cached`. Make it truthful by EITHER (a) mutating a real `Dockerfile`/`ARG` input in the fork project so the rebuild must happen, OR (b) accepting `cached` when the step digest already exists under `.slipfork-store/images`. Prefer (a); keep the `PG_VERSION` mtime assertion at `:963-970` (fork must never rewrite `PG_VERSION`) untouched.
- Do not weaken or delete protected assertions elsewhere; `tools/critical_guard.py` guards these files. If a fix would require editing `src/` (a genuine contract mismatch), stop and report `STATUS: blocked` (`BLOCK_KIND: missing-input`) with exact `render.rs`/engine evidence instead of editing production code from a test-only slice.

# Acceptance
- `cargo fmt --all -- --check`
- `cargo clippy -p slipfork --tests -- -D warnings`
- `cargo test -p slipfork --test critical_world_journey --no-run && cargo test -p slipfork --test critical_fork_live --no-run` (compiles clean; live suites, no unit-theater additions)
- On a Btrfs host with a tmpfs runtime dir: `SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test` passes 8/8 suites.
- `uv run --no-project tools/guard.py` exit 0 and `uv run --no-project tools/plan.py validate` exit 0.
- No `RESULT.md`; report progress via `plan.py log`.
