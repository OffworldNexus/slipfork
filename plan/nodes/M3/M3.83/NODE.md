---
id: M3.83
title: "One logging manager: persistent run catalog, target resolution, and failure hints"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.79, M3.80, M3.81, M3.82]
paths:
  - "crates/slipfork-world/src/{run_log.rs"
  - pod_log.rs
  - "layout.rs}"
  - crates/slipfork/src/world/runtime.rs
  - "crates/slipfork/src/world/runtime/images/**"
  - crates/slipfork/src/daemon/lifecycle/logs.rs
  - "crates/slipfork/src/commands/{streams.rs"
  - "runs.rs}"
  - crates/slipfork/src/boot/render.rs
  - crates/slipfork/src/world/supervise.rs
  - crates/slipfork/tests/critical_status_live.rs
  - crates/slipfork/tests/critical_world_journey.rs
  - "specs/03-under-the-hood.md"
  - docs/cli.md
  - docs/troubleshooting.md
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-24T00:17"
updated: "2026-11-24T00:17"
---

# Problem (owner hit all of these in one `up`)

1. `up` failed an image build and printed `slipfork logs -r 25f642`; `slipfork logs -r 25f642` then said **no recorded run 25f642 in `/.slipfork-store/logs/test1/runs`**, and `slipfork logs --runs` said **no recorded runs yet**.
2. `slipfork logs api` (the exact command the build error suggested) said `'api' is not running and has no captured logs`, even though the failed build had a run id.
3. The failure block printed **three** remedy lines (`next: run slipfork logs api`, `next: fix the command above`, and the final `check logs with: -r 25f642`).

Root causes:
- Log paths are computed in three places with different inputs: supervisor (`StoreLayout::world_logs(world)`), daemon (`store_root_for(project)` + `world_name(project)`), and the CLI hints (hand-built from `kind`/`name`). After a failed boot/rollback they disagree, and a failed run's artifacts can be swept (`fail_boot` → `forget`, `down` sweeps, fork teardown) before the user can read them.
- `logs <target>` only accepts ids in the live pod roster or `<logs_dir>/<id>.log` captures; image/hook builds are not in the roster, so `logs api` refuses unless a capture happens to be at exactly that path.
- `store_root` is `<longest mount>/.slipfork-store`. On a Btrfs-root host that is **`/.slipfork-store`**, i.e. a store at the filesystem root — wrong location, permission-dependent, and the reason the owner's `-r` hint points at an empty catalog.

# Contract

**One manager, one path resolver, one catalog.**

- A single `LogStore`/`RunCatalog` module owns every path: resolve the store root and world name **once** per project (in the daemon/supervisor), expose `world_logs(world)`, `run_dir(world)`, `capture(world, id)`, `index(world)`. Nothing else computes `store + "/logs"` strings.
- Every executed step (image pull/build, hook, setup, service start, service runtime) mints a `run_id` and **creates its capture + index record before the step runs**, so a crash or a failed boot leaves a readable artifact. Index record: `run_id, kind, target, command, state, started/finished, duration, project, world`.
- **Failure artifacts are durable**: a failed `up`, a rolled-back fork, or `down` must not delete the failed run's log/index; retention keeps the newest N runs (documented) and prunes older ones explicitly.
- **Resolution order for `logs <target>`**: live pod (stream, as today) → latest capture by target (image/hook/service) → `run_id`. `logs -r <id>|last|failed` and `--runs` read the same catalog. The store root must be user-writable on a Btrfs-root host: prefer `$XDG_DATA_HOME/slipfork/store` (same filesystem as the project) over a `/`-rooted `.slipfork-store`; keep `<mount>/.slipfork-store` for non-`/` Btrfs mounts (document the rule and migrate/ignore old state safely pre-1.0).
- **One remedy line**: the failure presenter asks the catalog for the failed run and prints exactly one `next:` (`slipfork logs -r <id>`); step errors stop embedding their own `next:` for build/run failures. `logs <target>` must work for a failed image build.
- `--json` mirrors the same catalog fields; no ANSI under `--json`/`NO_COLOR`.

# Acceptance

- Real lab: image build fails on a `RUN`; immediately `logs api` and `logs -r <id>` both return the captured output, `logs --runs` lists the failed run, and the same holds after `down` and after a failed fork rollback.
- A Btrfs-root host resolves the store under the user's data dir (never `/.slipfork-store`); a separate Btrfs mount keeps `<mount>/.slipfork-store`.
- One and only one `next:` line per failure.
- `make check`, `make docs-check`, targeted `critical_status_live`/`critical_world_journey`; live proof on `165.232.86.174` and a Btrfs-root machine if available.
