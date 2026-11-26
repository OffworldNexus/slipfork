---
id: M3.118
title: Dev services mount at the Dockerfile WORKDIR; fail closed when it is /
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.117]
paths:
  - "crates/slipfork-image/src/dockerfile/**"
  - "crates/slipfork/src/world/**"
  - "crates/slipfork-world/**"
  - "specs/01-forkfile.md"
  - "specs/03-under-the-hood.md"
  - docs/forkfile.md
  - crates/slipfork/tests/critical_world_journey.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T16:48"
updated: "2026-11-24T19:42"
---

# Problem

With the built-in `dev` feature, the checkout is bind-mounted at `/project`, but a service's cwd is the image's `WORKDIR`. A Dockerfile with no `WORKDIR` runs at `/` against the `COPY . .` snapshot baked into the image, so host edits never reach the running process. Owner: "I'm changing the files outside the container and they don't change inside."

# Contract

- A `dev` service must execute against the live checkout:
  - If the image's `WORKDIR` is explicit and under `/project` (the demo's `/project/api`), keep it.
  - If `WORKDIR` is `/` (or empty), run at `/project/<relative image build context>` (the live code for `image x ./sub`), never `/`.
  - If `WORKDIR` is a non-`/` path that was populated from the context (e.g. `/app` from `COPY . /app`), bind the live checkout at that `WORKDIR` as well (overlay) so the image's working dir sees host edits; keep `/project`.
  - Never bind over `/`.
- `-F -dev` (pure Docker semantics) keeps the image copy and its WORKDIR.
- `slipfork exec <svc> -- pwd` / `cat` must see a host edit immediately; `slipfork shell` too.
- Document the rule in `specs/01-forkfile.md` / `specs/03` and `docs/forkfile.md` (one short paragraph, no lawyer prose).

# Acceptance

- Live: boot with `dev`, edit a file on the host, `exec web -- cat <file>` shows the new bytes (no restart); `-F -dev` shows the image copy.
- A Dockerfile with no `WORKDIR` and one with `WORKDIR /app` both work.
- `make check`, `critical_world_journey`; commit `[M3.118] ...`, done, no push.
