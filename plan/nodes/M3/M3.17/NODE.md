---
id: M3.17
title: "poc benchmark + support registry on the Btrfs engine (fails closed off-Btrfs)"
kind: task
status: ready
owner: implementer
priority: 4
depends_on: [M3.21]
paths:
  - poc/run_benchmark.py
  - tools/tests/test_run_benchmark.py
  - support/integrations.toml
  - "docs/support-matrix.md"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:58"
updated: "2026-11-16T17:25"
---

# Goal

Align the PoC benchmark harness and the integration support registry with the Btrfs-only world engine: `poc/run_benchmark.py` measures `BtrfsEngine` subvolume snapshot/clone costs (failing closed off-Btrfs), and `support/integrations.toml` + `docs/support-matrix.md` name the shipped Btrfs engine evidence instead of the deleted reflink engine.

# Context

- `poc/run_benchmark.py` (2339 lines) still measures a reflink-based topology; `M3` exit criterion 1 only requires it to stay bounded and honest, but it must not import or describe a deleted engine.
- `support/integrations.toml` carries a `reflink-netns-world-engine` entry whose `critical_test` pointer must name a suite that still exists after `M3.14`.

# Design / Invariants

1. `poc/run_benchmark.py`: replace the reflink clone step with a `BtrfsEngine`/`KernelBtrfsDriver` subvolume snapshot + writable clone measurement; probe `statfs` first and exit non-zero with the `BtrfsRequired` mount hint on a non-Btrfs host (never silently report zeros). Keep the existing report schema, `--check` drift tolerance, and `--write-report` mode.
2. Record in the script header that this host (XFS) can only exercise the fail-closed branch and that the measured report is regenerated on the Btrfs DO VM (`M3.19`).
3. `support/integrations.toml`: rename the reflink engine entry to a Btrfs-specific id, point every `critical_test` value at a test function that exists in `crates/slipfork-engine/tests/**` or `crates/slipfork/tests/**` after `M3.14`/`M3.15`, and keep tiers honest (`declared` only with a recipe page plus critical test). No entry for a deleted engine.
4. Regenerate `docs/support-matrix.md` with `make support-sync` (never hand-edit it), then verify parity with `make support-check`.
5. `tools/tests/test_run_benchmark.py` covers the new probe/clone behaviour (fake `statfs`, fake driver) and still never contacts a real droplet.

# Acceptance

```bash
uv run --no-project --with pytest pytest -q tools/tests/test_run_benchmark.py
uvx --offline ruff check poc tools && uvx --offline ruff format --check poc tools
! grep -rniE 'incus|reflink|ficlone|zfs' poc/run_benchmark.py support/integrations.toml
grep -n 'btrfs' support/integrations.toml
make support-sync
make support-check
uv run --no-project poc/run_benchmark.py --check; test $? -ne 0
uv run --no-project poc/run_benchmark.py --check 2>&1 | grep -n 'user_subvol_rm_allowed,noatime'
```

Expected: the targeted pytest and both `ruff` commands exit 0; the inverted `grep` prints nothing; a `btrfs` integration id exists; `support-sync` then `support-check` exit 0 with no matrix drift; on this XFS host `--check` exits non-zero and prints the exact mount hint.

# Terminology Rule (OWNER, 2026-11-16 — binding)

Never call the architecture "Option A" (or reference "Option B", "Spike A", "Spike B", or any "we chose X over Y" framing). There was only ever one design: **`BtrfsEngine` over one unified Btrfs subvolume per world + Linux `userns`/`netns`/`ipcns`/`pidns`/`mntns` + `pivot_root` + daemonless static `crun`**. Write it as what Slipfork is and does.

Enforceable gate before Phase 1 completes (owner scope):

```bash
! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/
```

If this node writes any file under `docs/`, `crates/`, or `tools/`, add that inverted grep (scoped to your paths) to your own verification and delete rather than reword any alternative-architecture history.
