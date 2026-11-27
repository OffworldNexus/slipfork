---
id: M3.180
title: "Fix PR #8 CI reds: silent cached image rows on up, padded counter match, and Windows LF Forkfile checkout"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - .gitattributes
  - "crates/slipfork/src/daemon/reconcile/**"
  - crates/slipfork/src/daemon/boot.rs
  - "crates/slipfork/src/boot/**"
  - "crates/slipfork-image/src/puller.rs"
  - "crates/slipfork-image/src/error.rs"
  - "crates/slipfork-image/src/step_log.rs"
  - "crates/slipfork-image/src/builder/mod.rs"
  - "crates/slipfork-image/src/plan/expand.rs"
  - "crates/slipfork-image/src/plan/planner.rs"
  - crates/slipfork/tests/critical_world_journey.rs
  - crates/slipfork/tests/critical_status_live.rs
  - "crates/slipfork-forkfile/src/tests/format.rs"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T19:22"
updated: "2026-11-25T20:47"
---

# Goal

Turn GitHub Actions CI green on `release/0.1.1` (PR #8, failing in run `45670641396` at `4a20e55`).

# Evidence

1. **Cached image reconciles emit `→` + inner `✓ 1/N..N/N` rows on `up` (`critical_world_journey.rs:3408` & `:1273` failing in job `139404664170`):**
   - `changed_build_inputs_rebuild_and_restart_converges` (`crates/slipfork/tests/critical_world_journey.rs:3408`) panics because a warm `slipfork up` after `--no-cache` prints `→ [1/2] image app running` + `✓ 1/5..5/5` (leaking `echo built-three` into stdout) before `✓ [1/2] image app cached 3ms`, violating `specs/02-cli-ux.md:159` (*"a cached image emits `✓ [1/2] image web cached 8ms` with zero inner rows"*).
   - `world_up_restart_down_journey` (`crates/slipfork/tests/critical_world_journey.rs:1273-1289`) fails `first_out.contains("→ [1/")` against right-padded `→ [ 1/11]` when `total >= 10` and checks `built ` when the image hit cache.
2. **Windows CRLF checkout breaks `slipfork-forkfile` (`crates/slipfork-forkfile/src/tests/format.rs:240` failing in jobs `139404664164` & `139404664183`):**
   - `include_str!("../../../../support/demo/Forkfile")` checks out with `\r\n` on `windows/x86_64` and `windows/aarch64` because no `.gitattributes` pins `* text=auto eol=lf`.

# Acceptance

- Add `.gitattributes` (`* text=auto eol=lf`) and verify `cargo test -p slipfork-forkfile`.
- Cached `image` nodes (`cache_misses == 0`) settle as one `✓ [i/n] image <id> cached <dur>` row with zero inner substep rows; executed builds emit inner `1/N..N/N` rows and `built N step(s)`.
- `critical_world_journey` passes all 5 tests on real Btrfs (`make live-test`), and `cargo test --workspace` + `tools/guard.py` exit `0`.
