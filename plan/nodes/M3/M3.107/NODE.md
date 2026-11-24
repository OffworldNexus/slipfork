---
id: M3.107
title: "Boot-failure and run-replay visual hierarchy; keep captures pure (stream, no synthetic summary)"
kind: epic
status: active
priority: 3
depends_on: []
paths:
  - crates/slipfork/src/boot/render.rs
  - crates/slipfork/src/commands/streams.rs
  - crates/slipfork/src/world/runtime/images/log.rs
  - crates/slipfork/src/daemon/lifecycle/logs.rs
  - crates/slipfork/src/commands/timeparse.rs
  - "crates/slipfork-world/src/pod_log.rs"
  - crates/slipfork/tests/critical_status_live.rs
  - crates/slipfork/tests/critical_world_journey.rs
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-24T12:02"
updated: "2026-11-24T12:04"
---

# Problems (owner screenshot, 0.1.1 dev binary)

1. **Stream label is wrong.** `logs -r` shows the installer's stderr as `out`. `pod_log` stores `O`/`E` correctly, but `daemon/lifecycle/logs.rs::read_captures` emits `line_event(pod, kind, text, ts)` **without the stream**, so the renderer defaults to `out`. Carry `stream` in the capture event and render `err` for `E`.
2. **Synthetic summary inside the capture.** `world/runtime/images/log.rs::BuildLog::fail` appends `build \`api\` failed: …` into the log (and `emit` hardcodes Stdout). A capture is command output only; the failure summary belongs to the envelope outside it.
3. **Header duplicates target/command.** `#5be421  image  api  api` — omit `command` when it equals `target`; for images show the Dockerfile/context when available.
4. **No visual hierarchy.** The failure block and `logs -r` are near-monochrome grey; kinds, targets, states, details, `next:`, timestamps, and `out`/`err` are not visually distinct, and the columns don't align.

See `# Visual contract` below (Opus-owned v2; supersedes every earlier role/colour table, including the per-child acceptance styling).


# Acceptance

- Real `RUN` writing to both streams: `logs -r` shows `err` for stderr, `out` for stdout, no synthetic summary, no duplicate header fields.
- `up` failure screenshot-equivalent in a PTY shows 4+ distinct styles; `NO_COLOR`/`--json` emit zero `\x1b[`.
- `make check`, targeted live tests; commit `[M3.107] ...`, done, no push.
# Visual contract (Opus-owned, v3.2 — keyword magenta per owner; supersedes v1/v2 and every earlier role/colour table)

Mental model: **we're running X → here is what it said → it settled → here's what to do next.**
The phase row is the anchor (by position + bold target); output is subordinate (indented gutter); metadata recedes (dim). No `out`/`err` words, no stream column.

| Role | Exact SGR | Used for (same role → same style on every surface) |
|---|---|---|
| handle | bold `1` | `#<run id>` in the replay header ONLY |
| subject | bold `1` | the target name (`api`) — header, phase row, verdict, `✓ image` row. Never blue, never any colour |
| content | neutral (no SGR) | command/args (phase row + header), the failure reason, every child output line, `→`, the spinner, `done` |
| keyword | magenta `35` (`Palette::verb`) | instruction/verb/kind: `FROM`, `ADD`, `RUN`, `COPY`, `WORKDIR`, `start`, `setup`, `hook`, `image` — step rows, `logs -r` header, verdicts |
| metadata | dim `2` | `[i/n]`, elapsed/timings, timestamps, `(exit N)`, `(N steps, Xs)`, `next:` label, stdout gutter `│` |
| stderr channel | yellow `33` | the stderr gutter glyph `┃` ONLY |
| failure | red `31` | `✗` and the word `failed` ONLY |
| success | green `32` | `✓`, `ready`, `running` ONLY |
| actionable | cyan `36` | the copy-pasteable command after `next:`; URLs `36;4` |

Rule (owner, v3.2): every hue is bound to exactly one role; `palette.label`/blue and `palette.kind`/cyan are banned for verbs and targets. A golden unit test pins the exact bytes of the header, a phase row, a `✓` row and a verdict.

SGR 1/2/31/32/33/36 only — readable on dark and light themes, no bright 9x codes, no backgrounds.

## Stream gutter (one rule everywhere)
- Line shape: 2-space indent, gutter glyph, 1 space, text. Text starts at column 4, under the counter of the phase row; nothing else sits between gutter and text.
- stdout = `│` (U+2502 light bar, dim). stderr = `┃` (U+2503 heavy bar, yellow). The glyphs differ by weight, so `NO_COLOR`/non-TTY stays unambiguous with zero escapes and no words.
- Long lines: in scrollback they wrap with the same gutter glyph repeated on continuation rows; inside the live region they truncate to the width with `…` (ANSI-stripped measuring).

## `up` — the narrative
```
✓ [1/4] FROM  api  python:3.13-slim                      1.2s
✓ [2/4] COPY  api  . .                                   0.1s
→ [3/4] RUN   api  sh -c "curl -fsSL https://offworld-nexus.app/install.sh | sh && apt install blastit"
  │ downloading installer…
  ┃ offworld-nexus: run as root: curl -fsSL https://offworld-nexus.app/install.sh | sudo sh
✗ RUN api failed (exit 1)
  next: slipfork logs -r 5be421
```
- Phase row: glyph (`→` neutral while it runs; spinner in the live region; `✓` green / `✗` red once settled), `[i/n]` dim, instruction dim, target bold, args neutral. Instruction and target columns are padded to this image's max visible width (ANSI-stripped). Elapsed dim, right after the args, on settled rows.
- `[i/n]` = instruction index / count of THIS image's Dockerfile; service/hook steps use the boot step counter and verb `start`/`hook` + their command.
- Verdict: column 0, one line, the same verb token as the phase row: `✗` red, verb dim, target bold, `failed` red, then `(exit N)` dim for RUN or `: <reason>` neutral when no gutter line above carries it. Remedy: `  next: ` dim + one cyan command. Exactly one. No `Failed to …` sentence, no repeated output, no `build … failed:` prose.
- `crun`/container plumbing only with `-v`: one dim `  runtime: crun exited with code 1` row between verdict and `next:`.

## Live Dockerfile build (owner addendum — non-negotiable)
- Every Dockerfile instruction is a phase row printed BEFORE it executes. Running step (TTY): spinner glyph + ticking dim elapsed, so pull/COPY/context walk never freeze; `RUN`/`ADD` output streams under it in the gutter shape. While running, the step + its last <= 8 output lines live inside the anchored `DECSTBM` region; scrollback only receives settled rows.
- Settled success: the step collapses to ONE scrollback row `✓ [i/n] INSTR target args  1.2s`. Build end: `✓ image api (4 steps, 12.3s)` (✓ green, `api` bold, parens dim).
- Settled failure: earlier steps are already collapsed; the failing step prints expanded into scrollback — `✗`-glyph phase row, its FULL captured output in the gutter shape, the verdict, one `next:`.
- Region restored on exit/ctrl-C; no prompt corruption.

## `logs -r <id>` — the same story, replayed
```
#5be421  RUN  api  sh -c "curl -fsSL https://offworld-nexus.app/install.sh | sh && apt install blastit"  2026-11-24 10:59:40

  10:59:41.019 │ downloading installer…
  10:59:41.026 ┃ offworld-nexus: run as root: curl -fsSL https://offworld-nexus.app/install.sh | sudo sh

✗ RUN api failed (exit 1)
```
- Header mirrors the phase row: `#id` bold, instruction/kind dim, target bold, command neutral (omitted when == target; never the image id twice), started dim. One blank line.
- Rows: 2-space indent, `HH:MM:SS.mmm` dim, then the gutter glyph, 1 space, neutral text — the timestamp is part of the dim gutter, not a column.
- One blank line, then the verdict from the run RECORD (never the capture): same line as `up`, or `✓ RUN api done (0.8s)`. No `next:`.

## Collapse rules
- `NO_COLOR` / non-TTY (CI): append-only — phase row (`→`) when a step starts, every output line in the gutter shape (`│`/`┃`), `✓ [i/n] …  1.2s` when it settles, verdict + `next:`; zero `\x1b[`, no spinner frames.
- `--json`: NDJSON only, zero escapes, no pre-rendered strings. `up`: `step{image,index,total,instruction,args,state:"started"|"done"|"failed",elapsed_ms,exit_code?}`, `log{image,index,stream:"stdout"|"stderr",line,ts_ms}`, `failed{phase,target,instruction,source,exit_code,run_id,next,runtime}`. `logs -r --json`: the run record (`phase`,`instruction`,`source`,`state`,`exit_code`) then `{stream,line,ts_ms}` lines.

## Engine-step failures explain themselves (owner regression, see M3.107.4)
Every instruction kind gets its step note at start; an engine-executed step's one-line error is that step's `┃` output in the capture (never wrapper prose); records carry `reason`; a step-less failure puts the reason in the verdict (`✗ image api failed: <reason>`). No failure replays as an empty block.
