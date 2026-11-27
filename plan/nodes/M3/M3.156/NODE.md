---
id: M3.156
title: "CLI fmt canonical still drops the authored header blank (HEAD red): update critical_fmt_migrate and cover the CLI crate"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: [crates/slipfork/tests/critical_fmt_migrate.rs]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-25T11:10"
updated: "2026-11-25T11:12"
---

# Problem

`M3.155` (6b095fa) made `fmt` preserve one authored blank line; the owner authorised the `critical_forkfile.rs` golden update. But `crates/slipfork/tests/critical_fmt_migrate.rs` has the same fixture: `UNFORMATTED` at `:12-13` is `"# forkfile: 0.1\n\nimage backend . {…"` (authored blank) and `CANONICAL` at `:15-16` still expects `"# forkfile: 0.1\nimage backend . {…"`. HEAD is therefore **red** at `fmt_checks_rewrites_and_emits_pure_json` (`:126`), found by M3.140 on a pristine `6b095fa` worktree.

# Change

- Update `CANONICAL` to keep the authored blank after `# forkfile: 0.1` (the same owner-authorised rule as `critical_forkfile.rs:251`; nothing else changes).
- Confirm there is no other CLI-side fmt golden that encodes the old blank-eating output (`grep` the `slipfork` crate tests for `# forkfile: 0.1\n`).
- Record in the node why the M3.155 gate missed it (it ran `-p slipfork-forkfile` and the forkfile suites, not the CLI's `critical_fmt_migrate`).
- Note for the future (one line in `TESTING_PLAN.md`): a formatter change must run the CLI crate's `critical_fmt_migrate` too.

# Acceptance

- Pristine `6b095fa` + this change: `cargo test -p slipfork --test critical_fmt_migrate` green; `cargo test -p slipfork-forkfile` green; `fmt --check` idempotent on the fixture.
- `critical_guard.py` flags this protected golden — expected, authorised; explain in the commit message.
- Commit `[M3.156] ...`, done, no push.
