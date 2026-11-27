---
id: M3.184
title: "Restore critical_fork_live: merged plan positions, drop rows, adopted-image stamp, unwritable-registry refusal"
kind: task
status: done
owner: implementer
priority: 2
depends_on: [M3.180]
paths:
  - crates/slipfork/tests/critical_fork_live.rs
  - "crates/slipfork/src/daemon/lifecycle/**"
  - "crates/slipfork/src/daemon/image/**"
  - "crates/slipfork/src/daemon/reconcile/**"
  - "crates/slipfork/src/world/**"
  - crates/slipfork/src/boot.rs
  - "crates/slipfork/src/boot/**"
  - "crates/slipfork/src/ui/report/**"
  - "crates/slipfork-world/src/boot/stamp/**"
  - "crates/slipfork-world/src/ledger/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T19:48"
updated: "2026-11-25T21:43"
---

# Goal

Restore `critical_fork_live` (live Btrfs) after the M3.164–M3.179 Report/plan reorganisation: 4 of its 5 flip tests fail on a clean `4a20e55` + M3.181 tree.

# Evidence (2026-11-25, VM `165.232.86.174`)

Run: clean `4a20e55` + the M3.181 patch at `/home/slipfork/m3181`, private Btrfs `SLIPFORK_TEST_ROOT=/mnt/m3181`, `XDG_RUNTIME_DIR=/dev/shm/m3181-runtime`, `CARGO_TARGET_DIR=/var/tmp/m3181-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `cargo test -p slipfork --features slipfork/live --test critical_fork_live -- --test-threads=1` → **1 passed, 4 failed (156s)**:

- `fork_and_drop_a_live_world_without_leaving_snapshots` (`crates/slipfork/tests/critical_fork_live.rs:751`): the `image backend` `finished` frame echoes `"step":1,"total":14`, but its merged plan position is 3 (`fork feat-auth`). Cached/duplicate image frames must carry their merged row index and denominator.
- `fork_adopts_a_root_context_image_cached_across_a_branch_switch` (`:1725`): the `rootctx` world stamp is `{"before_start":{},"pre_deploy":{},"tools":{},"world_epoch":"<hex>"}` — there is **no `images` map**, so an adopted (cached) root-context image is never stamped and the later rebuild/`image_rootfs` lookup cannot resolve.
- `drop_prints_one_four_row_plan_and_json_mirrors_it` (`:2151`): `NO_COLOR slipfork drop jello` prints 3 lines instead of 5 (four frames + verdict); only `→ [2/4] stop jello` and `→ [4/4] delete jello` survive, so rows `[1/4]` and `[3/4]` are dropped from both the TTY plan and `--json`.
- `failed_fork_publish_rolls_back_without_leaking` (`:1373`): with `state_dir` made unwritable, `slipfork fork rollback --json` incorrectly **succeeds**; it must fail writing the registry (transcript must contain `worlds.json`) and roll back without leaking.

`critical_world_journey`'s 2 failures in the same run (`:1273` `[ 1/11]` prefix, `:3408` cached inner rows) belong to **M3.180** — do not duplicate.

# Acceptance

- `cargo test -p slipfork --features slipfork/live --test critical_fork_live -- --test-threads=1` green on real Btrfs (`SLIPFORK_TEST_ROOT=<btrfs>`, tmpfs `XDG_RUNTIME_DIR`).
- `cargo test --workspace` green; no protected `critical_*` assertion weakened or deleted.
