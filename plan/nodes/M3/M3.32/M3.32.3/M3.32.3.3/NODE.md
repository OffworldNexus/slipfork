---
id: M3.32.3.3
title: "Daemon/CLI cut-over on EvaluatedFork, project identity, delete slipfork-snow"
kind: epic
status: active
priority: 2
depends_on: [M3.32.3.2]
paths:
  - "crates/slipfork/**"
  - "crates/slipfork-snow/**"
  - "crates/slipfork-forkfile/**"
  - Makefile
  - tools/tests/test_tools.py
  - Cargo.toml
  - Cargo.lock
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T19:49"
updated: "2026-11-19T20:34"
---

Goal: retype daemon types/env/resolve/world/service/protocol/services/vault/ospp_supervisor/base/postgres/backing/mod, vaults.rs and commands/process.rs on EvaluatedFork (features not modes, resolve_env, resource ids); split daemon/world.rs (534 LOC) under 500; project identity from Forkfile project or VCS, delete the slipfork.toml loader; delete crates/slipfork-snow plus workspace member and migrate surviving assertions into crates/slipfork-forkfile/tests (authorized, never weaken). Acceptance: no slipfork_snow/slipfork-snow/snow.yml under crates or Cargo.toml; cargo test -p slipfork -p slipfork-forkfile -p slipfork-dag; clippy; fmt; make check; VM sync+test exit 0 with evidence in LOG.md. No RESULT.md.

# Acceptance

- No `slipfork_snow`, `slipfork-snow` or `snow.yml` under `crates/` or `Cargo.toml`.
- `cargo test -p slipfork -p slipfork-forkfile -p slipfork-dag` exits 0; clippy/fmt clean.
- `tools/do_vm.py sync && tools/do_vm.py test` exit 0 with evidence in `LOG.md`.
