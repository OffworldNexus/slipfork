---
id: M3
title: "Clean-Sheet Slipfork 0.1.0: Pure-Btrfs Native Engine, Enclosing Image Forkfile, High-Craft CLI, Visual Zensical Docs & All-Distro Linux Packaging"
kind: milestone
status: done
priority: 3
depends_on: [M2]
paths: []
attempts: 0
max_attempts: 3
escalation: architect
created: "2026-11-14T22:03"
updated: "2026-11-23T21:29"
---

# Goal

Deliver Slipfork **`0.1.0`** on `release/0.1.0` over the unified Btrfs subvolume `WorldEngine` (`BtrfsEngine`, `ADR-0010`), `:80` `slipforkd` `.localhost` edge, high-craft CLI, and `OSPP v1` end-to-end encrypted secrets:

1. **Unified Btrfs subvolume `BtrfsEngine` (`ADR-0010`):** one Btrfs subvolume per world holding the read-only `Dockerfile` `L0` rootfs + `/var/lib/slipfork/state/{pgdata,redis,s3}`, `BTRFS_IOC_*` RO snapshots and writable clones, fail-closed `BTRFS_SUPER_MAGIC` (`0x9123683e`) probe with `mount -o user_subvol_rm_allowed,noatime` remedy, `--ipc` SysV IPC isolation, `-pgid` process-group supervision, `crun` backing containers (`postgres:<v>`, `redis:<v>`, SeaweedFS), and AppArmor `userns` profile — sole `WorldEngine` with zero fallbacks.
2. **`:80` systemd socket-activated `slipforkd` `.localhost` edge:** `sudo slipfork daemon install` writes `/etc/systemd/system/slipforkd.{socket,service}` plus `/etc/apparmor.d/slipfork`, performs version-agnostic socket handover, and serves port-less `http://<project>.localhost` origins (`ADR-0011`).
3. **High-craft ANSI/spinner CLI & `@slipfork/opencode` V2 plugin:** `status` on bare `slipfork`, `up`, `restart`, `ps`, `open --remote`, no-arg `fork`, `logs`, `shell`, `exec`, `hook run`, `db`, `redis`, `forward`, `doctor`, `drop`, `down`, `list`, `prune`, `secret`, `auth`, `daemon unit`, `daemon install`, and `snow resolve`, paired with `Git` worktrees, `Jj` workspaces, `git-remote-slipfork`, and `@slipfork/opencode`.
4. **`OSPP v1` (`docs/design/ospp.md`, `ADR-0008`):** `p256` challenge-response auth, ALTCHA+PoP `#handle=` browser-fragment pairing saved as public `{endpoint, account, machine}` metadata in `<state_dir>/ospp.json`, ID-only `claims`, `hpke-p256` base64 secret encryption decrypted strictly in `slipforkd` memory + out-of-band HPKE-encrypted PostgreSQL seed dump streaming (`zstd -d | pg_restore`), with zero remote secrets ever written to disk.
5. **Visual Zensical docs & zero-mock lab E2E verification:** problem-first visual `docs/` (`make docs-check` + `tools/check_docs_honesty.py` strict clean), deterministic `make check` on dev hosts, and live Btrfs E2E + benchmark verification on Ubuntu 24.04 Btrfs (`165.232.86.174`).

> **Milestone boundary (`0.2.0` next):** the CBOR `STP v1` Testing Protocol over `SLIPFORK_PROTOCOL_FD` (`slipfork test`), live Kerfufoo `OSPP v1` provider integration, and ad-hoc `pytest`/`vitest`/`phpunit` `STP v1` plugins are scheduled for **`0.2.0`** (followed by `M4` `0.4.0` and `M5` `0.5.0` Editor & IDE Integration), and are never claimed as `0.1.0` surfaces.

# Acceptance

- [x] Plain `make check` (zero environment overrides) and `make docs-check` exit `0` on a non-Btrfs dev host; real `BtrfsEngine` + `crun` + `postgres:18` lab E2E suites, PTY color/spinner E2E, `.localhost` `:80` demo, and bounded `make bench --check` exit `0` on Ubuntu 24.04 Btrfs (`165.232.86.174`) with `kernel.apparmor_restrict_unprivileged_userns=1`.
- [x] `0.2.0` (`STP v1` over `SLIPFORK_PROTOCOL_FD` + Kerfufoo `OSPP v1` provider + `pytest`/`vitest`/`phpunit` plugins), `M4` (`0.4.0`), and `M5` (`0.5.0`) are cleanly separated from `0.1.0` with zero vaporware (`tools/check_docs_honesty.py`).
- [x] `uv run --no-project tools/plan.py validate` and `uv run --no-project tools/guard.py` exit `0`.
