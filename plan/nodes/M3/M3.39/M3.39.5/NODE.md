---
id: M3.39.5
title: "Parallel World Boot DAG: resources/images, deduped @tool + when-changed setup, pre/post/failed deploy hooks, minimal up/down/restart wiring"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.39.4]
paths: ["crates/slipfork-world/**", "crates/slipfork/**", Cargo.toml, Cargo.lock]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T01:53"
updated: "2026-11-21T01:53"
---

# Goal
Schedule the whole world boot as one parallel DAG — resources and images, deduplicated `@<tool>` and `before-start when changed(...)` setup nodes, `pre-deploy` hooks, services with readiness, then `post-deploy` / `failed-deploy` — with `.slipfork/` stamps skipping unchanged steps in under 1ms, plus the minimal `slipfork up`/`down`/`restart` composition wiring (no full CLI polish).

# Context
- DAG shape and edges, dedupe rule, sub-millisecond stamps, and keep-running-on-failure semantics: `specs/03-under-the-hood.md:74-92`.
- `@<tool>` synthesis (`@uv`,`@poetry`,`@pnpm`,`@npm`,`@yarn`,`@bun`,`@composer`,`@bundle`,`@go`,`@cargo`), deduplicated `(image, lockfile_dir)` key with workspace-root lockfile walk-up, and `when changed(...)` semantics: `specs/01-forkfile.md:125-130`.
- User-story acceptance for dedupe + private/cdn buckets + fail-closed checks: `specs/04-testing-plan.md:19-22`.
- Hook and `run` fail-fast/multi-line rules, optional OCI ENTRYPOINT/CMD when `run:` is omitted: `specs/01-forkfile.md:127-129`.
- Wiring point: `crates/slipfork/src/main.rs:8-11` (modules) and `:30-36` (dispatch); the CLI verb enum is `crates/slipfork/src/cli.rs:20-27`, run dispatch at `crates/slipfork/src/cli.rs:95-105`, session construction in `crates/slipfork/src/commands/mod.rs`. Add only `up`, `down`, `restart`; no extra commands, no flags beyond what the spec needs, no vaporware help text.
- `crates/slipfork/src/main.rs` is the single composition root: build the `WorldEngine`/`BaseImageBuilder` adapters here and inject them; library crates stay env-free. This slice serializes on `crates/slipfork/src/main.rs`, `cli.rs`, and root `Cargo.toml`.
- Stamps live in the project subvolume `.slipfork/` (`specs/03-under-the-hood.md:17,22-24`) and must carry across fork; a failed service/hook leaves the world running for inspection instead of tearing it down.

# Acceptance
- `cargo test -p slipfork-world -p slipfork`
- Protected `crates/slipfork/tests/critical_world_journey.rs` on real Btrfs + `crun`: one consolidated journey that runs `slipfork up`, asserts a single deduplicated `@uv`/`@pnpm` install node executed once across services, asserts unchanged second `up`/`restart` skips setup in under 1ms per stamped step, asserts `pre-deploy` then services then `post-deploy` ordering and that a failing service keeps the world running with routes answerable, runs `restart` and `down`, and asserts zero leaked containers, processes, or subvolumes.
- `cargo run -p slipfork -- --help` lists only shipped verbs including `up`/`down`/`restart`, zero `Unavailable*` or `[derived]` markers.
- `cargo clippy --workspace --all-targets -- -D warnings`
- `uv run --no-project tools/guard.py`
