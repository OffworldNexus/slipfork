---
id: M3.165
title: "logs --last N; -f implies --last 20"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.164]
paths:
  - "crates/slipfork/src/commands/logs/**"
  - "crates/slipfork/src/commands/streams/**"
  - "crates/slipfork/src/cli/**"
  - "crates/slipfork/src/daemon/lifecycle/logs/**"
  - "specs/02-cli-ux.md"
  - crates/slipfork/tests/logs_last_live.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T12:31"
updated: "2026-11-25T16:53"
---

# Owner request

"`logs` should have a `--last xxx` where xxx is the number of lines you want, and `-f` implies `--last 20`."

# Contract

- `slipfork logs --last <N>` prints only the last `N` rendered lines (tail) of the merged view; `slipfork logs -r [id] --last <N>` tails that run; both treat `N` as *rendered* lines (post strip, one per line as displayed), not raw bytes.
- `slipfork logs -f` (follow) defaults to `--last 20` when `--last` is absent; an explicit `--last` wins. Follow keeps streaming new lines after the tail.
- `N == 0` is rejected with one line + `next:` (use `--last 1` or omit); non-numeric rejected the same way. `--last` with `--json` limits the emitted line objects the same way and stays escape-free.
- The default (no `--last`, no `-f`) keeps today's full output.
- Update `specs/02-cli-ux.md` §6 (`logs` line) and `--help`; keep the `-r`/`--runs`/`--since`/`--until`/`-t` semantics unchanged.

# Acceptance

- Byte-exact tests: a 100-line run with `--last 25` prints exactly the last 25; `-f` starts with 20 then appends; `--last 0` fails closed; `--json --last 3` emits 3 records, no ESC; `logs --last 25 | head` still exits quietly.
- Live VM: a service with >20 lines, `logs -f` shows 20 then follows; `logs -r <id> --last 5` tails a recorded build.
- `make check`; Triad; commit `[M3.165] ...`, done, no push.
