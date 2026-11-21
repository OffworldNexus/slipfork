---
id: M3.38.3
title: "slipfork-forkfile semantics: all-branch typecheck, flat namespace, arg secret-taint rejection, routes/resources & snow.yml migrate"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.38.2]
paths: ["crates/slipfork-forkfile/**", Cargo.toml, Cargo.lock]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-21T00:58"
updated: "2026-11-21T01:33"
---

# Goal
Add whole-file semantics to `slipfork-forkfile`: typecheck every `on` branch simultaneously, enforce the flat global symbol namespace, statically reject `secret(...)` and secret-tainted values in build-time `arg`, validate resources/routes/args/ports, and ship deterministic lossless `snow.yml` -> `# forkfile: 0.1` migration.

# Context
- Multi-branch `Forkfile:line:col` checking + static validation contract: `specs/01-forkfile.md:131`; `arg` strictly non-secret and enclosing-scope inheritance vs runtime `env`: `specs/01-forkfile.md:79` and `:106`.
- Flat globally unique namespace across `database`, `cdn`, `domain`, `image`, `service`; `s3` private-by-default vs `cdn` `public-read` with `<id>.domain`/`<id>.url`: `specs/01-forkfile.md:92`.
- `strip-prefix` vs `rewrite` mutually exclusive; `domain <id | prefix.*>` verified against declared top-level `domain`s: `specs/01-forkfile.md:110`.
- `@<tool>` set and `before-start when changed(...)` typing: `specs/01-forkfile.md:125`.
- Migration contract: `specs/05-ospp-v1.md:213` (§10), input schema `:217`, `${{ ... }}` template rules `:235`, command/cross-reference refusal rules `:251`.
- Migrate must re-parse its own output before writing, emit byte-identical output for identical input, fixed block order (`features`, resources, env groups, mode-gated groups, services), one blank line between blocks and a single `# migrated from snow.yml` header.

# Acceptance
- `cargo test -p slipfork-forkfile` green for: `arg`/`env` scope misuse, `secret()` in `arg`, `s3 { cdn }` typing, `domain`/`url` properties, `route exact`/`strip-prefix`/`rewrite` conflict, duplicate symbol across kinds naming both, boolean `on` expression typed on all branches.
- Migrate golden tests: full `snow.yml` fixture -> byte-identical `Forkfile`; unknown top-level key and `modes.<mode>.components.<id>.disabled: true` refused at their path with a `next:` remedy; output re-parses as valid `# forkfile: 0.1`.
- `crates/slipfork-forkfile/tests/critical_forkfile.rs` seeded here with the taint + flat-namespace + migrate invariants, still guarded by `tools/critical_guard.py`.
- `cargo clippy -p slipfork-forkfile --all-targets -- -D warnings`, `cargo fmt --all -- --check`, `uv run --no-project tools/guard.py`.
