---
id: M2.29
title: "slipforkd/CLI: container-scoped db/redis, --host-caches, container-aware up/down"
kind: task
status: active
owner: implementer
priority: 1
depends_on: [M2.28, M2.17, M2.25]
paths:
  - "crates/slipfork/**"
  - "crates/slipfork-edge/src/daemon.rs"
  - "crates/slipfork-edge/src/daemon/**"
  - "crates/slipfork-edge/tests/critical_edge.rs"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-15T20:40"
updated: "2026-11-15T22:31"
---

# Goal

Wire `ADR-0009` into the daemon/CLI composition root (no new engine code):

- `slipfork db` / `slipfork redis` exec **into the service container** via `WorldEngine::service_entry_command` (`psql` / `redis-cli` live in the container, never in the app rootfs); fail closed naming the service when it is not running.
- `--host-caches`: ABI-probed read-only `WorldMount`s (`~/.cargo`, `~/.rustup`, uv/pnpm stores) appended to `WorldSpec.mounts` only when the probe passes; otherwise fail closed with the measured mismatch.
- `up` / `down` / `fork` / `drop` drive `WorldRootfs` materialization through `BaseImageBuilder` (progress events on TTY), and `freeze_world`/`down` honour the stop-containers-first ordering.
- `snow.yml` `resources:` map to the pinned service images (`pg version` -> `postgres:<v>`, `redis version` -> `redis:<v>`, `s3_buckets` -> SeaweedFS) with digest-pinned caching; no new user-facing syntax beyond `world.image` / `world.stage`.

# Context

- Depends on `M2.17` (CLI part 2) and `M2.25` (doc sweep), so both must be `done` before this starts.
- Published docs stay truthful until this ships: if a command becomes real here, update its page **in this node** so `check_docs_honesty.py` stays green.

# Acceptance

- [ ] `cargo test -p slipfork --test critical_l0_wiring` exits 0: `db`/`redis` target the container entry command; `--host-caches` refuses on ABI mismatch (with the exact message) and passes on this host.
- [ ] `uv run --no-project tools/check_docs_honesty.py` exits 0 and any newly-real command is documented (no unimplemented command advertised).
- [ ] `rg -n "psql|redis-cli" docs/reference/cli.md` shows the container-scoped semantics (manual note in `RESULT.md`).
- [ ] `make check` exits 0.
