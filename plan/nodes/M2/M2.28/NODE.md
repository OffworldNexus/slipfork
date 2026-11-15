---
id: M2.28
title: "slipfork-engine: ServiceRuntime::OciContainer over static crun + pivot_root rootfs + Incus provenance"
kind: task
status: ready
owner: implementer
priority: 1
depends_on: [M2.27]
paths: ["crates/slipfork-engine/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T20:40"
updated: "2026-11-15T20:53"
---

<!-- splice (extra, beyond the five requested): replace everything after the frontmatter of plan/nodes/M2/M2.28/NODE.md with this file. Frontmatter unchanged. Required because `ADR-0009`'s `ServiceRuntime::OciContainer` now carries `readonly_mounts: Vec<WorldMount>` — without it `ADR-0008`'s container-scoped `pg_restore` cannot read the verified `<sha256>.pgdump`. Source of truth: ADR-0009 §2–§3 (`M2.25`). -->

# Goal

Implement `ADR-0009` `Decision` rules 2–3 (single supervision surface; `$UID:$GID` container runtime):

- `ServiceRuntime { WorldProcess { argv, cwd }, OciContainer { image, argv, state_mounts, readonly_mounts } }` on `ServiceSpec`; **one** supervision path through the existing `spawn_service` / `stop_service` / `service_logs`.
- `crun` invocation exactly as probed: `--cgroup-manager=disabled`, identity user/pid/mount/ipc/uts maps, read-only `/etc/passwd` overlay, state subdirs bind-mounted from `<world_dir>/state/{pgdata,redis,s3}`, and read-only host mounts (`readonly_mounts`) for files a service client must read — e.g. the verified `<sha256>.pgdump` restored by `ADR-0008` §2.
- `exec_in_service(&ServiceHandle, ..)` (for `ADR-0008` `pg_restore` + `CHECKPOINT;`) and `service_entry_command(&ServiceHandle)` (returns a `crun exec` `WorldEntryCommand` for `slipfork db` -> `psql` / `slipfork redis` -> `redis-cli`); both fail closed `EngineError::ServiceNotRunning` when the container is not `ServiceState::Running`.
- `freeze_world` stops `WorldProcess` services first, then `OciContainer` services cleanly, then reflinks `state_dir`.
- `ReflinkNetnsEngine`: `slipfork-world-init` mounts the read-only two-lowerdir `userxattr` overlay (`<world_dir>/mountpoints:<rootfs.app_rootfs>`), binds `/proc`, `/dev/*`, tmpfs `/tmp`,`/var/tmp`,`/run`,`/dev/shm`, synthesized `/etc/passwd`+`/etc/group`, `/.slipfork/oci/*`, `<world_dir>/state`, `spec.mounts`, `rootfs.host_caches`, then `pivot_root`s into the merged root.
- `IncusEngine`: replace `base_instance: String` with `rootfs.app_rootfs` imported once per digest as immutable `slipfork-l0-<digest12>`, cloned via `POST /1.0/instances` (`source.type = "copy"`), attaching `spec.mounts`, `rootfs.host_caches`, `rootfs.service_images` (`shift=true`, `readonly=true`).

# Context

- `M2.27` lands `WorldRootfs` + `trait BaseImageBuilder`; this node consumes it and owns the **only** `match runtime` over service kinds in the workspace.
- `readonly_mounts` is new relative to the first `M2.24` draft: `ADR-0009` §2 states a verified seed dump reaches the service rootfs as a read-only mount, and `ADR-0008` §2's `pg_restore` runs inside the container. Adding it to `state_mounts` would be wrong (that field is RW CoW state under `<world_dir>/state`), and piping the dump through argv is impossible (`exec_in_service` carries no stdin).
- `M2.29` (daemon/CLI wiring) is the only consumer that populates `readonly_mounts` with the resolved dump; the engine must fail closed (`EngineError::InvalidWorldSpec`) when a mount overlaps a `state_mounts` container path.

# Acceptance

- [ ] `cargo test -p slipfork-engine --test critical_oci_runtime` exits 0 and re-runs the ADR probe **for real**: inside a genuine `unshare` world, start `postgres:18` and `redis:8` through `ServiceRuntime::OciContainer`, connect `127.0.0.1:5432` (SELECT `server_version`) and `:6379` (PING), then `FICLONE`-clone the state dir into a second world and boot it cleanly. Fails (not skips) when `crun` or the images are unavailable.
- [ ] `cargo test -p slipfork-engine --test critical_oci_runtime -- --nocapture readonly_dump_mount` exits 0: a `readonly_mounts` entry is visible and read-only inside the container (`EROFS` on write), `pg_restore` from that path succeeds inside `postgres:<v>`, and a `readonly_mounts` guest path colliding with a `state_mounts` path fails closed with `EngineError::InvalidWorldSpec`.
- [ ] `rg -n "base_instance" crates/slipfork-engine` exits 1 and `rg -n "readonly_mounts" crates/slipfork-engine/src` exits 0.
- [ ] `make check` exits 0.
