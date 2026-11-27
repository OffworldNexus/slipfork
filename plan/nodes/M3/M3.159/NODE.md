---
id: M3.159
title: "Append-only output prints every step twice and out of instruction order: one row per step, in order"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork/src/boot/**", crates/slipfork/tests/boot_pty.rs]
attempts: 3
max_attempts: 3
escalation: none
created: "2026-11-25T11:32"
updated: "2026-11-25T11:48"
---

# Owner report (real `up --no-cache` output)

```
→ [1/2] image  api  running
  →  2/11 copy     --from=ghcr.io/astral-sh/uv:latest
  →  1/11 from     python:3.14-trixie
  ✓  1/11 from     python:3.14-trixie  11.2s
  →  2/11 copy     --from=ghcr.io/astral-sh/uv:latest /uv /uvx /bin/
  ✓  2/11 copy     …  52ms
…
→ 11/11 cmd      uv run blastit
✓ 11/11 cmd      uv run blastit  <1ms
```

Owner: "am I supposed to see every line twice with the arrow and the check?"

No. Two defects: (a) each instruction emits a `→ … running` line **and** a `✓ …` line (append-only made the started frames visible as separate lines), and (b) rows appear in *emission* order (`2/11` before `1/11`), violating the "rows in instruction order" contract from M3.113.

# Contract

- **One line per completed instruction**, printed in **plan order** (`1/11 … 11/11`): buffer a completed step until every earlier step in its image has been printed, then print in index order. No `→` line for an instruction that carries no streamed output.
- A step that streams child output gets **one header line** (`→  5/11 run  sh /tmp/oni.sh …`) at the moment its output starts (never a second identical line), then `│` output lines, then the single verdict (`✓ 5/11 run … 3.5s` / `✗ … failed (exit 1)`).
- Outer plan rows (`image`, `start`, `setup`, `fork`): one verdict line each; a service that streams gets the same once-only header + gutters. No duplicate `→`/`✓` pair for the same row.
- Keep the single in-place status line, tail phases, padded counters, `cached 9 · built 2`, and zero escapes on `NO_COLOR`/`--json`/pipes.

# Acceptance

- Byte-exact PTY test with the owner's 11-instruction shape (including a parallel `copy --from`): exactly 11 verdict lines in index order, no duplicate row text, header only for the streaming step(s), and a failing step yielding one `✗` row with its output above it.
- Real PTY captures (110×30) of cached and `--no-cache` builds.
- `make check`; Triad; commit `[M3.159] ...`, done, no push.
