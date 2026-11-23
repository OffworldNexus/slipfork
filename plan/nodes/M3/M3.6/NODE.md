---
id: M3.6
title: "Restrict IncusEngine to LXC containers only: delete QEMU path & stateful-RAM promise"
kind: task
status: superseded
owner: designer
priority: 1
depends_on: []
paths: [ARCHITECTURE.md, "docs/decisions/**", "docs/design/**", docs/VISION.md, docs/knowledge/LESSONS.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T12:33"
updated: "2026-11-16T16:53"
---

# Goal

Owner ruling: **Slipfork targets Incus LXC system containers only.** Delete the QEMU-VM path and the stateful-RAM-snapshot promise from the design of record, so we stop shipping a claim nothing has ever exercised (and that the DO dev VM cannot exercise: no nested KVM).

## What to change

1. **`ADR-0002`** — rewrite §2 to describe `IncusEngine` as the **LXC-only** driver:
   - world rootfs = the `L0` container image imported as immutable `slipfork-l0-<digest12>`, cloned with `POST /1.0/instances` (`source.type = "copy"`, `instance_only = true`);
   - shared-`$UID` host mount via the idmapped `disk` device (`shift = "true"`);
   - backing services are `crun` containers **inside** the instance (`ADR-0009`), never QEMU guests;
   - CoW snapshots come from the ZFS/Btrfs/LVM-thin/xfs-reflink pool.
   - **Delete** every QEMU/VM sentence, `stateful: true`, "RAM restore", and CRIU mention from `ADR-0002`.
2. **Stateful RAM is explicitly out of scope for `0.x`.** `WorldEngine::create_snapshot(..., stateful_ram = true)` must stay fail-closed for **every** engine (`EngineError::StatefulRamUnsupported`) — including `IncusEngine`. Record the reason honestly: it requires CRIU/QEMU RAM capture, cannot be validated on our only live target (nested-virt-less droplets), and our measured warm-pool path (`~118 ms`/test) already delivers the speed without it. If it ever returns, it returns as its own ADR with measured evidence.
3. **Scrub the same claims** from `ARCHITECTURE.md` §3.2/§3.3, `docs/design/target-dx.md`, `docs/design/world-base-image.md`, `docs/design/m1-core-and-poc.md`, and any ADR that restates "LXC + QEMU" (`rg -ni "qemu|stateful.{0,12}ram|criu|/dev/kvm"`). Where the capability matrix is described, say: **Incus LXC (live), ReflinkNetns (live), QEMU (not a target)**.
4. **`docs/knowledge/LESSONS.md`**: one line — DO droplets expose no nested KVM, so Incus there is LXC-only; `stateful: true` snapshots are therefore untestable on that path and are deliberately not promised.

## Rules

- Single writer for `ARCHITECTURE.md` + `docs/decisions/**` in this node (serialize against any other node touching them).
- Deletions, not hedges: no "QEMU is also supported but", no "if CRIU is available". One target.
- Do **not** touch code; the engine already fails closed. If a `rg` finds a doc-only claim, fix the doc.
- Published site: these are internal docs, so nothing should reach `docs/index.md`/`reference/`. Verify it does not.

# Acceptance

- [ ] `rg -ni "qemu|/dev/kvm|stateful[: ]*true|criu" ARCHITECTURE.md docs/decisions docs/design docs/VISION.md` exits 1 (0 hits outside an explicit "not a target" line in `ADR-0002`, quoted in `RESULT.md`).
- [ ] `rg -ni "incus.{0,40}(lxc|container)|lxc.{0,40}only" ARCHITECTURE.md docs/decisions/ADR-0002*.md` exits 0.
- [ ] `rg -ni "qemu|/dev/kvm" docs/index.md docs/reference README.md` exits 1.
- [ ] `cargo test -p slipfork-engine --test critical_engine` exits 0 (`tmpfs` fail-closed + `StatefulRamUnsupported` still pinned for every engine).
- [ ] `make check` exits 0 (`zensical --strict`, `check_docs_honesty.py` clean).
