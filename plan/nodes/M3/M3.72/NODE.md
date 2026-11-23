---
id: M3.72
title: "Demo runs one uv command per mode; drop uv flag injection, keep @<tool> as the dev-only sync-at-boot prequel"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "support/demo/**"
  - "crates/slipfork-forkfile/**"
  - "crates/slipfork/**"
  - "crates/slipfork-world/tests/boot_dag.rs"
  - tools/do_vm.py
  - tools/tests/test_do_vm.py
  - TESTING_PLAN.md
  - "specs/**"
  - "docs/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-23T19:02"
updated: "2026-11-23T19:28"
---

# Goal

Collapse the shipping demo onto one `uv` command per mode and make the uv-sync contract honest:
Slipfork never injects `uv` flags (`no_sync_uv` is deleted), `uv run` executes verbatim, and
`@<tool>` is only the boot-time "sync at boot" prequel marker, used inside `on dev`.

# Changes

1. `support/demo/Forkfile`: `api` keeps `run:` only under `on dev` (`@uv run uvicorn … --reload`),
   so prod is the image `CMD` (`command: None`); `worker` and `pre-deploy` carry literal
   `uv run --no-sync …` in both modes; `front` is unchanged (dev `@pnpm exec vite dev`, prod CMD).
   Exactly one `@uv` remains in the file.
2. `crates/slipfork-forkfile`: `no_sync_uv`/`no_sync_uv_argv` deleted; `tool_prefix`/`direct_bin`
   unchanged. `crates/slipfork`: `ServiceLaunch` keeps only the dev direct-venv fast path,
   injection and `tool_env` deleted, hooks run `shell_command` verbatim, and `exec` passes argv
   through untouched.
3. `support/demo/api|front/Dockerfile` + `front/package.json`: prod `api` inherits the `uv` binary
   and `exec`s `uvicorn`; `front` builds in a `build` stage and ships `build/` only; Corepack pin
   `pnpm@10.18.3` with `onlyBuiltDependencies` in `pnpm-workspace.yaml`.
4. Docs/specs: `specs/01` lists `@<tool>` as a dev-only sync-at-boot prequel and forbids flag
   injection; `specs/03`, `docs/forkfile.md`, `TESTING_PLAN.md` and the demo README follow.
5. `tools/do_vm.py sync`: refresh the stamped `/home/slipfork/demo` from `support/demo` on every
   run, preserving ignored state, and commit `sync <short-rev>` with the pinned identity.

# Acceptance

- `make check`, `make docs-check` and `make test-tools` exit `0`.
- `boot_dag` prod plan: `api`/`front` `command: None`, `worker`/`pre-deploy` literal
  `uv run --no-sync …`, zero `uv sync` Setup nodes; the dev plan has exactly one.
- Live VM: `slipfork fork prod-check -F -dev` answers `200` on `/` and `/api/status` with the
  worker running and no `mailpit`; the dev demo world is unchanged.
