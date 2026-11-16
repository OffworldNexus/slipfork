---
id: M3.19
title: "Live verification on the Btrfs DO VM: BtrfsEngine + crun + postgres:18 suites, measured numbers, reboot-safe SSH"
kind: task
status: ready
owner: implementer
priority: 5
depends_on: [M3.11, M3.16, M3.18]
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:58"
updated: "2026-11-16T17:25"
---

# Goal

Prove the Btrfs + `crun` world engine live: on the Btrfs DO VM from `M3.16`, run the real `KernelBtrfsDriver` + `BtrfsEngine` + `crun` + `postgres:18` suites green, record the measured snapshot/clone/warm-SQL numbers, and confirm the harness survives a droplet reboot without losing SSH access.

# Context

- This container cannot run kernel Btrfs (XFS rootfs, no `/dev/btrfs-control`), so every real-kernel claim in `ADR-0010` has exactly one place to be verified: the remote VM.
- Everything this node needs must already be merged: the engine (`M3.13`), engine tests (`M3.14`), daemon/CLI (`M3.15`), harness (`M3.16`).

# Design / Invariants

1. Run, in order, keeping the full streamed output in `RESULT.md`: `tools/do_vm.py up`, `status`, `sync`, `test`; the remote suites are `critical_btrfs_live` for `slipfork-engine` and for `slipfork`, both serialized.
2. Assert the real path is exercised: `statfs` magic `0x9123683e` on `/var/lib/slipfork`, a RO snapshot, two sibling writable clones with independent `postgres:18` state, both clones binding the same in-world port without SysV IPC collisions (`--ipc`), and clear-RO-then-destroy teardown followed by `slipfork prune --yes`, leaving no subvolume for any world this scenario created (`btrfs subvolume list /var/lib/slipfork | grep -v '/snapshots/'` empty). The digest-keyed sealed base under `<state_dir>/snapshots/` legitimately survives until the Phase-2 `M3.2.13` GC lands — do not assert a globally empty list.
3. Re-run the benchmark remotely (`uv run --no-project poc/run_benchmark.py --write-report`) and paste the measured snapshot/clone/warm-SQL/RSS numbers plus the host fingerprint into `RESULT.md`; cross-check them against `docs/research/btrfs-crun-benchmarks.md` and note any material deviation (do not edit the benchmark report here).
4. Reboot check: reboot the droplet, then confirm `status`/`ssh` still authenticate with the persistent `--identity` key and that `/var/lib/slipfork` remounts with `user_subvol_rm_allowed,noatime`.
5. This is a verification node: it writes no product code. Any failure returns `STATUS: blocked` naming the exact command and first failing assertion — never a workaround.
6. Cost control: finish with the exact `down --yes` command printed in `RESULT.md` (the owner decides when to destroy the droplet).

# Acceptance

```bash
uv run --no-project tools/do_vm.py status
uv run --no-project tools/do_vm.py sync
uv run --no-project tools/do_vm.py test
uv run --no-project tools/do_vm.py ssh -- findmnt -no OPTIONS /var/lib/slipfork
uv run --no-project tools/do_vm.py ssh -- stat -f -c %t /var/lib/slipfork
uv run --no-project tools/do_vm.py ssh -- slipfork prune --yes
uv run --no-project tools/do_vm.py ssh -- btrfs subvolume list /var/lib/slipfork | grep -v /snapshots/
uv run --no-project tools/do_vm.py ssh -- crun --version
uv run --no-project tools/do_vm.py ssh -- docker buildx version
```

Expected: `status`/`sync` exit 0; `test` exits 0 with both live suites green; `findmnt` prints options containing `user_subvol_rm_allowed,noatime`; `stat -f -c %t` prints `9123683e`; `slipfork prune --yes` then `btrfs subvolume list | grep -v /snapshots/` prints nothing (the digest-keyed base survives until the Phase-2 M3.2.13 GC); `crun --version` and `docker buildx version` both answer. `RESULT.md` records the measured numbers, the reboot check, and the teardown command.

# Terminology Rule (OWNER, 2026-11-16 — binding)

Never call the architecture "Option A" (or reference "Option B", "Spike A", "Spike B", or any "we chose X over Y" framing). There was only ever one design: **`BtrfsEngine` over one unified Btrfs subvolume per world + Linux `userns`/`netns`/`ipcns`/`pidns`/`mntns` + `pivot_root` + daemonless static `crun`**. Write it as what Slipfork is and does.

Enforceable gate before Phase 1 completes (owner scope):

```bash
! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/
```

If this node writes any file under `docs/`, `crates/`, or `tools/`, add that inverted grep (scoped to your paths) to your own verification and delete rather than reword any alternative-architecture history.
