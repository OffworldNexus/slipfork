---
id: M3.107.4
title: "Every failing instruction explains itself: step notes for all kinds + engine reason in capture/record"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.107.3]
paths:
  - "crates/slipfork-image/src/**"
  - "crates/slipfork-image/tests/**"
  - crates/slipfork/src/world/runtime.rs
  - "crates/slipfork/src/world/runtime/**"
  - crates/slipfork/src/world/supervise.rs
  - "crates/slipfork-world/src/run_log.rs"
  - "crates/slipfork-world/src/log_store.rs"
  - "crates/slipfork-world/tests/critical_log_store.rs"
  - crates/slipfork/src/commands/streams.rs
  - "crates/slipfork/src/ui/**"
  - "crates/slipfork/src/boot/**"
  - crates/slipfork/tests/critical_world_journey.rs
  - crates/slipfork/tests/critical_status_live.rs
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-24T12:54"
updated: "2026-11-24T12:54"
---

# Problem (owner regression at 1157050)
`COPY --from=ghcr.io/astral-sh/uv:latest …` failed with `no build stage named …`; `logs -r` showed an empty capture, a header with no instruction (`#5fa0fb  image  api  <started>`) and a bare `✗ image api failed` — the reason vanished. M3.107.1 only noted RUN/success.

# Contract (lead decision; v3 roles unchanged)
- Step note for EVERY instruction kind (`FROM`, `COPY`, `ADD`, `RUN`, `WORKDIR`, …) is written when the step STARTS (phase + as-written text + `Dockerfile:N`), so any failure leaves a header naming what ran: `#5fa0fb  COPY  api  --from=ghcr.io/astral-sh/uv:latest /uv /uvx /bin/  <started>`.
- A step the ENGINE executes (FROM/pull, COPY, ADD, context walk) has the engine as its "command": its one-line error is that step's stderr output, written into the capture as one `┃` line (`┃ no build stage named `ghcr.io/astral-sh/uv:latest``). Never the wrapper prose (`build `api` failed: Dockerfile:3: … failed:`), never `next:` text.
- The record carries `reason` (same one line) for every non-success; `--json` exposes it.
- Verdict: `✗ COPY api failed` (RUN keeps `(exit N)`); the reason sits directly above it as the failing step's last gutter line in both `up` and `logs -r`. If the failure has no step at all (Dockerfile parse/stage-graph error before step 1), the verdict carries it: `✗ image api failed: <reason>`.
- No failure ever replays as an empty block: either the capture has the engine/child lines, or the verdict carries the reason.

# Acceptance
- Live journey: a Dockerfile whose `COPY --from=<missing stage>` fails, and one whose `FROM <unpullable ref>` fails: `logs -r` header names `COPY …`/`FROM …`; capture shows the `┃ <reason>` line; verdict `✗ COPY <t> failed` / `✗ FROM <t> failed`; `up` shows the same block + one `next:`; `--json` record has `phase`,`instruction`,`source`,`reason`.
- Existing RUN behaviour unchanged (`(exit 3)`, no reason duplication). NO_COLOR zero `\x1b[`.
- fmt, clippy -D warnings, targeted tests; lead re-runs VM live suites.

# Styling
Parent `plan/nodes/M3/M3.107/NODE.md` `# Visual contract (v3)` is authoritative.
