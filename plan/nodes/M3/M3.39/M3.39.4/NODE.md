---
id: M3.39.4
title: "Unprivileged world namespaces: slipfork-world-init lifeline + per-service crun pods for pg/redis/s3"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.39.3]
paths: ["crates/slipfork-world/**", Cargo.toml, Cargo.lock]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-21T01:53"
updated: "2026-11-21T10:34"
---

# Goal
Give each world one unprivileged identity userns + private netns (127.0.0.1) + CLONE_NEWIPC, held open by a persistent `slipfork-world-init` PID 1, and run every service/resource as its own `crun --cgroup-manager=disabled` OCI container with tmpfs-only config/env and clean `-pgid` teardown.

# Context
- Shared namespace + per-service containers table (`api`/`worker`/`front`/`pg`/`cache`/`uploads`/`assets` rootfs, workdir, ports): `specs/03-under-the-hood.md:62-68`; tmpfs-only runtime env and clean process-tree stop: `specs/03-under-the-hood.md:70`.
- Verified kernel traps: identity mapping `"$UID $UID 1"` after writing `deny` to setgroups grants CAP_NET_ADMIN without root (`.opencode/LESSONS.md:6`); multithreaded `setns` into an unprivileged netns fails, so enter through PID 1 and pathname AF_UNIX sockets (`.opencode/LESSONS.md:7`); unprivileged :80 needs `--keep-caps` or `ip_unprivileged_port_start=0` (`.opencode/LESSONS.md:8`); `CLONE_NEWIPC` is mandatory for parallel PostgreSQL worlds (`.opencode/LESSONS.md:9`); minimal non-root `config.json` needs `--cgroup-manager=disabled` plus `user` + `pid` + `mount` + `ipc` + `uts`, omit `network`, identity uid/gid mappings, and `$UID` present in the read-only `/etc/passwd` bind (`.opencode/LESSONS.md:11`); reap exited one-shots with `try_wait` before `crun kill`, and pass env inside the executed argv for `crun exec` (`.opencode/LESSONS.md:26`); `AF_UNIX` sun_path is 107 bytes, honor a short scratch root (`.opencode/LESSONS.md:20`).
- `slipfork-world-init` contract: `O_CLOEXEC` stdin-lifeline; EOF exits; translate `/proc/1/ns/*` -> `/proc/<init_pid>/ns/*` for every pod. It is a second `[[bin]]` of the `slipfork-world` crate, not a new crate and not a second composition root (it may read only its own fd/paths passed as argv).
- Backing resources: real `postgres:18`, `redis:7`, and SeaweedFS — `private` bucket returns 403 on anonymous GET, `cdn <id>` bucket is public-read (`specs/03-under-the-hood.md:61`, `specs/01-forkfile.md:92-104`). Data subvolumes under `worlds/<world>/*` per `specs/03-under-the-hood.md:25-29`.
- Pod config/env live at `$XDG_RUNTIME_DIR/slipfork/pods/<world>` mode 0700 tmpfs: zero secrets on persistent disk, zero secrets in `/proc/<pid>/cmdline`. Never pass secrets as argv; never log them.
- Build on M3.39.1's subvolume/store types and M3.39.2/M3.39.3 sealed image subvolumes; compose rather than duplicate ioctl code.

# Acceptance
- `cargo test -p slipfork-world`
- Protected `crates/slipfork-world/tests/critical_pod_runtime.rs` on real Btrfs + real `crun`: boots `slipfork-world-init`, asserts identity `geteuid()` inside the userns, asserts `127.0.0.1` is reachable from two pods while host and sibling-world namespaces stay isolated, asserts System V shm does not collide across two worlds, asserts pg/redis/s3 respond, asserts anonymous GET on the `private` bucket is 403 while the `cdn` bucket is public-read, asserts no secret appears in `/proc/<pid>/cmdline`, and asserts full `-pgid` reap leaves zero processes and zero leaked snapshots. Fail closed when `crun`/Btrfs is absent; no mocks or `TestTreeDriver`.
- `cargo clippy --workspace --all-targets -- -D warnings`
- `uv run --no-project tools/guard.py`
