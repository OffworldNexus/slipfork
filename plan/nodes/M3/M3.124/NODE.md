---
id: M3.124
title: "Row schema: separate state and command columns; visible neutral metadata colour (not SGR 2)"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - crates/slipfork/src/boot.rs
  - "crates/slipfork/src/boot/**"
  - "crates/slipfork/src/ui/**"
  - "crates/slipfork/src/status/**"
  - crates/slipfork/src/commands/streams.rs
  - crates/slipfork/src/daemon/client.rs
  - crates/slipfork/src/daemon/ps.rs
  - crates/slipfork/src/daemon/view.rs
  - crates/slipfork/src/daemon/lifecycle/ps.rs
  - crates/slipfork/src/world/target.rs
  - "crates/slipfork-forkfile/src/visit/mod.rs"
  - crates/slipfork/tests/critical_status_live.rs
  - crates/slipfork/tests/critical_fork_live.rs
  - crates/slipfork/tests/status_surface.rs
  - crates/slipfork/tests/tree_domain_surface.rs
  - crates/slipfork/tests/ansi_matrix.rs
  - crates/slipfork/tests/doctor_surface.rs
attempts: 3
max_attempts: 3
escalation: none
created: "2026-11-24T19:03"
updated: "2026-11-24T19:29"
---

# Problem

Owner's live rows:
```
✓ [1/2] image  api  cached  8ms
✓ [2/2] start  web  @uv run blastit dev --port 8000  1.5s
```
1. The detail column holds **state** (`cached`, `up to date`, `running`) OR **command** (`@uv run …`) depending on the row — two semantics in one column, no consistency.
2. Metadata (counters `[i/n]`, timings, timestamps, `next:` label, stdout bar) uses SGR **2 (dim)**, which the owner's terminal renders as the default foreground — so there is no visual coding at all. Same for `[1/3]`.
3. Across `up`/`fork`/`status` the same role is not always the same style.

# Contract

- One row schema everywhere (outer plan, fork prep, status): `icon | [i/n] | keyword | target | state | command | timing | #id` with dynamic widths; every column can be empty but keeps its position.
- **state** is always the lifecycle word (`cached`, `up to date`, `running`, `ready`, `built`, `failed`, `stopped`) in its status colour (green/cyan/yellow/red).
- **command** is always the command/args, plain; a command equal to the target is omitted, not replaced by state.
- **metadata colour**: stop using SGR 2. Use a real neutral 256-colour (`38;5;244`, fallback `90` if 256-colour unsupported is acceptable) for counters, timings, timestamps, `next:` label, stdout bar, run id? (`#id` stays bold per approved roles). It must be visibly distinct in a basic terminal.
- Keyword magenta, target bold, `✗`/`failed` red, `✓` green, `next:` command cyan; `NO_COLOR`/`--json` zero escapes.
- Byte-exact tests for: cached image row, running service row, finished service row, fork prep row, status row — coloured and `NO_COLOR`; verify the codes include the neutral for counters/timings and no `ESC[2m`.
- PTY capture of `up` (cached + fresh) and `fork`; `--json` unchanged fields.

# Acceptance

- `make check`, live `critical_status_live`/`critical_fork_live`; commit `[M3.124] ...`, done, no push.
