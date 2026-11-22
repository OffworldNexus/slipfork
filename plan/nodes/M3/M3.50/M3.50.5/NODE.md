---
id: M3.50.5
title: "Docs/manpage/CHANGELOG: purge db+redis, document shell <target>/logs/TUI"
kind: task
status: done
owner: "doc-writer"
priority: 4
depends_on: [M3.50.2]
paths:
  - "specs/01-forkfile.md"
  - "specs/02-cli-ux.md"
  - "specs/04-testing-plan.md"
  - TESTING_PLAN.md
  - "docs/**"
  - README.md
  - CHANGELOG.md
  - packaging/man/slipfork.1
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T12:41"
updated: "2026-11-22T13:23"
---

# Goal
Published docs, `README.md`, `CHANGELOG.md`, and `packaging/man/slipfork.1` describe only shipped behavior: a single `slipfork shell <target>`, no `slipfork db`/`slipfork redis`, `slipfork logs <image>` for failed builds, and the fixed-bottom-bar TUI — passing the docs honesty gate.

# Context
- Authoritative contract: `specs/02-cli-ux.md` §3-§5 (DECSTBM frame/counter math, `< 1.0s` fork, the `shell <target>` kind table incl. both exact stderr refusals) and `specs/03-under-the-hood.md` §3-§4 (direct-exec PG/Redis, `SCM_RIGHTS` PTY, canonical `<store>/logs/<world>/<id>.log`). Document it; do not re-litigate.
- Surfaces: `docs/cli.md:40-43,107` (drop `db`/`redis` bullets, replace the "no PTY is allocated" `shell` wording), `docs/index.md`, `docs/getting-started.md`, `docs/under-the-hood.md`, `docs/agents-and-opencode.md`, `docs/troubleshooting.md` (failed build -> `slipfork logs <image>`), root `README.md`, `CHANGELOG.md`.
- `packaging/man/slipfork.1` is generated from `crates/slipfork/src/cli/manpage.rs` (`clap_mangen`); regenerate via the crate helper. Do not edit `crates/slipfork/src/cli/**` (owned by M3.50.2) except to read.
- DX: user value + a copy-pasteable snippet in the first 25 lines, Mermaid + Zensical `!!!` admonitions, `<= 2`-sentence paragraphs, no `ADR-*`/`M3.*` lawyer prose. Every `forkfile` fence must still pass `critical_forkfile` static checking.
- `tools/check_docs_honesty.py` (invoked by `make docs-check`) fails on unshipped commands or flags. If shipped TUI strings from M3.50.4 differ from spec wording, align the docs to shipped bytes at the epic gate.

# Acceptance
- `make docs-check` exits `0` (honesty + every `forkfile` fence valid).
- `cargo test -p slipfork --test manpage_drift` exits `0`.
- `grep -rn "slipfork db\\|slipfork redis" docs README.md CHANGELOG.md packaging/man/slipfork.1` returns nothing; `docs/cli.md` documents `<target>` as required.
- `uv run --no-project tools/guard.py`.
