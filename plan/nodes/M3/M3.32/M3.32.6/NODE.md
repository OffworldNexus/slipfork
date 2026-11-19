---
id: M3.32.6
title: "Zero-mock lab E2E + live multi-service PTY proof on 165.232.86.174, demo left on :80"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.32.5]
paths: [docs/knowledge/LESSONS.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T13:02"
updated: "2026-11-19T13:02"
---

# Goal

Prove the whole epic on real kernels: local `make check`/`make docs-check`, then a live Btrfs + `crun` + `postgres:18` + OSPP + multi-service pod journey on `165.232.86.174`, ending with `demo` running on `:80`.

# Context

- Lab harness: `tools/do_vm.py` (`sync`, `test`) and `Makefile` targets; VM is Ubuntu 24.04 on Btrfs with `kernel.apparmor_restrict_unprivileged_userns=1`. Worktrees/volumes stay under `.worktrees/` and `~/.cache/slipfork` (never `/tmp`).
- Existing suites to extend, not duplicate: `crates/slipfork-testkit/**` (world harness), `crates/slipfork-ospp-testkit/**` (`reference_server.rs` loopback), `crates/*/tests/critical_*.rs` (protected — never weaken or delete), `packages/opencode-slipfork/tests/coordinator.critical.test.ts`.
- Journeys to cover with one rich E2E lifecycle (not one test per assertion): `up` boots `pg` + `api` + `front` as three `crun` pods in one world; `http://demo.localhost/` and `http://demo.localhost/api` both `200`; `logs -f` streams both `api` and `front` stdout/stderr and survives `slipfork restart` with the divider; host file edit visible in-container over `MS_BIND`; `slipfork env`, `slipfork run`, no-arg `fork`, `ps`, `open`, `drop`, `down --all`, `prune`.
- PTY proof uses `script -qec` on `/home/slipfork/demo` so spinners/colors are exercised under a real TTY; assert `● demo` tree shows `api`, `front`, `postgres` and port-less URLs, and that a piped/no-TTY run emits zero `\x1b[`.
- Record durable host/kernel facts (crun + userns sharing, Btrfs volume submount behaviour, port-stripping ingress, PTY quirks) in `docs/knowledge/LESSONS.md`.
- `tools/critical_guard.py` must stay satisfied; no `RESULT.md`; evidence in `LOG.md`/`JOURNAL.md`.

# Acceptance

- `make check` and `make docs-check` exit `0` locally.
- `uv run --no-project tools/do_vm.py sync && uv run --no-project tools/do_vm.py test` both exit `0` on `165.232.86.174`.
- Interactive PTY transcript captured in `LOG.md` shows: clean `● demo  http://demo.localhost  ~/demo` tree with `api`/`front`/`pg` rows, `curl -sS http://demo.localhost/api` and `http://demo.localhost/` both `200`, `logs -f` output from both `api` and `front`, edit-over-bind-mount reload, `slipfork env`, `slipfork run`, no-arg `fork`.
- VM is left with stock `0.1.0` binaries built from the slice head and the `demo` world running on `:80`.
- `uv run --no-project tools/plan.py validate` exits `0`; write NO `RESULT.md`.
