---
id: M3.2.18
title: "Live end-to-end slipfork test -j 4 on the Btrfs DO VM (warm pool, typed coverage, output survival, prune)"
kind: task
status: superseded
owner: implementer
priority: 5
depends_on: [M3.2.17]
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-18T22:19"
---

# Goal

Prove `0.3.0` end-to-end on the Btrfs DO VM: a real `slipfork test -j 4` run over a multi-slot warm pool with typed coverage, `SLIPFORK_OUTPUT_DIR` survival, `pytest-slipfork` and `CommandTestAdapter` both exercised, and every measured number recorded.

# Context

- This container cannot run kernel Btrfs, so the only honest end-to-end proof is the remote VM from `M3.16`. Everything this node needs is merged: engine + pool + coverage + CLI + docs (`M3.2.1`–`M3.2.17`).
- The run must use the shipped binary and a real project manifest, not a unit fixture.

# Design / Invariants

1. On the VM: `tools/do_vm.py up`, `sync`, then a real `slipfork test -j 4 --json` run over a small two-adapter project (a `pytest-slipfork` suite plus a `CommandTestAdapter` suite), verifying four world slots lease concurrently, each on its own `test-<slot>` origin.
2. Verify: `coverage = true` with `coverage_report = ["html","lcov"]` produces both report files and the exit code honours `coverage_fail_under` (run once above and once below the measured percentage); `.slipfork/runs/<run_id>/<slot>/output` exists for every slot after teardown; `pytest-slipfork`'s warm Zygote path is observable in the run log.
3. Verify `slipfork prune --dry-run` then `--yes` reclaims the run's snapshots and leaves zero world subvolumes while keeping the run tree directories.
4. Record in `RESULT.md`: the exact commands, the four slot origins, wall time, per-item durations, coverage percentages, prune reclaim bytes, and `slipfork doctor` output on the VM. Cross-check pool/snapshot timings against `docs/research/spike-option-a-btrfs-crun.md` and note deviations.
5. Verification node: no product code. Any failure returns `STATUS: blocked` with the exact command and first failing assertion — never a workaround or a hand-edited expectation.
6. Finish with the exact `down --yes` command in `RESULT.md` (the owner decides when to destroy the droplet).

# Acceptance

```bash
uv run --no-project tools/do_vm.py status
uv run --no-project tools/do_vm.py sync
uv run --no-project tools/do_vm.py test
uv run --no-project tools/do_vm.py ssh -- slipfork test -j 4 --json
uv run --no-project tools/do_vm.py ssh -- slipfork prune --dry-run
uv run --no-project tools/do_vm.py ssh -- find .slipfork/runs -name output -type d
uv run --no-project tools/do_vm.py ssh -- slipfork doctor
```

Expected: `status`/`sync`/`test` exit 0; the `slipfork test -j 4 --json` run prints exactly one JSON document with four distinct slot origins and per-item results, exit code 0; `prune --dry-run` lists the run's snapshots; the `find` prints one `output` directory per slot; `doctor` reports Btrfs `ok` fields. `RESULT.md` records all numbers plus the teardown command.
