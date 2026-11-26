---
id: M3.170
title: "13-step build printed step 2 twice, once truncated and once after step 13: verify row release at HEAD"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork/src/boot/**"
  - "crates/slipfork-image/src/plan/**"
  - "crates/slipfork-image/src/builder/**"
  - "crates/slipfork-image/src/step_log.rs"
  - crates/slipfork/src/world/runtime/frames.rs
  - crates/slipfork/tests/boot_pty.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T12:56"
updated: "2026-11-25T16:39"
---

# Owner report (13-step build, `up`)

```
  ✓  2/13 copy     --from=ghcr.io/astral-sh/uv:latest  <1ms
  … unchanged steps 3..13 …
  ✓ 13/13 cmd      uv run blastit  <1ms
  ✓  2/13 copy     --from=ghcr.io/astral-sh/uv:latest /uv /uvx /bin/  <1ms
```

Step 2 appears **twice**: first with the destination args missing, then again after 13/13 with the full text. That is either a pre-`M3.159` binary (verify!) or a surviving duplicate-release path for a step whose label is finalised late.

# Work

1. Reproduce at current HEAD on the VM with the owner's Dockerfile shape (13 instructions incl. `COPY --from`, ARG/ENV, CMD). If the duplicate is gone, record the evidence and close as "fixed by M3.159; owner was on an older build".
2. If it reproduces: find which release path emits the early truncated row (label built before the destination args are parsed/expanded) and make each `(index, key)` release exactly once, with the final text only.
3. While there: the `env PORT=$port` row shows the *unexpanded* text. Verify the Dockerfile `ENV` value is expanded (ARG/ENV chaining) and make the step row show the resolved value (or the raw text plus the expanded value under `-v`), never a literal `$port` when it was expanded.

# Acceptance

- PTY test with the 13-instruction fixture: exactly one line per index, in order, with full arguments; `ARG`/`ENV` rows show resolved values.
- `make check`; Triad-light (`review-ux`); commit `[M3.170] ...`, done, no push.
