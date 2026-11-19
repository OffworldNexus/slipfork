# ADR-0009: Per-Service `crun` Pod Execution Model, Deduplicated Service & Backing OCI Rootfses, `$UID:$GID` Runtime, and World Btrfs Volumes

- **Status:** Superseded & Updated in `0.1.0` — **the single-app-`/` `L0` `pivot_root` (`ServiceRuntime::WorldProcess`) and `[world] image` / `world.stage` selector in `slipfork.toml` are superseded and deleted pre-`1.0` in favor of the Per-Service `crun` Pod Model ("A World Is a Pod, Not One Image") and [`docs/design/forkfile.md`](../design/forkfile.md)**. Static `/.slipfork/bin/crun` provisioning, `$UID:$GID` execution, `OciRootfs.readiness`, `--host-caches` ABI probing, and unified per-world Btrfs subvolume CoW (`ADR-0010`) remain active and normative.
- **Date:** 2026-11-15 (superseded/updated 2026-11-19)

## Context

1. **Polyglot applications are pods, not a single monolithic OS image:** Real stacks run `api` (`./api` Python), `front` + `tailwind` (`./front` Node at `stage dev`), `mailpit` (`from axllent/mailpit:latest`), `postgres:18`, `redis:7`, and `seaweedfs` side-by-side. Forcing one `world.image` onto `/` could not run multi-image services cleanly and forced host-OS vs container-OS friction on `.venv` and `node_modules`.
2. **CoW container volumes + live host source mounts:** Mounting `<world_subvol>/volumes/<service>/<sanitized_path>` over `<worktree>/<service_dir>/<rel_path>` (`volume .venv`, `volume node_modules`) keeps compiled native dependencies matched to each service's container OS and clones them in ~20 ms on `slipfork fork`, while source files remain live `MS_BIND`-mounted from the host worktree for instant `inotify` / HMR.

## Decision

1. **A World Is a Pod, Not One Image (`slipfork-world-init` + Per-Service `crun` Containers):**
   - `slipfork-world-init` (`ADR-0010` §6) holds the world pod's shared `userns + netns + ipcns` (`127.0.0.1` loopback + `:80` `$BASE_URL` ingress bridge + `<control_dir>/netns.sock` `SCM_RIGHTS` dialer) and stages static `/.slipfork/bin/crun` inside the pod mount namespace (`ADR-0010` §7 AppArmor attachment preserved).
   - **Every** `Forkfile` `service` (`service <id> ./dir` `Dockerfile` stage OR `service <id> from <image_ref>`) **and** every backing resource (`postgres:<v>`, `redis:<v>`, `seaweedfs`) runs as its own daemonless static `crun` OCI container (`--cgroup-manager=disabled`, `readonly: true` two-lowerdir `userxattr` OverlayFS rootfs, `[user, pid, mount, ipc, uts]` with identity `$UID:$GID` maps, and **no** `"network"` namespace so the entire pod shares `127.0.0.1`). `ServiceRuntime::WorldProcess` and `WorldImageConfig` are deleted.
2. **Deduplicated Rootfs Materialization (`BaseImageBuilder`):**
   - `BaseImageBuilder` (`crates/slipfork-engine::rootfs`) materializes unique `ImageSource::Dockerfile { context_dir, dockerfile, stage, build_env }` stages via `docker buildx build --output type=local` and `ImageSource::OciImage { image_ref }` images into content-addressed `$UID:$GID` directories under `<state_dir>/cache/{rootfs,oci}/<digest>/rootfs`. Services sharing identical `(./dir, dockerfile, stage, build_env)` tuples (e.g. `front` and `tailwind`) build and cache **once**.
3. **Unified Btrfs Subvolume State & Per-Service `volume` Mounts (`ADR-0010` §2):**
   - Each world's single Btrfs subvolume (`<state_dir>/worlds/<world_id>`, sealed into `<state_dir>/snapshots/<snapshot_id>`) holds `<subvol>/state/{pgdata/<cluster>,redis/<id>,s3}`, `<subvol>/volumes/<service>/<sanitized_path>` (seeded from the image's `<workdir>/<rel_path>` on initial creation if present, populated by `setup`, and mounted RW over `<worktree>/<service_dir>/<rel_path>`), and `<subvol>/home` (`/.slipfork/home`), all CoW-cloned atomically by one `BTRFS_IOC_SNAP_CREATE_V2` ioctl.
4. **Service-Scoped Container Entry (`shell`, `exec`, `run`, `db`, `redis`):**
   - `slipfork shell [service]`, `slipfork exec [-s <service>] <cmd...>`, and `slipfork run <task>` enter that service's running `crun` container (`crun exec`) — or spawn an ephemeral `crun` container over its materialized rootfs + pod `netns` + live worktree + subvolume `volumes` when the service is stopped/failed — at `<worktree>/<service_dir>` as `$UID:$GID` with synthesized `/etc/passwd` + `/etc/group`.

## Consequences

- Eliminates `slipfork.toml` `[world]`, unifies app services and backing resources onto one daemonless `crun` container runtime per pod, deduplicates shared service rootfses, and gives every fork instant ~20 ms CoW `.venv` / `node_modules` volumes alongside live host source editing.
