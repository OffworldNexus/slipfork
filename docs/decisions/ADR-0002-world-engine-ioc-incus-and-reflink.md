# ADR-0002: `WorldEngine` IoC Contract — Incus Primary Driver, Rootless Reflink+Netns Driver, and Shared `$UID` Mounts

- **Status:** Accepted (`H-004` tracked for M2 live host `incusd` mount)
- **Date:** 2026-11-14

## Context

Slipfork needs millisecond copy-on-write cloning of stateful services (PostgreSQL, Redis, S3, app workers) with:
1. Full isolation between clones (ports `:5432`, `:6379`, `:8000`, `:3000` must never collide).
2. Direct sharing of the host git checkout / worktree under the developer's exact `$UID:$GID` (never leaving Docker-style `root:root` files).
3. Support for both bare-metal workstations running **Incus (`incusd`)** (LXC system containers + QEMU VMs with disk & stateful RAM CoW) and unprivileged nested Linux containers / CI runners where `incusd` or `/dev/kvm` is absent.

## Decision

1. **One `WorldEngine` Trait, Explicit Capabilities, Zero Unisolated Fallbacks:**
   - All orchestration depends exclusively on `trait WorldEngine`. Every engine declares `EngineCapabilities` honestly and passes the shared `WorldEngine` conformance test suite.
   - If the configured engine is unavailable (e.g., `/var/lib/incus/unix.socket` missing), Slipfork fails closed — never falling back silently to unisolated host execution.
2. **`IncusEngine` (Primary Workstation & Dedicated Server Driver):**
   - Communicates directly with the Incus `/1.0` REST API over Unix socket.
   - Clones snapshots via `POST /1.0/instances` (`source.type = "copy"`, `instance_only = true`) backed by ZFS, Btrfs, LVM-thin, or XFS-reflink pools, with optional `stateful: true` RAM restore on QEMU VMs / CRIU-capable containers.
   - Attaches the host repo/worktree via an Incus `disk` device with `shift = "true"` (`MOUNT_ATTR_IDMAP`) and executes commands as `host_uid:host_gid`.
3. **`ReflinkNetnsEngine` (Unprivileged Nested-Container & CI Driver):**
   - Clones stateful service directories via kernel `ioctl(FICLONE)` (`cp --reflink=always` semantics on XFS/Btrfs) on disk (never `/tmp` `tmpfs`).
   - Executes each world inside an unprivileged Linux namespace (`CLONE_NEWUSER | CLONE_NEWNET | CLONE_NEWNS | CLONE_NEWPID`) mapping `host_uid -> host_uid` (`uid_map = "$UID $UID 1"`), bringing up private loopback `127.0.0.1` via `CAP_NET_ADMIN` inside the userns so `postgres` runs natively as `$UID` on private port `5432` with zero cross-world collisions and instant PID-namespace reaping on exit.

## Consequences

- Identical developer UX across bare-metal Incus hosts and nested Linux containers (including OpenCode's own container host), with zero root-owned files and millisecond CoW cloning.
