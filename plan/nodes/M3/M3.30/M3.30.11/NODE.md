---
id: M3.30.11
title: Harden the browser opener test against ETXTBSY under parallel test load
kind: task
status: done
owner: implementer
priority: 1
depends_on: [M3.30.4]
paths: [crates/slipfork/tests/browser.rs]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-19T06:34"
updated: "2026-11-19T06:37"
---

# Goal
`make check` must not flake: `crates/slipfork/tests/browser.rs::open_spawns_the_resolved_program_and_the_printed_surfaces_agree` fails with `Text file busy (os error 26)` when the workspace suites run in parallel (another thread forks between writing the opener script and exec-ing it, inheriting the write fd).

# Fix
Retry the opener spawn on `ETXTBSY` with a short bounded backoff (for example 20 x 10 ms) and keep every existing assertion; do not skip. Note the classic Linux write-fd/fork race in a comment.

# Acceptance
`cargo test -p slipfork --test browser` 5/5; `make check` green.
