# `M2.24` Result — `L0` World Base Image, Daemonless `crun` Backing-Service Containers & World Image Contract (`ADR-0009`)

## Summary

Authored `docs/decisions/ADR-0009-world-base-image-and-image-contract.md` (`Accepted`) and `docs/design/world-base-image.md` (`Accepted`) honoring the original directive plus **both binding owner corrections**:

1. **One Canonical `L0` Path, Zero Fallbacks (Iron Law #3):** Every world's `L0` app rootfs derives strictly from a `BuildImage` in the project's `snow.yml` `images:` list via `docker buildx build -f <scratch_dockerfile> --target slipfork-world --output type=local` (`$UID:$GID`-owned extraction). Curated OS base images, separate registry-pull modes, and host-`/usr` sharing are deleted altogether. `[world]` in `slipfork.toml` exposes **at most two optional keys** (`world.image`, `world.stage`, `#[serde(deny_unknown_fields)]`), failing closed with a 4-line `snow.yml` snippet when no `BuildImage` exists.
2. **Daemonless `crun` OCI Backing-Service Containers (Superseding `apt`/`apk` Capability Layers):** `snow.yml` `resources:` map deterministically (`pg_clusters[].version` → `postgres:<v>`, `redis_clusters[].version` → `redis:<v>`, `s3_buckets` → pinned `chrislusf/seaweedfs`) and run via static `crun` (`--cgroup-manager=disabled`, `readonly: true` rootfs, identity `uidMappings`/`gidMappings` `$UID -> $UID`, isolated `user`/`pid`/`mount`/`ipc`/`uts`, shared world `127.0.0.1` `netns`) with RW bind mounts from CoW `<state_dir>/{pgdata/<id>,redis/<id>,s3}`, unified behind `ServiceRuntime { WorldProcess, OciContainer }` on `WorldEngine::{spawn_service, stop_service, service_logs}` plus `exec_in_service` / `service_entry_command` (`pg_restore`, `CHECKPOINT`, `slipfork db`, `slipfork redis`).
3. **Live Host Feasibility Probe (`docs/design/world-base-image.md` §7 + `docs/knowledge/LESSONS.md`):** Proven live on this `kernel 6.12.117+deb13-amd64` XFS (`reflink=1`) LXC host inside `unshare --map-user=1000 --map-group=1000 --keep-caps --mount --net --pid --fork --mount-proc` with static `crun 1.20` (`sha256:ff8407185ec4...`), `postgres:18` (`:5432`), `redis:8` (`:6379`), cross-distro `alpine:3.21` client connectivity in the same `netns`, read-only `userxattr` two-lowerdir `overlay` (`EROFS` on `/`) + `pivot_root`, and XFS `FICLONE` state cloning into a second world.

## Updated Acceptance Verification (incorporating Owner Corrections #1 & #2)

- [x] `test -f docs/decisions/ADR-0009-world-base-image-and-image-contract.md` -> `PASS` (exits `0`, contains `## Composition Root & DI Tree`).
- [x] `test -f docs/design/world-base-image.md` -> `PASS` (exits `0`, contains 6-rule World Image Contract table §4.2, supported distro table §4.1, exact failure-mode error strings §5.1–§5.11, Merkle key §6.1, live `crun` probe §7, and `M2.25`–`M2.29` / `M3.1` sequencing §8).
- [x] `! rg -n "slipfork_base|CuratedBase|prebuilt|world\.base" docs/design/world-base-image.md docs/decisions/ADR-0009*.md` -> `PASS` (`rg` exits `1`, 0 hits).
- [x] `rg -n "world\.image|world\.stage" docs/design/world-base-image.md docs/decisions/ADR-0009-world-base-image-and-image-contract.md` -> `PASS` (exits `0`, sole two `[world]` keys documented).
- [x] `! rg -n "slipfork_base|world\.base|world\.image|world\.stage" docs/*.md docs/reference README.md` -> `PASS` (`rg` exits `1`, zero vaporware in published docs).
- [x] `uvx --offline zensical build --strict && make docs-check && uv run --no-project tools/guard.py` -> `PASS` (all exit `0`).
