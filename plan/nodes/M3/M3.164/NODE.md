---
id: M3.164
title: "Child processes emit colour when asked (env hints, no fake TTY); logs strips colour only at the viewing end"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.157]
paths:
  - "crates/slipfork-world/src/pod_env.rs"
  - "crates/slipfork-image/src/runtime.rs"
  - "crates/slipfork-image/src/identity.rs"
  - "crates/slipfork/src/commands/logs/**"
  - "crates/slipfork/src/daemon/lifecycle/logs/**"
  - "crates/slipfork/src/commands/streams/**"
  - crates/slipfork/src/ui/ansi.rs
  - crates/slipfork/src/ui/mod.rs
  - crates/slipfork/tests/logs_colour_live.rs
  - "crates/slipfork-world/tests/pod_run_live.rs"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T12:29"
updated: "2026-11-25T13:12"
---

# Owner direction

Services print coloured logs in a normal terminal; under Slipfork they arrive plain. Wanted:

- **Encourage colour from the programs we run**, without ever pretending they have a TTY: "MUST NOT make them believe they got a TTY" — so **no pty**, no isatty shim.
- **`slipfork logs` is the one that drops colour** when the viewer's output is not colour-capable (non-TTY, `NO_COLOR`, `--json`). The stored log keeps the raw bytes.
- "GIVE UP these instructions if there's no easy way to get colors out of programs we run" — if a program strictly requires `isatty`, leave it plain and say so; do not hack.

# Contract

1. **Child env (services, resources, build `RUN` steps)**: set unambiguous, widely-honoured colour hints when they are not already set:
   - `TERM` default `xterm-256color` (respect an explicit user value),
   - `CLICOLOR=1`, `CLICOLOR_FORCE=1` (de-facto standard for BSD/GNU tools and many app loggers),
   - `COLORTERM=truecolor`,
   - `FORCE_COLOR=1` only if a documented, non-Node tool honours it — otherwise skip to avoid surprising library behaviour; decide and justify in the node LOG.
   - Never set `TERM=dumb`, never allocate a pty, never fake `isatty`.
2. **Storage**: logs are stored byte-faithful (ANSI included) so a later viewer can choose.
3. **Viewing**: `slipfork logs -r` / `logs --json` / any piped invocation strips SGR (`\x1b[...m`, and other CSI colour sequences) before rendering; a real TTY keeps them. `--json` is always escape-free. Our own gutter/width math must measure after stripping so columns never shear on coloured lines. Also strip on `NO_COLOR`.
4. No double-coding: child ANSI passes through untouched (we do not recolour app output), only our own structural roles use the palette.

# Acceptance

- A service printing `\x1b[31mred\x1b[0m` (log crate with `.force_styling`-style behaviour, or a tiny shell/`printf` probe that keys off `CLICOLOR_FORCE`): the live `up` stream shows it coloured when stdout is a TTY; `slipfork logs -r | cat` and `logs -r --json` contain **zero** ESC; `NO_COLOR=1` zero ESC; the stored run record still contains the raw ESC bytes.
- A program that colours only on `isatty` stays plain, and the node LOG records that as accepted rather than hacks around it.
- Build `RUN` output follows the same rules (streamed + replayed).
- `make check`; Triad (ux: no ANSI leaks in JSON/plain, gutter width on coloured lines); commit `[M3.164] ...`, done, no push.
