---
id: M3.39
title: "Pure-Btrfs engine, 100% Docker-free native OCI/Dockerfile builder, per-service crun pods & World Boot DAG"
kind: epic
status: ready
priority: 3
depends_on: [M3.38]
paths:
  - Cargo.toml
  - Cargo.lock
  - Makefile
  - ARCHITECTURE.md
  - "crates/slipfork-world/**"
  - "crates/slipfork-image/**"
  - "crates/slipfork/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T00:44"
updated: "2026-11-21T00:57"
---

# Goal

Implement `specs/03-under-the-hood.md` §1–§3 cleanly in `crates/slipfork-world` (Btrfs lifecycle/snapshots) + `crates/slipfork-image` (native OCI puller/`Dockerfile` builder) with **zero Docker/BuildKit dependency** and **pure Btrfs subvolumes end-to-end** (consulting `.opencode/LESSONS.md` for verified Linux namespace, Btrfs ioctl, and `crun` invariants):
1. **Pure Btrfs Subvolume Layout:**
   - Btrfs filesystem verification (`BTRFS_SUPER_MAGIC`), transparent in-place conversion of a plain git checkout directory on Btrfs into a Btrfs subvolume on first `up`, `.slipfork-store/{images,worlds}` management, `BTRFS_SUBVOL_RDONLY` sealing & flag-clearing before `BTRFS_IOC_SNAP_DESTROY_V2`, and `< 50ms` atomic CoW `fork` of BOTH `<project>/.forks/<name>` (including `.venv`, `node_modules`, `.slipfork/` input stamps + git branch switch) AND `worlds/<fork>--<world>` data subvolumes, plus dirty-checkout refusal on `drop` without `--force`.
2. **100% Docker-Free Native OCI Registry Puller & Btrfs + `crun` `Dockerfile` Builder:**
   - Direct HTTPS OCI Distribution API v2 client (Docker Hub / GHCR / Quay bearer token auth, multi-arch manifest list resolution for `amd64`/`arm64`, streaming layer sha256 verification & tar+gzip/zstd whiteout extraction into cached RO Btrfs subvolumes).
   - Native Btrfs CoW + `crun` `Dockerfile` evaluator (`FROM ... AS`, non-secret `ARG` only, `ENV`, `WORKDIR`, `COPY [--from] [--chown]`, `ADD`, `RUN` including heredocs and `--mount=type=cache`, `USER`, `EXPOSE`, `CMD`/`ENTRYPOINT`, `.dockerignore`), caching each step as a read-only Btrfs subvolume.
3. **Unprivileged Shared World Namespace, Per-Service `crun` Pods & Parallel Boot DAG:**
   - Persistent `slipfork-world-init` lifeline (`O_CLOEXEC` stdin EOF exit, identity `"$UID $UID 1"` userns + private `netns` `127.0.0.1` + `CLONE_NEWIPC`, translating `/proc/1/ns/*` -> `/proc/<init_pid>/ns/*`).
   - Individual `crun --cgroup-manager=disabled` OCI containers for every service (writable Btrfs snapshot of enclosing `image`, bind-mounting project subvolume at `$UID:$GID`, `PORT` + `$port`, optional OCI entrypoint default when `run:` omitted), setup/hook step, and backing resource (`postgres:18`, `redis:7`, S3/SeaweedFS enforcing `private` `403` anonymous `GET` vs `cdn <id>` `public-read`).
   - Mode-`0700` `tmpfs` (`$XDG_RUNTIME_DIR/slipfork/pods/<world>`) for OCI `config.json` and runtime `env`/secrets (zero secrets on persistent disk or `/proc/<pid>/cmdline`).
   - Parallel World Boot DAG scheduling resources, images, deduplicated `(image, lockfile_dir)` `@<tool>` & `before-start when changed(...)` setup nodes (`.slipfork/` Btrfs stamps skipping unchanged steps in `< 1ms`), `pre-deploy` hooks, services + readiness, and `post-deploy` / `failed-deploy` hooks (keeping world running on hook/service failure for inspection).

# Acceptance

- `make check` exits `0` cleanly on the local XFS host, and real Btrfs kernel/OCI/`Dockerfile`/pod/fork tests are wired for live execution on `165.232.86.174` and loop-backed Btrfs GHA runners with zero `Mock*`/`Fake*`/`TestTreeDriver` shims.
- Protected `crates/*/tests/critical_btrfs_live.rs` and `crates/slipfork/tests/critical_world_journey.rs` run against real Btrfs + `crun` and fail closed on a non-Btrfs host.
