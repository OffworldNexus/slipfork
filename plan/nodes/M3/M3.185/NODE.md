---
id: M3.185
title: "live-test lab must be CI-shaped: test root IS the Btrfs mount, cold store, fresh init (no ambient warmth)"
kind: task
status: review
owner: implementer
priority: 3
depends_on: []
paths: [tools/do_vm.py, "tools/tests/**", Makefile, TESTING_PLAN.md, "crates/slipfork/tests/support/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T21:39"
updated: "2026-11-25T22:35"
---

# Finding (from M3.184's acceptance runs)

Two suites failed purely because of **lab assumptions**, not product bugs:

- `critical_status_live` expects a `from:` image to be pulled (a `from` row); a warm OCI cache in the shared store settles it `cached`, so the row never appears.
- Some suites hardcode `<test_root>/.slipfork-store`, which requires `SLIPFORK_TEST_ROOT` to **be** the Btrfs mount; using a subdirectory silently shares the parent store.

`make live-test` / `do_vm.py` must make this shape explicit and fail closed instead of relying on whoever runs it:

- Provision a **fresh, cold** Btrfs mount per run (or a per-run store root inside it) and set `SLIPFORK_TEST_ROOT` to the mount root, exactly like CI.
- Build `slipfork-world-init` for the same tree and export `SLIPFORK_WORLD_INIT` (no `touch`/mtime hacks; `M3.131`'s stamp check must be satisfied honestly).
- Fail closed with one line if the resolved store already contains a warm image cache when a suite declares it needs a cold pull.
- Document the contract in `TESTING_PLAN.md` (one short paragraph) so a manual run reproduces CI.

# Acceptance

- On a brand-new VM: `make live-test` from a clean checkout runs the 16 suites with the cold-store guarantee and reports per-suite pass/fail.
- A deliberately warm/foreign store fails closed with the one-line remedy instead of silently changing outcomes.
- `make check`; Triad-light (`review-tests`); commit `[M3.185] ...`, done, no push.
