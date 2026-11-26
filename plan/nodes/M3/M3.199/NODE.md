---
id: M3.199
title: "Docs: drop the 'breaks things on purpose' line (nobody used 0.1)"
kind: task
status: done
owner: "doc-writer"
priority: 3
depends_on: []
paths: [docs/index.md, README.md]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-26T07:39"
updated: "2026-11-26T07:39"
---

# Problem

`docs/index.md:33` says "0.1.1 breaks things on purpose." Nobody used 0.1, so there is no stability promise to break — the line reads as an excuse. `README.md:13` repeats the "very experimental release / expect breaking changes" framing.

# Change

- Delete the "breaks things on purpose" claim. Replace the home-page caveat with one honest pre-1.0 sentence, no theatre, e.g.:

  > **Pre-1.0.** The Forkfile and CLI can change between releases; report what breaks in the [issue tracker](https://github.com/OffworldNexus/slipfork/issues).

- Rewrite `README.md:13` in the same spirit: pre-1.0, interfaces may move, link the tracker. Drop "very experimental" and "expect breaking changes" phrasing.
- Keep it to one short admonition on the home page and one line in the README — no per-page repetition.

# Acceptance

- No "breaks things on purpose", no "very experimental release", no stale `0.1.0` anywhere in `docs/` or `README.md`.
- `make docs-check` green; commit `[M3.199] ...` with `docs/index.md` and `README.md` only, done, no push.
