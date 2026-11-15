# ADR-0002: `WorldEngine` IoC Contract — Incus Primary Driver, Rootless Reflink+Netns Driver, and Shared `$UID` Mounts

- **Status:** Accepted — amended for M2 (`H-004` tracked for M2 live host `incusd` mount; `EngineCapabilities` boolean bag deleted in favor of typed `EngineError::StatefulRamUnsupported`; amended by `ADR-0009` — the world `L0` rootfs comes from the project `Dockerfile` and backing services run as daemonless `crun` OCI containers, replacing `base_instance: String` with `WorldRootfs`)
- **Date:** 2026-11-15

## Context

Slipfork needs millisecond copy-on-write cloning of stateful services (PostgreSQL, Redis, S3, app workers) with:
1. Full isolation between clones (ports `:5432`, `:6379`, `:8000`, `:3000` must never collide).
2. Direct sharing of the host git checkout / worktree under the developer's exact `$UID:$GID` (never leaving Docker-style `root:root` files).
3. Support for both bare-metal workstations running **Incus (`incusd`)** (LXC system containers + QEMU VMs with disk & stateful RAM CoW) and unprivileged nested Linux containers / CI runners where `incusd` or `/dev/kvm` is absent.

## Decision

1. **One `WorldEngine` Trait, Fail-Closed Modes, Zero Unisolated Fallbacks:**
   - All orchestration depends exclusively on `trait WorldEngine`. Every engine implements `WorldEngine` honestly — failing closed with `EngineError::StatefulRamUnsupported` when `SnapshotMode::StatefulRam` is unsupported (zero `EngineCapabilities` boolean bag) — and passes the shared `WorldEngine` conformance test suite.
   - If the configured engine is unavailable (e.g., `/var/lib/incus/unix.socket` missing), Slipfork fails closed — never falling back silently to unisolated host execution.
2. **`IncusEngine` (Primary Workstation & Dedicated Server Driver):**
   - Communicates directly with the Incus `/1.0` REST API over Unix socket.
   - Clones from the imported `WorldRootfs` image (`slipfork-l0-<digest12>`, `ADR-0009` Composition Root) — never from a caller-supplied `base_instance: String` — via `POST /1.0/instances` (`source.type = "copy"`, `instance_only = true`) backed by ZFS, Btrfs, LVM-thin, or XFS-reflink pools, with optional `stateful: true` RAM restore on QEMU VMs / CRIU-capable containers.
   - Attaches the host repo/worktree via an Incus `disk` device with `shift = "true"` (`MOUNT_ATTR_IDMAP`), the read-only `rootfs.host_caches` / `rootfs.service_images` with `readonly = "true"`, and executes commands as `host_uid:host_gid`.
3. **`ReflinkNetnsEngine` (Unprivileged Nested-Container & CI Driver):**
   - Clones stateful service directories via kernel `ioctl(FICLONE)` (`cp --reflink=always` semantics on XFS/Btrfs) on disk (never `/tmp` `tmpfs`); those directories are `<world_dir>/state/{pgdata,redis,s3}`, written only by `crun` backing-service containers (`ADR-0009` §2).
   - Executes each world inside an unprivileged Linux namespace (`CLONE_NEWUSER | CLONE_NEWNET | CLONE_NEWNS | CLONE_NEWPID`) mapping `host_uid -> host_uid` (`uid_map = "$UID $UID 1"`), bringing up private loopback `127.0.0.1` via `CAP_NET_ADMIN` inside the userns so `postgres:<v>`, `redis:<v>`, and SeaweedFS bind private ports `5432`, `6379`, and `9000` inside that netns as `crun` OCI containers running as `$UID` (zero cross-world collisions, instant PID-namespace reaping on exit), over an application rootfs mounted read-only from the project `Dockerfile` stage (`ADR-0009` §1–§3).

## Consequences

- Identical developer UX across bare-metal Incus hosts and nested Linux containers (including OpenCode's own container host), with zero root-owned files and millisecond CoW cloning.
