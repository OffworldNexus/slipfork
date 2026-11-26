---
id: M3.157
title: "Update critical_status_live to the append-only boot contract (drop DECSTBM/ESC[r/DSR assertions)"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: [crates/slipfork/tests/critical_status_live.rs]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T11:13"
updated: "2026-11-25T12:49"
---

# Problem

`M3.152` replaced the boot pane with append-only rendering: no DECSTBM (`ESC[1;{rows-1}r`), no `ESC[{row};1H`, no `ESC[r`, no DSR cursor queries, no park — completed rows are normal lines and only one status line uses `\r` + `ESC[2K`.

`critical_status_live.rs:580-989` still asserts the old widget's byte stream (scroll regions, `ESC[r`, DSR), so the live gate will fail for an intentional design change.

# Change (owner-authorised protected-test update)

- Rewrite those assertions to the shipped contract with **equivalent strength**: no `ESC[<n>;1H`, `ESC[r`, `ESC[J`, `ESC c`, alt-screen or DSR anywhere; completed rows appear once each as newline-terminated lines; only the status line uses `\r` + `ESC[2K`; `NO_COLOR`/`--json`/pipes emit zero escapes and zero `\r`; counters padded (` 1/11` vs `10/11`); `cached 9 · built 2` mixed summary; the failing-build case keeps its `✗ … failed at i/n` row and one `next:`.
- Do not delete assertions wholesale: for each removed sequence assertion, state the replacement invariant in the test or a comment, and keep the same number of covered lifecycle cases (or more).
- Note the authorisation in the commit message ("owner-authorised: boot pane is append-only").

# Acceptance

- Live on the Btrfs VM (private tree, as project user, `XDG_RUNTIME_DIR`, never `do_vm.py sync`, never `~/demo`): `critical_status_live` green.
- `make check`; Triad (tests: no vacuous replacements); commit `[M3.157] ...`, done, no push.
