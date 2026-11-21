---
id: M3.39.3
title: "Native Dockerfile step builder on Btrfs + crun: multi-stage, COPY --from/--chown, RUN heredocs & --mount=type=cache, .dockerignore"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.39.2]
paths: ["crates/slipfork-image/**", Cargo.toml, Cargo.lock]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T01:53"
updated: "2026-11-21T01:53"
---

# Goal
Evaluate `Dockerfile`s natively with zero external builder: `FROM ... AS`, non-secret `ARG`, `ENV`, `WORKDIR`, `COPY [--from] [--chown]`, `ADD`, `RUN` (heredocs and `--mount=type=cache`), `USER`, `EXPOSE`, `CMD`/`ENTRYPOINT`, and `.dockerignore`, caching every step as a read-only Btrfs subvolume so a rebuild is a sub-millisecond CoW snapshot.

# Context
- `specs/03-under-the-hood.md:60` is the authoritative feature list; every `FROM` starts from a `< 1ms` Btrfs CoW snapshot and every `RUN` snapshots into a cached step subvolume.
- Build-time `arg` vs runtime `env`, `arg VITE_CDN_URL = static_cdn.url`, and secret-taint rejection across `on` branches: `specs/01-forkfile.md:79-88`. Secret-tainted values must be refused before any step runs.
- End-to-end expectations (multi-stage, `COPY --from`, `RUN --mount=type=cache`, `.dockerignore`, no external daemon installed): `specs/04-testing-plan.md:15-16`.
- Build on `crates/slipfork-image/src/lib.rs` and the OCI puller from M3.39.2 — reuse the extracted RO base subvolumes as `FROM` roots instead of re-pulling. Keep parsing/execution inside cohesive structs (`DockerfileParser`, `StepBuilder`) and add predicate helpers in this module, never re-scan instructions downstream.
- `RUN` executes through `crun` against a writable CoW snapshot of the current step subvolume; cache mounts are host directories under the store, keyed by instruction + target, never inside the sealed image subvolume. Fail closed when `crun` is missing.
- `.dockerignore` matcher is part of this module and must be applied to `COPY`/`ADD` contexts only.

# Acceptance
- `cargo test -p slipfork-image`
- Protected `crates/slipfork-image/tests/critical_dockerfile_live.rs` builds a real multi-stage Dockerfile (base -> builder with `RUN --mount=type=cache` + heredoc -> final with `COPY --from` and `--chown`), asserts `.dockerignore` excludes a planted file, asserts a secret-tainted `arg` is rejected before execution, asserts a second identical build hits the cached RO step subvolumes and takes under 100ms of builder time, and asserts the resulting rootfs is a sealed read-only Btrfs subvolume. Real `crun` + real Btrfs, no shims, fail closed when absent.
- `cargo clippy --workspace --all-targets -- -D warnings`
- `uv run --no-project tools/guard.py`
