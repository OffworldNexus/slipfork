---
id: M3.169
title: "Unwritable supervisor.log aborts up with a bare EACCES: actionable diagnosis + rotate when append is denied"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.158]
paths:
  - "crates/slipfork/src/daemon/lifecycle/up/**"
  - crates/slipfork/src/world/fork.rs
  - crates/slipfork/src/world/up.rs
  - crates/slipfork/src/world/runtime/frames.rs
  - crates/slipfork/src/world/supervise.rs
  - "crates/slipfork/src/daemon/reconcile/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T12:56"
updated: "2026-11-25T13:15"
---

# Owner report

After `chmod -R ga-w .`, `up` aborts:

```
error: open `/home/remy/slproj/test1/.forks/sloop/.slipfork/supervisor.log`: Permission denied (os error 13)
next: read `.slipfork/supervisor.log`, then run `slipfork up`
```

The file was `-r--r--r--`, so **append** as the owner is impossible; the failure is correct but the diagnosis/remedy are useless (the file is readable, and the suggested `next:` does not fix anything).

# Fix

- The open at `daemon/lifecycle/up/mod.rs:243` must classify `EACCES`/`EROFS` and produce one line naming the path + why ("the daemon must append to it") and one `next:` that actually works (`chmod u+w <dir> <file>` / `chmod -R u+w .slipfork`), never a bare `EACCES` path.
- **Rotate instead of aborting** when append is denied: create `supervisor.<n>.log` (next free) and note one line (`could not append to supervisor.log; writing supervisor.2.log`) so a read-only historical log cannot brick `up`. Keep the previous file untouched.
- Same treatment for every other append we do to `.slipfork/` (world handle, run logs): one helper in the log owner, not per-call `open` errors.
- Fix the stale remedy text in `world/fork.rs:100` / `runtime/frames.rs:160` ("read …, then run up") to something actionable.
- `logs`/`logs -r` must stay read-only and unaffected by file modes.

# Acceptance

- Lab: a `.slipfork/supervisor.log` with mode `0444` → `up` still boots and writes `supervisor.<n>.log` with one informational line; `up` on a directory with mode `0555` fails closed with the `chmod u+w` remedy (no raw EACCES alone).
- `make check`; Triad-light (`review-ux` + `review-tests`); commit `[M3.169] ...`, done, no push.
