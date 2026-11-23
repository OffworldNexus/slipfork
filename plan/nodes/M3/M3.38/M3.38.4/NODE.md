---
id: M3.38.4
title: "Deduplicated @<tool> (image, lockfile_dir) DAG synthesis, single-root CLI fmt/migrate wiring & lab E2E + critical tests"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.38.3]
paths: ["crates/slipfork-forkfile/**", "crates/slipfork/**", Cargo.toml, Cargo.lock]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-21T00:58"
updated: "2026-11-21T01:40"
---

# Goal
Synthesize deduplicated `@<tool>` setup nodes into the boot DAG, wire `slipfork fmt` and `slipfork migrate` through the single composition root with high-craft streamed/JSON output, and land the protected critical suite plus a real-disk lab E2E journey.

# Context
- `@<tool>` auto-prequel table (`@uv`, `@poetry`, `@pnpm`, `@npm`, `@yarn`, `@bun`, `@composer`, `@bundle`, `@go`, `@cargo`), workspace-root lockfile walk-up, and `before-start when changed(...)` input hashing: `specs/01-forkfile.md:125`; DAG synthesis/ordering: `specs/03-under-the-hood.md:74`.
- `fmt`/`migrate` language logic stays in `slipfork-forkfile`; the CLI only parses argv, streams progress and renders (`specs/01-forkfile.md:131`).
- Composition root: `crates/slipfork/src/main.rs` owns every `std::env`/TTY/`/proc` read and builds the DI tree (`specs/03-under-the-hood.md:116-120`).
- UX law: stream slow child work instead of `.output()`, one line + `next:` on error, semantic colour only on interactive TTY, zero `\x1b[` under `NO_COLOR`/pipes/`--json`, `--json` output is pure data.
- Coverage economy: one cohesive journey on real disk (`/var/tmp` or `~/.cache/slipfork`, never RAM `/tmp`), zero `Mock*`/`Fake*`/`Stub*`; protected tests under `crates/slipfork-forkfile/tests/critical_forkfile.rs` are guarded by `tools/critical_guard.py`.

# Acceptance
- Dedup unit tests: two `@uv` services in one image produce exactly one `(image, lockfile_dir)` node; differing lockfile dirs produce two; `@pnpm` detects the workspace root lockfile; `when changed(prisma/schema.prisma)` re-runs only after that file changes.
- `cargo test -p slipfork-forkfile` and `cargo test -p slipfork` green; `cargo test -p slipfork-forkfile --test critical_forkfile` green.
- Lab E2E (real git repo on disk): `slipfork fmt --check` fails, `slipfork fmt` rewrites in place, second `--check` passes; `slipfork migrate` output parses via the CLI; `NO_COLOR=1 slipfork fmt --check` output contains zero escape bytes; `slipfork fmt --json` is valid JSON.
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`, `uv run --no-project tools/guard.py`.
