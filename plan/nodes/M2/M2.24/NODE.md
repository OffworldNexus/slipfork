---
id: M2.24
title: "Design L0 world base image: Dockerfile-derived rootfs, 3-layer composition, World Image Contract (ADR-0009)"
kind: task
status: done
owner: architect
priority: 1
depends_on: []
paths:
  - "docs/decisions/ADR-0009-world-base-image-and-image-contract.md"
  - "docs/design/world-base-image.md"
  - docs/knowledge/LESSONS.md
attempts: 1
max_attempts: 3
escalation: architect
created: "2026-11-15T20:11"
updated: "2026-11-15T20:36"
---

# Goal

Design (ADR + design doc, **no code**) the `L0` **world base image** strategy: what root filesystem a world actually runs on, and the published contract a project's own image must satisfy. Owner directive: prefer **deriving the world rootfs from the project's existing Dockerfile** so the user gets exactly their runtime, over curating language-by-language base images.

## Current state (why this is needed)

`ReflinkNetnsEngine` today only CoW-clones mutable state (`PGDATA`, Redis dir, S3 dir) and **shares the host `/usr` + `/lib`**. That worked for the M1 micro-benchmark but fails on real projects (external probe: no host `redis-server`, no `weed`, no PostGIS, no WebKit). `IncusEngine::base_instance` presumes a pre-built instance that nothing builds. So `L0` is unspecified and must be designed before `slipfork up` can honestly claim portability.

## Design direction to evaluate (start here, do not re-derive from zero)

**Three layers, composed per world:**

1. **Fidelity layer — the project's own production image.** Slipfork appends a stage to the project's Dockerfile and builds only that target:
   ```dockerfile
   # (user's existing file, untouched)
   FROM python:3.14-slim-trixie AS api          # must be a NAMED stage
   ...
   # appended by Slipfork, machine-generated, never edited by hand:
   FROM api AS slipfork-world
   <capability-layer provisioning for the detected distro>
   ```
   `docker buildx build --target slipfork-world --output type=local,dest=<rootfs>` yields a plain rootfs, cached by `(Dockerfile + context Merkle hash)`. Imported into the Incus pool (`incus image import`) or used directly as a `pivot_root`/`chroot`-able directory for `ReflinkNetnsEngine`.
2. **Capability layer — backing services + test/dev tooling installed by Slipfork on top**, matched to the distro detected from the image's own `/etc/os-release` (`debian:trixie|bookworm`, `ubuntu:noble`, `alpine`): `postgresql-<clustered version>` + `pg_ctl`/`psql`, `redis-server`, SeaweedFS (`weed`), browsers if a `[test]` adapter needs them, plus the toolchain **only when the project's test command needs it** (Rust: `cargo`/`rustc` are absent from a production image by design, so the capability layer installs a pinned toolchain — this is exactly the case the owner flagged).
3. **Host-passthrough layer — opt-in read-only mounts of host tooling/caches** (`~/.cargo`, `~/.rustup`, `~/.local/share/uv`, pnpm store) so nothing is duplicated or re-downloaded. Only when a real ABI probe passes (same libc family/major); otherwise fail closed with the exact mismatch.

**Published "World Image Contract" (each rule MUST produce an actionable error naming the fix):**
1. The runtime stage must be **named** (or `world.stage = "<name>"` in `slipfork.toml`). Unnamed last stage → error naming the one-line edit.
2. Base distro must be Debian/Ubuntu-family or Alpine (detected, never assumed). Anything else → error listing the supported set.
3. The image must not require root at runtime to write outside the checkout. Slipfork always executes as `$UID:$GID` (`--map-user`); a baked entrypoint that needs root or writes `/` must fail closed with the reason.
4. The image must not bake absolute host paths, hostnames, or ports that conflict with the single-origin rule (`ADR-0005`); `BASE_URL`-class values are injected.
5. App runtime deps belong to the image; **dev/test-only deps and backing services are the capability layer's job** (nobody should `apt install postgres` in their production Dockerfile).
6. `EXPOSE`/`HEALTHCHECK`, if present, must agree with `snow.yml` `ingresses`/components.

**Base selection must be exactly one, no precedence ladder** (Iron Law #3): `world.base = { dockerfile = "api" }` | `{ image = "acme-api:1.4" }` | `{ slipfork_base = "trixie" }` (curated fallback for projects with no usable Dockerfile). The curated `slipfork-base` is the *fallback*, not the default; document the trade-off (zero Dockerfile requirement vs. no production fidelity, and the language-version matrix pain the owner named).

## Required outputs

- `docs/decisions/ADR-0009-world-base-image-and-image-contract.md` (`Proposed`, with `## Composition Root & DI Tree` impact: a `BaseImageBuilder`/`WorldRootfsProvider` port, adapters `DockerfileStageBuilder`, `PrebuiltImageImporter`, `CuratedBaseProvider`; how `IncusEngine` (`base_instance` provenance) and `ReflinkNetnsEngine` (rootfs dir) both consume one `WorldRootfs`).
- `docs/design/world-base-image.md`: the decision, the contract table, the failure-mode catalogue, the caching/invalidation model (Merkle key = Dockerfile + context + capability-layer spec), and the exact user-facing error strings.
- Sequencing note: which existing node owns the code (do **not** implement here; propose node ids/deltas for `lead`).
- Zero public-docs changes (nothing ships yet → keep it out of the published site).

# Acceptance

- [ ] `test -f docs/decisions/ADR-0009-world-base-image-and-image-contract.md` exits 0 and contains a `## Composition Root & DI Tree` section.
- [ ] `test -f docs/design/world-base-image.md` exits 0 and contains the 6-rule contract table plus an explicit "supported distro" list.
- [ ] `rg -n "slipfork_base|dockerfile =|world\.base" docs/design/world-base-image.md` exits 0 (single-selection model documented, no precedence ladder).
- [ ] `rg -n "slipfork_base|world\.base" docs/*.md docs/reference README.md` exits 1 (no vaporware in published docs).
- [ ] `make docs-check` exits 0; `uv run --no-project tools/guard.py` exits 0.
