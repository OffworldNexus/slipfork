---
id: M3.25
title: "Close doctor host-probe DI gap and world-identity spelling split in the design of record"
kind: task
status: done
owner: architect
priority: 2
depends_on: [M3.7]
paths:
  - ARCHITECTURE.md
  - "docs/decisions/ADR-0010-btrfs-only-world-engine.md"
  - "docs/decisions/ADR-0002-world-engine-ioc-btrfs.md"
  - "docs/decisions/ADR-0005-localhost-edge-weaved-ipc-and-vcs-remote.md"
  - "docs/decisions/ADR-0009-world-base-image-and-image-contract.md"
  - "docs/decisions/ADR-0008-ospp-v1-websocket-tpmd-resource-strategy-and-world-ux.md"
  - "docs/decisions/ADR-0006-stp-v1-and-world-owned-stack.md"
attempts: 4
max_attempts: 5
escalation: architect
created: "2026-11-16T21:00"
updated: "2026-11-16T23:48"
---

# Goal

Close the two `SPEC_BLIND_SPOT`s surfaced by the `M3.9` IoC gate, in the design of record: (1) `slipfork doctor`'s `netns` / `crun` / `unshare_userns` probes have no port in the DI tree, and (2) world identity has two unreconciled spellings (`<project>[:<fork>]` display vs bare `<fork>` accepted by `--world`).

# Context

- `ARCHITECTURE.md` §1 gives the thin CLI only `KernelBtrfsDriver` + `probe_with` → `BtrfsProbeReport`, yet `M3.15`'s own goal requires `doctor` to report `netns`, `crun`, and `unshare_userns` (including the AppArmor `EPERM` hint). Either a probe port is missing or `M3.15` promises output nothing owns.
- `crun` is not a host tool in the target design: `WorldRootfs.crun_bin = /.slipfork/bin/crun` ships inside the world rootfs built by `BaseImageBuilder`, so a host-level "crun present and runnable" probe is a category error.
- Shipped `slipfork_edge::registry::world_id` renders `<project>[:<fork>]` for `list`, while `--world` accepts only bare `<fork>` (`validate_fork_name` forbids `:`), so one screen can print an id the same CLI rejects.
- `docs/design/target-dx.md` must be truthful immediately; `M3.15` must not implement a probe surface the architecture does not own.

# Design / Invariants

1. Rule on the diagnostic seam: either (A) a small `HostProbe`/`DoctorProbes` capability trait with `probe_namespaces`, `probe_userns`, and an in-world `crun` exec check, implemented by one OS adapter wired at `crates/slipfork/src/main.rs` and injected into `doctor` (and, if boot needs it, `slipforkd` boot), or (B) restrict `doctor` to `BtrfsProbeReport` fields and move namespace/userns/crun diagnosis to `slipfork up` preflight and world-boot errors, amending `M3.15`'s goal accordingly. State the single owner per probe and the exact JSON keys.
2. Remove the host-level `crun` probe from the design: `crun` is verified inside a world (or via `BaseImageBuilder`'s rootfs contents), never as a host binary.
3. State precisely what a Btrfs magic probe does and does not prove: `ensure_supported` is magic-only, so a mount missing `user_subvol_rm_allowed` is caught by the scratch-subvolume round-trip, not by the magic check. Do not claim boot ownership of a check it does not run.
4. Reconcile world identity: one canonical id spelling for display and selection (`<project>[:<fork>]`), or keep the display form but state explicitly that `--world` takes the bare fork and how the printed id is stripped — no screen may show an id the CLI rejects.
5. Update `ARCHITECTURE.md` §1/§3.3 (and the owning ADR) so the DI tree stays drawable, and record which of `M3.15` / `M3.2` consumes each decision. No new Cargo features; no fallbacks.

# Acceptance

```bash
grep -n 'BtrfsProbeReport\|HostProbe\|DoctorProbe' ARCHITECTURE.md
grep -rn 'world_id\|--world' ARCHITECTURE.md docs/decisions/ | head
uvx --offline zensical build --strict
uv run --no-project tools/check_docs_honesty.py
uv run --no-project tools/guard.py
uv run --no-project tools/plan.py validate
```

Expected: the chosen diagnostic seam is named with one owner and exact fields; world identity has one rule; Zensical `--strict`, docs honesty, guard, and plan validation exit 0.
